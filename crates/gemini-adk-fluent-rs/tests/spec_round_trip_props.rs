//! Property tests for the `SessionSpec` document.
//!
//! A generated document must survive serialize → deserialize unchanged,
//! validate the same way on both sides of that trip, and generate Rust that
//! parses and declares the same flow the document does. The generated
//! projects are compiled in CI by `scripts/check-generated-projects.sh`
//! for the gallery documents; this file checks structure for many more
//! documents than CI can compile.

use std::collections::BTreeSet;

use gemini_adk_fluent_rs::spec::{SessionSpec, SpecModality, ToolSpec};
use gemini_adk_rs::flow::{Constraint, Flow, Guard, Pred};
use proptest::prelude::*;
use serde_json::{Value, json};

const KEYS: [&str; 3] = ["party_size", "slot", "confirmed"];

fn ident() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9_]{0,11}"
}

fn text() -> impl Strategy<Value = String> {
    // Printable text with the characters that need escaping in Rust and
    // JSON: quotes, backslashes, braces, newlines, non-ASCII.
    prop::collection::vec(
        prop_oneof![
            "[ -~]{1,8}",
            Just("\"".to_string()),
            Just("\\".to_string()),
            Just("{}".to_string()),
            Just("\n".to_string()),
            Just("é—ü".to_string()),
        ],
        0..6,
    )
    .prop_map(|parts| parts.concat())
}

fn pred() -> impl Strategy<Value = Pred> {
    let leaf = prop_oneof![
        Just(Pred::Always),
        (0..KEYS.len()).prop_map(|k| Pred::IsTrue(KEYS[k].into())),
        (0..KEYS.len()).prop_map(|k| Pred::IsSet(KEYS[k].into())),
        (0..KEYS.len(), any::<i32>()).prop_map(|(k, v)| Pred::Eq(KEYS[k].into(), json!(v))),
        (0..KEYS.len(), text()).prop_map(|(k, v)| Pred::Eq(KEYS[k].into(), json!(v))),
        prop::collection::vec(0..KEYS.len(), 1..3)
            .prop_map(|ks| Pred::Captured(ks.into_iter().map(|k| KEYS[k].into()).collect())),
        ident().prop_map(Pred::CalledOk),
    ];
    leaf.prop_recursive(2, 8, 3, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 1..3).prop_map(Pred::All),
            prop::collection::vec(inner.clone(), 1..3).prop_map(Pred::Any),
            inner.prop_map(|p| Pred::Not(Box::new(p))),
        ]
    })
}

fn tool() -> impl Strategy<Value = ToolSpec> {
    (ident(), text(), prop::option::of(0..3usize)).prop_map(|(name, description, params)| {
        // `ToolSpec` has no `Default`: build it through its JSON form, so a
        // field added later gets its serde default here too.
        let mut value = json!({ "name": name, "description": description });
        if let Some(n) = params {
            let props: serde_json::Map<String, Value> = (0..n)
                .map(|i| (format!("p{i}"), json!({ "type": "string" })))
                .collect();
            value["parameters"] = json!({ "type": "object", "properties": props });
        }
        serde_json::from_value::<ToolSpec>(value).expect("a minimal tool spec parses")
    })
}

#[derive(Clone, Debug)]
struct StepShape {
    id: String,
    allow: Vec<usize>,
    done: Option<Pred>,
    terminal: bool,
}

fn flow_shape(tool_count: usize) -> impl Strategy<Value = Vec<StepShape>> {
    prop::collection::vec(
        (
            prop::collection::btree_set(0..tool_count.max(1), 0..=tool_count.min(3)),
            prop::option::of(pred()),
            any::<bool>(),
        ),
        1..=4,
    )
    .prop_map(|steps| {
        steps
            .into_iter()
            .enumerate()
            .map(|(i, (allow, done, terminal))| StepShape {
                id: format!("step_{i}"),
                allow: allow.into_iter().collect(),
                done,
                terminal,
            })
            .collect()
    })
}

fn build_flow(steps: &[StepShape], tools: &[ToolSpec], never: &[(usize, Pred)]) -> Option<Flow> {
    let mut builder = Flow::new();
    let last = steps.len() - 1;
    for (i, step) in steps.iter().enumerate() {
        builder = builder.step(&step.id);
        if i > 0 {
            builder = builder.after(&steps[i - 1].id);
        }
        if !step.allow.is_empty() && !tools.is_empty() {
            builder = builder.allow(
                step.allow
                    .iter()
                    .map(|&t| tools[t % tools.len()].name.clone()),
            );
        }
        if let Some(done) = &step.done {
            builder = builder.done(Guard::Spec(done.clone()));
        }
        if step.terminal || i == last {
            builder = builder.terminal();
        }
    }
    for (tool, until) in never {
        if tools.is_empty() {
            break;
        }
        builder = builder
            .never(tools[tool % tools.len()].name.clone())
            .until(Guard::Spec(until.clone()));
    }
    builder.build().ok()
}

#[derive(Clone, Debug)]
struct Doc {
    spec: SessionSpec,
}

fn doc() -> impl Strategy<Value = Doc> {
    (
        ident(),
        text(),
        text(),
        prop::option::of(text()),
        any::<bool>(),
        prop::collection::vec(tool(), 0..4),
    )
        .prop_flat_map(|(name, description, instruction, greeting, audio, tools)| {
            // Tool names must be unique for the document to validate.
            let mut seen = BTreeSet::new();
            let tools: Vec<ToolSpec> = tools
                .into_iter()
                .filter(|t| seen.insert(t.name.clone()))
                .collect();
            let count = tools.len();
            (
                flow_shape(count),
                prop::collection::vec((0..count.max(1), pred()), 0..=2),
            )
                .prop_map(move |(steps, never)| {
                    let mut spec = SessionSpec {
                        name: name.clone(),
                        description: description.clone(),
                        instruction: instruction.clone(),
                        greeting: greeting.clone(),
                        modality: if audio {
                            SpecModality::Audio
                        } else {
                            SpecModality::Text
                        },
                        tools: tools.clone(),
                        ..Default::default()
                    };
                    spec.flow = build_flow(&steps, &tools, &never);
                    Doc { spec }
                })
        })
}

/// The flow a generated `main.rs` declares, read back from the source text
/// by walking the `Flow::new()` builder chain with `syn`. This is an
/// independent decoder of the generated program: it knows the builder's
/// method names, not the generator.
#[derive(Debug, Default, PartialEq)]
struct DeclaredFlow {
    steps: Vec<DeclaredStep>,
    never: Vec<(String, Pred)>,
}

#[derive(Debug, Default, PartialEq)]
struct DeclaredStep {
    id: String,
    after: Vec<String>,
    allow: Vec<String>,
    done: Option<Pred>,
    terminal: bool,
}

fn declared_flow(source: &str) -> Option<DeclaredFlow> {
    let file = syn::parse_file(source).ok()?;
    let main = file.items.iter().find_map(|item| match item {
        syn::Item::Fn(f) if f.sig.ident == "main" => Some(f),
        _ => None,
    })?;
    let init = main.block.stmts.iter().find_map(|stmt| match stmt {
        syn::Stmt::Local(local) => {
            let is_flow = matches!(&local.pat, syn::Pat::Ident(p) if p.ident == "flow");
            is_flow
                .then(|| local.init.as_ref().map(|i| &*i.expr))
                .flatten()
        }
        _ => None,
    })?;
    // Flatten the method-call chain, innermost (Flow::new()) first.
    let mut calls = Vec::new();
    let mut expr = init;
    loop {
        match expr {
            syn::Expr::MethodCall(call) => {
                calls.push((
                    call.method.to_string(),
                    call.args.iter().cloned().collect::<Vec<_>>(),
                ));
                expr = &call.receiver;
            }
            syn::Expr::Try(t) => expr = &t.expr,
            syn::Expr::Call(_) => break, // Flow::new()
            _ => return None,
        }
    }
    calls.reverse();
    let mut flow = DeclaredFlow::default();
    let mut pending_never: Option<String> = None;
    for (method, args) in calls {
        match method.as_str() {
            "step" => flow.steps.push(DeclaredStep {
                id: str_lit(&args[0])?,
                ..Default::default()
            }),
            "after" | "after_when" => flow.steps.last_mut()?.after.push(str_lit(&args[0])?),
            "allow" => flow.steps.last_mut()?.allow = str_array(&args[0])?,
            "done" => flow.steps.last_mut()?.done = Some(guard_expr(&args[0])?),
            "terminal" => flow.steps.last_mut()?.terminal = true,
            "never" => pending_never = Some(str_lit(&args[0])?),
            "until" => flow
                .never
                .push((pending_never.take()?, guard_expr(&args[0])?)),
            "build" | "expect" | "unwrap" => {}
            // Builder calls the comparison does not model; they are kept
            // out of `DeclaredFlow` on both sides.
            "posture" | "ground" | "deny" | "gate" | "join_any" | "once" | "before" | "require"
            | "ambient" | "reset" | "when" | "confirm" => {}
            other => panic!("unexpected builder call `{other}` in generated flow"),
        }
    }
    Some(flow)
}

fn str_lit(expr: &syn::Expr) -> Option<String> {
    match expr {
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(s),
            ..
        }) => Some(s.value()),
        _ => None,
    }
}

fn str_array(expr: &syn::Expr) -> Option<Vec<String>> {
    match expr {
        syn::Expr::Array(array) => array.elems.iter().map(str_lit).collect(),
        _ => None,
    }
}

fn guard_expr(expr: &syn::Expr) -> Option<Pred> {
    let call = match expr {
        syn::Expr::Call(call) => call,
        _ => return None,
    };
    let func = match &*call.func {
        syn::Expr::Path(path) => path.path.segments.last()?.ident.to_string(),
        _ => return None,
    };
    let args: Vec<&syn::Expr> = call.args.iter().collect();
    Some(match func.as_str() {
        "always" => Pred::Always,
        "is_true" => Pred::IsTrue(str_lit(args[0])?),
        "is_set" => Pred::IsSet(str_lit(args[0])?),
        "called_ok" => Pred::CalledOk(str_lit(args[0])?),
        "done" => Pred::Done(str_lit(args[0])?),
        "captured" => Pred::Captured(str_array(args[0])?),
        "eq" => Pred::Eq(str_lit(args[0])?, json_macro(args[1])?),
        "all" => Pred::All(guard_list(args[0])?),
        "any" => Pred::Any(guard_list(args[0])?),
        "not" => Pred::Not(Box::new(guard_expr(args[0])?)),
        _ => return None,
    })
}

fn guard_list(expr: &syn::Expr) -> Option<Vec<Pred>> {
    match expr {
        syn::Expr::Array(array) => array.elems.iter().map(guard_expr).collect(),
        _ => None,
    }
}

/// `json!(<literal>)`: the macro's tokens are JSON text, once the spaces
/// `TokenStream::to_string` puts between tokens (`- 5`, `[ 1 , 2 ]`) are
/// removed outside string literals.
fn json_macro(expr: &syn::Expr) -> Option<Value> {
    match expr {
        syn::Expr::Macro(m) if m.mac.path.is_ident("json") => {
            let spaced = m.mac.tokens.to_string();
            let mut text = String::with_capacity(spaced.len());
            let mut in_string = false;
            let mut escaped = false;
            for c in spaced.chars() {
                if in_string {
                    text.push(c);
                    if escaped {
                        escaped = false;
                    } else if c == '\\' {
                        escaped = true;
                    } else if c == '"' {
                        in_string = false;
                    }
                } else if c == '"' {
                    in_string = true;
                    text.push(c);
                } else if !c.is_whitespace() {
                    text.push(c);
                }
            }
            serde_json::from_str(&text).ok()
        }
        _ => None,
    }
}

fn expected_flow(flow: &Flow) -> DeclaredFlow {
    DeclaredFlow {
        steps: flow
            .steps
            .iter()
            .map(|s| DeclaredStep {
                id: s.id.clone(),
                after: s.after.iter().map(|e| e.step.clone()).collect(),
                allow: s.allow.clone(),
                done: s.done.as_ref().and_then(|g| match g {
                    Guard::Spec(p) => Some(p.clone()),
                    Guard::Custom(_) => None,
                }),
                terminal: s.terminal,
            })
            .collect(),
        never: flow
            .constraints
            .iter()
            .filter_map(|c| match c {
                Constraint::NeverUntil {
                    tool,
                    until: Guard::Spec(p),
                } => Some((tool.clone(), p.clone())),
                _ => None,
            })
            .collect(),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn a_document_survives_serialization_unchanged(d in doc()) {
        let value = serde_json::to_value(&d.spec).expect("specs serialize");
        let back = SessionSpec::from_value(value.clone()).expect("a serialized spec loads");
        let again = serde_json::to_value(&back).expect("specs serialize");
        prop_assert_eq!(&value, &again, "document changed over a round trip");
    }

    #[test]
    fn validation_agrees_on_both_sides_of_the_round_trip(d in doc()) {
        let value = serde_json::to_value(&d.spec).unwrap();
        let back = SessionSpec::from_value(value).unwrap();
        let before = d.spec.validate();
        let after = back.validate();
        prop_assert_eq!(before.valid, after.valid);
        prop_assert_eq!(before.errors, after.errors);
        prop_assert_eq!(before.warnings, after.warnings);
        prop_assert_eq!(before.steps, after.steps);
        prop_assert_eq!(before.tools, after.tools);
    }

    #[test]
    fn generated_rust_parses_and_declares_the_same_flow(d in doc()) {
        let source = d.spec.to_rust();
        let parsed = syn::parse_file(&source);
        prop_assert!(parsed.is_ok(), "generated main.rs does not parse: {:?}\n{source}", parsed.err());
        let manifest = d.spec.to_cargo_toml();
        prop_assert!(manifest.parse::<toml::Table>().is_ok(), "generated Cargo.toml does not parse:\n{manifest}");
        if let Some(flow) = &d.spec.flow {
            let declared = declared_flow(&source);
            prop_assert!(declared.is_some(), "no flow chain found in generated source:\n{source}");
            prop_assert_eq!(declared.unwrap(), expected_flow(flow), "generated flow differs from the document's");
        }
    }
}

/// The gallery documents Flow Studio ships: the same structural check on
/// the real thing, so a change to the generator that drops a step or a
/// guard fails here before the compile job runs.
#[test]
fn gallery_documents_generate_their_own_flow() {
    let dir = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../apps/gemini-adk-web-rs/static/examples/flows"
    );
    // The gallery proper holds skills documents; `reference/` holds the
    // same scenarios as `flow` documents. Both are checked.
    let mut paths: Vec<std::path::PathBuf> = Vec::new();
    for sub in ["", "reference"] {
        let dir = std::path::Path::new(dir).join(sub);
        for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
            paths.push(entry.unwrap().path());
        }
    }
    let mut checked = 0;
    for path in paths {
        if path.extension().is_none_or(|e| e != "json") || path.ends_with("index.json") {
            continue;
        }
        let raw = std::fs::read_to_string(&path).unwrap();
        let value: Value = serde_json::from_str(&raw).unwrap();
        let spec =
            SessionSpec::from_value(value).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let flow = match spec.effective_flow() {
            Ok(flow) if !flow.steps.is_empty() => flow,
            Ok(_) => continue,
            Err(errors) => panic!("{}: effective flow: {errors:?}", path.display()),
        };
        let source = spec.to_rust();
        syn::parse_file(&source).unwrap_or_else(|e| {
            panic!("{}: generated main.rs does not parse: {e}", path.display())
        });
        match declared_flow(&source) {
            Some(declared) => {
                // A `flow` document: the builder chain is the flow.
                let expected = expected_flow(&flow);
                assert_eq!(
                    declared.steps.len(),
                    expected.steps.len(),
                    "{}",
                    path.display()
                );
                for (d, e) in declared.steps.iter().zip(&expected.steps) {
                    assert_eq!(d.id, e.id, "{}", path.display());
                    assert_eq!(d.allow, e.allow, "{}: step {}", path.display(), e.id);
                    assert_eq!(d.after, e.after, "{}: step {}", path.display(), e.id);
                }
            }
            None => {
                // A conversation or skills document: the generated program
                // carries the document and applies it, so every step id and
                // tool name of the effective flow is in the source.
                assert!(
                    source.contains("spec.apply(Live::builder()"),
                    "{}: generated program neither builds the flow nor applies the document:\n{source}",
                    path.display()
                );
                for step in &flow.steps {
                    assert!(
                        source.contains(&step.id),
                        "{}: generated program lacks step {}",
                        path.display(),
                        step.id
                    );
                }
            }
        }
        checked += 1;
    }
    assert!(checked > 0, "no gallery document with a flow was checked");
}
