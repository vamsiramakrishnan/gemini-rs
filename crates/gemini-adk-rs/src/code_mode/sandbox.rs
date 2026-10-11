//! Where programs run: the sandbox interface.

use async_trait::async_trait;
use serde_json::{Map, Value};

use super::CodeLimits;

/// Where a program has stopped: at a call to one of its functions, or at its
/// end.
#[derive(Debug, Clone, PartialEq)]
pub enum Suspension {
    /// The program called `function` and waits for its result.
    Call {
        /// The function's name.
        function: String,
        /// Positional arguments, in order.
        positional: Vec<Value>,
        /// Keyword arguments.
        keywords: Map<String, Value>,
    },
    /// The program ended.
    Finished {
        /// The value of its last expression, or the error that ended it.
        result: Result<Value, String>,
        /// What it printed.
        printed: String,
    },
}

impl Suspension {
    /// A program that ended with `error` and printed nothing.
    pub fn failed(error: impl Into<String>) -> Self {
        Self::Finished {
            result: Err(error.into()),
            printed: String::new(),
        }
    }
}

/// A program suspended at a call. Dropping it cancels the program.
#[async_trait]
pub trait Execution: Send {
    /// Resume the program with the result of the call it waits on, or with
    /// the reason the call failed (raised as `RuntimeError`), and run it to
    /// its next suspension.
    async fn resume(&mut self, answer: Result<Value, String>) -> Suspension;
}

/// Runs programs: Python source with the given functions defined as globals.
#[async_trait]
pub trait CodeSandbox: Send + Sync {
    /// Start `program` with each of `functions` defined as a global that
    /// suspends the program when called. Returns the execution and where it
    /// first stopped.
    async fn start(
        &self,
        program: &str,
        functions: &[String],
        limits: &CodeLimits,
    ) -> (Box<dyn Execution>, Suspension);
}

/// A sandbox that refuses every program: the default when this crate is
/// built without the `code-mode` feature.
#[derive(Debug, Clone, Copy, Default)]
pub struct Unavailable;

struct Ended;

/// An execution that has already ended.
pub(super) fn ended() -> Box<dyn Execution> {
    Box::new(Ended)
}

#[async_trait]
impl Execution for Ended {
    async fn resume(&mut self, _answer: Result<Value, String>) -> Suspension {
        Suspension::failed("the program has ended")
    }
}

#[async_trait]
impl CodeSandbox for Unavailable {
    async fn start(
        &self,
        _program: &str,
        _functions: &[String],
        _limits: &CodeLimits,
    ) -> (Box<dyn Execution>, Suspension) {
        (
            Box::new(Ended),
            Suspension::failed(
                "code mode is unavailable: this build has no sandbox (enable the \
                 `code-mode` feature)",
            ),
        )
    }
}
