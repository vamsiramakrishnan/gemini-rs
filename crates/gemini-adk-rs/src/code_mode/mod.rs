//! Code mode: one tool through which the model runs a short Python program
//! that calls the session's own tools.
//!
//! Function calling makes the model do the arithmetic, sorting and joining in
//! its head, one call at a time, with every result added to the conversation.
//! In code mode the model writes a program instead: `run_code` runs it in a
//! sandbox, each function the program calls is the session's tool of that
//! name, and the model reads only the program's final value. Measured on
//! Gemini 3.8 Live over 120 payments, the answer was right in 5 of 5 runs
//! against 2 of 5 with function calls, and the prompt held 77% fewer tokens.
//! On a single lookup, writing a program cost the caller up to 700 ms before
//! the first action, so by default the functions stay callable directly as
//! well, and the tool's description says when to use which.
//!
//! # Every call is a governed call
//!
//! A program cannot reach around the session's governance. Each call it makes
//! pauses the interpreter and runs down the same path as a direct call: the
//! phase's tool list, the decision gate, the flow gate (with extraction of the
//! turn in progress), middleware, confirmation, the tool itself, and the flow
//! advance on its result. A refused call raises `RuntimeError` in the program
//! with the reason, and every call and its full result is recorded as a tool
//! execution. A program cannot write state or move the flow except through
//! the calls it makes.
//!
//! Commit tools (a booking, a payment) are better left out of the functions:
//! a commit should be one visible call, not a line inside a program.
//!
//! # Sandboxes
//!
//! [`InProcess`] runs [Monty](https://github.com/pydantic/monty), a Python
//! interpreter written in Rust, on a blocking thread of this process. It is
//! fast and needs nothing installed, but it is not crash-isolated: a program
//! that asks for a huge allocation (`'a' * 10**10`) can abort the process or
//! run past its time limit while the allocation completes. Use it for
//! development, tests and trusted deployments. [`MontyPool`] (feature
//! `code-pool`) runs programs in Monty worker processes with a memory cap and
//! hard deadlines, so a runaway program kills only its worker.
//!
//! ```rust,no_run
//! use gemini_adk_rs::code_mode::{CodeLimits, CodeMode};
//!
//! let code = CodeMode::new()
//!     .function("lookup_payment_history")
//!     .function("check_availability")
//!     .limits(CodeLimits {
//!         calls: 10,
//!         ..CodeLimits::default()
//!     });
//! ```

mod sandbox;
mod stubs;

#[cfg(feature = "code-mode")]
mod monty;
#[cfg(feature = "code-pool")]
mod pool;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use gemini_genai_rs::prelude::FunctionDeclaration;
use serde_json::{Value, json};

#[cfg(feature = "code-mode")]
pub use monty::InProcess;
#[cfg(feature = "code-pool")]
pub use pool::MontyPool;
pub use sandbox::{CodeSandbox, Execution, Suspension, Unavailable};
pub use stubs::{arguments, parameters, python_stub};

/// The default name of the code tool.
pub const DEFAULT_TOOL: &str = "run_code";

/// Bounds on one program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodeLimits {
    /// Function calls one program may make. The call past the limit ends it.
    pub calls: usize,
    /// Interpreter time, summed over the program and excluding the time its
    /// calls take. A program over it ends with `TimeoutError`.
    pub cpu: Duration,
    /// Wall-clock time for the whole program, its calls included. The caller
    /// is waiting on the line for all of it.
    pub wall: Duration,
    /// Characters of the program's result (as JSON) and of its printed
    /// output that the model reads. A larger result is refused with a note
    /// to return less, rather than cut mid-value.
    pub output_chars: usize,
    /// Memory a program may allocate, in bytes. Enforced by
    /// [`MontyPool`](self#sandboxes), whose workers have their own allocator;
    /// the in-process sandbox cannot bound memory.
    pub memory: usize,
}

impl Default for CodeLimits {
    fn default() -> Self {
        Self {
            calls: 20,
            cpu: Duration::from_secs(2),
            wall: Duration::from_secs(30),
            output_chars: 4_000,
            memory: 64 * 1024 * 1024,
        }
    }
}

/// A function a program can call: a tool of the session, by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeFunction {
    /// The tool's name, which is also the function's name in the program.
    pub name: String,
    /// What the tool returns, shown in the function's docstring, such as
    /// `{"payments": [{"paid": str, "due": str, "amount": float}]}`. Without
    /// it the model guesses the result's keys, and a wrong guess costs it a
    /// failed program.
    pub returns: Option<String>,
}

/// The code tool of a session: its name, the tools a program can call, its
/// limits and the sandbox it runs in. See the [module docs](self).
#[derive(Clone)]
pub struct CodeMode {
    tool: String,
    functions: Vec<CodeFunction>,
    limits: CodeLimits,
    direct: bool,
    sandbox: Arc<dyn CodeSandbox>,
}

impl std::fmt::Debug for CodeMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodeMode")
            .field("tool", &self.tool)
            .field("functions", &self.functions)
            .field("limits", &self.limits)
            .field("direct", &self.direct)
            .finish_non_exhaustive()
    }
}

impl Default for CodeMode {
    fn default() -> Self {
        Self::new()
    }
}

impl CodeMode {
    /// A code tool named `run_code` with no functions yet, the default
    /// limits, and the in-process sandbox (or, built without the `code-mode`
    /// feature, a sandbox that refuses every program).
    pub fn new() -> Self {
        Self {
            tool: DEFAULT_TOOL.to_string(),
            functions: Vec::new(),
            limits: CodeLimits::default(),
            direct: true,
            sandbox: default_sandbox(),
        }
    }

    /// Let programs call the tool `name`.
    #[must_use]
    pub fn function(mut self, name: impl Into<String>) -> Self {
        self.push(CodeFunction {
            name: name.into(),
            returns: None,
        });
        self
    }

    /// Let programs call the tool `name`, documenting what it returns.
    #[must_use]
    pub fn function_returning(
        mut self,
        name: impl Into<String>,
        returns: impl Into<String>,
    ) -> Self {
        self.push(CodeFunction {
            name: name.into(),
            returns: Some(returns.into()),
        });
        self
    }

    /// Let programs call each of `names`.
    #[must_use]
    pub fn functions<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for name in names {
            self = self.function(name);
        }
        self
    }

    /// Name the code tool `name` instead of `run_code`.
    #[must_use]
    pub fn tool_name(mut self, name: impl Into<String>) -> Self {
        self.tool = name.into();
        self
    }

    /// Bound each program by `limits`.
    #[must_use]
    pub fn limits(mut self, limits: CodeLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Whether the functions stay declared as tools of their own (the
    /// default). With `false` the model reaches them only through programs.
    #[must_use]
    pub fn direct(mut self, direct: bool) -> Self {
        self.direct = direct;
        self
    }

    /// Run programs in `sandbox`.
    #[must_use]
    pub fn sandbox(mut self, sandbox: Arc<dyn CodeSandbox>) -> Self {
        self.sandbox = sandbox;
        self
    }

    fn push(&mut self, function: CodeFunction) {
        match self.functions.iter_mut().find(|f| f.name == function.name) {
            Some(existing) => {
                if function.returns.is_some() {
                    existing.returns = function.returns;
                }
            }
            None => self.functions.push(function),
        }
    }

    /// The code tool's name.
    pub fn tool(&self) -> &str {
        &self.tool
    }

    /// The functions programs can call.
    pub fn function_list(&self) -> &[CodeFunction] {
        &self.functions
    }

    /// The limits on each program.
    pub fn limit(&self) -> &CodeLimits {
        &self.limits
    }

    /// Whether the functions stay declared as tools of their own.
    pub fn is_direct(&self) -> bool {
        self.direct
    }

    /// The sandbox programs run in.
    pub fn sandbox_ref(&self) -> &Arc<dyn CodeSandbox> {
        &self.sandbox
    }

    /// Whether programs can call the tool `name`.
    pub fn exposes(&self, name: &str) -> bool {
        self.functions.iter().any(|f| f.name == name)
    }

    /// Whether the model reaches the tool `name` only through programs.
    pub fn hides(&self, name: &str) -> bool {
        !self.direct && self.exposes(name)
    }

    /// The code tool's declaration, listing the functions in `catalog` that
    /// programs can call and `offered` admits. `None` when there is none,
    /// in which case the code tool is not declared either.
    pub fn declaration<'a>(
        &self,
        catalog: impl IntoIterator<Item = &'a FunctionDeclaration>,
        offered: impl Fn(&str) -> bool,
    ) -> Option<FunctionDeclaration> {
        let catalog: Vec<&FunctionDeclaration> = catalog.into_iter().collect();
        let stubs: Vec<String> = self
            .functions
            .iter()
            .filter(|f| offered(&f.name))
            .filter_map(|f| {
                catalog
                    .iter()
                    .find(|d| d.name == f.name)
                    .map(|d| python_stub(d, f.returns.as_deref()))
            })
            .collect();
        if stubs.is_empty() {
            return None;
        }
        let when = if self.direct {
            "Use it whenever the answer needs arithmetic, dates, counting, sorting \
             or comparing over what a function returns, or several calls: call \
             the function inside the program rather than calling the tool first \
             and copying its result in. Call a tool directly only when its result \
             is the answer as it stands."
        } else {
            "Use it to call the functions below and to work out arithmetic, \
             dates, counting or sorting over what they return."
        };
        Some(FunctionDeclaration {
            name: self.tool.clone(),
            description: format!(
                "Run a short Python program and get back the value of its last \
                 expression, and anything it prints. {when} Do the whole request \
                 in one program. Each function's docstring says what it returns: \
                 rely on it rather than printing results to look at them. End the \
                 program with an expression holding only the values you need to \
                 answer. The functions are already defined: call them directly; \
                 never import, define or mock them. A call the conversation does \
                 not allow yet raises RuntimeError with the reason: tell the \
                 caller what is needed instead of retrying. The program can \
                 import datetime, math, json and re.\n\n{}",
                stubs.join("\n")
            ),
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "code": {"type": "string", "description": "The Python program."}
                },
                "required": ["code"],
            })),
            behavior: None,
        })
    }

    /// The function names of `catalog` that the code tool's declaration
    /// would list when `offered` admits them, sorted.
    pub fn offered_functions<'a>(
        &self,
        catalog: impl IntoIterator<Item = &'a FunctionDeclaration>,
        offered: impl Fn(&str) -> bool,
    ) -> BTreeSet<String> {
        catalog
            .into_iter()
            .filter(|d| self.exposes(&d.name) && offered(&d.name))
            .map(|d| d.name.clone())
            .collect()
    }

    /// What the model reads for a program that ended with `result` and
    /// printed `printed`: `{"result": ..}` or `{"error": ..}`, with `printed`
    /// when it printed anything.
    pub fn response(&self, result: Result<Value, String>, printed: &str) -> Value {
        let limit = self.limits.output_chars;
        let mut response = match result {
            Ok(value) => {
                let size = value.to_string().chars().count();
                if size > limit {
                    json!({
                        "error": format!(
                            "The program's result is {size} characters, over the \
                             limit of {limit}. End it with only the values you \
                             need to answer."
                        )
                    })
                } else {
                    json!({ "result": value })
                }
            }
            Err(error) => json!({ "error": error }),
        };
        let printed = printed.trim();
        if !printed.is_empty() {
            response["printed"] = Value::String(truncate(printed, limit));
        }
        response
    }
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(limit).collect();
    cut.push_str(&format!(" [cut at {limit} characters]"));
    cut
}

fn default_sandbox() -> Arc<dyn CodeSandbox> {
    #[cfg(feature = "code-mode")]
    {
        Arc::new(InProcess)
    }
    #[cfg(not(feature = "code-mode"))]
    {
        Arc::new(Unavailable)
    }
}

impl CodeMode {
    /// Start `program` in the sandbox, with the functions this mode exposes
    /// defined as globals and named by their declarations in `catalog`.
    ///
    /// The caller drives it: while [`Program::call`] names a call, run that
    /// call (down the governed path, in a session) and pass its result to
    /// [`Program::answer`]; then [`Program::response`] is what the model
    /// reads. Dropping a [`Program`] before it finishes cancels it.
    pub async fn start(&self, program: &str, catalog: &[FunctionDeclaration]) -> Program {
        let names: Vec<String> = self.functions.iter().map(|f| f.name.clone()).collect();
        let declarations = catalog
            .iter()
            .filter(|d| self.exposes(&d.name))
            .cloned()
            .collect();
        let (execution, first) = self.sandbox.start(program, &names, &self.limits).await;
        let mut program = Program {
            mode: self.clone(),
            execution,
            declarations,
            pending: None,
            finished: None,
            calls: Vec::new(),
        };
        program.advance(first).await;
        program
    }
}

/// A program running in a sandbox, suspended at a call or finished. See
/// [`CodeMode::start`].
pub struct Program {
    mode: CodeMode,
    execution: Box<dyn Execution>,
    declarations: Vec<FunctionDeclaration>,
    /// The call the program waits on: the tool and its named arguments.
    pending: Option<(String, Value)>,
    /// The program's end: its value or error, and what it printed.
    finished: Option<(Result<Value, String>, String)>,
    /// The calls answered so far: tool name and whether it ran.
    calls: Vec<(String, bool)>,
}

impl std::fmt::Debug for Program {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Program")
            .field("pending", &self.pending)
            .field("finished", &self.finished)
            .field("calls", &self.calls)
            .finish_non_exhaustive()
    }
}

impl Program {
    /// The call the program waits on: the tool's name and its arguments,
    /// named as its declaration names them. `None` once it has finished.
    pub fn call(&self) -> Option<(&str, &Value)> {
        self.pending
            .as_ref()
            .map(|(name, args)| (name.as_str(), args))
    }

    /// Answer the pending call with the tool's result, or with the reason it
    /// failed or was refused (raised in the program as `RuntimeError`), and
    /// run the program to its next call or its end.
    pub async fn answer(&mut self, result: Result<Value, String>) {
        let Some((name, _)) = self.pending.take() else {
            return;
        };
        self.calls.push((name, result.is_ok()));
        let next = self.execution.resume(result).await;
        self.advance(next).await;
    }

    /// End the program with `error`, as when its time runs out.
    pub fn abort(&mut self, error: impl Into<String>) {
        self.pending = None;
        self.finished = Some((Err(error.into()), String::new()));
    }

    /// Whether the program has finished.
    pub fn is_finished(&self) -> bool {
        self.finished.is_some()
    }

    /// The calls the program made, in order: tool name and whether it ran.
    pub fn calls(&self) -> &[(String, bool)] {
        &self.calls
    }

    /// What the model reads: `{"result": ..}` or `{"error": ..}`, with
    /// `printed` when the program printed anything. `None` until it has
    /// finished.
    pub fn response(&self) -> Option<Value> {
        self.finished
            .as_ref()
            .map(|(result, printed)| self.mode.response(result.clone(), printed))
    }

    /// Follow suspensions until the program waits on a call this session
    /// should run, or ends. Calls with arguments that do not fit the
    /// declaration are answered here with the reason, as Python would.
    async fn advance(&mut self, mut next: Suspension) {
        loop {
            match next {
                Suspension::Finished { result, printed } => {
                    self.finished = Some((result, printed));
                    return;
                }
                Suspension::Call {
                    function,
                    positional,
                    keywords,
                } => {
                    let limit = self.mode.limits.calls;
                    if self.calls.len() >= limit {
                        self.abort(format!(
                            "The program made more than {limit} function calls. \
                             Do the work in fewer calls."
                        ));
                        return;
                    }
                    let named = match self.declarations.iter().find(|d| d.name == function) {
                        Some(declaration) => arguments(declaration, positional, keywords),
                        None => Err(format!("NameError: name '{function}' is not defined")),
                    };
                    match named {
                        Ok(args) => {
                            self.pending = Some((function, args));
                            return;
                        }
                        Err(reason) => {
                            self.calls.push((function, false));
                            next = self.execution.resume(Err(reason)).await;
                        }
                    }
                }
            }
        }
    }
}
