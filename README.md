# Battlesnakes

Battlesnakes is a set of Battlesnake bot experiments using different decision-making strategies. The repository compares a rules-based snake, an OpenAI-assisted prototype, a reinforcement-learning-oriented prototype, and a game-tree search engine.

## Repository Structure

```text
LogicSnake/       Rule-based Battlesnake implementation
GPTSnake/         LLM-assisted Battlesnake experiment
ReinforceSnake/   Reinforcement-learning experiment scaffold
SearchSnake/      Game-tree search engine (Rust)
README.md         Project overview
```

## Requirements

The rule-based implementation includes a dependency file:

```bash
cd LogicSnake
pip install -r requirements.txt
```

The Python snakes each expose a server entry point. Run the implementation you want to test from its folder:

```bash
python server.py
```

`SearchSnake` is Rust rather than Python and has no dependencies at all:

```bash
cd SearchSnake
cargo run --release     # serves on :8000
cargo test --release    # 82 tests
```

## How It Works

Each Python implementation keeps its game logic in `snake.py` and exposes a Battlesnake-compatible API through `server.py`. The server receives board state from the Battlesnake engine, chooses a move, and returns the next direction.

`SearchSnake` serves the same API but decides moves by searching the game tree: iterative-deepening maximin with alpha-beta, locality-restricted opponent enumeration, and a time-aware flood fill for area control. It reaches depth 8-10 in a one-on-one against the engine's 500 ms deadline. See [SearchSnake/README.md](SearchSnake/README.md) for the design and the research it is based on, and [SearchSnake/DEPLOY.md](SearchSnake/DEPLOY.md) for deploying and laddering.

## Development Notes

- `LogicSnake` is the most direct implementation and is the best starting point for local testing.
- `GPTSnake` is experimental and depends on model access/configuration.
- `ReinforceSnake` is a scaffold for learning-based move selection.
- `SearchSnake` is the strongest of the four. It wins 100% of 100-game duels against a flood-fill heuristic baseline and takes 90% of four-snake melees where an even split would be 25%; those baselines ship with it, so the numbers are reproducible via `cargo run --release --bin arena duel`. It has not played the live ladder.

## Useful Links

- [Battlesnake documentation](https://docs.battlesnake.com/)
- [Battlesnake rules](https://docs.battlesnake.com/rules)
