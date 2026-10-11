//! The worker-process sandbox: Monty in a pool of subprocesses.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use monty_pool::{
    Checkout, Pool, PoolConfig, PoolError, ReplConfig, ResumeValue, TurnEvent, on_print_sync,
};
use monty_types::{ExcType, MontyException, MontyObject, NameLookupResult, ResourceLimits};
use serde_json::{Map, Value};

use super::CodeLimits;
use super::monty::{from_json, key, to_json};
use super::sandbox::{CodeSandbox, Execution, Suspension};

/// Runs programs in a pool of Monty worker processes.
///
/// Each program gets a worker of its own. A crash, an allocation past the
/// memory limit or a hang kills only that worker, which the pool replaces;
/// the session's process and every other call are untouched. Workers are the
/// `monty` binary: `cargo install monty-runtime`, or `pip install
/// pydantic-monty-runtime`.
#[derive(Clone)]
pub struct MontyPool {
    pool: Arc<Pool>,
}

impl std::fmt::Debug for MontyPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MontyPool").finish_non_exhaustive()
    }
}

impl MontyPool {
    /// Start a pool of up to `workers` processes of the `monty` binary at
    /// `binary`, one kept warm. A turn that outlives 30 s kills its worker,
    /// whatever the program's own limits say.
    pub async fn spawn(binary: impl Into<PathBuf>, workers: usize) -> Result<Self, String> {
        let mut config = PoolConfig::subprocess(binary);
        config.min_processes = 1.min(workers);
        config.max_processes = workers.max(1);
        config.request_timeout = Some(Duration::from_secs(30));
        config.checkout_timeout = Some(Duration::from_secs(5));
        Self::with_config(config).await
    }

    /// A pool built from `config`, for a remote relay or other settings.
    pub async fn with_config(config: PoolConfig) -> Result<Self, String> {
        let pool = Pool::new(config).await.map_err(|e| e.to_string())?;
        Ok(Self {
            pool: Arc::new(pool),
        })
    }
}

#[async_trait]
impl CodeSandbox for MontyPool {
    async fn start(
        &self,
        program: &str,
        functions: &[String],
        limits: &CodeLimits,
    ) -> (Box<dyn Execution>, Suspension) {
        let repl = ReplConfig {
            limits: Some(ResourceLimits {
                max_feed_duration: Some(limits.cpu),
                max_memory: Some(limits.memory),
                ..ResourceLimits::default()
            }),
            ..ReplConfig::default()
        };
        let mut execution = Worker {
            checkout: None,
            functions: functions.to_vec(),
            printed: String::new(),
            cap: limits.output_chars,
        };
        let mut checkout = match self.pool.checkout(&repl).await {
            Ok(checkout) => checkout,
            Err(e) => {
                return (
                    Box::new(execution),
                    Suspension::failed(format!("no sandbox worker: {e}")),
                );
            }
        };
        let inputs: Vec<(String, MontyObject)> = functions
            .iter()
            .map(|name| (name.clone(), MontyObject::function(name.clone(), None)))
            .collect();
        let event = {
            let printed = &mut execution.printed;
            let cap = execution.cap;
            let mut on_print = on_print_sync(move |_, text| append(printed, text, cap));
            checkout
                .feed(program, inputs, Vec::new(), true, &mut on_print)
                .await
        };
        execution.checkout = Some(checkout);
        let first = execution.settle(event).await;
        (Box::new(execution), first)
    }
}

/// One program's worker, held from its start to its end.
struct Worker {
    /// `None` once the program has ended and the worker is returned or
    /// discarded.
    checkout: Option<Checkout>,
    functions: Vec<String>,
    printed: String,
    cap: usize,
}

/// How to answer an event the host does not see.
enum Next {
    Resume(ResumeValue),
    Undefined,
}

fn append(printed: &mut String, text: &str, cap: usize) {
    if printed.chars().count() < cap {
        printed.push_str(text);
    }
}

impl Worker {
    /// Answer every event the host need not see (an unknown name, an OS
    /// call) and return where the program stops for the host.
    async fn settle(&mut self, mut event: Result<TurnEvent, PoolError>) -> Suspension {
        loop {
            let next = match event {
                Ok(TurnEvent::FunctionCall {
                    function_name,
                    args,
                    ..
                }) => {
                    if self.functions.contains(&function_name) {
                        let positional = args.args().map(to_json).collect();
                        let keywords: Map<String, Value> =
                            args.kwargs().map(|(k, v)| (key(k), to_json(v))).collect();
                        return Suspension::Call {
                            function: function_name,
                            positional,
                            keywords,
                        };
                    }
                    Next::Resume(ResumeValue::Error(MontyException::new(
                        ExcType::NameError,
                        Some(format!("name '{function_name}' is not defined")),
                    )))
                }
                Ok(TurnEvent::OsCall { .. }) => {
                    Next::Resume(ResumeValue::Error(MontyException::new(
                        ExcType::PermissionError,
                        Some("this sandbox has no file system, network or clock access".into()),
                    )))
                }
                Ok(TurnEvent::NameLookup { .. }) => Next::Undefined,
                Ok(TurnEvent::ResolveFutures { .. }) => {
                    // Mid-execution state is not returned to the pool.
                    self.checkout = None;
                    return self.finished(Err(
                        "async code is not supported; call the functions directly".into(),
                    ));
                }
                Ok(TurnEvent::Complete(value)) => {
                    let result = to_json(value.as_ref());
                    self.release().await;
                    return self.finished(Ok(result));
                }
                Err(PoolError::Runtime(exception)) => {
                    // After a time or memory limit the worker's heap is not
                    // to be trusted: discard it rather than return it.
                    if matches!(
                        exception.exc_type(),
                        ExcType::TimeoutError | ExcType::MemoryError
                    ) {
                        self.checkout = None;
                    } else {
                        self.release().await;
                    }
                    return self.finished(Err(exception.to_string()));
                }
                Err(error) => {
                    self.checkout = None;
                    return self.finished(Err(format!("the sandbox worker failed: {error}")));
                }
            };
            let Some(checkout) = self.checkout.as_mut() else {
                return Suspension::failed("the program has ended");
            };
            let printed = &mut self.printed;
            let cap = self.cap;
            let mut on_print = on_print_sync(move |_, text| append(printed, text, cap));
            event = match next {
                Next::Resume(value) => checkout.resume(value, &mut on_print).await,
                Next::Undefined => {
                    checkout
                        .resume_name_lookup(NameLookupResult::Undefined, &mut on_print)
                        .await
                }
            };
        }
    }

    /// Return the worker to the pool.
    async fn release(&mut self) {
        if let Some(checkout) = self.checkout.take() {
            let _ = checkout.finish().await;
        }
    }

    fn finished(&mut self, result: Result<Value, String>) -> Suspension {
        Suspension::Finished {
            result,
            printed: std::mem::take(&mut self.printed),
        }
    }
}

#[async_trait]
impl Execution for Worker {
    async fn resume(&mut self, answer: Result<Value, String>) -> Suspension {
        let Some(checkout) = self.checkout.as_mut() else {
            return Suspension::failed("the program has ended");
        };
        let value = match answer {
            Ok(value) => ResumeValue::Return(from_json(&value)),
            Err(reason) => {
                ResumeValue::Error(MontyException::new(ExcType::RuntimeError, Some(reason)))
            }
        };
        let event = {
            let printed = &mut self.printed;
            let cap = self.cap;
            let mut on_print = on_print_sync(move |_, text| append(printed, text, cap));
            checkout.resume(value, &mut on_print).await
        };
        self.settle(event).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The worker binary, from `MONTY_BIN`. Without it these tests pass
    /// without running.
    fn binary() -> Option<PathBuf> {
        std::env::var_os("MONTY_BIN").map(PathBuf::from)
    }

    #[tokio::test]
    async fn a_program_runs_in_a_worker_and_calls_back() {
        let Some(bin) = binary() else { return };
        let pool = MontyPool::spawn(bin, 2).await.expect("pool");
        let functions = vec!["double".to_string()];
        let (mut run, stop) = pool
            .start(
                "print('hi')\n[double(21), sum(range(4))]",
                &functions,
                &CodeLimits::default(),
            )
            .await;
        assert!(matches!(stop, Suspension::Call { .. }), "{stop:?}");
        let stop = run.resume(Ok(serde_json::json!(42))).await;
        assert_eq!(
            stop,
            Suspension::Finished {
                result: Ok(serde_json::json!([42, 6])),
                printed: "hi\n".into(),
            }
        );
    }

    #[tokio::test]
    async fn a_huge_allocation_kills_only_the_worker() {
        let Some(bin) = binary() else { return };
        let pool = MontyPool::spawn(bin, 1).await.expect("pool");
        let (_, stop) = pool
            .start("len('a' * 10**10)", &[], &CodeLimits::default())
            .await;
        let Suspension::Finished { result: Err(e), .. } = stop else {
            panic!("expected a memory error, got {stop:?}");
        };
        assert!(e.contains("MemoryError") || e.contains("memory"), "{e}");
        // The pool replaced the worker: the next program runs.
        let (_, stop) = pool.start("1 + 1", &[], &CodeLimits::default()).await;
        assert_eq!(
            stop,
            Suspension::Finished {
                result: Ok(serde_json::json!(2)),
                printed: String::new(),
            }
        );
    }
}
