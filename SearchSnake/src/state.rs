//! Game state and the official turn-resolution rules.
//!
//! # Occupancy encoding
//!
//! The hot path of the search clones a state at every node, so the state must
//! be a flat `memcpy` with no heap indirection and no per-snake body arrays.
//!
//! Instead of storing bodies, each cell records the snake that last entered it
//! (`owner`) and the turn its head arrived (`entered`). A cell is still body at
//! turn `t` exactly when
//!
//! ```text
//! t - entered[cell] < len[owner] - dupes[owner]
//! ```
//!
//! `dupes` counts the stacked (duplicated) tail segments a snake currently
//! carries, so `len - dupes` is the number of *distinct* cells it occupies.
//! This matters because of how the official rules grow a snake: `moveSnakes`
//! pops the old tail and `growSnake` then appends a copy of the *new* last
//! segment. The old tail cell is therefore always vacated, even on the turn the
//! snake eats — the extra length shows up as a tail that fails to advance on
//! the *following* turn. Tracking `dupes` reproduces that exactly, which plain
//! "turns until free" counters get wrong.
//!
//! A snake starts with its three segments stacked on one cell, which is just
//! `dupes == 2` decaying by one per turn, so the opening needs no special case.
//!
//! Cells whose occupancy has expired need no cleanup: the comparison above
//! reports them free, and the next snake to enter overwrites the record. Bodies
//! of eliminated snakes stop blocking because the test also requires the owner
//! to be alive.

pub const MAX_CELLS: usize = 361; // 19x19, the largest standard board
pub const MAX_SNAKES: usize = 8;
pub const NO_OWNER: u8 = 0xFF;
pub const NO_CELL: u16 = u16::MAX;

pub type Cell = u16;
pub type Dir = u8;

pub const UP: Dir = 0;
pub const DOWN: Dir = 1;
pub const LEFT: Dir = 2;
pub const RIGHT: Dir = 3;
pub const DIRS: [Dir; 4] = [UP, DOWN, LEFT, RIGHT];

pub fn dir_name(d: Dir) -> &'static str {
    match d {
        UP => "up",
        DOWN => "down",
        LEFT => "left",
        _ => "right",
    }
}

// ---------------------------------------------------------------------------
// BitSet
// ---------------------------------------------------------------------------

/// A fixed 384-bit set, enough for the largest board, sized to copy cheaply.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct BitSet {
    words: [u64; 6],
}

impl BitSet {
    pub const fn new() -> Self {
        BitSet { words: [0; 6] }
    }

    #[inline]
    pub fn insert(&mut self, cell: Cell) {
        self.words[cell as usize >> 6] |= 1u64 << (cell & 63);
    }

    #[inline]
    pub fn remove(&mut self, cell: Cell) {
        self.words[cell as usize >> 6] &= !(1u64 << (cell & 63));
    }

    #[inline]
    pub fn contains(&self, cell: Cell) -> bool {
        self.words[cell as usize >> 6] & (1u64 << (cell & 63)) != 0
    }

    pub fn len(&self) -> u32 {
        self.words.iter().map(|w| w.count_ones()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.words.iter().all(|w| *w == 0)
    }

    pub fn iter(&self) -> impl Iterator<Item = Cell> + '_ {
        (0..6).flat_map(move |w| {
            let mut bits = self.words[w];
            std::iter::from_fn(move || {
                if bits == 0 {
                    None
                } else {
                    let b = bits.trailing_zeros();
                    bits &= bits - 1;
                    Some((w as u16) * 64 + b as u16)
                }
            })
        })
    }
}

// ---------------------------------------------------------------------------
// Rules
// ---------------------------------------------------------------------------

/// Board geometry and game-mode flags. Constant for the whole game, so it is
/// kept out of `State` and shared by reference.
#[derive(Clone)]
pub struct Rules {
    pub width: u8,
    pub height: u8,
    pub cells: u16,
    /// Toroidal board (`wrapped` mode): moves never leave the board.
    pub wrapped: bool,
    /// `constrictor` mode: no food, and every snake grows every turn.
    pub constrictor: bool,
    pub hazard_damage: i16,
    /// Number of snake slots in play, including ones already eliminated.
    pub n: u8,
    /// Precomputed adjacency, `NO_CELL` where a move leaves the board.
    neighbors: Vec<[Cell; 4]>,
}

impl Rules {
    pub fn new(width: u8, height: u8, n: u8) -> Self {
        let cells = width as u16 * height as u16;
        assert!(
            cells as usize <= MAX_CELLS,
            "board {}x{} exceeds MAX_CELLS",
            width,
            height
        );
        let mut rules = Rules {
            width,
            height,
            cells,
            wrapped: false,
            constrictor: false,
            hazard_damage: 14,
            n,
            neighbors: Vec::new(),
        };
        rules.rebuild_neighbors();
        rules
    }

    pub fn wrapped(mut self, wrapped: bool) -> Self {
        self.wrapped = wrapped;
        self.rebuild_neighbors();
        self
    }

    pub fn constrictor(mut self, constrictor: bool) -> Self {
        self.constrictor = constrictor;
        self
    }

    pub fn hazard_damage(mut self, damage: i16) -> Self {
        self.hazard_damage = damage;
        self
    }

    fn rebuild_neighbors(&mut self) {
        let mut table = vec![[NO_CELL; 4]; self.cells as usize];
        for cell in 0..self.cells {
            for &d in &DIRS {
                table[cell as usize][d as usize] = self.step_uncached(cell, d).unwrap_or(NO_CELL);
            }
        }
        self.neighbors = table;
    }

    fn step_uncached(&self, cell: Cell, d: Dir) -> Option<Cell> {
        let w = self.width as i32;
        let h = self.height as i32;
        let x = cell as i32 % w;
        let y = cell as i32 / w;
        let (mut nx, mut ny) = match d {
            UP => (x, y + 1),
            DOWN => (x, y - 1),
            LEFT => (x - 1, y),
            _ => (x + 1, y),
        };
        if self.wrapped {
            nx = nx.rem_euclid(w);
            ny = ny.rem_euclid(h);
        } else if nx < 0 || ny < 0 || nx >= w || ny >= h {
            return None;
        }
        Some((ny * w + nx) as Cell)
    }

    /// The cell reached by moving `d` from `cell`, or `None` if that leaves the
    /// board (never `None` on a wrapped board).
    #[inline]
    pub fn step(&self, cell: Cell, d: Dir) -> Option<Cell> {
        let n = self.neighbors[cell as usize][d as usize];
        if n == NO_CELL {
            None
        } else {
            Some(n)
        }
    }

    #[inline]
    pub fn neighbors_of(&self, cell: Cell) -> &[Cell; 4] {
        &self.neighbors[cell as usize]
    }

    #[inline]
    pub fn xy(&self, cell: Cell) -> (i32, i32) {
        (
            cell as i32 % self.width as i32,
            cell as i32 / self.width as i32,
        )
    }

    #[inline]
    pub fn cell(&self, x: i32, y: i32) -> Cell {
        (y * self.width as i32 + x) as Cell
    }

    /// Manhattan distance, taking the short way round on a wrapped board.
    pub fn dist(&self, a: Cell, b: Cell) -> i32 {
        let (ax, ay) = self.xy(a);
        let (bx, by) = self.xy(b);
        let mut dx = (ax - bx).abs();
        let mut dy = (ay - by).abs();
        if self.wrapped {
            dx = dx.min(self.width as i32 - dx);
            dy = dy.min(self.height as i32 - dy);
        }
        dx + dy
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct State {
    pub owner: [u8; MAX_CELLS],
    pub entered: [i32; MAX_CELLS],
    pub food: BitSet,
    pub hazard: BitSet,
    pub head: [Cell; MAX_SNAKES],
    pub len: [u16; MAX_SNAKES],
    /// Stacked tail segments currently carried; `len - dupes` distinct cells.
    pub dupes: [u8; MAX_SNAKES],
    pub health: [i16; MAX_SNAKES],
    /// Bitmask of snakes still in the game.
    pub alive: u8,
    pub turn: i32,
}

impl State {
    pub fn empty() -> Self {
        State {
            owner: [NO_OWNER; MAX_CELLS],
            entered: [i32::MIN / 2; MAX_CELLS],
            food: BitSet::new(),
            hazard: BitSet::new(),
            head: [NO_CELL; MAX_SNAKES],
            len: [0; MAX_SNAKES],
            dupes: [0; MAX_SNAKES],
            health: [0; MAX_SNAKES],
            alive: 0,
            turn: 0,
        }
    }

    #[inline]
    pub fn is_alive(&self, s: usize) -> bool {
        self.alive & (1 << s) != 0
    }

    pub fn alive_count(&self) -> u32 {
        self.alive.count_ones()
    }

    /// Writes a snake's body onto the grid. `body` runs head-first and may
    /// repeat cells where segments are stacked.
    pub fn place_snake(&mut self, s: usize, body: &[Cell], health: i16) {
        debug_assert!(!body.is_empty());
        let mut claimed = BitSet::new();
        let mut distinct = 0u16;
        for (i, &cell) in body.iter().enumerate() {
            // The smallest index wins: that is when the head entered the cell,
            // and any stacked segments trail behind it.
            if claimed.contains(cell) {
                continue;
            }
            claimed.insert(cell);
            self.owner[cell as usize] = s as u8;
            self.entered[cell as usize] = self.turn - i as i32;
            distinct += 1;
        }
        self.head[s] = body[0];
        self.len[s] = body.len() as u16;
        self.dupes[s] = (body.len() as u16 - distinct) as u8;
        self.health[s] = health;
        self.alive |= 1 << s;
    }

    /// Number of distinct cells snake `s` occupies.
    #[inline]
    pub fn body_cells(&self, s: usize) -> i32 {
        self.len[s] as i32 - self.dupes[s] as i32
    }

    /// Turns from now until `cell` is free; zero means already free.
    ///
    /// Stacked tails have to be projected forward, not just subtracted. Writing
    /// `age = turn - entered[cell]`, the cell is still body `d` turns from now
    /// when `d + age < len - max(0, dupes - d)`, because each turn consumes one
    /// stacked segment. For `d <= dupes` that reduces to `age < len - dupes`
    /// — independent of `d`, since the tail is standing still — and beyond that
    /// to `d < len - age`. So the first free turn is
    /// `max(dupes + 1, len - age)`.
    ///
    /// Collapsing this to `len - dupes - age` would report a snake that just
    /// ate as vacating its tail a turn early, which is exactly the kind of
    /// move that gets a snake killed.
    #[inline]
    pub fn free_in(&self, cell: Cell) -> i32 {
        let owner = self.owner[cell as usize];
        if owner == NO_OWNER || !self.is_alive(owner as usize) {
            return 0;
        }
        let o = owner as usize;
        let age = self.turn - self.entered[cell as usize];
        let len = self.len[o] as i32;
        let dupes = self.dupes[o] as i32;
        if age >= len - dupes {
            return 0; // the tail has already passed this cell
        }
        (dupes + 1).max(len - age)
    }

    #[inline]
    pub fn occupied(&self, cell: Cell) -> bool {
        self.free_in(cell) > 0
    }

    /// Whether moving into `cell` this turn is certain death from a body that
    /// will still be there on arrival. A cell whose last segment vacates this
    /// turn (`free_in == 1`) is safe to enter.
    #[inline]
    pub fn blocked_on_arrival(&self, cell: Cell) -> bool {
        self.free_in(cell) >= 2
    }

    /// Moves that are not immediately fatal for geometric reasons: on the board
    /// and not into a body that persists. Collisions that depend on what other
    /// snakes choose (head-to-head) are deliberately left in — the search
    /// resolves those.
    pub fn safe_moves(&self, rules: &Rules, s: usize) -> Vec<Dir> {
        let mut out = Vec::with_capacity(4);
        for &d in &DIRS {
            if let Some(cell) = rules.step(self.head[s], d) {
                if !self.blocked_on_arrival(cell) {
                    out.push(d);
                }
            }
        }
        out
    }

    /// `safe_moves`, falling back to any on-board move and finally to `UP` so
    /// callers always have something to play. A doomed snake still has to move.
    pub fn legal_moves(&self, rules: &Rules, s: usize) -> Vec<Dir> {
        let safe = self.safe_moves(rules, s);
        if !safe.is_empty() {
            return safe;
        }
        let onboard: Vec<Dir> = DIRS
            .iter()
            .copied()
            .filter(|&d| rules.step(self.head[s], d).is_some())
            .collect();
        if onboard.is_empty() {
            vec![UP]
        } else {
            onboard
        }
    }

    /// Advances one turn with every living snake moving simultaneously.
    ///
    /// Mirrors the official pipeline: move, reduce health, apply hazard damage,
    /// feed, then eliminate. Eliminations are decided against a single snapshot
    /// and applied together, so two snakes can die on the same turn.
    pub fn apply(&mut self, rules: &Rules, moves: &[Dir; MAX_SNAKES]) {
        let t = self.turn + 1;
        let n = rules.n as usize;

        let mut new_head = [NO_CELL; MAX_SNAKES];
        let mut new_len = self.len;
        let mut new_dupes = self.dupes;
        let mut new_health = self.health;
        let mut ate = [false; MAX_SNAKES];
        let mut doomed = 0u8;

        // Move, lose health, take hazard damage, then eat.
        for s in 0..n {
            if !self.is_alive(s) {
                continue;
            }
            match rules.step(self.head[s], moves[s]) {
                Some(cell) => {
                    new_head[s] = cell;
                    new_health[s] -= 1;
                    if self.hazard.contains(cell) {
                        new_health[s] -= rules.hazard_damage;
                    }
                    let eats = rules.constrictor || self.food.contains(cell);
                    // A stacked tail is consumed by the move; eating adds one back.
                    new_dupes[s] = new_dupes[s].saturating_sub(1);
                    if eats {
                        ate[s] = true;
                        new_health[s] = 100;
                        new_len[s] += 1;
                        new_dupes[s] += 1;
                    }
                }
                None => doomed |= 1 << s, // left the board
            }
        }

        for s in 0..n {
            if self.is_alive(s) && doomed & (1 << s) == 0 && new_health[s] <= 0 {
                doomed |= 1 << s;
            }
        }

        // Snakes already out of bounds or out of health no longer block anyone.
        let standing = self.alive & !doomed;

        // Body and head-to-head collisions, all judged against `standing`.
        let mut collided = 0u8;
        for s in 0..n {
            if standing & (1 << s) == 0 {
                continue;
            }
            let h = new_head[s];
            if self.body_at(h, t, standing, &new_len, &new_dupes) {
                collided |= 1 << s;
                continue;
            }
            for o in 0..n {
                if o == s || standing & (1 << o) == 0 {
                    continue;
                }
                if new_head[o] == h && new_len[s] <= new_len[o] {
                    collided |= 1 << s;
                    break;
                }
            }
        }

        // Food is consumed before eliminations, so a snake that dies on the
        // same turn still takes the food with it.
        for s in 0..n {
            if ate[s] {
                self.food.remove(new_head[s]);
            }
        }

        let survivors = standing & !collided;
        for s in 0..n {
            if survivors & (1 << s) == 0 {
                continue;
            }
            let h = new_head[s];
            self.owner[h as usize] = s as u8;
            self.entered[h as usize] = t;
            self.head[s] = h;
            self.len[s] = new_len[s];
            self.dupes[s] = new_dupes[s];
            self.health[s] = new_health[s];
        }

        self.alive = survivors;
        self.turn = t;
    }

    /// Whether `cell` is a body segment at turn `t`, given post-feed lengths.
    /// Only snakes in `mask` count.
    #[inline]
    fn body_at(
        &self,
        cell: Cell,
        t: i32,
        mask: u8,
        len: &[u16; MAX_SNAKES],
        dupes: &[u8; MAX_SNAKES],
    ) -> bool {
        let owner = self.owner[cell as usize];
        if owner == NO_OWNER || mask & (1 << owner) == 0 {
            return false;
        }
        let o = owner as usize;
        let distinct = len[o] as i32 - dupes[o] as i32;
        t - self.entered[cell as usize] < distinct
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules_11(n: u8) -> Rules {
        Rules::new(11, 11, n)
    }

    /// Builds a state from straight-line bodies given as (x, y) head-first.
    fn state_with(rules: &Rules, bodies: &[&[(i32, i32)]], health: i16) -> State {
        let mut st = State::empty();
        for (s, body) in bodies.iter().enumerate() {
            let cells: Vec<Cell> = body.iter().map(|&(x, y)| rules.cell(x, y)).collect();
            st.place_snake(s, &cells, health);
        }
        st
    }

    fn moves(list: &[Dir]) -> [Dir; MAX_SNAKES] {
        let mut m = [UP; MAX_SNAKES];
        for (i, &d) in list.iter().enumerate() {
            m[i] = d;
        }
        m
    }

    #[test]
    fn stacked_start_decays_over_three_turns() {
        let r = rules_11(1);
        let mut st = state_with(&r, &[&[(5, 5), (5, 5), (5, 5)]], 100);
        let start = r.cell(5, 5);
        assert_eq!(st.len[0], 3);
        assert_eq!(st.dupes[0], 2);
        assert!(st.occupied(start));

        st.apply(&r, &moves(&[UP])); // turn 1
        assert_eq!(st.dupes[0], 1);
        assert!(st.occupied(start), "tail still stacked on the start cell");

        st.apply(&r, &moves(&[UP])); // turn 2
        assert_eq!(st.dupes[0], 0);
        assert!(st.occupied(start));

        st.apply(&r, &moves(&[UP])); // turn 3
        assert!(!st.occupied(start), "start cell is finally vacated");
        assert_eq!(st.len[0], 3);
    }

    #[test]
    fn tail_vacates_on_the_turn_the_snake_eats() {
        // Official order pops the old tail, then duplicates the *new* last
        // segment, so the old tail cell is free even when the snake eats.
        let r = rules_11(1);
        let mut st = state_with(&r, &[&[(5, 5), (5, 4), (5, 3)]], 50);
        let old_tail = r.cell(5, 3);
        st.food.insert(r.cell(5, 6));

        st.apply(&r, &moves(&[UP]));
        assert_eq!(st.len[0], 4, "grew by one");
        assert_eq!(st.dupes[0], 1, "carries a stacked tail");
        assert_eq!(st.health[0], 100);
        assert!(!st.occupied(old_tail), "old tail cell is free");
        assert!(st.occupied(r.cell(5, 4)), "new tail is still body");

        // The extra length shows up as a tail that does not advance.
        st.apply(&r, &moves(&[UP]));
        assert_eq!(st.dupes[0], 0);
        assert!(st.occupied(r.cell(5, 4)), "tail held position for a turn");
        assert_eq!(st.body_cells(0), 4);
    }

    #[test]
    fn following_your_own_tail_is_safe() {
        let r = rules_11(1);
        // A 4-long snake curled so its head sits beside its tail.
        let mut st = state_with(&r, &[&[(5, 5), (5, 4), (6, 4), (6, 5)]], 100);
        let tail = r.cell(6, 5);
        assert!(st.occupied(tail));
        assert!(
            !st.blocked_on_arrival(tail),
            "tail vacates this turn, so moving onto it is legal"
        );
        st.apply(&r, &moves(&[RIGHT]));
        assert!(st.is_alive(0), "chasing the tail keeps the snake alive");
        assert_eq!(st.head[0], tail);
    }

    #[test]
    fn moving_onto_a_tail_that_grows_is_fatal() {
        let r = rules_11(2);
        // Snake 1 is about to eat, so its tail will not advance next turn.
        let mut st = state_with(&r, &[&[(0, 0), (0, 1), (0, 2)], &[(5, 5), (5, 4), (5, 3)]], 100);
        st.food.insert(r.cell(5, 6));
        st.apply(&r, &moves(&[DOWN, UP]));
        assert_eq!(st.dupes[1], 1);
        // (5,4) is snake 1's tail but is held for an extra turn.
        assert!(
            st.blocked_on_arrival(r.cell(5, 4)),
            "a stacked tail does not vacate"
        );
    }

    #[test]
    fn self_collision_and_walls_eliminate() {
        let r = rules_11(1);
        let mut st = state_with(&r, &[&[(5, 5), (5, 4), (4, 4), (4, 5)]], 100);
        st.apply(&r, &moves(&[DOWN])); // into its own neck
        assert!(!st.is_alive(0));

        let mut st = state_with(&r, &[&[(0, 0), (1, 0), (2, 0)]], 100);
        st.apply(&r, &moves(&[LEFT])); // off the board
        assert!(!st.is_alive(0));
    }

    #[test]
    fn starvation_eliminates() {
        let r = rules_11(1);
        let mut st = state_with(&r, &[&[(5, 5), (5, 4), (5, 3)]], 1);
        st.apply(&r, &moves(&[UP]));
        assert!(!st.is_alive(0), "health hit zero");
    }

    #[test]
    fn head_to_head_longer_snake_survives() {
        let r = rules_11(2);
        let mut st = State::empty();
        st.place_snake(
            0,
            &[r.cell(4, 5), r.cell(3, 5), r.cell(2, 5), r.cell(1, 5)],
            100,
        );
        st.place_snake(1, &[r.cell(6, 5), r.cell(7, 5), r.cell(8, 5)], 100);
        st.apply(&r, &moves(&[RIGHT, LEFT])); // both into (5,5)
        assert!(st.is_alive(0), "longer snake wins the head-to-head");
        assert!(!st.is_alive(1));
        assert_eq!(st.head[0], r.cell(5, 5));
    }

    #[test]
    fn head_to_head_equal_length_kills_both() {
        let r = rules_11(2);
        let mut st = State::empty();
        st.place_snake(0, &[r.cell(4, 5), r.cell(3, 5), r.cell(2, 5)], 100);
        st.place_snake(1, &[r.cell(6, 5), r.cell(7, 5), r.cell(8, 5)], 100);
        st.apply(&r, &moves(&[RIGHT, LEFT]));
        assert_eq!(st.alive_count(), 0, "equal lengths both die");
    }

    #[test]
    fn body_collision_kills_only_the_snake_that_ran_into_it() {
        let r = rules_11(2);
        let mut st = State::empty();
        st.place_snake(0, &[r.cell(5, 5), r.cell(5, 4), r.cell(5, 3)], 100);
        st.place_snake(1, &[r.cell(4, 4), r.cell(3, 4), r.cell(2, 4)], 100);
        st.apply(&r, &moves(&[UP, RIGHT])); // snake 1 into snake 0's body
        assert!(st.is_alive(0));
        assert!(!st.is_alive(1));
    }

    #[test]
    fn a_dead_snakes_body_stops_blocking() {
        let r = rules_11(2);
        let mut st = State::empty();
        st.place_snake(0, &[r.cell(5, 5), r.cell(5, 4), r.cell(5, 3)], 100);
        st.place_snake(1, &[r.cell(0, 0), r.cell(1, 0), r.cell(2, 0)], 1);
        let corpse = r.cell(1, 0);
        assert!(st.occupied(corpse));
        st.apply(&r, &moves(&[UP, UP])); // snake 1 starves
        assert!(!st.is_alive(1));
        assert!(!st.occupied(corpse), "the corpse is removed from the board");
    }

    #[test]
    fn wrapped_board_moves_round_the_edge() {
        let r = rules_11(1).wrapped(true);
        let mut st = State::empty();
        st.place_snake(0, &[r.cell(0, 5), r.cell(1, 5), r.cell(2, 5)], 100);
        st.apply(&r, &moves(&[LEFT]));
        assert!(st.is_alive(0));
        assert_eq!(st.head[0], r.cell(10, 5), "wrapped to the far edge");
    }

    #[test]
    fn hazard_damage_applies_to_the_new_head() {
        let r = rules_11(1).hazard_damage(14);
        let mut st = State::empty();
        st.place_snake(0, &[r.cell(5, 5), r.cell(5, 4), r.cell(5, 3)], 100);
        st.hazard.insert(r.cell(5, 6));
        st.apply(&r, &moves(&[UP]));
        assert_eq!(st.health[0], 100 - 1 - 14);
    }

    #[test]
    fn constrictor_grows_every_turn_and_pins_its_tail() {
        let r = rules_11(1).constrictor(true);
        let mut st = State::empty();
        st.place_snake(0, &[r.cell(5, 5), r.cell(5, 4), r.cell(5, 3)], 100);

        st.apply(&r, &moves(&[UP]));
        assert_eq!(st.len[0], 4);
        assert_eq!(st.health[0], 100);
        // The very first move still pops the old tail before growing, so that
        // one cell is released — then the tail never advances again.
        assert!(!st.occupied(r.cell(5, 3)));
        assert!(st.occupied(r.cell(5, 4)));

        for _ in 0..4 {
            st.apply(&r, &moves(&[UP]));
        }
        assert_eq!(st.len[0], 8);
        assert!(
            st.occupied(r.cell(5, 4)),
            "the tail stays pinned while the body keeps growing"
        );
        assert_eq!(st.body_cells(0), 7, "one cell gained per turn");
    }

    #[test]
    fn safe_moves_excludes_walls_and_bodies() {
        let r = rules_11(1);
        let st = state_with(&r, &[&[(0, 0), (0, 1), (0, 2)]], 100);
        let mut safe = st.safe_moves(&r, 0);
        safe.sort();
        assert_eq!(safe, vec![RIGHT], "left and down are walls, up is the neck");
    }

    #[test]
    fn legal_moves_never_returns_empty() {
        let r = Rules::new(3, 1, 1);
        let mut st = State::empty();
        st.place_snake(0, &[r.cell(1, 0), r.cell(0, 0), r.cell(2, 0)], 100);
        assert!(!st.legal_moves(&r, 0).is_empty());
    }

    #[test]
    fn bitset_roundtrips() {
        let mut b = BitSet::new();
        for c in [0u16, 1, 63, 64, 120, 360] {
            b.insert(c);
        }
        assert_eq!(b.len(), 6);
        assert!(b.contains(64) && b.contains(360) && !b.contains(65));
        b.remove(64);
        assert!(!b.contains(64));
        let mut got: Vec<Cell> = b.iter().collect();
        got.sort();
        assert_eq!(got, vec![0, 1, 63, 120, 360]);
    }
}
