//! Battlesnake HTTP server.
//!
//! Configuration comes from the environment so the same binary can be tuned
//! and deployed without a rebuild:
//!
//! ```text
//! PORT=8000                     listen port
//! THREADS=4                     accept workers
//! BUDGET_MARGIN_MS=120          held back from the engine's timeout
//! BUDGET_MS=                    fixed think time, overriding the timeout
//! MAX_ENUMERATED=2              opponents branched on jointly
//! MAX_DEPTH=40                  iterative-deepening ceiling
//! WEIGHTS=area=30,trap=800      evaluation weight overrides
//! SNAKE_COLOR / SNAKE_HEAD / SNAKE_TAIL / SNAKE_AUTHOR
//! ```

use std::env;
use std::time::Duration;

use battlesnake::api::{self, Info};
use battlesnake::eval::Weights;
use battlesnake::http::{Response, Server};
use battlesnake::search::{Config, Searcher};
use battlesnake::state::dir_name;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn env_i64(key: &str, default: i64) -> i64 {
    env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(default)
}

fn env_string(key: &str, default: &'static str) -> &'static str {
    match env::var(key) {
        // Leaked deliberately: read once at startup and used for the process
        // lifetime, which keeps `Info` a cheap `Copy`-ish struct of `&'static str`.
        Ok(value) if !value.trim().is_empty() => Box::leak(value.into_boxed_str()),
        _ => default,
    }
}

/// Parses `WEIGHTS=area=30,trap=800`, warning about anything unrecognised
/// rather than silently ignoring a typo in a tuning run.
fn weights_from_env() -> Weights {
    let mut weights = Weights::DEFAULT;
    let Ok(spec) = env::var("WEIGHTS") else {
        return weights;
    };
    for item in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        match item.split_once('=') {
            Some((name, value)) => match value.trim().parse::<i32>() {
                Ok(parsed) => {
                    if !weights.set(name.trim(), parsed) {
                        eprintln!("warning: unknown weight '{}'", name.trim());
                    }
                }
                Err(_) => eprintln!("warning: '{}' is not an integer", value.trim()),
            },
            None => eprintln!("warning: malformed weight '{}'", item),
        }
    }
    weights
}

fn main() {
    let port = env_i64("PORT", 8000);
    let threads = env_i64("THREADS", 4).clamp(1, 64) as usize;
    let margin_ms = env_i64("BUDGET_MARGIN_MS", 120);
    let fixed_budget_ms = env_i64("BUDGET_MS", 0);
    let max_enumerated = env_i64("MAX_ENUMERATED", 2).clamp(1, 7) as usize;
    let max_depth = env_i64("MAX_DEPTH", 40).clamp(1, 96) as i32;
    let weights = weights_from_env();

    let info = Info {
        author: env_string("SNAKE_AUTHOR", "claude"),
        color: env_string("SNAKE_COLOR", "#1f6f4a"),
        head: env_string("SNAKE_HEAD", "smart-caterpillar"),
        tail: env_string("SNAKE_TAIL", "weight"),
        version: VERSION,
    };
    let info_body = info.to_json();

    let server = match Server::bind(("0.0.0.0", port as u16)) {
        Ok(server) => server,
        Err(e) => {
            eprintln!("could not bind port {}: {}", port, e);
            std::process::exit(1);
        }
    };
    eprintln!(
        "battlesnake {} listening on 0.0.0.0:{} ({} threads, depth<={}, enumerate<={})",
        VERSION, port, threads, max_depth, max_enumerated
    );

    let result = server.run(threads, move |method, path, body| match (method, path) {
        ("GET", "/") => Response::json(info_body.clone()),
        ("POST", "/start") => Response::empty(),
        ("POST", "/end") => Response::empty(),
        ("POST", "/move") => {
            let chosen = handle_move(
                body,
                margin_ms,
                fixed_budget_ms,
                max_enumerated,
                max_depth,
                weights,
            );
            Response::json(chosen)
        }
        ("GET", "/move") | ("GET", "/start") | ("GET", "/end") => {
            Response::text(405, "method not allowed")
        }
        _ => Response::text(404, "not found"),
    });
    if let Err(e) = result {
        eprintln!("server stopped: {}", e);
        std::process::exit(1);
    }
}

fn handle_move(
    body: &str,
    margin_ms: i64,
    fixed_budget_ms: i64,
    max_enumerated: usize,
    max_depth: i32,
    weights: Weights,
) -> String {
    let request = match api::parse_request(body) {
        Ok(request) => request,
        Err(e) => {
            // Never forfeit a turn over a payload problem: play the best move
            // that can be read straight off the JSON.
            eprintln!("warning: {} — falling back to a reflex move", e);
            return api::move_json(api::fallback_move(body), "");
        }
    };

    let budget_ms = if fixed_budget_ms > 0 {
        fixed_budget_ms
    } else {
        (request.timeout_ms - margin_ms).clamp(30, 3_000)
    };

    let mut searcher = Searcher::new(Config {
        budget: Duration::from_millis(budget_ms as u64),
        max_depth,
        max_enumerated,
        weights,
    });
    let outcome = searcher.find_move(&request.state, &request.rules, request.me);

    eprintln!(
        "turn {:>4}  {:>5}  depth {:>2}  score {:>8}  nodes {:>9}  {:>4}ms",
        request.turn,
        dir_name(outcome.best),
        outcome.depth,
        outcome.score,
        outcome.nodes,
        outcome.elapsed.as_millis(),
    );

    api::move_json(outcome.best, &format!("d{} n{}", outcome.depth, outcome.nodes))
}
