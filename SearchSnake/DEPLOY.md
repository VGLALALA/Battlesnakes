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

## Hosting it for free

There is a real tension here, and it is sharper for this snake than for a
typical web app: **this search is CPU-bound and on a 500 ms clock.** Free tiers
economise on exactly the two things it spends — CPU and uptime. A throttled or
sleeping host does not just feel slower, it costs search depth, and depth is
the strength.

Ranked for *this* program:

### Oracle Cloud Always Free — the best genuinely-free option

A real always-on Linux VM: ARM Ampere A1, no sleep, no expiry. Oracle halved
the Always Free ARM allowance in June 2026 from 4 OCPU / 24 GB to **2 OCPU /
12 GB**, which is still two full cores — roughly twenty times the CPU of a free
Render instance, and it never sleeps. For a snake that converts CPU directly
into plies, nothing else free comes close.

```bash
# on the VM, once
sudo apt update && sudo apt install -y git build-essential
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"
git clone https://github.com/VGLALALA/Battlesnakes && cd Battlesnakes/SearchSnake
cargo build --release
```

Then keep it running across reboots and logouts with systemd:

```ini
# /etc/systemd/system/searchsnake.service
[Unit]
Description=SearchSnake
After=network.target

[Service]
ExecStart=/home/ubuntu/Battlesnakes/SearchSnake/target/release/battlesnake
Environment=PORT=8000
Environment=THREADS=2
Restart=always
User=ubuntu

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl enable --now searchsnake
```

Two things will bite you. Oracle's firewall blocks everything by default — open
your port in both the VCN security list *and* the instance's own `iptables`.
And A1 capacity is scarce in US regions; "out of host capacity" is common, so
you may have to retry or pick a different region (which costs latency).

### Render — easiest free, but understand what you are getting

`render.yaml` and the `Dockerfile` are in this folder. Easiest route is the
dashboard: New → Web Service, point it at the repo, set **Root Directory** to
`SearchSnake`, **Language** to Docker, region Oregon. Render sets `PORT` and
the server reads it.

The free plan gives **0.1 CPU and 512 MB**, and **sleeps after 15 minutes**
idle with a cold start of about a minute. Both matter:

- *The sleep is fatal if unmitigated.* A cold start burns far more than the
  500 ms deadline, so the first move of a game is forfeited. Fix it with an
  uptime monitor (UptimeRobot's free tier, or cron-job.org) pinging `/` every
  10 minutes — that counts as inbound traffic and it never idles out. Watch
  the allowance: free gives 750 instance-hours a month and a 24/7 service
  burns ~720 of them, so this works but leaves no room for a second service.
- *0.1 CPU costs plies.* A tenth of a core will not reach depth 8. Expect
  materially weaker play than the numbers in the README, which were measured
  on a full core. Raise `BUDGET_MARGIN_MS` so it answers shallow rather than
  late.

Render's own docs say not to use free instances for production. For trying it
out, fine. For laddering, Oracle or $7/mo on Render's Starter plan.

### Fly.io — no longer free

`fly.toml` is still here and Fly is still a good host, but **the free tier is
gone** for organisations created after 7 October 2024: a short trial, then
pay-as-you-go at roughly $5/month minimum with a card on file. Treat `fly.toml`
as the cheap-paid option, not the free one.

```bash
fly launch --copy-config --no-deploy   # keeps the committed fly.toml
fly deploy
fly open
```

### Cloudflare — the wrong shape for this workload

Worth explaining rather than waving away, because Cloudflare is excellent and
free and the reason it does not fit here is specific and interesting.

**Workers Free gives 10 ms of CPU time per request.** This snake wants ~380 ms.
Cloudflare's own figure for the average Worker is about 2.2 ms, which is the
giveaway: Workers is built for short, I/O-bound edge requests, and the limit is
on *CPU time* precisely because waiting on the network is the expected
behaviour. A tree search is the opposite — it is all CPU and no waiting, so
nothing about that limit is forgiving. Exceeding it fails that invocation, so
the move is forfeited rather than merely weak.

For scale: 10 ms at the measured ~1.2M nodes/s is about 12,000 nodes, so depth
5-ish — and that is before WASM's slowdown and before JSON parsing eats into
the same budget. You could force it to fit by setting `BUDGET_MS=5`, but you
would be deliberately crippling the one thing that makes the snake strong, and
flirting with hard failures, to use a platform that is free in a way Oracle
already is without the compromise.

**Workers Paid ($5/mo) lifts CPU to 30 s**, which would be ample. The obstacle
there is architectural, not numeric: Workers runs V8 isolates and WASM, where
`std::net::TcpListener` and `std::thread` do not exist. `src/http.rs` and
`src/main.rs` would have to be replaced with a `workers-rs` fetch handler and
the crate compiled to `wasm32-unknown-unknown`. The interesting part is that
everything that matters would port cleanly — `state.rs`, `fill.rs`, `eval.rs`
and `search.rs` are pure computation with no I/O and no threads — so this is a
plausible afternoon, not a rewrite. Expect to lose roughly a ply to WASM being
slower than native.

**Cloudflare Containers** are paid-only (they need the same $5/mo Workers Paid
plan) and the instance types are fractional: `lite` is 1/16 vCPU and `basic`
1/4 vCPU. They also scale to zero, bringing back the cold-start problem. At
that price a full core elsewhere is the better trade.

One thing Cloudflare's edge network does *not* buy you here: the engine runs
from three fixed regions, so what matters is the hop from the engine to your
server, not from a user to the edge.

### Not worth it for this

Vercel and Netlify are built for short-lived functions and static sites, with
the same CPU-shaped problem as Workers. Replit sleeps on free and its pricing
is effort-based. GitHub Codespaces is a dev environment, not a host.

> The Dockerfile has not been built on this machine — Docker was not
> installed. It is conventional multi-stage Rust and the build/runtime images
> are pinned to the same Debian release, but your first deploy is where it
> gets proven. The Oracle route above skips Docker entirely.

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
idling"; on a free tier that sleeps, it means an uptime monitor pinging `/`
every 10 minutes. This is the single most common reason a snake that looks
strong locally plays badly on the ladder.

**Give it a whole CPU if you can.** Unlike most web services, this one spends
its entire request budget computing. Search cost grows exponentially with
depth, so CPU converts almost linearly into plies — a tenth of a core costs
roughly three plies against a full one. It is the difference between the
depth-8 play the README measured and something distinctly weaker.

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
