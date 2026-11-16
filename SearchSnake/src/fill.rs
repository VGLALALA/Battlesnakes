//! Time-aware flood fill and Voronoi area control.
//!
//! Both fills are *time-aware*: a cell is passable at BFS depth `d` only if the
//! body sitting on it has vacated by then, i.e. `State::free_in(cell) <= d`.
//! That single detail subsumes a pile of special cases other snakes handle —
//! tail-chasing, squeezing through a gap that is about to open, and refusing a
//! gap that a snake which just ate will still be blocking.
//!
//! The Voronoi fill expands from every living head at once and awards each cell
//! to whoever reaches it first. Cells reached on the same turn go to the longer
//! snake, matching the head-to-head rule; when lengths are equal both snakes
//! would die, so the cell is marked contested and neither side expands through
//! it.

use crate::state::{BitSet, Cell, Rules, State, MAX_CELLS, MAX_SNAKES};

const UNCLAIMED: u8 = 0xFF;
const CONTESTED: u8 = 0xFE;

/// A cell's worth in the area score, in half-cells. Hazard cells drain health,
/// so controlling them is worth less than controlling clear ground.
const CLEAR_WEIGHT: i32 = 2;
const HAZARD_WEIGHT: i32 = 1;

pub const UNREACHABLE: i32 = 9999;

#[derive(Clone, Debug)]
pub struct Voronoi {
    /// Hazard-weighted controlled area per snake, in half-cells.
    pub area: [i32; MAX_SNAKES],
    /// Controlled cell count per snake.
    pub cells: [i32; MAX_SNAKES],
    /// Distance to the nearest food this snake reaches before anyone else,
    /// or [`UNREACHABLE`].
    pub food_dist: [i32; MAX_SNAKES],
}

impl Voronoi {
    fn new() -> Self {
        Voronoi {
            area: [0; MAX_SNAKES],
            cells: [0; MAX_SNAKES],
            food_dist: [UNREACHABLE; MAX_SNAKES],
        }
    }
}

/// Reusable BFS buffers. One per searching thread; keeping them alive across
/// nodes keeps the evaluation allocation-free.
pub struct Scratch {
    claim: [u8; MAX_CELLS],
    cand: [u8; MAX_CELLS],
    tie: [bool; MAX_CELLS],
    seen: [u32; MAX_CELLS],
    /// Turns-until-free per cell, cached for the duration of one fill.
    free_in: [i16; MAX_CELLS],
    generation: u32,
    frontier: Vec<(Cell, u8)>,
    next: Vec<(Cell, u8)>,
    pending: Vec<Cell>,
    queue: Vec<Cell>,
}

impl Default for Scratch {
    fn default() -> Self {
        Self::new()
    }
}

impl Scratch {
    pub fn new() -> Self {
        Scratch {
            claim: [UNCLAIMED; MAX_CELLS],
            cand: [UNCLAIMED; MAX_CELLS],
            tie: [false; MAX_CELLS],
            seen: [0; MAX_CELLS],
            free_in: [0; MAX_CELLS],
            generation: 0,
            frontier: Vec::with_capacity(MAX_CELLS),
            next: Vec::with_capacity(MAX_CELLS),
            pending: Vec::with_capacity(MAX_CELLS),
            queue: Vec::with_capacity(MAX_CELLS),
        }
    }
}

/// Caches turns-until-free for every cell.
///
/// Without this, each cell's `free_in` is recomputed once per neighbour that
/// looks at it — up to four times per cell per fill. Both fills run at every
/// leaf of the search, so this is the hottest arithmetic in the program.
fn prepare(state: &State, rules: &Rules, scratch: &mut Scratch) {
    for cell in 0..rules.cells {
        scratch.free_in[cell as usize] = state.free_in(cell) as i16;
    }
}

#[inline]
fn weight(state: &State, cell: Cell) -> i32 {
    if state.hazard.contains(cell) {
        HAZARD_WEIGHT
    } else {
        CLEAR_WEIGHT
    }
}

/// Partitions the board between the living snakes by first arrival.
pub fn voronoi(state: &State, rules: &Rules, scratch: &mut Scratch) -> Voronoi {
    let mut out = Voronoi::new();
    let n = rules.n as usize;
    let cells = rules.cells as usize;

    prepare(state, rules, scratch);
    scratch.claim[..cells].fill(UNCLAIMED);
    scratch.frontier.clear();

    for s in 0..n {
        if !state.is_alive(s) {
            continue;
        }
        let head = state.head[s];
        scratch.claim[head as usize] = s as u8;
        out.area[s] += weight(state, head);
        out.cells[s] += 1;
        scratch.frontier.push((head, s as u8));
    }

    let mut depth = 0i32;
    while !scratch.frontier.is_empty() {
        depth += 1;
        scratch.next.clear();
        scratch.pending.clear();

        for i in 0..scratch.frontier.len() {
            let (cell, s) = scratch.frontier[i];
            for &nb in rules.neighbors_of(cell) {
                if nb == crate::state::NO_CELL {
                    continue;
                }
                let idx = nb as usize;
                if scratch.claim[idx] != UNCLAIMED {
                    continue;
                }
                // Would the body on this cell have moved on by the time we
                // arrive?
                if i32::from(scratch.free_in[idx]) > depth {
                    continue;
                }
                let held = scratch.cand[idx];
                if held == UNCLAIMED {
                    scratch.cand[idx] = s;
                    scratch.pending.push(nb);
                } else if held != s {
                    let ours = state.len[s as usize];
                    let theirs = state.len[held as usize];
                    if ours > theirs {
                        scratch.cand[idx] = s;
                        scratch.tie[idx] = false;
                    } else if ours == theirs {
                        scratch.tie[idx] = true;
                    }
                }
            }
        }

        for i in 0..scratch.pending.len() {
            let cell = scratch.pending[i];
            let idx = cell as usize;
            let s = scratch.cand[idx];
            scratch.cand[idx] = UNCLAIMED;
            if scratch.tie[idx] {
                // Both snakes would die arriving together; nobody gets it and
                // nobody passes through.
                scratch.tie[idx] = false;
                scratch.claim[idx] = CONTESTED;
                continue;
            }
            scratch.claim[idx] = s;
            let si = s as usize;
            out.area[si] += weight(state, cell);
            out.cells[si] += 1;
            if state.food.contains(cell) && out.food_dist[si] == UNREACHABLE {
                out.food_dist[si] = depth;
            }
            scratch.next.push((cell, s));
        }

        std::mem::swap(&mut scratch.frontier, &mut scratch.next);
    }

    out
}

/// Cells snake `s` can reach on its own, ignoring where other snakes might
/// move but respecting their bodies. Used to detect self-entrapment: a snake
/// sealed into a region smaller than its own length cannot survive there.
pub fn reach(state: &State, rules: &Rules, s: usize, scratch: &mut Scratch) -> i32 {
    reach_limited(state, rules, s, i32::MAX, scratch)
}

/// [`reach`], stopping once `limit` cells have been found. The evaluation only
/// cares whether the space is larger than the snake, so there is no point
/// filling an open board.
pub fn reach_limited(
    state: &State,
    rules: &Rules,
    s: usize,
    limit: i32,
    scratch: &mut Scratch,
) -> i32 {
    if !state.is_alive(s) {
        return 0;
    }
    prepare(state, rules, scratch);
    scratch.generation += 1;
    let generation = scratch.generation;
    scratch.queue.clear();

    let head = state.head[s];
    scratch.seen[head as usize] = generation;
    scratch.queue.push(head);

    let mut found = 0i32;
    let mut depth = 0i32;
    // `queue[start..end]` is the layer at distance `depth`.
    let mut start = 0usize;
    let mut end = 1usize;

    while start < end {
        for i in start..end {
            let cell = scratch.queue[i];
            for &nb in rules.neighbors_of(cell) {
                if nb == crate::state::NO_CELL {
                    continue;
                }
                let idx = nb as usize;
                if scratch.seen[idx] == generation {
                    continue;
                }
                if i32::from(scratch.free_in[idx]) > depth + 1 {
                    continue;
                }
                scratch.seen[idx] = generation;
                scratch.queue.push(nb);
                found += 1;
                if found >= limit {
                    return found;
                }
            }
        }
        start = end;
        end = scratch.queue.len();
        depth += 1;
    }
    found
}

/// The set of cells an opponent head could occupy next turn. Used to steer
/// clear of head-to-head losses without needing an extra search ply.
pub fn head_threats(state: &State, rules: &Rules, me: usize) -> BitSet {
    let mut threats = BitSet::new();
    for o in 0..rules.n as usize {
        if o == me || !state.is_alive(o) {
            continue;
        }
        if state.len[o] < state.len[me] {
            continue; // we win this collision, so it is not a threat
        }
        for &nb in rules.neighbors_of(state.head[o]) {
            if nb != crate::state::NO_CELL && !state.blocked_on_arrival(nb) {
                threats.insert(nb);
            }
        }
    }
    threats
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Rules, State, DOWN, MAX_SNAKES, UP};

    fn place(rules: &Rules, st: &mut State, s: usize, body: &[(i32, i32)], health: i16) {
        let cells: Vec<Cell> = body.iter().map(|&(x, y)| rules.cell(x, y)).collect();
        st.place_snake(s, &cells, health);
    }

    #[test]
    fn single_snake_controls_the_whole_board() {
        let r = Rules::new(11, 11, 1);
        let mut st = State::empty();
        place(&r, &mut st, 0, &[(5, 5), (5, 4), (5, 3)], 100);
        let v = voronoi(&st, &r, &mut Scratch::new());
        // Including the cells its own tail is in the middle of vacating: the
        // fill reaches them on a later turn, by which point they are clear.
        assert_eq!(v.cells[0], 121);
    }

    #[test]
    fn symmetric_position_splits_the_board_evenly() {
        let r = Rules::new(11, 11, 2);
        let mut st = State::empty();
        place(&r, &mut st, 0, &[(1, 5), (0, 5), (0, 4)], 100);
        place(&r, &mut st, 1, &[(9, 5), (10, 5), (10, 4)], 100);
        let v = voronoi(&st, &r, &mut Scratch::new());
        assert_eq!(
            v.cells[0], v.cells[1],
            "mirror-image snakes of equal length control equal area"
        );
    }

    #[test]
    fn longer_snake_wins_contested_ground() {
        let r = Rules::new(11, 11, 2);
        let mut st = State::empty();
        // Snake 0 is longer, so it takes every cell they reach together.
        place(&r, &mut st, 0, &[(1, 5), (0, 5), (0, 4), (0, 3), (0, 2)], 100);
        place(&r, &mut st, 1, &[(9, 5), (10, 5), (10, 4)], 100);
        let v = voronoi(&st, &r, &mut Scratch::new());
        assert!(
            v.cells[0] > v.cells[1],
            "longer snake controls more: {} vs {}",
            v.cells[0],
            v.cells[1]
        );
    }

    #[test]
    fn your_own_wall_is_not_a_trap_because_it_moves() {
        let r = Rules::new(11, 11, 1);
        let mut st = State::empty();
        // A wall across row 1 built from the snake's own body. A naive fill
        // calls this a 10-cell pocket; in truth the snake outruns its own tail
        // and the whole board is still available.
        let mut body = vec![(0, 0)];
        for x in 0..11 {
            body.push((x, 1));
        }
        place(&r, &mut st, 0, &body, 100);
        let space = reach(&st, &r, 0, &mut Scratch::new());
        assert_eq!(space, 120, "the wall dissolves as the tail advances");
    }

    #[test]
    fn a_longer_snakes_wall_does_seal_a_pocket() {
        let r = Rules::new(11, 11, 2);
        let mut st = State::empty();
        // Snake 1 is long, and the wall cells are near its head, so they stay
        // put for many turns. The corner really is closed.
        let mut wall = vec![(4, 10), (4, 9), (4, 8), (4, 7), (4, 6), (4, 5)];
        for y in (0..5).rev() {
            wall.push((4, y));
        }
        for x in 5..11 {
            wall.push((x, 0));
        }
        for y in 1..7 {
            wall.push((10, y));
        }
        place(&r, &mut st, 1, &wall, 100);
        place(&r, &mut st, 0, &[(0, 5), (0, 4), (0, 3)], 100);

        let space = reach(&st, &r, 0, &mut Scratch::new());
        // Columns x=0..3 over all 11 rows, less the two body cells still held.
        assert!(
            space < 45,
            "sealed into the left strip, got {} cells",
            space
        );
        assert!(space > 30, "the strip itself is still roomy, got {}", space);
    }

    #[test]
    fn reach_counts_a_tail_that_is_about_to_move() {
        let r = Rules::new(3, 3, 1);
        let mut st = State::empty();
        // A spiral that fills the board except one cell; the snake survives
        // only because its tail keeps vacating ahead of it.
        place(
            &r,
            &mut st,
            0,
            &[(1, 1), (0, 1), (0, 0), (1, 0), (2, 0), (2, 1), (2, 2), (1, 2)],
            100,
        );
        let space = reach(&st, &r, 0, &mut Scratch::new());
        assert!(space > 0, "the vacating tail opens an escape route");
    }

    #[test]
    fn reach_limited_stops_early() {
        let r = Rules::new(11, 11, 1);
        let mut st = State::empty();
        place(&r, &mut st, 0, &[(5, 5), (5, 4), (5, 3)], 100);
        assert_eq!(reach_limited(&st, &r, 0, 10, &mut Scratch::new()), 10);
    }

    #[test]
    fn food_distance_is_the_bfs_distance() {
        let r = Rules::new(11, 11, 1);
        let mut st = State::empty();
        place(&r, &mut st, 0, &[(5, 5), (5, 4), (5, 3)], 100);
        st.food.insert(r.cell(5, 8));
        let v = voronoi(&st, &r, &mut Scratch::new());
        assert_eq!(v.food_dist[0], 3);
    }

    #[test]
    fn hazard_ground_is_worth_less_than_clear_ground() {
        let r = Rules::new(11, 11, 1);
        let mut st = State::empty();
        place(&r, &mut st, 0, &[(5, 5), (5, 4), (5, 3)], 100);
        let clear = voronoi(&st, &r, &mut Scratch::new()).area[0];
        for c in 0..r.cells {
            st.hazard.insert(c);
        }
        let hazardous = voronoi(&st, &r, &mut Scratch::new()).area[0];
        assert!(hazardous < clear);
    }

    #[test]
    fn head_threats_only_covers_snakes_we_cannot_beat() {
        let r = Rules::new(11, 11, 2);
        let mut st = State::empty();
        place(&r, &mut st, 0, &[(5, 5), (5, 4), (5, 3)], 100);
        place(&r, &mut st, 1, &[(5, 8), (5, 9), (5, 10)], 100);
        // Equal length: their next squares are dangerous.
        let threats = head_threats(&st, &r, 0);
        assert!(threats.contains(r.cell(5, 7)));
        assert!(threats.contains(r.cell(4, 8)));

        // Make ourselves longer and the same squares stop being threats.
        st.len[0] = 5;
        assert!(head_threats(&st, &r, 0).is_empty());
    }

    #[test]
    fn dead_snakes_do_not_claim_area() {
        let r = Rules::new(11, 11, 2);
        let mut st = State::empty();
        place(&r, &mut st, 0, &[(5, 5), (5, 4), (5, 3)], 100);
        place(&r, &mut st, 1, &[(1, 1), (1, 2), (1, 3)], 1);
        let mut m = [UP; MAX_SNAKES];
        m[1] = DOWN;
        st.apply(&r, &m);
        assert!(!st.is_alive(1));
        let v = voronoi(&st, &r, &mut Scratch::new());
        assert_eq!(v.cells[1], 0);
        assert!(v.cells[0] > 110, "the survivor inherits the board");
    }
}
