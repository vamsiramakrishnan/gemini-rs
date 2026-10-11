//! The in-process sandbox: Monty on a blocking thread.

use std::sync::mpsc;

use async_trait::async_trait;
use monty::{MontyRun, RunProgress};
use monty_types::{
    CompileOptions, ExcType, ExtFunctionResult, MontyException, MontyObject, NameLookupResult,
    ObjectRef, PrintWriter, ResourceLimits, ResourceTracker,
};
use serde_json::{Map, Value, json};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use super::CodeLimits;
use super::sandbox::{CodeSandbox, Execution, Suspension};

/// Runs programs in [Monty](https://github.com/pydantic/monty) on a blocking
/// thread of this process.
///
/// Monty bounds a program's interpreter time, but in-process it cannot bound
/// memory: a program that asks for a huge allocation can abort the process,
/// or hold the thread past its time limit while the allocation completes.
/// Use [`MontyPool`](super::MontyPool) where programs must not be able to
/// take the process down.
#[derive(Debug, Clone, Copy, Default)]
pub struct InProcess;

#[async_trait]
impl CodeSandbox for InProcess {
    async fn start(
        &self,
        program: &str,
        functions: &[String],
        limits: &CodeLimits,
    ) -> (Box<dyn Execution>, Suspension) {
        let (stops, stopped) = unbounded_channel();
        let (answers, answered) = mpsc::channel();
        let program = program.to_string();
        let functions = functions.to_vec();
        let limits = *limits;
        tokio::task::spawn_blocking(move || run(program, functions, limits, &stops, &answered));
        let mut execution = Thread { stopped, answers };
        let first = execution.next().await;
        (Box::new(execution), first)
    }
}

/// The host's end of a program running on a blocking thread.
struct Thread {
    stopped: UnboundedReceiver<Suspension>,
    answers: mpsc::Sender<Result<Value, String>>,
}

impl Thread {
    async fn next(&mut self) -> Suspension {
        self.stopped
            .recv()
            .await
            .unwrap_or_else(|| Suspension::failed("the sandbox stopped"))
    }
}

#[async_trait]
impl Execution for Thread {
    async fn resume(&mut self, answer: Result<Value, String>) -> Suspension {
        if self.answers.send(answer).is_err() {
            return Suspension::failed("the program has ended");
        }
        self.next().await
    }
}

/// Run `program` to its end, reporting each call on `stops` and waiting on
/// `answers` for its result. Returns early when the host drops its end.
fn run(
    program: String,
    functions: Vec<String>,
    limits: CodeLimits,
    stops: &UnboundedSender<Suspension>,
    answers: &mpsc::Receiver<Result<Value, String>>,
) {
    // Monty raises MemoryError past the print cap, which would end a program
    // that only printed too much; collect with headroom and let the response
    // cut what the model reads.
    let cap = Some(limits.output_chars.saturating_mul(16).max(1 << 16));
    let mut printed = String::new();
    let finish = |result: Result<Value, String>, printed: String| {
        let _ = stops.send(Suspension::Finished { result, printed });
    };
    let runner = match MontyRun::new(
        program,
        "main.py",
        functions.clone(),
        CompileOptions::default(),
    ) {
        Ok(runner) => runner,
        Err(e) => return finish(Err(e.to_string()), printed),
    };
    let tracker = ResourceTracker::new(ResourceLimits {
        max_feed_duration: Some(limits.cpu),
        ..ResourceLimits::default()
    });
    let inputs = functions
        .iter()
        .map(|name| MontyObject::function(name.clone(), None))
        .collect();
    let mut progress = runner.start(
        inputs,
        tracker,
        PrintWriter::CollectString(&mut printed, cap),
    );
    loop {
        let step = match progress {
            Err(e) => return finish(Err(e.to_string()), printed),
            Ok(step) => step,
        };
        progress = match step {
            RunProgress::Complete(value) => {
                return finish(Ok(to_json(value.as_ref())), printed);
            }
            // Monty reports a call to any unknown name as an external call;
            // only the functions given are the host's.
            RunProgress::FunctionCall(call) if !functions.contains(&call.function_name) => {
                let name = call.function_name.clone();
                call.resume(
                    ExtFunctionResult::NotFound(name),
                    PrintWriter::CollectString(&mut printed, cap),
                )
            }
            RunProgress::FunctionCall(call) => {
                let positional = call.args.args().map(to_json).collect();
                let keywords: Map<String, Value> = call
                    .args
                    .kwargs()
                    .map(|(k, v)| (key(k), to_json(v)))
                    .collect();
                let stop = Suspension::Call {
                    function: call.function_name.clone(),
                    positional,
                    keywords,
                };
                if stops.send(stop).is_err() {
                    return;
                }
                // The host dropped its end: the program is cancelled.
                let Ok(answer) = answers.recv() else {
                    return;
                };
                let print = PrintWriter::CollectString(&mut printed, cap);
                match answer {
                    Ok(value) => call.resume(from_json(&value), print),
                    Err(reason) => call.resume(
                        MontyException::new(ExcType::RuntimeError, Some(reason)),
                        print,
                    ),
                }
            }
            RunProgress::NameLookup(lookup) => lookup.resume(
                NameLookupResult::Undefined,
                PrintWriter::CollectString(&mut printed, cap),
            ),
            RunProgress::OsCall(os) => os.resume(
                MontyException::new(
                    ExcType::PermissionError,
                    Some("this sandbox has no file system, network or clock access".into()),
                ),
                PrintWriter::CollectString(&mut printed, cap),
            ),
            RunProgress::ResolveFutures(_) => {
                return finish(
                    Err("async code is not supported; call the functions directly".into()),
                    printed,
                );
            }
        };
    }
}

pub(super) fn key(k: ObjectRef<'_>) -> String {
    k.as_str().map_or_else(|| k.py_repr(), str::to_string)
}

/// A Monty value as JSON. Tuples become lists; a value JSON cannot hold is
/// given as its Python repr.
pub(super) fn to_json(o: ObjectRef<'_>) -> Value {
    match o.type_name() {
        "NoneType" => Value::Null,
        "bool" => o.as_bool().map_or(Value::Null, Value::Bool),
        "int" => o
            .as_int()
            .map_or_else(|| Value::String(o.py_repr()), |i| json!(i)),
        "float" => o
            .as_float()
            .and_then(serde_json::Number::from_f64)
            .map_or(Value::Null, Value::Number),
        "str" => o
            .as_str()
            .map_or(Value::Null, |s| Value::String(s.to_string())),
        "list" | "tuple" => Value::Array(
            o.items()
                .unwrap_or_default()
                .into_iter()
                .map(to_json)
                .collect(),
        ),
        "dict" => Value::Object(
            o.pairs()
                .unwrap_or_default()
                .into_iter()
                .map(|(k, v)| (key(k), to_json(v)))
                .collect(),
        ),
        _ => Value::String(o.py_repr()),
    }
}

/// JSON as a Monty value.
pub(super) fn from_json(v: &Value) -> MontyObject {
    match v {
        Value::Null => MontyObject::none(),
        Value::Bool(b) => MontyObject::bool(*b),
        Value::Number(n) => n.as_i64().map_or_else(
            || MontyObject::float(n.as_f64().unwrap_or(f64::NAN)),
            MontyObject::int,
        ),
        Value::String(s) => MontyObject::string(s.clone()),
        Value::Array(a) => MontyObject::list(a.iter().map(from_json)),
        Value::Object(m) => MontyObject::dict(
            m.iter()
                .map(|(k, v)| (MontyObject::string(k.clone()), from_json(v))),
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    async fn finish(program: &str) -> Suspension {
        let (_, stop) = InProcess.start(program, &[], &CodeLimits::default()).await;
        stop
    }

    #[tokio::test]
    async fn a_program_returns_its_last_expression_as_json() {
        let stop = finish("x = [1, 2.5, 'a', None, True]\n{'x': x, 'n': len(x)}").await;
        assert_eq!(
            stop,
            Suspension::Finished {
                result: Ok(json!({"x": [1, 2.5, "a", null, true], "n": 5})),
                printed: String::new(),
            }
        );
    }

    #[tokio::test]
    async fn dates_and_printing_work() {
        let stop = finish(
            "import datetime\nd = datetime.date(2026, 10, 11)\nprint('day', d.weekday())\nd.isoformat()",
        )
        .await;
        assert_eq!(
            stop,
            Suspension::Finished {
                result: Ok(json!("2026-10-11")),
                printed: "day 6\n".into(),
            }
        );
    }

    #[tokio::test]
    async fn a_loop_ends_at_the_time_limit() {
        let limits = CodeLimits {
            cpu: Duration::from_millis(200),
            ..CodeLimits::default()
        };
        let (_, stop) = InProcess.start("while True:\n    pass", &[], &limits).await;
        let Suspension::Finished { result: Err(e), .. } = stop else {
            panic!("expected a timeout, got {stop:?}");
        };
        assert!(e.contains("TimeoutError"), "{e}");
    }

    #[tokio::test]
    async fn the_file_system_is_refused() {
        let stop = finish("from pathlib import Path\nPath('/etc/passwd').read_text()").await;
        let Suspension::Finished { result: Err(e), .. } = stop else {
            panic!("expected a refusal, got {stop:?}");
        };
        assert!(e.contains("PermissionError"), "{e}");
    }

    #[tokio::test]
    async fn a_call_suspends_until_the_host_answers() {
        let functions = vec!["double".to_string()];
        let (mut run, stop) = InProcess
            .start(
                "try:\n    double(1)\nexcept RuntimeError as e:\n    r = str(e)\n[double(n=21), r]",
                &functions,
                &CodeLimits::default(),
            )
            .await;
        assert_eq!(
            stop,
            Suspension::Call {
                function: "double".into(),
                positional: vec![json!(1)],
                keywords: Map::new(),
            }
        );
        let stop = run.resume(Err("not yet".into())).await;
        let Suspension::Call { keywords, .. } = stop else {
            panic!("expected a second call, got {stop:?}");
        };
        assert_eq!(keywords.get("n"), Some(&json!(21)));
        let stop = run.resume(Ok(json!({"value": 42}))).await;
        assert_eq!(
            stop,
            Suspension::Finished {
                result: Ok(json!([{"value": 42}, "not yet"])),
                printed: String::new(),
            }
        );
    }

    #[tokio::test]
    async fn an_undefined_name_raises_name_error() {
        let stop = finish("lookup_everything()").await;
        let Suspension::Finished { result: Err(e), .. } = stop else {
            panic!("expected a NameError, got {stop:?}");
        };
        assert!(e.contains("NameError"), "{e}");
    }
}
