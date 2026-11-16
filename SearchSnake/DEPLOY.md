# Deploying and competing

## The shape of it

Battlesnake does not run your code. You host a web server; the game engine
sends it an HTTP request each turn and your server answers with a move. So
"deploying a snake" means "putting this server on a public URL" and then
pasting that URL into your account once.

The server already implements the four endpoints the engine calls:

| Method | Path | Purpose |
| --- | --- | --- |
| `GET` | `/` | appearance and API version |
| `POST` | `/start` | a game is beginning |
| `POST` | `/move` | **the only one that matters** — returns the direction |
| `POST` | `/end` | the game is over |

Confirm it locally first:

```bash
cargo run --release          # serves on :8000
curl localhost:8000/         # {"apiversion":"1",...}
```

## Deploy

### Fly.io

`fly.toml` and `Dockerfile` are in this folder, so:

```bash
fly launch --copy-config --no-deploy   # keeps the committed fly.toml
fly deploy
fly open                               # prints the public URL
```

Pick the region closest to the games you intend to play — see the comments in
`fly.toml`. Any Docker host works equally well (Render, Railway, Cloud Run,
a VPS); the Dockerfile is plain and has no build arguments.

> The Dockerfile has not been built on this machine — Docker was not
> installed. It is conventional multi-stage Rust and the build/runtime images
> are pinned to the same Debian release, but the first `fly deploy` is where
> it gets proven.

### Just to try it, without deploying

Expose your local server with a tunnel and register that URL:

```bash
cargo run --release
ngrok http 8000              # use the https URL it prints
```

Fine for testing. Not for laddering: your laptop's uplink becomes the latency
budget, and the snake sleeps when the lid closes.

## Two settings that decide whether it plays well

**Never let the host scale to zero.** The engine allows 500 ms per move. A
cold start spends all of it, the move is forfeited, and a forfeited move early
in a game usually loses it. On Fly that is `auto_stop_machines = "off"` plus
`min_machines_running = 1`; other platforms call it "always on" or "no
idling". This is the single most common reason a snake that looks strong
locally plays badly on the ladder.

**Host near the engine.** The engine runs in Oregon (US-WEST), Virginia
(US-EAST) and the Netherlands (EUROPE). Round-trip time is subtracted from
thinking time, so a snake hosted across an ocean searches less deeply.
`play.battlesnake.com` shows the measured latency per game — watch it, and if
it is high, raise `BUDGET_MARGIN_MS` so the snake answers in time at a
shallower depth rather than timing out at a deeper one.

## Register and compete

1. **Create it** at
   [play.battlesnake.com/account/battlesnakes](https://play.battlesnake.com/account/battlesnakes)
   and paste your public URL into the *URL* field. You only do this once —
   afterwards you can redeploy freely without re-registering.
2. **Play a test game** at
   [play.battlesnake.com/account/games/create](https://play.battlesnake.com/account/games/create).
   Watch the replay, and watch the latency column.
3. **Join a leaderboard.** Standard, Duels, Royale and Constrictor each have
   their own, and this snake handles all four modes (plus wrapped), detecting
   the mode from each request. **Duels is where this one is strongest** — the
   search approximates nothing with a single opponent, and reaches depth 8-10
   against a real 500 ms deadline. In four-snake Standard it only gets to
   depth 4, so expect it to place lower there.
4. **Tournaments** are announced on the site and in the community Discord.

Check [docs.battlesnake.com](https://docs.battlesnake.com/) for the current
state of the platform — competition formats and schedules change, and this
file describes the mechanics, not the calendar.

## Tuning it once it is live

Everything is an environment variable, so no rebuild is needed:

| Variable | Default | Try changing it when |
| --- | --- | --- |
| `BUDGET_MARGIN_MS` | 120 | latency is high, or you see timeouts |
| `BUDGET_MS` | — | you want a fixed think time, ignoring the engine's timeout |
| `MAX_ENUMERATED` | 2 | raise to 3 for stronger (slower) four-snake play |
| `MAX_DEPTH` | 40 | rarely — the clock binds long before this does |
| `WEIGHTS` | — | e.g. `WEIGHTS=area=30,trap=800` after an `arena tune` run |

Watch the logs. One line per move, with the depth reached, the score and the
time taken — if the depth is consistently 4 or 5 in duels, latency or CPU is
the problem, not the search.
