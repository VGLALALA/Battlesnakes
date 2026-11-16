# Battlesnake

A competitive Battlesnake agent in Rust: iterative-deepening maximin search
with locality-restricted opponent enumeration, time-aware flood fill, and
Voronoi area control. No dependencies — standard library only, including the
JSON parser and the HTTP server.

```
cargo run --release                    # serve on :8000
cargo test --release                   # 82 tests
cargo run --release --bin arena bench  # throughput and reachable depth
cargo run --release --bin arena micro  # where the time in a node goes
cargo run --release --bin arena duel 100 6
cargo run --release --bin arena melee 60 5
cargo run --release --bin arena noise  # how much of a win rate is noise
cargo run --release --bin arena tune   # self-play weight tuning
```

## What the literature says, and what I took from it

The design follows what the published work and the strongest public snakes
converge on, rather than inventing a new approach.

**Chung, Luo, Raffin & Perry, "Battlesnake Challenge: A Multi-agent
Reinforcement Learning Playground with Human-in-the-loop"**
([arXiv:2007.10504](https://arxiv.org/pdf/2007.10504)) frames Battlesnake as a
multi-agent RL benchmark and — the useful part here — identifies the baseline
heuristics worth instilling in an agent. Its headline finding for a builder is
that hand-designed heuristics remain the thing to beat; the RL agents need them
injected to learn usefully. That is why this snake is a searcher with a
hand-built evaluation, not a learned policy.

**"Adversarial N-player Search using Locality for the Game of Battlesnake"**
([GI proceedings](https://dl.gi.de/server/api/core/bitstreams/358e228c-0917-4849-a4fe-40d155c3232c/content))
is the most directly applicable paper. Three ideas are used here:

- *Locality*. Enumerating every opponent's moves jointly costs `3^k` per turn.
  An opponent too far away to reach you inside the search horizon cannot change
  your score, so it does not deserve a branch. Its IDAPOS algorithm beat both
  plain max^n and plain alpha-beta on this basis.
- *Switching by player count*. Use max^n while the board is crowded and
  alpha-beta once it is a duel. Here this falls out of the same mechanism:
  with one opponent nothing is approximated at all.
- *Flood fill as the board-control metric*, counting tiles a snake reaches
  first rather than tiles it can merely reach.

**Area control via competitive flood fill.** The
[`nbw/battlesnake_crystal`](https://github.com/nbw/battlesnake_crystal) write-up
is the canonical treatment of the Voronoi heuristic, and an
[arXiv paper on simultaneous games](https://arxiv.org/pdf/2402.03136) treats it
as the standard baseline evaluation, with two refinements adopted here: delete
tails as the fill advances to simulate snakes vacating ground, and combine area
control with a relative health term to prevent starvation.

**Engineering practice from strong public snakes.**
[Robosnake](https://github.com/smallsco/robosnake) (removing *all* snakes' tails
while recursing, and a flood-fill failsafe),
[FusionSnake](https://github.com/FusionStreak/FusionSnake) (iterative deepening
to a time budget), and
[Orion's Fang](https://github.com/ycheng11065/2023-python-snake) (paranoid
minimax with alpha-beta, depth ~7, 16th on the global standard ladder) set the
bar this aims at. Tournament retrospectives contributed two specific
lessons that are implemented directly: *score moves rather than branching on
if/else chains*, and *eliminate moves into a region smaller than your own
length* — you starve there even if it contains food.

## The algorithm, start to finish

What happens in the ~380 ms between the engine asking for a move and getting
one.

### 1. Parse the board into a searchable state

The request JSON becomes a `State`: where every snake is, what it is carrying,
where the food and hazard are. The game mode (standard / royale / wrapped /
constrictor) is read from the ruleset name and changes how moves resolve.

### 2. Decide how long to think

The engine advertises a timeout, usually 500 ms. Subtract a margin for network
round-trip and parsing, and that is the budget. Nothing else about the search
is time-dependent — it just deepens until the clock runs out.

### 3. Search, one game turn at a time

A turn of Battlesnake is simultaneous: everyone moves at once. There is no
natural "my turn, your turn" to alternate over, so each game turn is modelled
as **two layers**:

```
         max layer          we pick among our ≤3 legal moves
            │
         min layer          opponents pick their joint reply
            │
         apply              all moves resolve together, snakes may die
            │
         (next turn, one ply deeper)
```

The min layer assumes opponents choose whatever is worst for us. That is
pessimistic — they might not spot it — but it is the right bias when the
penalty for being wrong is death. Letting them "see" our move before replying
is what makes the structure a valid alternating tree, so **alpha-beta pruning**
applies: once a branch is proven worse than one already found, the rest of it
is skipped unexamined.

Two refinements keep the tree small enough to go deep:

**Move ordering.** Alpha-beta only pays if the best move is tried first — a
perfectly ordered tree is the *square root* of the size of an unordered one.
Each node tries whatever was best at that ply in the previous iteration, then
orders the rest by how much room the target square opens onto.

**Locality.** Branching on every opponent jointly costs `3ⁱ` per turn, which
collapses the depth reachable in a four-snake game. But an opponent too far
away to reach us before the search horizon *cannot affect our score*, so it
gets one cheap policy move instead of a branch. Only the nearest two of those
close enough to matter are branched on. In a one-on-one, nothing is
approximated at all — which is why duels are this snake's strongest format.

### 4. Iterative deepening

Rather than guess a depth, search depth 1, then 2, then 3, and so on until the
budget runs out, keeping the best move from the last iteration that *finished*.
This sounds wasteful — the shallow searches are thrown away — but each costs a
fraction of the next, and the move ordering they produce makes the deep search
cheaper than it would have been alone. It also means the snake always has an
answer ready, however little time it had.

### 5. Score the leaves

At the horizon the position gets a number. Terminal positions come first:
winning scores `MATE - ply` and losing scores `-MATE + ply`, so the search
**prefers winning sooner and dying later** — a snake that scores all deaths
equally stops trying to survive. A draw sits far above a loss and far below any
live position, so it never trades a living position for mutual destruction.

Otherwise the evaluation combines, in order of how much they matter:

| Term | Why |
| --- | --- |
| **Trapped** | Sealed into a region smaller than your own body — you cannot survive there, whatever else is true |
| **Starving** | Food further away than your remaining health |
| **Area control** | Cells you reach before anyone else, versus your strongest rival |
| **Length** | Being longer wins head-to-heads, with diminishing returns |
| **Tactics** | Head-to-head geometry, squeezing opponents, centre bias, hazard |

### 6. The two flood fills

Area control and trap detection both come from a **time-aware flood fill**: a
breadth-first expansion where a cell is passable at depth `d` only if the body
sitting on it has moved off by then. That single rule replaces a pile of
special cases — tail-chasing, slipping through a gap that is about to open,
and refusing a gap a snake which just ate will still be blocking.

The area-control fill expands from *every* living head at once and awards each
cell to whoever arrives first; ties go to the longer snake, matching the
head-to-head rule, and equal-length ties are marked contested because both
snakes would die there. That is the Voronoi partition of the board, and the
difference between our share and our best rival's is the main positional term.

### 7. When everything loses

Pure maximin reports `-MATE` for every move in a lost position and cannot tell
them apart. But a move that loses only to one precise reply is far better in
practice, because opponents make mistakes. So when every root move is a loss,
the root re-ranks by the *mean* over opponent replies instead of the worst
case — picking the line that gives the opponent the most chances to go wrong.

## Design notes

### Occupancy without bodies

The search clones a state at every node, so the state has to be a flat `memcpy`
with no per-snake body arrays. Each cell instead records which snake last
entered it and on which turn. A cell is still body at turn `t` exactly when

```
t - entered[cell] < len[owner] - dupes[owner]
```

`dupes` counts stacked tail segments, so `len - dupes` is the number of
*distinct* cells a snake occupies. This matters because of how the official
rules grow a snake: `moveSnakes` pops the old tail, then `growSnake` appends a
copy of the *new* last segment. The old tail is therefore always vacated — even
on the turn the snake eats — and the extra length shows up instead as a tail
that fails to advance on the *following* turn. Tracking `dupes` reproduces that
exactly, where a plain "turns until free" counter gets it wrong and walks the
snake into a tail that was never going to move. A snake's three stacked opening
segments are just `dupes == 2` decaying by one per turn, so the opening needs no
special case either.

Expired cells need no cleanup: the comparison reports them free and the next
snake to arrive overwrites the record. Eliminated snakes stop blocking because
the test also requires the owner to be alive. A state is ~2 KB and clones in
40 ns.

### Time-aware fills

A cell is passable at fill depth `d` only if the body on it has vacated by
then. Projecting that forward is not a subtraction — stacked tails decay as
turns pass, so the first free turn is `max(dupes + 1, len - age)`. Collapsing it
to `len - dupes - age` reports a snake that just ate as vacating a turn early.

This one rule subsumes a pile of special cases: tail-chasing, squeezing through
a gap that is about to open, and refusing a gap a snake which just ate is still
going to be blocking. It also means a wall built from your *own* body is
correctly *not* treated as a trap — you outrun your own tail — while the same
wall built by a longer snake is.

### Head-to-head geometry

Heads can only collide on a square both can enter. On a 4-connected grid,
neighbouring cells always have opposite parity, so two cells share a neighbour
only at *even* distance. Head-to-head risk is therefore evaluated at distance
two, not one — at distance one the danger is running into the other snake's
body, which the occupancy test already covers.

### Where the weights live

All of them in one `Weights` struct, tunable by self-play (`arena tune`) or at
runtime via `WEIGHTS=area=30,trap=800` — no rebuild. Keeping them in one place
is what makes the tuner possible; scattered magic numbers cannot be searched.

### Code map

| File | Contents |
| --- | --- |
| `src/state.rs` | board representation and the official turn rules |
| `src/fill.rs` | time-aware flood fill and Voronoi area control |
| `src/eval.rs` | the evaluation and its weights |
| `src/search.rs` | iterative-deepening maximin, alpha-beta, locality |
| `src/api.rs` | Battlesnake JSON in and out |
| `src/http.rs` | the HTTP server |
| `src/json.rs` | the JSON parser |
| `src/arena.rs` | self-play harness and baseline opponents |

## Measured strength

100 games per pairing, search depth 6, seats swapped every game, seeded and
reproducible (`arena duel 100 6`):

| Opponent | Record | Score |
| --- | --- | --- |
| Flood-fill heuristic snake | 100W 0L 0D | 100% |
| One-ply greedy (same evaluation) | 100W 0L 0D | 100% |
| Safe-random | 100W 0L 0D | 100% |
| Itself at depth 2 | 94W 5L 1D | 94.5% |

The one-ply comparison is the informative one: same evaluation, same code, no
lookahead. The 100% result is what the search depth is buying. The depth-2
comparison confirms strength keeps scaling with depth rather than saturating.

Four-snake melee against a field of two flood-fill heuristics and one one-ply
greedy, 60 games, depth 5, rotating our seat each game (`arena melee 60 5`):

| | Won | Share |
| --- | --- | --- |
| This snake | 54/60 | 90.0% |
| The other three combined | 6/60 | 10.0% |
| *(an even four-way split)* | | *25.0%* |

Throughput (`arena bench`, 11x11, Apple Silicon, single thread):

| Position | 50 ms | 150 ms | 300 ms | Rate |
| --- | --- | --- | --- | --- |
| One-on-one | depth 6 | depth 7 | depth 8 | ~1.23M nodes/s |
| Four snakes | depth 3-4 | depth 4 | depth 4 | ~850k nodes/s |

Move ordering is what makes alpha-beta worth having: a perfectly ordered tree
is the square root of an unordered one. Each node tries the move that was best
at that ply in the previous iteration, then orders the rest by how much room
the target square opens onto. Because alpha-beta returns the same value at a
fixed depth whatever the order, ordering is free strength — it buys depth
without changing what the search believes. The depth-5 melee suite scores
54/60 before and after, which is the check that this held.

## Honest limits

- **"Beats any snake in the world" is not a claim I can support.** It beats
  every baseline I built, convincingly, and its depth and evaluation are in the
  range the strongest documented public snakes report. It has never played the
  live ladder, and no amount of self-play substitutes for that. Self-play
  overstates strength against unfamiliar styles in a known way.
- **Four-snake play is the weak spot.** Depth 4 versus depth 8 in a duel.
  Locality helps while snakes are apart, but a crowded mid-game is shallow.
  The fix is a faster fill (see below), not a different algorithm.
- **The evaluation is hand-tuned, not optimised.** The tuner (`arena tune`)
  does coordinate descent and runs, but only briefly so far; the shipped
  weights are reasoned defaults, not a search result. The obstacle is sample
  size rather than the tuner: `arena noise` duels the default agent against an
  identical copy to measure the floor, and two identical agents score 48% over
  50 games — unbiased, but wide. A change needs to clear 64% over 50 games to
  be two standard errors from even, so resolving a genuine 2-3% improvement
  takes thousands of games. `accept_threshold` enforces that rather than a flat
  cutoff, which is why a short run accepts nothing.
- **No transposition table.** Snake positions rarely repeat — bodies advance
  monotonically — so a TT buys little and costs a hash of the whole board per
  node. Move ordering got that effort instead.
- **Food spawning is ignored inside the search**, which is standard: it is
  random and unknowable. The arena spawns food properly, so measured results
  include the resulting noise.
- **Boards are capped at 19x19** (`MAX_CELLS`), which covers every official
  size. Anything larger falls back to a reflex move rather than failing.
- **Baselines are ones I wrote.** They implement what the write-ups describe —
  flood-fill space maximisation, food tie-breaking, head-to-head avoidance —
  but a baseline built by the same hand as the agent is a weaker test than a
  stranger's snake.

With ordering done, the clearest next win is a bitwise flood fill. The Voronoi
pass is ~935 ns and dominates the ~1 µs leaf evaluation (`arena micro` breaks
the node cost down); on an 11x11 board the frontier fits in a `u128`, so layer
expansion becomes a handful of shift-and-mask operations instead of ~500
per-cell visits. Allowing for the per-cell setup pass that would remain, that
is worth perhaps 2-3x — another ply, and the melee needs it more than the duel
does.

## Deploying

See [DEPLOY.md](DEPLOY.md) for the full walkthrough — registering on
play.battlesnake.com, the `Dockerfile` and `fly.toml` in this folder, and the
two host settings (never scale to zero; host near the engine) that decide
whether it plays as well live as it does locally.

In short: set `PORT`, put it on a public URL, paste that URL into your account
once. Everything is configurable by environment variable:

| Variable | Default | Meaning |
| --- | --- | --- |
| `PORT` | 8000 | listen port |
| `THREADS` | 4 | accept workers |
| `BUDGET_MARGIN_MS` | 120 | held back from the engine's advertised timeout |
| `BUDGET_MS` | — | fixed think time, overriding the timeout |
| `MAX_ENUMERATED` | 2 | opponents branched on jointly |
| `MAX_DEPTH` | 40 | iterative-deepening ceiling |
| `WEIGHTS` | — | e.g. `area=30,trap=800` |
| `SNAKE_COLOR` / `SNAKE_HEAD` / `SNAKE_TAIL` / `SNAKE_AUTHOR` | | appearance |

Supported modes: standard, royale (hazards), wrapped (toroidal), and
constrictor, detected from the ruleset name and map in each request.
