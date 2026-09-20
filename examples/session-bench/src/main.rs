//! `session-bench`: hold N Live sessions open in one process and measure what
//! the runtime costs, with no model and no network.
//!
//! ```text
//! session-bench [--sessions N] [--turns T] [--think-ms MS]
//!               [--response-delay-ms MS] [--hold-secs S] [--json PATH]
//! ```

use std::time::Duration;

use example_session_bench::{BenchConfig, run};

fn usage() -> ! {
    eprintln!(
        "usage: session-bench [--sessions N] [--turns T] [--think-ms MS] \
         [--response-delay-ms MS] [--hold-secs S] [--turn-timeout-secs S] [--json PATH]"
    );
    std::process::exit(2)
}

fn parse<T: std::str::FromStr>(flag: &str, value: Option<String>) -> T {
    match value.and_then(|v| v.parse().ok()) {
        Some(v) => v,
        None => {
            eprintln!("{flag} needs a value");
            usage()
        }
    }
}

#[tokio::main]
async fn main() {
    let mut cfg = BenchConfig::default();
    let mut json_path: Option<String> = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--sessions" => cfg.sessions = parse(&arg, args.next()),
            "--turns" => cfg.turns = parse(&arg, args.next()),
            "--think-ms" => cfg.think = Duration::from_millis(parse(&arg, args.next())),
            "--response-delay-ms" => {
                cfg.response_delay = Duration::from_millis(parse(&arg, args.next()));
            }
            "--hold-secs" => cfg.hold = Duration::from_secs(parse(&arg, args.next())),
            "--turn-timeout-secs" => {
                cfg.turn_timeout = Duration::from_secs(parse(&arg, args.next()));
            }
            "--json" => json_path = Some(parse(&arg, args.next())),
            "-h" | "--help" => usage(),
            other => {
                eprintln!("unknown argument {other}");
                usage()
            }
        }
    }

    let report = run(cfg).await;
    print!("{}", report.summary());

    if let Some(path) = json_path {
        let json = serde_json::to_string_pretty(&report).expect("report serializes");
        if let Err(e) = std::fs::write(&path, json) {
            eprintln!("could not write {path}: {e}");
            std::process::exit(1);
        }
        eprintln!("wrote {path}");
    }

    if report.turns_failed > 0 || report.telemetry_response_count != report.turns_completed as u64 {
        std::process::exit(1);
    }
}
