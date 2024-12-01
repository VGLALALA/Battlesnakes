

import random
import typing


# info is called when you create your Battlesnake on play.battlesnake.com
# and controls your Battlesnake's appearance
# TIP: If you open your Battlesnake URL in a browser you should see this data
def info() -> typing.Dict:
    print("INFO")

    return {
        "apiversion": "1",
        "author": "",  # TODO: Your Battlesnake Username
        "color": "#888888",  # TODO: Choose color
        "head": "#FF00FF",  # TODO: Choose head
        "tail": "default",  # TODO: Choose tail
    }


# start is called when your Battlesnake begins a game
def start(game_state: typing.Dict):
    print("GAME START")


# end is called when your Battlesnake finishes a game
def end(game_state: typing.Dict):
    print("GAME OVER\n")


# move is called on every turn and returns your next move
# Valid moves are "up", "down", "left", or "right"
# See https://docs.battlesnake.com/api/example-move for available data
def move(game_state: typing.Dict) -> typing.Dict:
    from openai import OpenAI
    import anthropic
    import os
    from pathlib import Path
    from dotenv import load_dotenv
    # Load environment variables from .env file

    # Load the finetune prompt
    prompt_path = Path(__file__).parent / "finetune_prompt.txt"
    with open(prompt_path) as f:
        FINETUNE_PROMPT = f.read()

    # Get API mode from environment variable (default to OpenAI)
    API_MODE = "anthropic"

    move_direction = None
    if API_MODE == "openai":
        client = OpenAI()

        completion = client.chat.completions.create(
            model="gpt-4o-mini",
            messages=[
                {"role": "system", "content": "You are a helpful assistant."},
                {
                    "role": "user",
                    "content": "Write a haiku about recursion in programming."
                }
            ]
        )

        print(completion.choices[0].message)
    
    elif API_MODE == "anthropic":
        try:
            claude = anthropic.Client(os.getenv("ANTHROPIC_KEY"))
            response = claude.messages.create(
                model="claude-3-5-sonnet-latest",
                max_tokens=10,
                temperature=0,
                system=FINETUNE_PROMPT,
                messages=[{"role": "user", "content": str(game_state)}]
            )
            move_direction = response.content.strip()
        except Exception as e:
            print(f"Anthropic API error: {e}")
            
    # Default to random move if API call fails or invalid mode
    if move_direction not in ["up", "down", "left", "right"]:
        move_direction = random.choice(["up", "down", "left", "right"])

    print(f"MOVE {game_state['turn']}: {move_direction}")
    return {"move": move_direction}

    


# Start server when `python main.py` is run
if __name__ == "__main__":
    from ReinforceSnake.server import run_server

    run_server({"info": info, "start": start, "move": move, "end": end})