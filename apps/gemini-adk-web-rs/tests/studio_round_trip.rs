//! Flow Studio <-> runtime document round trip.
//!
//! The Studio prunes a document (`prune` in `apps/studio/src/spec/paths.ts`)
//! before saving it. The runtime must load that document and write back the
//! same thing, or the Studio would lose user data on the next load.
#![allow(
    dead_code,
    unused_imports,
    reason = "the Studio router modules are included by path; this test uses a subset of them"
)]

// The web app is a binary crate, so the Studio router is compiled into this
// test from its source file. `app` is needed by the module's own unit tests.
#[path = "../src/app.rs"]
mod app;
#[path = "../src/studio.rs"]
mod studio;

use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::Request;
use gemini_adk_fluent_rs::spec::SessionSpec;
use serde_json::{Map, Value, json};
use tower::ServiceExt;

/// Mirrors `prune` in `apps/studio/src/spec/paths.ts` exactly:
///
/// - an array maps every element through `prune`, substituting `null` for an
///   element that pruned to nothing (positions are kept); an array that ends
///   up empty is dropped by its parent;
/// - an object keeps only the entries whose pruned value is not `undefined`,
///   not an empty array and not an empty object;
/// - `""` becomes `undefined`; every other scalar (0, false, null) is kept.
///
/// `None` is `undefined`. Note that an object always prunes to an object
/// (possibly `{}`); it is the parent that removes it.
fn prune(value: &Value) -> Option<Value> {
    match value {
        Value::Array(items) => Some(Value::Array(
            items
                .iter()
                .map(|v| prune(v).unwrap_or(Value::Null))
                .collect(),
        )),
        Value::Object(map) => {
            let mut out = Map::new();
            for (k, v) in map {
                let Some(pruned) = prune(v) else { continue };
                match &pruned {
                    Value::Array(a) if a.is_empty() => continue,
                    Value::Object(o) if o.is_empty() => continue,
                    _ => {}
                }
                out.insert(k.clone(), pruned);
            }
            Some(Value::Object(out))
        }
        Value::String(s) if s.is_empty() => None,
        other => Some(other.clone()),
    }
}

#[test]
fn prune_matches_the_studio_semantics() {
    let pruned = prune(&json!({
        "a": "", "b": [], "c": {}, "d": {"e": ""}, "f": 0, "g": false
    }))
    .unwrap();
    assert_eq!(pruned, json!({"f": 0, "g": false}));
    // Arrays keep positions.
    assert_eq!(
        prune(&json!({"x": ["", "a", {}, []]})).unwrap(),
        json!({"x": [null, "a", {}, []]})
    );
    assert_eq!(prune(&json!("")), None);
}

fn flows_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("static/examples/flows")
}

/// Every spec document under `static/examples/flows` (and its `reference`
/// directory); the two `index.json` files are catalogues, not specs.
fn documents() -> Vec<(String, Value)> {
    let mut out = Vec::new();
    for dir in [flows_dir(), flows_dir().join("reference")] {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|e| e != "json")
                || path.file_name().is_some_and(|n| n == "index.json")
            {
                continue;
            }
            let label = path
                .strip_prefix(flows_dir())
                .unwrap()
                .display()
                .to_string();
            let value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap())
                .unwrap_or_else(|e| panic!("{label}: {e}"));
            out.push((label, value));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    assert!(
        out.len() >= 12,
        "expected the example flows, got {}",
        out.len()
    );
    out
}

/// Every difference between two JSON values, as `path: before -> after`.
fn diff(path: &str, before: &Value, after: &Value, out: &mut Vec<String>) {
    match (before, after) {
        (Value::Object(a), Value::Object(b)) => {
            for (k, v) in a {
                match b.get(k) {
                    Some(w) => diff(&format!("{path}/{k}"), v, w, out),
                    None => out.push(format!("{path}/{k}: {v} -> (missing)")),
                }
            }
            for (k, w) in b {
                if !a.contains_key(k) {
                    out.push(format!("{path}/{k}: (missing) -> {w}"));
                }
            }
        }
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => {
            for (i, (v, w)) in a.iter().zip(b).enumerate() {
                diff(&format!("{path}/{i}"), v, w, out);
            }
        }
        _ if before == after => {}
        _ => out.push(format!("{path}: {before} -> {after}")),
    }
}

/// Normalisations the runtime is allowed to make, written as
/// `"<pointer with array indices as *>: <before> -> <after>"`. Every entry is
/// the runtime filling a field the document left out, with its default; none
/// changes or removes a value the author wrote. Anything not listed here
/// fails the round trip.
const ALLOWED_NORMALISATIONS: &[&str] = &[
    // TaskStep (`task_scenarios[].steps[]`): `defer` is a plain bool with no
    // `skip_serializing_if`, so the default `false` is written out.
    "/task_scenarios/*/steps/*/defer: (missing) -> false",
    // TaskStep: `trigger` defaults to the `turn` trigger and is always written.
    "/task_scenarios/*/steps/*/trigger: (missing) -> \"turn\"",
    // TaskStep: `model` is a `String` defaulting to "" and always written.
    // (The Studio's own `prune` drops the empty string on its next save.)
    "/task_scenarios/*/steps/*/model: (missing) -> \"\"",
    // Task commands: optional fields are `Option` without
    // `skip_serializing_if`, so they serialise as `null`.
    "/task_scenarios/*/steps/*/command/idempotency_key: (missing) -> null",
    "/task_scenarios/*/steps/*/command/parent: (missing) -> null",
    // Task commands: absent `args` / `output` objects default to `{}`.
    "/task_scenarios/*/steps/*/command/args: (missing) -> {}",
    "/task_scenarios/*/steps/*/command/output: (missing) -> {}",
    "/task_scenarios/*/steps/*/tasks/*/output: (missing) -> {}",
    // Extractors: `window` (default_window = 3) and `trigger` (every_turn)
    // are always written, at the top level and inside skills.
    "/extract/*/window: (missing) -> 3",
    "/extract/*/trigger: (missing) -> \"every_turn\"",
    "/skills/*/extract/*/window: (missing) -> 3",
    "/skills/*/extract/*/trigger: (missing) -> \"every_turn\"",
    // Promotion rules default to `keep_known`.
    "/extract/*/promote/*/policy: (missing) -> \"keep_known\"",
    // Skill tool parameters: an omitted `properties` / `required` becomes the
    // empty object / array in the written schema.
    "/skills/*/tools/*/parameters/required: (missing) -> []",
    "/skills/*/tools/*/parameters/properties: (missing) -> {}",
    // Skill inputs and outputs: an omitted `default` is written as "".
    "/skills/*/outputs/user:seating/default: (missing) -> \"\"",
    "/skills/*/outputs/user:diet/default: (missing) -> \"\"",
    "/skills/*/inputs/symptoms/default: (missing) -> \"\"",
    "/skills/*/inputs/preferred_time/default: (missing) -> \"\"",
];

fn generalise(difference: &str) -> String {
    let (pointer, rest) = difference.split_once(": ").unwrap();
    let pointer: Vec<&str> = pointer
        .split('/')
        .map(|seg| {
            if !seg.is_empty() && seg.bytes().all(|b| b.is_ascii_digit()) {
                "*"
            } else {
                seg
            }
        })
        .collect();
    format!("{}: {rest}", pointer.join("/"))
}

fn load(label: &str, value: &Value) -> SessionSpec {
    SessionSpec::from_value(value.clone()).unwrap_or_else(|e| panic!("{label}: {e}"))
}

#[test]
fn examples_round_trip_through_the_runtime_unchanged() {
    let mut report = Vec::new();
    for (label, original) in documents() {
        let spec = load(&label, &original);
        let after = serde_json::to_value(&spec).unwrap();
        let expected = prune(&original).unwrap();
        let mut diffs = Vec::new();
        diff("", &expected, &after, &mut diffs);
        diffs.retain(|d| !ALLOWED_NORMALISATIONS.contains(&generalise(d).as_str()));
        for d in diffs {
            report.push(format!("{label}: {d}"));
        }
    }
    assert!(
        report.is_empty(),
        "{} difference(s) between prune(original) and the runtime's serialization:\n{}",
        report.len(),
        report.join("\n")
    );
}

#[test]
fn serialization_is_idempotent() {
    for (label, original) in documents() {
        let first = serde_json::to_value(load(&label, &original)).unwrap();
        let second = serde_json::to_value(load(&label, &first)).unwrap();
        assert_eq!(first, second, "{label}: load/serialize is not idempotent");
    }
}

async fn post(app: &axum::Router, uri: &str, body: &Value) -> (u16, Value) {
    let request = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), 16 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn the_studio_routes_accept_every_example() {
    // `router` opens the bundle store lazily-by-uri; nothing is written here.
    let app = studio::router("static");
    let mut failures = Vec::new();
    for (label, doc) in documents() {
        let (status, validated) = post(&app, "/api/flows/validate", &doc).await;
        if status != 200 || validated["valid"] != json!(true) {
            failures.push(format!(
                "{label}: validate -> {status} valid={} errors={}",
                validated["valid"], validated["errors"]
            ));
            continue;
        }
        let (status, code) = post(&app, "/api/flows/codegen", &doc).await;
        let main_rs = code["main_rs"].as_str().unwrap_or_default();
        if status != 200 || code["valid"] != json!(true) || !main_rs.contains("Live::builder()") {
            failures.push(format!(
                "{label}: codegen -> {status} valid={} errors={} has Live::builder()={}",
                code["valid"],
                code["errors"],
                main_rs.contains("Live::builder()")
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
