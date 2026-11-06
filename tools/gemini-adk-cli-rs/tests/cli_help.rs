//! Every `adk` subcommand answers `--help`, usage errors exit non-zero with a
//! message on stderr, and a missing spec file is reported by path.
//!
//! The command tree is discovered recursively from the `Commands:` section of
//! `--help`, then compared with a hard-coded set so that adding a subcommand
//! means adding it to this test on purpose.

use std::collections::BTreeSet;
use std::process::{Command, Output};

fn adk(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_adk"))
        .args(args)
        // Nothing here may reach a provider.
        .env_remove("GEMINI_API_KEY")
        .env_remove("GOOGLE_API_KEY")
        .output()
        .expect("spawn adk")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Names listed under `Commands:` in a `--help` page, without clap's own
/// `help` pseudo-command.
fn listed_subcommands(help: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut in_section = false;
    for line in help.lines() {
        if line.trim_end() == "Commands:" || line.trim_end() == "Subcommands:" {
            in_section = true;
            continue;
        }
        if !in_section {
            continue;
        }
        if line.trim().is_empty() {
            break;
        }
        // Entries are indented two spaces; wrapped descriptions are indented further.
        if line.starts_with("  ")
            && !line.starts_with("   ")
            && let Some(name) = line.split_whitespace().next()
            && name != "help"
        {
            names.push(name.to_string());
        }
    }
    names
}

struct Node {
    path: Vec<String>,
    leaf: bool,
    /// The usage line shows a required positional (`<SPEC>`).
    required_args: bool,
}

fn discover() -> Vec<Node> {
    let mut nodes = Vec::new();
    let mut stack: Vec<Vec<String>> = vec![vec![]];
    while let Some(path) = stack.pop() {
        let mut args: Vec<&str> = path.iter().map(String::as_str).collect();
        args.push("--help");
        let out = adk(&args);
        let label = format!("adk {} --help", path.join(" "));
        assert!(out.status.success(), "{label} failed: {:?}", out.status);
        let stdout = text(&out.stdout);
        assert!(!stdout.trim().is_empty(), "{label} printed nothing");
        assert!(stdout.contains("Usage"), "{label} has no Usage:\n{stdout}");

        let children = listed_subcommands(&stdout);
        let usage = stdout
            .lines()
            .find(|l| l.starts_with("Usage:"))
            .unwrap_or_default()
            .to_string();
        let required_args = usage.contains('<') && !usage.contains("<COMMAND>");
        for child in &children {
            let mut next = path.clone();
            next.push(child.clone());
            stack.push(next);
        }
        nodes.push(Node {
            path,
            leaf: children.is_empty(),
            required_args,
        });
    }
    nodes.sort_by(|a, b| a.path.cmp(&b.path));
    nodes
}

const EXPECTED: &[&str] = &[
    "",
    "api",
    "bundle",
    "bundle get",
    "bundle label",
    "bundle list",
    "bundle push",
    "create",
    "deploy",
    "doctor",
    "eval",
    "flow",
    "flow ci",
    "flow graph",
    "flow inspect",
    "flow replay",
    "flow schema",
    "flow simulate",
    "flow validate",
    "flow why",
    "replay",
    "run",
    "session",
    "session replay",
    "session scenario",
    "spec",
    "spec answer",
    "spec call",
    "spec catalog",
    "spec check",
    "spec codegen",
    "spec graph",
    "spec patch",
    "spec plan",
    "spec run",
    "spec schema",
    "spec test",
    "web",
];

#[test]
fn every_subcommand_has_help_and_the_set_is_the_expected_one() {
    let nodes = discover();
    let found: BTreeSet<String> = nodes.iter().map(|n| n.path.join(" ")).collect();
    let expected: BTreeSet<String> = EXPECTED.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        found, expected,
        "the adk command tree changed: update EXPECTED in tests/cli_help.rs"
    );
}

#[test]
fn leaf_commands_with_required_arguments_fail_without_them() {
    let nodes = discover();
    let mut checked = 0;
    for node in nodes.iter().filter(|n| n.leaf && n.required_args) {
        let args: Vec<&str> = node.path.iter().map(String::as_str).collect();
        let out = adk(&args);
        let label = format!("adk {}", node.path.join(" "));
        assert!(
            !out.status.success(),
            "{label} with no arguments exited 0:\n{}",
            text(&out.stdout)
        );
        assert!(
            !text(&out.stderr).trim().is_empty(),
            "{label} failed without a message on stderr"
        );
        checked += 1;
    }
    assert!(
        checked >= 20,
        "only {checked} leaves had required arguments"
    );
}

#[test]
fn a_command_group_without_a_subcommand_is_a_usage_error() {
    for group in ["spec", "bundle", "flow", "session"] {
        let out = adk(&[group]);
        assert!(!out.status.success(), "adk {group} exited 0");
        assert!(
            !text(&out.stderr).trim().is_empty(),
            "adk {group}: no stderr"
        );
    }
}

#[test]
fn an_unknown_subcommand_exits_non_zero() {
    let out = adk(&["definitely-not-a-command"]);
    assert!(!out.status.success());
    assert!(text(&out.stderr).contains("definitely-not-a-command"));
}

fn assert_missing_file_is_named(args: &[&str], path: &str) {
    let out = adk(args);
    let label = format!("adk {}", args.join(" "));
    assert!(
        !out.status.success(),
        "{label} exited 0\nstdout: {}\nstderr: {}",
        text(&out.stdout),
        text(&out.stderr)
    );
    let all = format!("{}{}", text(&out.stdout), text(&out.stderr));
    assert!(all.contains(path), "{label} does not name the path:\n{all}");
}

#[test]
fn a_missing_spec_file_is_reported_by_path_with_a_non_zero_exit() {
    let path = "/nonexistent/file.json";
    for args in [
        vec!["spec", "check", path],
        vec!["spec", "test", path],
        vec!["spec", "graph", path],
        vec!["spec", "plan", path],
        vec!["spec", "run", path],
        vec!["bundle", "push", path],
    ] {
        assert_missing_file_is_named(&args, path);
    }
}

/// These commands exit non-zero but print a bare `Os { code: 2, .. }` without
/// the path (a known defect; this test fails until the error carries it).
#[test]
fn flow_session_and_replay_commands_name_a_missing_file() {
    let path = "/nonexistent/file.json";
    let mut failures = Vec::new();
    for args in [
        vec!["flow", "inspect", path],
        vec!["flow", "graph", path],
        vec!["flow", "validate", path],
        vec!["flow", "ci", path],
        vec!["replay", path],
        vec!["session", "replay", path],
        vec!["session", "scenario", path],
    ] {
        let out = adk(&args);
        let all = format!("{}{}", text(&out.stdout), text(&out.stderr));
        assert!(!out.status.success(), "adk {} exited 0", args.join(" "));
        if !all.contains(path) {
            failures.push(format!("adk {}: {}", args.join(" "), all.trim()));
        }
    }
    assert!(
        failures.is_empty(),
        "errors that do not name the path:\n{}",
        failures.join("\n")
    );
}
