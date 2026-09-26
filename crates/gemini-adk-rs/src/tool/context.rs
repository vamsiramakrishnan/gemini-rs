//! What a tool knows about the call it serves.
//!
//! A tool is usually a pure function of its arguments. Some need more: the
//! session [`State`] (the caller's account, a slot captured earlier), the
//! call's id (for logs and idempotency), or a way to notice the user barged
//! in and stop early. The runtime hands every call a [`ToolContext`]. A tool
//! that wants it asks for it:
//!
//! - `#[tool]`: add a `ctx: ToolContext` parameter. It is filled by the
//!   runtime and never shown to the model.
//! - A closure: [`ContextTool`], or `T::contextual` in the fluent crate.
//! - A hand-written [`ToolFunction`]: override
//!   [`call_with_context`](ToolFunction::call_with_context).
//!
//! ```
//! use gemini_adk_rs::tool::{ContextTool, ToolContext, ToolFunction};
//! use gemini_adk_rs::State;
//! use serde_json::json;
//!
//! # tokio_test::block_on(async {
//! let balance = ContextTool::new("balance", "The caller's balance", None, |_args, ctx: ToolContext| async move {
//!     let account: String = ctx.state.get("account_id").unwrap_or_default();
//!     Ok(json!({ "account": account, "cents": 1200 }))
//! });
//!
//! let state = State::new();
//! state.set("account_id", "A-17").unwrap();
//! let out = balance
//!     .call_with_context(json!({}), ToolContext::new(state).with_call_id("call-1"))
//!     .await
//!     .unwrap();
//! assert_eq!(out["account"], "A-17");
//! # });
//! ```

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::ToolFunction;
use crate::error::ToolError;
use crate::state::State;

/// The session a tool call runs in.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ToolContext {
    /// The session state: read what the conversation captured, write what
    /// the tool learned.
    pub state: State,
    /// The model's id for this call, when it gave one.
    pub call_id: Option<String>,
    /// Cancelled when the call should stop: the user barged in, or the
    /// session is ending. A long tool can check it between steps; the
    /// runtime also drops the call's future when it fires.
    pub cancel: CancellationToken,
}

impl ToolContext {
    /// A context over `state`, with no call id and a token nothing cancels.
    pub fn new(state: State) -> Self {
        Self {
            state,
            call_id: None,
            cancel: CancellationToken::new(),
        }
    }

    /// A context for a call made outside any session (a direct
    /// [`ToolFunction::call`], a unit test): fresh, empty state.
    pub fn detached() -> Self {
        Self::new(State::new())
    }

    /// With the model's call id.
    pub fn with_call_id(mut self, id: impl Into<String>) -> Self {
        self.call_id = Some(id.into());
        self
    }

    /// With a cancellation token.
    pub fn with_cancel(mut self, cancel: CancellationToken) -> Self {
        self.cancel = cancel;
        self
    }
}

type ContextFn = Arc<
    dyn Fn(Value, ToolContext) -> Pin<Box<dyn Future<Output = Result<Value, ToolError>> + Send>>
        + Send
        + Sync,
>;

/// A tool from a closure that receives the [`ToolContext`].
pub struct ContextTool {
    name: String,
    description: String,
    parameters: Option<Value>,
    run: ContextFn,
}

impl ContextTool {
    /// A tool named `name` running `f(args, ctx)`. `parameters` is the JSON
    /// Schema of its arguments (`None` for none).
    pub fn new<F, Fut>(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: Option<Value>,
        f: F,
    ) -> Self
    where
        F: Fn(Value, ToolContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, ToolError>> + Send + 'static,
    {
        let f = Arc::new(f);
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
            run: Arc::new(move |args, ctx| {
                let f = f.clone();
                Box::pin(async move { f(args, ctx).await })
            }),
        }
    }
}

#[async_trait]
impl ToolFunction for ContextTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> Option<Value> {
        self.parameters.clone()
    }

    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        self.call_with_context(args, ToolContext::detached()).await
    }

    async fn call_with_context(&self, args: Value, ctx: ToolContext) -> Result<Value, ToolError> {
        (self.run)(args, ctx).await
    }
}
