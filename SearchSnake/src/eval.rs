//! Static evaluation.
//!
//! Terms are ordered by how much they matter. Survival dominates: being sealed
//! into a region smaller than your own body, or out of reach of food with low
//! health, is worth more than any amount of positional advantage. Below that
//! sits area control, then the length race, then tactics.
//!
//! Every weight lives in [`Weights`] so the arena can tune them by self-play
//! rather than by argument.

use crate::fill::{self, Scratch, UNREACHABLE};
use crate::state::{Rules, State};

/// Score for a position where we are the last snake standing. Kept well clear
/// of the evaluation range so a win is never traded for position.
pub const MATE: i32 = 1_000_000;
/// Mutual elimination. Far better than losing, far worse than staying alive —
/// so the search never trades a live position for a draw.
pub const DRAW: i32 = -600_000;
/// Evaluations are clamped to this so they can never masquerade as a mate.
pub const EVAL_LIMIT: i32 = 200_000;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Weights {
    /// Per half-cell of controlled-area advantage over the strongest rival.
    pub area: i32,
    /// Per cell our escape space falls short of our own length.
    pub trap: i32,
    /// Flat penalty for being sealed in at all.
    pub trap_hard: i32,
    /// Per unit of length lead, clamped by `length_cap`.
    pub length: i32,
    pub length_cap: i32,
    /// Per step to the nearest food while hungry or out-lengthed.
    pub food_urgent: i32,
    /// Per step to the nearest food while comfortable.
    pub food_idle: i32,
    /// Food is further away than our remaining health.
    pub starvation: i32,
    /// Per point of health, to break ties toward eating.
    pub health: i32,
    /// Within striking distance of a shorter snake's head.
    pub h2h_win: i32,
    /// Within striking distance of a head we would lose to.
    pub h2h_lose: i32,
    /// Per cell an opponent's area falls short of its length.
    pub squeeze: i32,
    pub squeeze_cap: i32,
    /// Per step from the centre of the board.
    pub center: i32,
    /// Sitting in hazard.
    pub hazard: i32,
    /// Per opponent already eliminated.
    pub survivor: i32,
}

impl Default for Weights {
    fn default() -> Self {
        Weights::DEFAULT
    }
}

impl Weights {
    pub const DEFAULT: Weights = Weights {
        area: 24,
        trap: 700,
        trap_hard: 2500,
        length: 180,
        length_cap: 10,
        food_urgent: 60,
        food_idle: 6,
        starvation: 6000,
        health: 3,
        h2h_win: 140,
        h2h_lose: 700,
        squeeze: 350,
        squeeze_cap: 12,
        center: 8,
        hazard: 60,
        survivor: 4000,
    };

    /// Field order used by the arena's weight tuner.
    pub const FIELDS: [&'static str; 16] = [
        "area",
        "trap",
        "trap_hard",
        "length",
        "length_cap",
        "food_urgent",
        "food_idle",
        "starvation",
        "health",
        "h2h_win",
        "h2h_lose",
        "squeeze",
        "squeeze_cap",
        "center",
        "hazard",
        "survivor",
    ];

    pub fn get(&self, field: &str) -> Option<i32> {
        Some(match field {
            "area" => self.area,
            "trap" => self.trap,
            "trap_hard" => self.trap_hard,
            "length" => self.length,
            "length_cap" => self.length_cap,
            "food_urgent" => self.food_urgent,
            "food_idle" => self.food_idle,
            "starvation" => self.starvation,
            "health" => self.health,
            "h2h_win" => self.h2h_win,
            "h2h_lose" => self.h2h_lose,
            "squeeze" => self.squeeze,
            "squeeze_cap" => self.squeeze_cap,
            "center" => self.center,
            "hazard" => self.hazard,
            "survivor" => self.survivor,
            _ => return None,
        })
    }

    pub fn set(&mut self, field: &str, value: i32) -> bool {
        match field {
            "area" => self.area = value,
            "trap" => self.trap = value,
            "trap_hard" => self.trap_hard = value,
            "length" => self.length = value,
            "length_cap" => self.length_cap = value.max(1),
            "food_urgent" => self.food_urgent = value,
            "food_idle" => self.food_idle = value,
            "starvation" => self.starvation = value,
            "health" => self.health = value,
            "h2h_win" => self.h2h_win = value,
            "h2h_lose" => self.h2h_lose = value,
            "squeeze" => self.squeeze = value,
            "squeeze_cap" => self.squeeze_cap = value.max(1),
            "center" => self.center = value,
            "hazard" => self.hazard = value,
            "survivor" => self.survivor = value,
            _ => return false,
        }
        true
    }
}

/// Scores a position from `me`'s point of view. The caller has already ruled
/// out terminal positions, so `me` is alive and at least one rival remains.
pub fn evaluate(
    state: &State,
    rules: &Rules,
    me: usize,
    w: &Weights,
    scratch: &mut Scratch,
) -> i32 {
    let n = rules.n as usize;
    let my_len = state.len[me] as i32;
    let my_head = state.head[me];

    let v = fill::voronoi(state, rules, scratch);

    let mut best_opp_area = 0;
    let mut best_opp_len = 0;
    let mut dead_opponents = 0;
    for o in 0..n {
        if o == me {
            continue;
        }
        if !state.is_alive(o) {
            dead_opponents += 1;
            continue;
        }
        best_opp_area = best_opp_area.max(v.area[o]);
        best_opp_len = best_opp_len.max(state.len[o] as i32);
    }

    let mut score = 0i32;

    // --- Area control -----------------------------------------------------
    score += w.area * (v.area[me] - best_opp_area);

    // --- Our own escape space --------------------------------------------
    // The escape fill is a second BFS and this runs at every leaf, so it is
    // skipped when area control already shows plenty of room. The margin is
    // deliberately generous: controlled area is *nearly* a lower bound on
    // reachable space, but not strictly one — a cell can be reachable only by
    // the longer way round, which the uncontested fill may arrive at too early
    // to pass. Overshooting the guard costs a little speed; skipping a real
    // trap costs the game.
    if v.cells[me] < my_len + 8 {
        // Filled only as far as our own length: past that the space is ample
        // and the exact figure cannot change the decision.
        let space = fill::reach_limited(state, rules, me, my_len, scratch);
        if space < my_len {
            score -= w.trap_hard + w.trap * (my_len - space);
        }
    }

    // --- Length race ------------------------------------------------------
    let lead = (my_len - best_opp_len).clamp(-w.length_cap, w.length_cap);
    score += w.length * lead;

    // --- Food and health --------------------------------------------------
    if !rules.constrictor {
        let health = state.health[me] as i32;
        score += w.health * health;
        let fd = v.food_dist[me];
        // Eating matters when we are low, or when a rival can beat us in a
        // head-to-head and we need the length to change that.
        let hungry = health < 40 || my_len <= best_opp_len;
        if fd == UNREACHABLE {
            if health < 30 {
                score -= w.starvation;
            }
        } else {
            if fd >= health {
                score -= w.starvation;
            }
            score -= if hungry { w.food_urgent } else { w.food_idle } * fd;
        }
    }

    // --- Head-to-head tactics --------------------------------------------
    // Heads can only meet on a shared neighbour, and on a 4-connected grid two
    // cells share a neighbour only at distance two.
    for o in 0..n {
        if o == me || !state.is_alive(o) {
            continue;
        }
        if rules.dist(my_head, state.head[o]) == 2 {
            if my_len > state.len[o] as i32 {
                score += w.h2h_win;
            } else {
                score -= w.h2h_lose;
            }
        }
    }

    // --- Squeezing opponents ---------------------------------------------
    for o in 0..n {
        if o == me || !state.is_alive(o) {
            continue;
        }
        let their_len = state.len[o] as i32;
        if v.cells[o] < their_len {
            score += w.squeeze * (their_len - v.cells[o]).min(w.squeeze_cap);
        }
    }

    // --- Board position ---------------------------------------------------
    let (cx, cy) = (rules.width as i32 / 2, rules.height as i32 / 2);
    score -= w.center * rules.dist(my_head, rules.cell(cx, cy));
    if state.hazard.contains(my_head) {
        score -= w.hazard;
    }

    score += w.survivor * dead_opponents;

    score.clamp(-EVAL_LIMIT, EVAL_LIMIT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Cell, Rules, State};

    fn place(rules: &Rules, st: &mut State, s: usize, body: &[(i32, i32)], health: i16) {
        let cells: Vec<Cell> = body.iter().map(|&(x, y)| rules.cell(x, y)).collect();
        st.place_snake(s, &cells, health);
    }

    fn eval(st: &State, r: &Rules, me: usize) -> i32 {
        evaluate(st, r, me, &Weights::DEFAULT, &mut Scratch::new())
    }

    #[test]
    fn prefers_more_space_to_less() {
        let r = Rules::new(11, 11, 2);
        let mut open = State::empty();
        place(&r, &mut open, 0, &[(5, 5), (5, 4), (5, 3)], 100);
        place(&r, &mut open, 1, &[(9, 9), (9, 10), (10, 10)], 100);

        let mut cornered = State::empty();
        place(&r, &mut cornered, 0, &[(0, 0), (1, 0), (2, 0)], 100);
        place(&r, &mut cornered, 1, &[(9, 9), (9, 10), (10, 10)], 100);

        assert!(
            eval(&open, &r, 0) > eval(&cornered, &r, 0),
            "the open centre must score above the corner"
        );
    }

    #[test]
    fn being_sealed_in_is_heavily_punished() {
        let r = Rules::new(11, 11, 2);
        let mut st = State::empty();
        // A wall across row 1 traps snake 0 in a 10-cell strip while it is 12 long.
        let mut body = vec![(0, 0)];
        for x in 0..11 {
            body.push((x, 1));
        }
        body.push((10, 2));
        place(&r, &mut st, 0, &body, 100);
        place(&r, &mut st, 1, &[(5, 8), (5, 9), (5, 10)], 100);
        let trapped = eval(&st, &r, 0);

        let mut free = State::empty();
        place(&r, &mut free, 0, &[(5, 5), (5, 4), (5, 3)], 100);
        place(&r, &mut free, 1, &[(5, 8), (5, 9), (5, 10)], 100);

        assert!(
            trapped < eval(&free, &r, 0) - 2000,
            "sealed-in positions must be far worse"
        );
    }

    #[test]
    fn hunger_pulls_toward_food() {
        let r = Rules::new(11, 11, 2);
        let build = |food: (i32, i32), health: i16| {
            let mut st = State::empty();
            place(&r, &mut st, 0, &[(5, 5), (5, 4), (5, 3)], health);
            place(&r, &mut st, 1, &[(0, 10), (1, 10), (2, 10)], 100);
            st.food.insert(r.cell(food.0, food.1));
            st
        };
        let near = build((5, 7), 20);
        let far = build((5, 10), 20);
        assert!(
            eval(&near, &r, 0) > eval(&far, &r, 0),
            "closer food scores higher when hungry"
        );
    }

    #[test]
    fn unreachable_food_on_low_health_is_a_crisis() {
        let r = Rules::new(11, 11, 2);
        let mut st = State::empty();
        place(&r, &mut st, 0, &[(5, 5), (5, 4), (5, 3)], 5);
        place(&r, &mut st, 1, &[(0, 10), (1, 10), (2, 10)], 100);
        let no_food = eval(&st, &r, 0);
        st.food.insert(r.cell(5, 6));
        assert!(eval(&st, &r, 0) > no_food + 5000);
    }

    #[test]
    fn avoids_squares_a_bigger_snake_can_contest() {
        let r = Rules::new(11, 11, 2);
        // Heads two apart, so they share a square they could both move into.
        let mut danger = State::empty();
        place(&r, &mut danger, 0, &[(5, 5), (5, 4), (5, 3)], 100);
        place(&r, &mut danger, 1, &[(5, 7), (5, 8), (5, 9), (5, 10)], 100);

        let mut safe = State::empty();
        place(&r, &mut safe, 0, &[(5, 5), (5, 4), (5, 3)], 100);
        place(&r, &mut safe, 1, &[(5, 9), (5, 10), (4, 10), (3, 10)], 100);

        assert!(
            eval(&danger, &r, 0) < eval(&safe, &r, 0),
            "closing on a longer snake's head must cost"
        );
    }

    #[test]
    fn contact_is_an_opportunity_when_longer_and_a_risk_when_not() {
        let r = Rules::new(11, 11, 2);
        // Identical geometry; only the rival's length differs. Heads are two
        // apart, so they share a square either could take next turn.
        let build = |rival: &[(i32, i32)]| {
            let mut st = State::empty();
            place(&r, &mut st, 0, &[(5, 5), (5, 4), (5, 3), (5, 2), (5, 1)], 100);
            place(&r, &mut st, 1, rival, 100);
            st
        };
        let shorter = build(&[(5, 7), (5, 8), (5, 9)]);
        let equal = build(&[(5, 7), (5, 8), (5, 9), (5, 10), (4, 10)]);

        assert!(
            eval(&shorter, &r, 0) > eval(&equal, &r, 0),
            "meeting a snake we out-measure beats meeting one we do not"
        );
    }

    #[test]
    fn eliminating_an_opponent_is_an_improvement() {
        let r = Rules::new(11, 11, 3);
        let mut both = State::empty();
        place(&r, &mut both, 0, &[(5, 5), (5, 4), (5, 3)], 100);
        place(&r, &mut both, 1, &[(0, 0), (1, 0), (2, 0)], 100);
        place(&r, &mut both, 2, &[(10, 10), (9, 10), (8, 10)], 100);

        let mut one = both.clone();
        one.alive &= !(1 << 2);

        assert!(eval(&one, &r, 0) > eval(&both, &r, 0));
    }

    #[test]
    fn evaluation_never_reaches_mate_range() {
        let r = Rules::new(11, 11, 2);
        let mut st = State::empty();
        let mut body = vec![(0, 0)];
        for x in 0..11 {
            body.push((x, 1));
        }
        place(&r, &mut st, 0, &body, 1);
        place(&r, &mut st, 1, &[(5, 8), (5, 9), (5, 10)], 100);
        let s = eval(&st, &r, 0);
        assert!(s.abs() <= EVAL_LIMIT);
        assert!(s > DRAW, "a bad position still beats mutual destruction");
    }

    #[test]
    fn weight_fields_roundtrip() {
        let mut w = Weights::DEFAULT;
        for f in Weights::FIELDS {
            let original = w.get(f).expect("field is readable");
            assert!(w.set(f, original + 1));
            assert_eq!(w.get(f), Some(original + 1));
            w.set(f, original);
        }
        assert_eq!(w, Weights::DEFAULT);
        assert_eq!(w.get("nonexistent"), None);
        assert!(!w.set("nonexistent", 1));
    }
}
