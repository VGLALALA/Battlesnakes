# Battlesnakes

Battlesnakes is a set of Battlesnake bot experiments using different decision-making strategies. The repository compares a rules-based snake, an OpenAI-assisted prototype, and a reinforcement-learning-oriented prototype.

## Repository Structure

```text
LogicSnake/       Rule-based Battlesnake implementation
GPTSnake/         LLM-assisted Battlesnake experiment
ReinforceSnake/   Reinforcement-learning experiment scaffold
README.md         Project overview
```

## Requirements

The rule-based implementation includes a dependency file:

```bash
cd LogicSnake
pip install -r requirements.txt
```

Each snake exposes a Python server entry point. Run the implementation you want to test from its folder:

```bash
python server.py
```

## How It Works

Each implementation keeps its game logic in `snake.py` and exposes a Battlesnake-compatible API through `server.py`. The server receives board state from the Battlesnake engine, chooses a move, and returns the next direction.

## Development Notes

- `LogicSnake` is the most direct implementation and is the best starting point for local testing.
- `GPTSnake` is experimental and depends on model access/configuration.
- `ReinforceSnake` is a scaffold for learning-based move selection.

## Useful Links

- [Battlesnake documentation](https://docs.battlesnake.com/)
- [Battlesnake rules](https://docs.battlesnake.com/rules)
