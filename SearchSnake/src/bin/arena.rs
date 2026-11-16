//! Self-play arena: measures strength and tunes evaluation weights.
//!
//! ```text
//! arena bench                           nodes/sec and reachable depth
//! arena micro                           cost of each piece of a search node
//! arena duel  [games] [depth] [seed]    search vs the heuristic baseline
//! arena ladder [games] [depth]          round-robin over all agents
//! arena melee [games] [depth]           four-snake games
//! arena tune  [games] [depth] [rounds]  coordinate descent on the weights
//! arena noise [games] [depth]           self-play noise floor
//! ```

use std::env;
use std::time::{Duration, Instant};

use battlesnake::arena::{
    duel, play, Agent, GreedyAgent, HeuristicAgent, RandomAgent, Record, SearchAgent, Setup,
};
use battlesnake::eval::Weights;
use battlesnake::search::{Config, Searcher};
use battlesnake::state::{Rules, State};

fn arg<T: std::str::FromStr>(index: usize, default: T) -> T {
    env::args()
        .nth(index)
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn main() {
    let command = env::args().nth(1).unwrap_or_else(|| "duel".to_string());
    match command.as_str() {
        "bench" => bench(),
        "micro" => micro(),
        "duel" => run_duel(),
        "ladder" => run_ladder(),
        "melee" => run_melee(),
        "tune" => run_tune(),
        "noise" => run_noise(),
        other => {
            eprintln!("unknown command '{}'", other);
            eprintln!("usage: arena [bench|micro|duel|ladder|melee|tune|noise] ...");
            std::process::exit(2);
        }
    }
}

/// A mid-game position with four snakes, used as a fixed benchmark.
fn bench_position() -> (Rules, State) {
    let rules = Rules::new(11, 11, 4);
    let mut state = State::empty();
    let body = |pts: &[(i32, i32)]| -> Vec<u16> {
        pts.iter().map(|&(x, y)| rules.cell(x, y)).collect()
    };
    state.place_snake(0, &body(&[(5, 5), (5, 4), (5, 3), (4, 3)]), 80);
    state.place_snake(1, &body(&[(3, 7), (3, 8), (3, 9)]), 70);
    state.place_snake(2, &body(&[(8, 5), (9, 5), (9, 4), (9, 3)]), 90);
    state.place_snake(3, &body(&[(2, 2), (1, 2), (0, 2)]), 60);
    state.food.insert(rules.cell(5, 8));
    state.food.insert(rules.cell(1, 6));
    state.turn = 60;
    (rules, state)
}

/// Times the individual pieces of a search node, so tuning effort goes where
/// the time actually is rather than where it is assumed to be.
fn micro() {
    use battlesnake::eval::{evaluate, Weights};
    use battlesnake::fill::{reach, voronoi, Scratch};
    use battlesnake::state::{MAX_SNAKES, UP};

    let (rules, state) = bench_position();
    let mut scratch = Scratch::new();
    let reps = 200_000;

    let time = |label: &str, reps: u32, mut body: Box<dyn FnMut()>| {
        let start = Instant::now();
        for _ in 0..reps {
            body();
        }
        let per = start.elapsed().as_secs_f64() / reps as f64;
        println!("  {:<28} {:>8.0} ns/op", label, per * 1e9);
    };

    println!("per-operation costs (4-snake mid-game position)\n");
    {
        let s = state.clone();
        time(
            "state.clone()",
            reps,
            Box::new(move || {
                std::hint::black_box(s.clone());
            }),
        );
    }
    {
        let s = state.clone();
        let r = rules.clone();
        time(
            "clone + apply(one turn)",
            reps,
            Box::new(move || {
                let mut child = s.clone();
                child.apply(&r, &[UP; MAX_SNAKES]);
                std::hint::black_box(child.alive);
            }),
        );
    }
    {
        let s = state.clone();
        let r = rules.clone();
        let mut sc = Scratch::new();
        time(
            "voronoi()",
            reps,
            Box::new(move || {
                std::hint::black_box(voronoi(&s, &r, &mut sc).cells[0]);
            }),
        );
    }
    {
        let s = state.clone();
        let r = rules.clone();
        let mut sc = Scratch::new();
        time(
            "reach()",
            reps,
            Box::new(move || {
                std::hint::black_box(reach(&s, &r, 0, &mut sc));
            }),
        );
    }
    let w = Weights::DEFAULT;
    time(
        "evaluate() [full leaf]",
        reps,
        Box::new(move || {
            std::hint::black_box(evaluate(&state, &rules, 0, &w, &mut scratch));
        }),
    );
}

fn bench() {
    println!("search throughput\n");
    for (label, snakes) in [("one-on-one", 2u8), ("four snakes", 4u8)] {
        let (mut rules, state) = bench_position();
        rules.n = snakes;
        let mut trimmed = state.clone();
        for s in snakes as usize..4 {
            trimmed.alive &= !(1 << s);
        }
        for &budget_ms in &[50u64, 150, 300] {
            let mut searcher = Searcher::new(Config {
                budget: Duration::from_millis(budget_ms),
                ..Config::default()
            });
            let outcome = searcher.find_move(&trimmed, &rules, 0);
            let rate = outcome.nodes as f64 / outcome.elapsed.as_secs_f64().max(1e-9);
            println!(
                "  {:<12} {:>4}ms budget -> depth {:>2}  {:>9} nodes  {:>9.0} nodes/s  ({:?})",
                label,
                budget_ms,
                outcome.depth,
                outcome.nodes,
                rate,
                outcome.elapsed
            );
        }
    }
}

fn report(title: &str, record: Record) {
    println!(
        "  {:<34} {:>3}W {:>3}L {:>3}D   score {:.1}%",
        title,
        record.wins,
        record.losses,
        record.draws,
        100.0 * record.score()
    );
}

fn run_duel() {
    let games: u32 = arg(2, 40);
    let depth: i32 = arg(3, 6);
    let seed: u64 = arg(4, 20_260_101);
    let setup = Setup::standard(2);

    println!(
        "one-on-one, {} games, search depth {}, seed {}\n",
        games, depth, seed
    );

    let started = Instant::now();
    {
        let mut ours = SearchAgent::at_depth("search", depth, Weights::DEFAULT);
        let mut them = HeuristicAgent::new("heuristic");
        report(
            "search vs flood-fill heuristic",
            duel(&setup, &mut ours, &mut them, games, seed),
        );
    }
    {
        let mut ours = SearchAgent::at_depth("search", depth, Weights::DEFAULT);
        let mut them = GreedyAgent::new("greedy");
        report(
            "search vs one-ply greedy (same eval)",
            duel(&setup, &mut ours, &mut them, games, seed),
        );
    }
    {
        let mut ours = SearchAgent::at_depth("search", depth, Weights::DEFAULT);
        let mut them = RandomAgent::new("random", 7);
        report(
            "search vs safe-random",
            duel(&setup, &mut ours, &mut them, games, seed),
        );
    }
    {
        let mut deep = SearchAgent::at_depth("deep", depth, Weights::DEFAULT);
        let mut shallow = SearchAgent::at_depth("shallow", 2, Weights::DEFAULT);
        report(
            &format!("search depth {} vs depth 2", depth),
            duel(&setup, &mut deep, &mut shallow, games, seed),
        );
    }
    println!("\nfinished in {:?}", started.elapsed());
}

fn run_ladder() {
    let games: u32 = arg(2, 20);
    let depth: i32 = arg(3, 5);
    let setup = Setup::standard(2);
    let names = ["search", "greedy", "heuristic", "random"];

    let build = |which: usize| -> Box<dyn Agent> {
        match which {
            0 => Box::new(SearchAgent::at_depth("search", depth, Weights::DEFAULT)),
            1 => Box::new(GreedyAgent::new("greedy")),
            2 => Box::new(HeuristicAgent::new("heuristic")),
            _ => Box::new(RandomAgent::new("random", 99)),
        }
    };

    println!("round robin, {} games per pairing, depth {}\n", games, depth);
    let mut totals = [Record::default(); 4];
    for i in 0..names.len() {
        for j in (i + 1)..names.len() {
            let mut a = build(i);
            let mut b = build(j);
            let record = duel(&setup, a.as_mut(), b.as_mut(), games, 7_000 + (i * 10 + j) as u64);
            report(&format!("{} vs {}", names[i], names[j]), record);
            totals[i].wins += record.wins;
            totals[i].losses += record.losses;
            totals[i].draws += record.draws;
            totals[j].wins += record.losses;
            totals[j].losses += record.wins;
            totals[j].draws += record.draws;
        }
    }

    println!("\nstandings");
    let mut order: Vec<usize> = (0..names.len()).collect();
    order.sort_by(|&a, &b| totals[b].score().total_cmp(&totals[a].score()));
    for rank in order {
        println!(
            "  {:<12} {:>3}W {:>3}L {:>3}D   score {:.1}%",
            names[rank],
            totals[rank].wins,
            totals[rank].losses,
            totals[rank].draws,
            100.0 * totals[rank].score()
        );
    }
}

fn run_melee() {
    let games: u32 = arg(2, 20);
    let depth: i32 = arg(3, 5);
    let setup = Setup::standard(4);
    println!("four-snake melee, {} games, depth {}\n", games, depth);

    let mut wins = [0u32; 4];
    let mut draws = 0u32;
    for g in 0..games {
        let mut ours = SearchAgent::at_depth("search", depth, Weights::DEFAULT);
        let mut h1 = HeuristicAgent::new("heuristic-1");
        let mut h2 = HeuristicAgent::new("heuristic-2");
        let mut greedy = GreedyAgent::new("greedy");
        // Rotate our seat so start positions cannot flatter us.
        let seat = (g % 4) as usize;
        let mut others: Vec<&mut dyn Agent> = vec![&mut h1, &mut h2, &mut greedy];
        let mut seats: Vec<&mut dyn Agent> = Vec::with_capacity(4);
        let mut ours_slot: Option<&mut dyn Agent> = Some(&mut ours);
        for slot in 0..4 {
            if slot == seat {
                seats.push(ours_slot.take().expect("our agent is placed once"));
            } else {
                seats.push(others.remove(0));
            }
        }
        let result = play(&setup, &mut seats, 31_337 + g as u64);
        match result.winner {
            Some(w) if w == seat => wins[0] += 1,
            Some(_) => wins[1] += 1,
            None => draws += 1,
        }
    }
    println!(
        "  search won {}/{} ({:.1}%), field won {}, draws {}",
        wins[0],
        games,
        100.0 * wins[0] as f64 / games as f64,
        wins[1],
        draws
    );
    println!(
        "  (an even split between four snakes would be {:.1}%)",
        25.0
    );
}

/// The win rate a change must clear to be believed: two standard errors above
/// even, for this many games.
///
/// Worth being strict about. Two *identical* agents score 58% over 24 games
/// here — seeded games differ in start position, so the seat lottery alone
/// moves the number by a standard deviation or two. A flat "better than 60%"
/// rule would cheerfully accept pure noise and ratchet the weights nowhere.
fn accept_threshold(games: u32) -> f64 {
    0.5 + 2.0 * (0.25 / games.max(1) as f64).sqrt()
}

/// Duels the default agent against an identical copy to establish how much of
/// a win rate is noise. Any tuning result inside this band means nothing.
fn run_noise() {
    let games: u32 = arg(2, 50);
    let depth: i32 = arg(3, 4);
    let setup = Setup::standard(2);
    let mut a = SearchAgent::at_depth("default-a", depth, Weights::DEFAULT);
    let mut b = SearchAgent::at_depth("default-b", depth, Weights::DEFAULT);
    let record = duel(&setup, &mut a, &mut b, games, 4242);
    println!("noise floor, {} games, depth {}\n", games, depth);
    report("identical agents (expect ~50%)", record);
    println!(
        "\n  a change needs >{:.1}% over {} games to mean anything",
        100.0 * accept_threshold(games),
        games
    );
}

/// Coordinate descent: try each weight up and down, keep a change only if it
/// beats the incumbent by a margin the sample size can actually resolve.
fn run_tune() {
    let games: u32 = arg(2, 24);
    let depth: i32 = arg(3, 4);
    let rounds: usize = arg(4, 2);
    let setup = Setup::standard(2);

    let mut champion = Weights::DEFAULT;
    println!(
        "tuning: {} games per trial, depth {}, {} round(s)\n",
        games, depth, rounds
    );

    for round in 1..=rounds {
        let mut improved = 0;
        for field in Weights::FIELDS {
            let current = champion.get(field).expect("field exists");
            // Proportional steps, with a floor so small weights can still move.
            let step = (current.abs() / 4).max(2);
            for candidate_value in [current + step, current - step] {
                if candidate_value == current {
                    continue;
                }
                let mut candidate = champion;
                candidate.set(field, candidate_value);
                if candidate == champion {
                    continue;
                }
                let mut a = SearchAgent::at_depth("candidate", depth, candidate);
                let mut b = SearchAgent::at_depth("champion", depth, champion);
                let record = duel(&setup, &mut a, &mut b, games, 555 + round as u64);
                if record.score() > accept_threshold(games) {
                    println!(
                        "  round {} {:<12} {:>6} -> {:>6}   {:.0}%",
                        round,
                        field,
                        current,
                        candidate_value,
                        100.0 * record.score()
                    );
                    champion = candidate;
                    improved += 1;
                    break;
                }
            }
        }
        println!("  round {} accepted {} change(s)", round, improved);
        if improved == 0 {
            break;
        }
    }

    println!("\nWEIGHTS=");
    let spec: Vec<String> = Weights::FIELDS
        .iter()
        .map(|f| format!("{}={}", f, champion.get(f).expect("field exists")))
        .collect();
    println!("{}", spec.join(","));

    let mut a = SearchAgent::at_depth("tuned", depth, champion);
    let mut b = SearchAgent::at_depth("default", depth, Weights::DEFAULT);
    report(
        "tuned vs default",
        duel(&setup, &mut a, &mut b, games * 2, 909),
    );
}
