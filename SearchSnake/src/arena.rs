//! Self-play harness.
//!
//! Strength claims about a game-playing program are only worth what the games
//! say, so the arena runs full games under the official rules — including food
//! spawning, which the search deliberately ignores — and reports records.
//!
//! Games are seeded and agents are given a fixed `max_depth` rather than a
//! wall-clock budget, so a run is reproducible: the same seed and the same
//! agents always play the same game.

use crate::eval::{self, Weights};
use crate::fill::{self, Scratch};
use crate::search::{Config, Searcher};
use crate::state::{Cell, Dir, Rules, State, DIRS, MAX_SNAKES, NO_CELL, UP};

/// `splitmix64`. Small, seeded, and good enough to place food with.
#[derive(Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.wrapping_add(0x9E37_79B9_7F4A_7C15))
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() % n as u64) as usize
        }
    }

    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            items.swap(i, self.below(i + 1));
        }
    }
}

pub trait Agent {
    fn name(&self) -> &str;
    fn choose(&mut self, state: &State, rules: &Rules, me: usize) -> Dir;
}

/// The real agent: full iterative-deepening search.
pub struct SearchAgent {
    label: String,
    searcher: Searcher,
}

impl SearchAgent {
    pub fn new(label: impl Into<String>, config: Config) -> Self {
        SearchAgent {
            label: label.into(),
            searcher: Searcher::new(config),
        }
    }

    /// Fixed-depth and generously timed, so games are reproducible.
    pub fn at_depth(label: impl Into<String>, depth: i32, weights: Weights) -> Self {
        Self::new(
            label,
            Config {
                budget: std::time::Duration::from_secs(30),
                max_depth: depth,
                weights,
                ..Config::default()
            },
        )
    }
}

impl Agent for SearchAgent {
    fn name(&self) -> &str {
        &self.label
    }

    fn choose(&mut self, state: &State, rules: &Rules, me: usize) -> Dir {
        self.searcher.find_move(state, rules, me).best
    }
}

/// One ply of the same evaluation and no opponent modelling. Isolates what the
/// search depth is actually buying.
pub struct GreedyAgent {
    label: String,
    scratch: Scratch,
    weights: Weights,
}

impl GreedyAgent {
    pub fn new(label: impl Into<String>) -> Self {
        GreedyAgent {
            label: label.into(),
            scratch: Scratch::new(),
            weights: Weights::DEFAULT,
        }
    }
}

impl Agent for GreedyAgent {
    fn name(&self) -> &str {
        &self.label
    }

    fn choose(&mut self, state: &State, rules: &Rules, me: usize) -> Dir {
        let mut best = UP;
        let mut best_score = i32::MIN;
        for &d in &state.legal_moves(rules, me) {
            let mut child = state.clone();
            let mut moves = [UP; MAX_SNAKES];
            moves[me] = d;
            for o in 0..rules.n as usize {
                if o != me && child.is_alive(o) {
                    moves[o] = straight_or_safe(&child, rules, o);
                }
            }
            child.apply(rules, &moves);
            let score = if !child.is_alive(me) {
                -eval::MATE
            } else {
                eval::evaluate(&child, rules, me, &self.weights, &mut self.scratch)
            };
            if score > best_score {
                best_score = score;
                best = d;
            }
        }
        best
    }
}

/// A classic community-style heuristic snake: take the move with the most
/// reachable space, break ties toward food, and refuse head-to-heads it would
/// lose. Shares no evaluation code with the search, so beating it is a real
/// result rather than a measurement of the agent against itself.
pub struct HeuristicAgent {
    label: String,
    scratch: Scratch,
}

impl HeuristicAgent {
    pub fn new(label: impl Into<String>) -> Self {
        HeuristicAgent {
            label: label.into(),
            scratch: Scratch::new(),
        }
    }
}

impl Agent for HeuristicAgent {
    fn name(&self) -> &str {
        &self.label
    }

    fn choose(&mut self, state: &State, rules: &Rules, me: usize) -> Dir {
        let threats = fill::head_threats(state, rules, me);
        let mut best = UP;
        let mut best_score = i64::MIN;

        for &d in &state.legal_moves(rules, me) {
            let Some(target) = rules.step(state.head[me], d) else {
                continue;
            };
            let mut child = state.clone();
            let mut moves = [UP; MAX_SNAKES];
            moves[me] = d;
            for o in 0..rules.n as usize {
                if o != me && child.is_alive(o) {
                    moves[o] = straight_or_safe(&child, rules, o);
                }
            }
            child.apply(rules, &moves);
            if !child.is_alive(me) {
                continue;
            }

            let space = fill::reach(&child, rules, me, &mut self.scratch) as i64;
            let mut score = space * 100;
            if threats.contains(target) {
                score -= 5_000;
            }
            if let Some(d) = state.food.iter().map(|f| rules.dist(target, f)).min() {
                let urgency = if state.health[me] < 50 { 60 } else { 10 };
                score -= urgency * d as i64;
            }
            if score > best_score {
                best_score = score;
                best = d;
            }
        }
        if best_score == i64::MIN {
            // Every move dies; take any legal one.
            return state.legal_moves(rules, me)[0];
        }
        best
    }
}

/// Picks uniformly among moves that are not immediately fatal.
pub struct RandomAgent {
    label: String,
    rng: Rng,
}

impl RandomAgent {
    pub fn new(label: impl Into<String>, seed: u64) -> Self {
        RandomAgent {
            label: label.into(),
            rng: Rng::new(seed),
        }
    }
}

impl Agent for RandomAgent {
    fn name(&self) -> &str {
        &self.label
    }

    fn choose(&mut self, state: &State, rules: &Rules, me: usize) -> Dir {
        let moves = state.legal_moves(rules, me);
        moves[self.rng.below(moves.len())]
    }
}

/// Keeps going if that is safe, otherwise turns. Used to fill in opponents
/// inside the one-ply agents' lookahead.
fn straight_or_safe(state: &State, rules: &Rules, s: usize) -> Dir {
    let mut best = UP;
    let mut best_exits = -1;
    for &d in &DIRS {
        let Some(cell) = rules.step(state.head[s], d) else {
            continue;
        };
        if state.blocked_on_arrival(cell) {
            continue;
        }
        let exits = rules
            .neighbors_of(cell)
            .iter()
            .filter(|&&nb| nb != NO_CELL && !state.blocked_on_arrival(nb))
            .count() as i32;
        if exits > best_exits {
            best_exits = exits;
            best = d;
        }
    }
    best
}

// ---------------------------------------------------------------------------
// Running games
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GameResult {
    /// The last snake standing, or `None` for a draw or turn limit.
    pub winner: Option<usize>,
    pub turns: i32,
    pub lengths: [u16; MAX_SNAKES],
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Record {
    pub wins: u32,
    pub losses: u32,
    pub draws: u32,
}

impl Record {
    pub fn games(&self) -> u32 {
        self.wins + self.losses + self.draws
    }

    /// Win rate counting draws as half, the usual convention for game-playing
    /// strength.
    pub fn score(&self) -> f64 {
        if self.games() == 0 {
            return 0.0;
        }
        (self.wins as f64 + 0.5 * self.draws as f64) / self.games() as f64
    }
}

pub struct Setup {
    pub rules: Rules,
    pub food_spawn_chance: usize,
    pub minimum_food: usize,
    pub max_turns: i32,
}

impl Setup {
    pub fn standard(snakes: u8) -> Self {
        Setup {
            rules: Rules::new(11, 11, snakes),
            food_spawn_chance: 15,
            minimum_food: 1,
            max_turns: 1500,
        }
    }
}

/// The official fixed start positions: corners, edge midpoints, shuffled.
fn start_positions(rules: &Rules, rng: &mut Rng) -> Vec<Cell> {
    let lo = 1i32;
    let mid_x = (rules.width as i32 - 1) / 2;
    let mid_y = (rules.height as i32 - 1) / 2;
    let hi_x = rules.width as i32 - 2;
    let hi_y = rules.height as i32 - 2;
    let mut points = vec![
        (lo, lo),
        (lo, mid_y),
        (lo, hi_y),
        (mid_x, lo),
        (mid_x, hi_y),
        (hi_x, lo),
        (hi_x, mid_y),
        (hi_x, hi_y),
    ];
    rng.shuffle(&mut points);
    points.iter().map(|&(x, y)| rules.cell(x, y)).collect()
}

fn place_initial_food(state: &mut State, rules: &Rules, heads: &[Cell], rng: &mut Rng) {
    if rules.constrictor {
        return;
    }
    // One diagonal neighbour per snake, plus the centre.
    for &head in heads {
        let (hx, hy) = rules.xy(head);
        let mut options: Vec<(i32, i32)> = vec![
            (hx - 1, hy - 1),
            (hx - 1, hy + 1),
            (hx + 1, hy - 1),
            (hx + 1, hy + 1),
        ];
        rng.shuffle(&mut options);
        for (x, y) in options {
            if x < 0 || y < 0 || x >= rules.width as i32 || y >= rules.height as i32 {
                continue;
            }
            let cell = rules.cell(x, y);
            if !state.occupied(cell) && !state.food.contains(cell) {
                state.food.insert(cell);
                break;
            }
        }
    }
    let centre = rules.cell(rules.width as i32 / 2, rules.height as i32 / 2);
    if !state.occupied(centre) {
        state.food.insert(centre);
    }
}

fn spawn_food(state: &mut State, rules: &Rules, setup: &Setup, rng: &mut Rng) {
    if rules.constrictor {
        return;
    }
    let present = state.food.len() as usize;
    let wanted = if present < setup.minimum_food {
        setup.minimum_food - present
    } else if setup.food_spawn_chance > 0 && rng.below(100) < setup.food_spawn_chance {
        1
    } else {
        0
    };
    for _ in 0..wanted {
        let mut free: Vec<Cell> = (0..rules.cells)
            .filter(|&c| !state.occupied(c) && !state.food.contains(c))
            .collect();
        if free.is_empty() {
            return;
        }
        let pick = rng.below(free.len());
        state.food.insert(free.swap_remove(pick));
    }
}

/// Plays one complete game. `agents[i]` controls snake `i`.
pub fn play(setup: &Setup, agents: &mut [&mut dyn Agent], seed: u64) -> GameResult {
    let rules = &setup.rules;
    let mut rng = Rng::new(seed);
    let mut state = State::empty();

    let positions = start_positions(rules, &mut rng);
    let mut heads = Vec::new();
    for s in 0..agents.len() {
        let cell = positions[s % positions.len()];
        state.place_snake(s, &[cell, cell, cell], 100);
        heads.push(cell);
    }
    place_initial_food(&mut state, rules, &heads, &mut rng);

    let solo = agents.len() == 1;
    let mut turns = 0;

    while turns < setup.max_turns {
        let survivors = state.alive_count();
        if survivors == 0 || (!solo && survivors == 1) {
            break;
        }
        let mut moves = [UP; MAX_SNAKES];
        for (s, agent) in agents.iter_mut().enumerate() {
            if state.is_alive(s) {
                moves[s] = agent.choose(&state, rules, s);
            }
        }
        state.apply(rules, &moves);
        spawn_food(&mut state, rules, setup, &mut rng);
        turns += 1;
    }

    let winner = if state.alive_count() == 1 {
        (0..agents.len()).find(|&s| state.is_alive(s))
    } else {
        None
    };
    GameResult {
        winner,
        turns,
        lengths: state.len,
    }
}

/// Plays `games` one-on-one games, swapping seats every game so start
/// positions cannot favour either side.
pub fn duel(
    setup: &Setup,
    a: &mut dyn Agent,
    b: &mut dyn Agent,
    games: u32,
    seed: u64,
) -> Record {
    let mut record = Record::default();
    for g in 0..games {
        let a_first = g % 2 == 0;
        let result = {
            // Reborrowed per game so both agents stay usable across the loop.
            let mut seats: [&mut dyn Agent; 2] = if a_first {
                [&mut *a, &mut *b]
            } else {
                [&mut *b, &mut *a]
            };
            play(setup, &mut seats, seed.wrapping_add(g as u64))
        };
        let a_seat = if a_first { 0 } else { 1 };
        match result.winner {
            Some(w) if w == a_seat => record.wins += 1,
            Some(_) => record.losses += 1,
            None => record.draws += 1,
        }
    }
    record
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rng_is_deterministic_and_shuffles() {
        let mut a = Rng::new(7);
        let mut b = Rng::new(7);
        assert_eq!(a.next_u64(), b.next_u64());
        let mut items: Vec<u32> = (0..16).collect();
        let mut rng = Rng::new(3);
        rng.shuffle(&mut items);
        assert_ne!(items, (0..16).collect::<Vec<u32>>(), "order changed");
        items.sort();
        assert_eq!(items, (0..16).collect::<Vec<u32>>(), "nothing lost");
        assert!((0..100).all(|_| Rng::new(1).below(5) < 5));
    }

    #[test]
    fn a_game_terminates_and_names_one_winner() {
        let setup = Setup::standard(2);
        let mut a = HeuristicAgent::new("h1");
        let mut b = RandomAgent::new("r1", 11);
        let mut seats: [&mut dyn Agent; 2] = [&mut a, &mut b];
        let result = play(&setup, &mut seats, 42);
        assert!(result.turns > 0);
        assert!(result.turns <= setup.max_turns);
        if let Some(w) = result.winner {
            assert!(w < 2);
        }
    }

    #[test]
    fn games_are_reproducible() {
        let setup = Setup::standard(2);
        let run = |seed: u64| {
            let mut a = HeuristicAgent::new("h");
            let mut b = RandomAgent::new("r", 5);
            let mut seats: [&mut dyn Agent; 2] = [&mut a, &mut b];
            play(&setup, &mut seats, seed)
        };
        assert_eq!(run(99), run(99));
    }

    #[test]
    fn solo_games_run_until_the_snake_dies() {
        let setup = Setup {
            max_turns: 60,
            ..Setup::standard(1)
        };
        let mut a = HeuristicAgent::new("solo");
        let mut seats: [&mut dyn Agent; 1] = [&mut a];
        let result = play(&setup, &mut seats, 5);
        assert!(result.turns > 0);
    }

    #[test]
    fn food_is_always_on_the_board() {
        let setup = Setup::standard(2);
        let rules = &setup.rules;
        let mut state = State::empty();
        state.place_snake(0, &[rules.cell(5, 5)], 100);
        let mut rng = Rng::new(1);
        // Minimum food is 1, so an empty board gets one immediately.
        spawn_food(&mut state, rules, &setup, &mut rng);
        assert_eq!(state.food.len(), 1);
    }

    #[test]
    fn food_never_spawns_on_a_snake() {
        let setup = Setup::standard(1);
        let rules = &setup.rules;
        let mut state = State::empty();
        let body: Vec<Cell> = (0..11).map(|x| rules.cell(x, 0)).collect();
        state.place_snake(0, &body, 100);
        let mut rng = Rng::new(2);
        for _ in 0..200 {
            spawn_food(&mut state, rules, &setup, &mut rng);
        }
        for cell in state.food.iter() {
            assert!(!state.occupied(cell), "food landed on a body segment");
        }
    }

    #[test]
    fn record_scores_draws_as_half() {
        let r = Record {
            wins: 3,
            losses: 1,
            draws: 2,
        };
        assert_eq!(r.games(), 6);
        assert!((r.score() - (3.0 + 1.0) / 6.0).abs() < 1e-9);
        assert_eq!(Record::default().score(), 0.0);
    }

    #[test]
    fn duel_swaps_seats_and_counts_every_game() {
        let setup = Setup {
            max_turns: 120,
            ..Setup::standard(2)
        };
        let mut a = HeuristicAgent::new("h");
        let mut b = RandomAgent::new("r", 4);
        let record = duel(&setup, &mut a, &mut b, 4, 1234);
        assert_eq!(record.games(), 4);
    }

    #[test]
    fn start_positions_are_distinct_and_inside_the_board() {
        let rules = Rules::new(11, 11, 4);
        let mut rng = Rng::new(8);
        let positions = start_positions(&rules, &mut rng);
        assert_eq!(positions.len(), 8);
        let mut sorted = positions.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 8, "no duplicate start cells");
        for cell in positions {
            assert!(cell < rules.cells);
        }
    }
}
