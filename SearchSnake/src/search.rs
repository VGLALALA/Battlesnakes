//! Iterative-deepening maximin search.
//!
//! # Simultaneous moves
//!
//! Battlesnake resolves every snake's move at once, so there is no natural
//! turn order to alternate over. One game turn is modelled as a *pair* of
//! layers: a max layer choosing our move, then a min layer choosing the
//! opponents' joint reply, after which all moves are applied together. That
//! keeps alpha-beta valid while giving the opponents the benefit of seeing our
//! move — the pessimistic reading, which is the right bias when the cost of
//! being wrong is dying.
//!
//! # Locality
//!
//! Enumerating every opponent's moves jointly costs `3^k` per turn, which
//! collapses the reachable depth in a four-snake game. An opponent whose head
//! is further than the remaining horizon cannot affect us, so it is given a
//! single cheap policy move instead of a branch. Of those close enough to
//! matter, only the nearest [`Config::max_enumerated`] are branched on. In a
//! one-on-one — where ladder placement is actually decided — nothing is
//! approximated.
//!
//! # Losing positions
//!
//! Pure maximin is indifferent between two moves that both lose: it reports
//! `-MATE` for each. But a move that loses only to one precise reply is far
//! better in practice than one that loses to everything, because opponents err.
//! When every root move is a loss, the root re-ranks by the *mean* over
//! opponent replies instead of the minimum, which picks the line that gives
//! the opponent the most chances to go wrong.

use std::time::{Duration, Instant};

use crate::eval::{self, Weights, DRAW, MATE};
use crate::fill::Scratch;
use crate::state::{Cell, Dir, Rules, State, DIRS, MAX_SNAKES, NO_CELL, UP};

const INF: i32 = i32::MAX / 2;
const MAX_PLY: usize = 96;
/// Not a direction. Marks a ply with no best move recorded yet, so the first
/// iteration does not promote an arbitrary one on its merits.
const NO_KILLER: Dir = u8::MAX;
/// A score this close to `MATE` means the line is decided; no point searching on.
const DECIDED: i32 = MATE - 10_000;

#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub budget: Duration,
    pub max_depth: i32,
    /// Most opponents branched on jointly at any one node.
    pub max_enumerated: usize,
    pub weights: Weights,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            budget: Duration::from_millis(300),
            max_depth: 40,
            max_enumerated: 2,
            weights: Weights::DEFAULT,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Outcome {
    pub best: Dir,
    pub score: i32,
    /// Deepest fully completed iteration.
    pub depth: i32,
    pub nodes: u64,
    pub elapsed: Duration,
}

/// A fixed-capacity move list; the hot path must not allocate.
#[derive(Clone, Copy)]
struct MoveList {
    moves: [Dir; 4],
    len: usize,
}

impl MoveList {
    fn as_slice(&self) -> &[Dir] {
        &self.moves[..self.len]
    }
}

pub struct Searcher {
    scratch: Scratch,
    config: Config,
    deadline: Instant,
    nodes: u64,
    aborted: bool,
    /// Best move found at each ply in previous iterations, tried first.
    killer: [Dir; MAX_PLY],
}

impl Searcher {
    pub fn new(config: Config) -> Self {
        Searcher {
            scratch: Scratch::new(),
            config,
            deadline: Instant::now(),
            nodes: 0,
            aborted: false,
            killer: [NO_KILLER; MAX_PLY],
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn set_weights(&mut self, weights: Weights) {
        self.config.weights = weights;
    }

    pub fn set_budget(&mut self, budget: Duration) {
        self.config.budget = budget;
    }

    /// Picks a move for snake `me`, deepening until the time budget runs out.
    pub fn find_move(&mut self, state: &State, rules: &Rules, me: usize) -> Outcome {
        let started = Instant::now();
        self.deadline = started + self.config.budget;
        self.nodes = 0;
        self.aborted = false;
        self.killer = [NO_KILLER; MAX_PLY];

        let mut candidates = self.my_moves(state, rules, me);
        // A forced move needs no search at all.
        if candidates.len == 1 {
            return Outcome {
                best: candidates.moves[0],
                score: 0,
                depth: 0,
                nodes: 0,
                elapsed: started.elapsed(),
            };
        }

        let mut best = candidates.moves[0];
        let mut best_score = 0;
        let mut completed = 0;

        for depth in 1..=self.config.max_depth {
            let mut scored: Vec<(Dir, i32)> = Vec::with_capacity(candidates.len);
            let mut alpha = -INF;
            let mut iteration_best = candidates.moves[0];
            let mut iteration_score = -INF;

            for &m in candidates.as_slice() {
                let value = self.min_layer(state, rules, me, m, depth, 0, alpha, INF, None);
                if self.aborted {
                    break;
                }
                scored.push((m, value));
                if value > iteration_score {
                    iteration_score = value;
                    iteration_best = m;
                }
                alpha = alpha.max(value);
            }

            if self.aborted {
                break;
            }

            // Every move loses, and maximin cannot tell them apart. Re-rank by
            // the mean over replies, which favours the loss an opponent has to
            // play precisely to collect.
            if iteration_score <= -DECIDED {
                iteration_best = self.rank_doomed(state, rules, me, &candidates);
            }

            best = iteration_best;
            best_score = iteration_score;
            completed = depth;

            // Re-order the root for the next iteration: best first.
            scored.sort_by_key(|&(_, v)| -v);
            for (i, &(m, _)) in scored.iter().enumerate() {
                candidates.moves[i] = m;
            }

            if best_score >= DECIDED || best_score <= -DECIDED {
                break;
            }
        }

        Outcome {
            best,
            score: best_score,
            depth: completed,
            nodes: self.nodes,
            elapsed: started.elapsed(),
        }
    }

    // -- layers ------------------------------------------------------------

    /// Our move choice. Returns the value of the best move available.
    fn max_layer(
        &mut self,
        state: &State,
        rules: &Rules,
        me: usize,
        depth: i32,
        ply: usize,
        mut alpha: i32,
        beta: i32,
    ) -> i32 {
        // Ticked before the early returns so every visited node is counted;
        // short-circuiting past it made leaves — most of the tree — invisible.
        let out_of_time = self.tick();
        if let Some(score) = terminal(state, rules, me, ply) {
            return score;
        }
        if depth <= 0 || ply + 1 >= MAX_PLY || out_of_time {
            return eval::evaluate(state, rules, me, &self.config.weights, &mut self.scratch);
        }

        let mut candidates = self.my_moves(state, rules, me);
        order_mine(&mut candidates, state, rules, me, self.killer[ply]);

        let mut best = -INF;
        for &m in candidates.as_slice() {
            let value = self.min_layer(state, rules, me, m, depth, ply, alpha, beta, None);
            if value > best {
                best = value;
                self.killer[ply] = m;
            }
            alpha = alpha.max(best);
            if alpha >= beta {
                break;
            }
        }
        best
    }

    /// The opponents' joint reply to our `my_move`, then the turn is applied.
    ///
    /// `stats`, when supplied, accumulates every reply's value so the root can
    /// compute a mean alongside the minimum.
    #[allow(clippy::too_many_arguments)]
    fn min_layer(
        &mut self,
        state: &State,
        rules: &Rules,
        me: usize,
        my_move: Dir,
        depth: i32,
        ply: usize,
        alpha: i32,
        mut beta: i32,
        mut stats: Option<&mut Stats>,
    ) -> i32 {
        let n = rules.n as usize;

        // Opponents close enough to interact within the remaining horizon get
        // branched on; the rest move by policy.
        let mut base = [UP; MAX_SNAKES];
        base[me] = my_move;
        let mut branched: [usize; MAX_SNAKES] = [0; MAX_SNAKES];
        let mut branched_moves: [MoveList; MAX_SNAKES] = [MoveList {
            moves: [UP; 4],
            len: 0,
        }; MAX_SNAKES];
        let mut branched_len = 0usize;

        let horizon = 2 * depth + 2;
        let mut nearby: [(i32, usize); MAX_SNAKES] = [(0, 0); MAX_SNAKES];
        let mut nearby_len = 0usize;
        for o in 0..n {
            if o == me || !state.is_alive(o) {
                continue;
            }
            let d = rules.dist(state.head[me], state.head[o]);
            if d <= horizon {
                nearby[nearby_len] = (d, o);
                nearby_len += 1;
            } else {
                // Too far to reach us before the horizon, so its exact choice
                // cannot change our score.
                base[o] = self.policy_move(state, rules, o);
            }
        }
        nearby[..nearby_len].sort_unstable();
        for rank in 0..nearby_len {
            let o = nearby[rank].1;
            if rank < self.config.max_enumerated {
                let mut list = self.opponent_moves(state, rules, o);
                order_against(&mut list, state, rules, o, state.head[me]);
                branched[branched_len] = o;
                branched_moves[branched_len] = list;
                branched_len += 1;
            } else {
                base[o] = self.policy_move(state, rules, o);
            }
        }

        let mut worst = INF;
        let mut counters = [0usize; MAX_SNAKES];

        loop {
            let mut moves = base;
            for i in 0..branched_len {
                moves[branched[i]] = branched_moves[i].moves[counters[i]];
            }

            let mut child = state.clone();
            child.apply(rules, &moves);
            let value = self.max_layer(&child, rules, me, depth - 1, ply + 1, alpha, beta);

            if let Some(s) = stats.as_deref_mut() {
                s.sum += value as i64;
                s.count += 1;
            }
            worst = worst.min(value);
            beta = beta.min(worst);

            // With stats requested the root wants every reply, so pruning is
            // suppressed; it only ever asks at the shallowest depths.
            if beta <= alpha && stats.is_none() {
                break;
            }
            if self.aborted {
                break;
            }

            // Odometer over the branched opponents' move lists.
            let mut i = 0;
            loop {
                if i == branched_len {
                    return worst;
                }
                counters[i] += 1;
                if counters[i] < branched_moves[i].len {
                    break;
                }
                counters[i] = 0;
                i += 1;
            }
        }
        worst
    }

    // -- helpers -----------------------------------------------------------

    /// Ranks moves by the mean over opponent replies instead of the worst case.
    /// Only reached when maximin says every move loses, where it is the
    /// difference between resigning and making the opponent find the one reply
    /// that finishes us. Deliberately shallow: by this point the clock is
    /// usually spent, and the ordering matters more than its precision.
    fn rank_doomed(
        &mut self,
        state: &State,
        rules: &Rules,
        me: usize,
        candidates: &MoveList,
    ) -> Dir {
        let mut best = candidates.moves[0];
        let mut best_mean = i64::MIN;
        for &m in candidates.as_slice() {
            let mut stats = Stats::default();
            self.min_layer(state, rules, me, m, 2, 0, -INF, INF, Some(&mut stats));
            let mean = stats.sum / i64::from(stats.count.max(1));
            if mean > best_mean {
                best_mean = mean;
                best = m;
            }
        }
        best
    }

    #[inline]
    fn tick(&mut self) -> bool {
        self.nodes += 1;
        if self.nodes & 0x3FF == 0 && Instant::now() >= self.deadline {
            self.aborted = true;
        }
        self.aborted
    }

    fn my_moves(&self, state: &State, rules: &Rules, me: usize) -> MoveList {
        let mut list = MoveList {
            moves: [UP; 4],
            len: 0,
        };
        for &d in &DIRS {
            if let Some(cell) = rules.step(state.head[me], d) {
                if !state.blocked_on_arrival(cell) {
                    list.moves[list.len] = d;
                    list.len += 1;
                }
            }
        }
        if list.len == 0 {
            // Doomed, but a move still has to be returned.
            for &d in &DIRS {
                if rules.step(state.head[me], d).is_some() {
                    list.moves[list.len] = d;
                    list.len += 1;
                }
            }
            if list.len == 0 {
                list.moves[0] = UP;
                list.len = 1;
            }
        }
        list
    }

    fn opponent_moves(&self, state: &State, rules: &Rules, o: usize) -> MoveList {
        self.my_moves(state, rules, o)
    }

    /// A cheap, sensible move for an opponent too far away to branch on.
    /// Prefers open space, pulls toward food when hungry, and avoids heads it
    /// would lose to.
    fn policy_move(&self, state: &State, rules: &Rules, s: usize) -> Dir {
        let list = self.my_moves(state, rules, s);
        let mut best = list.moves[0];
        let mut best_score = i32::MIN;
        let hungry = state.health[s] < 50;

        for &d in list.as_slice() {
            let Some(cell) = rules.step(state.head[s], d) else {
                continue;
            };
            let mut score = 0i32;
            // Open space: count exits from the target cell.
            for &nb in rules.neighbors_of(cell) {
                if nb != NO_CELL && !state.blocked_on_arrival(nb) {
                    score += 12;
                }
            }
            if hungry || state.food.contains(cell) {
                if let Some(fd) = nearest_food(state, rules, cell) {
                    score -= 3 * fd;
                }
            }
            // Give way to anything it would lose a head-to-head against.
            for o in 0..rules.n as usize {
                if o == s || !state.is_alive(o) || state.len[o] < state.len[s] {
                    continue;
                }
                if rules.dist(cell, state.head[o]) == 1 {
                    score -= 40;
                }
            }
            score -= rules.dist(
                cell,
                rules.cell(rules.width as i32 / 2, rules.height as i32 / 2),
            );
            if score > best_score {
                best_score = score;
                best = d;
            }
        }
        best
    }
}

#[derive(Default)]
struct Stats {
    sum: i64,
    count: u32,
}

/// Scores a position that has already been decided, or `None` if play goes on.
/// Mate scores fold in `ply` so the search prefers winning sooner and dying
/// later — the difference between a snake that fights on and one that gives up.
fn terminal(state: &State, rules: &Rules, me: usize, ply: usize) -> Option<i32> {
    let alive = state.is_alive(me);
    let rivals = state.alive_count() - u32::from(alive);
    if !alive {
        return Some(if rivals == 0 {
            DRAW + ply as i32
        } else {
            -MATE + ply as i32
        });
    }
    // A solo game has no one to outlive, so it is never won — only survived.
    if rivals == 0 && rules.n > 1 {
        return Some(MATE - ply as i32);
    }
    None
}

/// Orders our own moves: whatever worked at this ply last time first, then by
/// how much room the target square opens onto.
///
/// Alpha-beta only pays when the best move is tried first — a perfectly
/// ordered tree is the square root of an unordered one — and the cheapest
/// usable signal for "best" in Battlesnake is space.
fn order_mine(list: &mut MoveList, state: &State, rules: &Rules, me: usize, killer: Dir) {
    if list.len < 2 {
        return;
    }
    let head = state.head[me];
    let mut keyed: [(i32, Dir); 4] = [(0, UP); 4];
    for i in 0..list.len {
        let d = list.moves[i];
        let cell = rules.step(head, d).unwrap_or(head);
        let mut score = 0;
        for &nb in rules.neighbors_of(cell) {
            if nb != NO_CELL && !state.blocked_on_arrival(nb) {
                score += 2;
            }
        }
        if state.food.contains(cell) {
            score += 1;
        }
        if d == killer {
            score += 100;
        }
        // Negated so the plain ascending sort puts the best first.
        keyed[i] = (-score, d);
    }
    keyed[..list.len].sort_unstable();
    for i in 0..list.len {
        list.moves[i] = keyed[i].1;
    }
}

/// Orders an opponent's replies so the ones that crowd our head come first;
/// finding the refutation early is what makes alpha-beta pay.
fn order_against(list: &mut MoveList, state: &State, rules: &Rules, o: usize, my_head: Cell) {
    let head = state.head[o];
    let mut keyed: [(i32, Dir); 4] = [(0, UP); 4];
    for i in 0..list.len {
        let d = list.moves[i];
        let cell = rules.step(head, d).unwrap_or(head);
        keyed[i] = (rules.dist(cell, my_head), d);
    }
    keyed[..list.len].sort_unstable();
    for i in 0..list.len {
        list.moves[i] = keyed[i].1;
    }
}

fn nearest_food(state: &State, rules: &Rules, from: Cell) -> Option<i32> {
    state.food.iter().map(|f| rules.dist(from, f)).min()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{LEFT, RIGHT};

    fn fast() -> Config {
        Config {
            budget: Duration::from_millis(120),
            max_depth: 12,
            ..Config::default()
        }
    }

    fn place(rules: &Rules, st: &mut State, s: usize, body: &[(i32, i32)], health: i16) {
        let cells: Vec<Cell> = body.iter().map(|&(x, y)| rules.cell(x, y)).collect();
        st.place_snake(s, &cells, health);
    }

    fn pick(st: &State, r: &Rules, me: usize) -> Dir {
        Searcher::new(fast()).find_move(st, r, me).best
    }

    #[test]
    fn never_walks_into_a_wall() {
        let r = Rules::new(11, 11, 1);
        let mut st = State::empty();
        place(&r, &mut st, 0, &[(0, 0), (0, 1), (0, 2)], 100);
        assert_eq!(pick(&st, &r, 0), RIGHT, "the only move that is not a wall");
    }

    #[test]
    fn takes_the_safe_side_of_a_fork() {
        let r = Rules::new(11, 11, 1);
        let mut st = State::empty();
        // Left leads into a 2-cell pocket; right opens onto the whole board.
        let mut body = vec![(5, 5)];
        for y in (0..5).rev() {
            body.push((5, y));
        }
        for x in 0..5 {
            body.push((x, 0));
        }
        body.push((0, 1));
        body.push((0, 2));
        place(&r, &mut st, 0, &body, 100);
        let chosen = pick(&st, &r, 0);
        assert!(
            chosen == RIGHT || chosen == UP,
            "must escape toward open board, got {}",
            crate::state::dir_name(chosen)
        );
    }

    #[test]
    fn eats_rather_than_starving() {
        let r = Rules::new(11, 11, 1);
        let mut st = State::empty();
        // One point of health left: every move but the one onto food starves.
        place(&r, &mut st, 0, &[(5, 5), (5, 4), (5, 3)], 1);
        st.food.insert(r.cell(5, 6));
        assert_eq!(pick(&st, &r, 0), UP, "the only move that survives");
    }

    #[test]
    fn wins_a_head_to_head_it_cannot_lose() {
        let r = Rules::new(11, 11, 2);
        let mut st = State::empty();
        // We are longer and the only contact square is (5,5).
        place(&r, &mut st, 0, &[(5, 4), (5, 3), (5, 2), (5, 1), (5, 0)], 100);
        place(&r, &mut st, 1, &[(5, 6), (5, 7), (5, 8)], 100);
        let out = Searcher::new(fast()).find_move(&st, &r, 0);
        assert!(out.score > 0, "being longer and in contact is good for us");
    }

    #[test]
    fn refuses_a_head_to_head_it_would_lose() {
        let r = Rules::new(11, 11, 2);
        let mut st = State::empty();
        // Snake 1 is longer; moving up to (5,5) risks a losing collision.
        place(&r, &mut st, 0, &[(5, 4), (5, 3), (5, 2)], 100);
        place(&r, &mut st, 1, &[(5, 6), (5, 7), (5, 8), (5, 9), (5, 10)], 100);
        assert_ne!(pick(&st, &r, 0), UP, "must not contest a square it loses");
    }

    #[test]
    fn refuses_to_enter_a_pocket_too_small_to_live_in() {
        let r = Rules::new(11, 11, 2);
        let mut st = State::empty();
        // Snake 1 is long and walls off the top-right corner, leaving one way
        // in at (10,7). Its wall cells sit near its head, so they stay put
        // long enough for the corner to be a genuine dead end.
        let wall = [
            (6, 10),
            (6, 9),
            (6, 8),
            (6, 7),
            (7, 7),
            (8, 7),
            (9, 7),
            (9, 6),
            (8, 6),
            (7, 6),
            (6, 6),
            (5, 6),
            (4, 6),
            (3, 6),
            (2, 6),
            (1, 6),
            (0, 6),
            (0, 5),
            (0, 4),
            (0, 3),
        ];
        place(&r, &mut st, 1, &wall, 100);
        // We are 18 long, with the corner pocket holding only 13 cells.
        let mine = [
            (10, 5),
            (10, 4),
            (10, 3),
            (10, 2),
            (10, 1),
            (10, 0),
            (9, 0),
            (8, 0),
            (7, 0),
            (6, 0),
            (5, 0),
            (4, 0),
            (3, 0),
            (2, 0),
            (1, 0),
            (0, 0),
            (0, 1),
            (0, 2),
        ];
        place(&r, &mut st, 0, &mine, 100);

        // Confirm the premise before testing the choice, so a mistake in the
        // setup fails loudly instead of quietly testing nothing.
        let after = |d: Dir| {
            let mut child = st.clone();
            let mut m = [UP; MAX_SNAKES];
            m[0] = d;
            m[1] = LEFT; // keeps the wall standing
            child.apply(&r, &m);
            assert!(child.is_alive(0), "both candidate moves are legal");
            crate::fill::reach(&child, &r, 0, &mut Scratch::new())
        };
        let into_pocket = after(UP);
        let into_open = after(LEFT);
        assert!(
            into_pocket < st.len[0] as i32,
            "premise: the pocket ({} cells) must be smaller than our {} length",
            into_pocket,
            st.len[0]
        );
        assert!(
            into_open > 2 * into_pocket,
            "premise: the open side ({}) must be far roomier than the pocket ({})",
            into_open,
            into_pocket
        );

        assert_eq!(pick(&st, &r, 0), LEFT, "must not shut itself in the corner");
    }

    #[test]
    fn mate_scores_prefer_winning_sooner() {
        let r = Rules::new(11, 11, 2);
        let mut st = State::empty();
        place(&r, &mut st, 0, &[(5, 5), (5, 4), (5, 3)], 100);
        st.alive = 1; // opponent already gone
        assert_eq!(terminal(&st, &r, 0, 0), Some(MATE));
        assert_eq!(terminal(&st, &r, 0, 4), Some(MATE - 4));
    }

    #[test]
    fn mate_scores_prefer_dying_later() {
        let r = Rules::new(11, 11, 2);
        let mut st = State::empty();
        place(&r, &mut st, 1, &[(5, 5), (5, 4), (5, 3)], 100);
        assert_eq!(terminal(&st, &r, 0, 2), Some(-MATE + 2));
        assert!(terminal(&st, &r, 0, 9).unwrap() > terminal(&st, &r, 0, 2).unwrap());
    }

    #[test]
    fn mutual_destruction_beats_losing_alone() {
        let r = Rules::new(11, 11, 2);
        let st = State::empty(); // nobody alive
        let draw = terminal(&st, &r, 0, 1).unwrap();
        assert_eq!(draw, DRAW + 1);
        assert!(draw > -MATE + 1);
    }

    #[test]
    fn solo_games_are_never_won() {
        let r = Rules::new(11, 11, 1);
        let mut st = State::empty();
        place(&r, &mut st, 0, &[(5, 5), (5, 4), (5, 3)], 100);
        assert_eq!(terminal(&st, &r, 0, 0), None);
    }

    #[test]
    fn a_forced_move_short_circuits_the_search() {
        let r = Rules::new(11, 11, 1);
        let mut st = State::empty();
        place(&r, &mut st, 0, &[(0, 0), (0, 1), (0, 2)], 100);
        let out = Searcher::new(fast()).find_move(&st, &r, 0);
        assert_eq!(out.nodes, 0, "no search needed when there is one legal move");
        assert_eq!(out.best, RIGHT);
    }

    #[test]
    fn respects_the_time_budget() {
        let r = Rules::new(11, 11, 4);
        let mut st = State::empty();
        place(&r, &mut st, 0, &[(2, 2), (2, 1), (2, 0)], 100);
        place(&r, &mut st, 1, &[(8, 8), (8, 9), (8, 10)], 100);
        place(&r, &mut st, 2, &[(2, 8), (1, 8), (0, 8)], 100);
        place(&r, &mut st, 3, &[(8, 2), (9, 2), (10, 2)], 100);
        let budget = Duration::from_millis(100);
        let mut s = Searcher::new(Config {
            budget,
            ..Config::default()
        });
        let out = s.find_move(&st, &r, 0);
        assert!(
            out.elapsed < budget * 4,
            "overran the budget: {:?}",
            out.elapsed
        );
        assert!(out.depth >= 1, "completed at least one iteration");
    }

    #[test]
    fn deeper_search_sees_further() {
        let r = Rules::new(11, 11, 1);
        let mut st = State::empty();
        place(&r, &mut st, 0, &[(5, 5), (5, 4), (5, 3)], 100);
        let shallow = Searcher::new(Config {
            budget: Duration::from_millis(5),
            max_depth: 2,
            ..Config::default()
        })
        .find_move(&st, &r, 0);
        let deep = Searcher::new(Config {
            budget: Duration::from_millis(250),
            max_depth: 20,
            ..Config::default()
        })
        .find_move(&st, &r, 0);
        assert!(deep.depth > shallow.depth);
    }

    #[test]
    fn ordering_puts_the_killer_first_and_keeps_every_move() {
        let r = Rules::new(11, 11, 1);
        let mut st = State::empty();
        place(&r, &mut st, 0, &[(5, 5), (5, 4), (5, 3)], 100);
        let mut list = MoveList {
            moves: [UP, LEFT, RIGHT, UP],
            len: 3,
        };
        order_mine(&mut list, &st, &r, 0, LEFT);
        assert_eq!(list.moves[0], LEFT, "the killer move is tried first");
        let mut all: Vec<Dir> = list.as_slice().to_vec();
        all.sort();
        assert_eq!(all, vec![UP, LEFT, RIGHT], "no move is dropped");
    }

    #[test]
    fn ordering_prefers_the_roomier_square() {
        let r = Rules::new(11, 11, 1);
        let mut st = State::empty();
        // Head in the bottom-left corner: UP and RIGHT both run along a wall,
        // but RIGHT is the neck side, so UP opens onto more space.
        place(&r, &mut st, 0, &[(1, 0), (2, 0), (3, 0)], 100);
        let mut list = MoveList {
            moves: [LEFT, UP, UP, UP],
            len: 2,
        };
        order_mine(&mut list, &st, &r, 0, NO_KILLER);
        assert_eq!(
            list.moves[0], UP,
            "away from the wall beats into the corner"
        );
    }

    #[test]
    fn survives_a_crowded_board_without_panicking() {
        let r = Rules::new(11, 11, 4);
        let mut st = State::empty();
        place(&r, &mut st, 0, &[(5, 5), (5, 4), (5, 3)], 80);
        place(&r, &mut st, 1, &[(4, 7), (4, 8), (4, 9)], 80);
        place(&r, &mut st, 2, &[(7, 5), (8, 5), (9, 5)], 80);
        place(&r, &mut st, 3, &[(2, 2), (1, 2), (0, 2)], 80);
        st.food.insert(r.cell(5, 7));
        let out = Searcher::new(fast()).find_move(&st, &r, 0);
        let cell = r.step(st.head[0], out.best).expect("stays on the board");
        assert!(!st.blocked_on_arrival(cell), "chose a survivable square");
    }
}
