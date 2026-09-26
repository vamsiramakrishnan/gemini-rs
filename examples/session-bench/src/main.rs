//! `session-bench`: hold N Live sessions open in one process and measure what
//! the runtime costs, with no model and no network.
//!
//! ```text
//! session-bench [--sessions N] [--turns T] [--think-ms MS]
//!               [--response-delay-ms MS] [--hold-secs S] [--json PATH]
//! session-bench --jitter-secs S [--sessions N] [--json PATH]
//! ```
//!
//! `--jitter-secs` measures microphone audio instead of text turns: every
//! session streams 20 ms chunks for `S` seconds, and the report gives the
//! mic-to-wire latency and the jitter of the audio reaching the transport.

use std::time::Duration;

use example_session_bench::{BenchConfig, JitterConfig, run, run_jitter};

fn usage() -> ! {
    eprintln!(
        "usage: session-bench [--sessions N] [--turns T] [--think-ms MS] \
         [--response-delay-ms MS] [--hold-secs S] [--turn-timeout-secs S] [--json PATH]\n\
         \x20      session-bench --jitter-secs S [--sessions N] [--json PATH]"
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
    let mut jitter_secs: Option<u64> = None;

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
            "--jitter-secs" => jitter_secs = Some(parse(&arg, args.next())),
            "-h" | "--help" => usage(),
            other => {
                eprintln!("unknown argument {other}");
                usage()
            }
        }
    }

    if let Some(secs) = jitter_secs {
        let report = run_jitter(JitterConfig {
            sessions: cfg.sessions,
            duration: Duration::from_secs(secs),
            ..JitterConfig::default()
        })
        .await;
        print!("{}", report.summary());
        write_json(json_path, &report);
        if report.chunks_wired != report.chunks_sent || report.chunks_reordered > 0 {
            std::process::exit(1);
        }
        return;
    }

    let report = run(cfg).await;
    print!("{}", report.summary());

    write_json(json_path, &report);

    if report.turns_failed > 0 || report.telemetry_response_count != report.turns_completed as u64 {
        std::process::exit(1);
    }
}

fn write_json(path: Option<String>, report: &impl serde::Serialize) {
    let Some(path) = path else { return };
    let json = serde_json::to_string_pretty(report).expect("report serializes");
    if let Err(e) = std::fs::write(&path, json) {
        eprintln!("could not write {path}: {e}");
        std::process::exit(1);
    }
    eprintln!("wrote {path}");
}
