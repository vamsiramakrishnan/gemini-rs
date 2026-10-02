//! Tool dispatcher — routes function calls to the right tool implementation.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use gemini_genai_rs::prelude::{FunctionCall, FunctionDeclaration, FunctionResponse, Tool};

use crate::error::ToolError;

use super::{ActiveStreamingTool, DEFAULT_TOOL_TIMEOUT, ToolClass, ToolFunction, ToolKind};

/// Routes function calls to the right tool implementation.
pub struct ToolDispatcher {
    /// Ordered by name, so declarations are identical from run to run.
    tools: BTreeMap<String, ToolKind>,
    active: Arc<tokio::sync::Mutex<HashMap<String, ActiveStreamingTool>>>,
    default_timeout: Duration,
    /// Tool declarations, computed on first access and cleared whenever a tool
    /// is registered.
    cached_declarations: std::sync::OnceLock<Vec<Tool>>,
    /// Optional provider consulted before running confirmation-gated tools.
    confirmation_provider: Option<Arc<dyn crate::confirmation::ConfirmationProvider>>,
}

impl ToolDispatcher {
    /// Create a new empty tool dispatcher with the default 30-second timeout.
    ///
    /// # Examples
    ///
    /// ```rust,ignore
    /// use gemini_adk_rs::tool::{ToolDispatcher, SimpleTool};
    /// use serde_json::json;
    ///
    /// let mut dispatcher = ToolDispatcher::new();
    /// dispatcher.register(SimpleTool::new(
    ///     "echo", "Echo input", None,
    ///     |args| async move { Ok(args) },
    /// ));
    /// ```
    pub fn new() -> Self {
        Self {
            tools: BTreeMap::new(),
            active: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            default_timeout: DEFAULT_TOOL_TIMEOUT,
            cached_declarations: std::sync::OnceLock::new(),
            confirmation_provider: None,
        }
    }

    /// Set the default timeout for tool calls.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.default_timeout = timeout;
        self
    }

    /// Attach a confirmation provider (builder form).
    ///
    /// Once set, any tool reporting
    /// [`requires_confirmation`](crate::tool::ToolFunction::requires_confirmation)
    /// — e.g. one built with `T::confirm(..)` — is checked against the provider
    /// before it executes; a denied decision returns a `ToolError` instead of
    /// running the tool. With no provider configured, confirmation-gated tools
    /// run normally (enforcement is opt-in).
    pub fn with_confirmation_provider(
        mut self,
        provider: Arc<dyn crate::confirmation::ConfirmationProvider>,
    ) -> Self {
        self.confirmation_provider = Some(provider);
        self
    }

    /// Attach a confirmation provider in place. See
    /// [`with_confirmation_provider`](Self::with_confirmation_provider).
    pub fn set_confirmation_provider(
        &mut self,
        provider: Arc<dyn crate::confirmation::ConfirmationProvider>,
    ) {
        self.confirmation_provider = Some(provider);
    }

    /// Whether a confirmation provider is configured.
    pub fn has_confirmation_provider(&self) -> bool {
        self.confirmation_provider.is_some()
    }

    /// Consult the confirmation provider for a gated tool. Returns `Ok(())`
    /// when the tool is not gated, no provider is set, or the call is approved;
    /// returns a `ToolError` when the provider denies it.
    async fn ensure_confirmed(
        &self,
        func: &Arc<dyn ToolFunction>,
        args: &serde_json::Value,
    ) -> Result<(), ToolError> {
        if !func.requires_confirmation() {
            return Ok(());
        }
        let Some(provider) = &self.confirmation_provider else {
            return Ok(());
        };
        let request = crate::confirmation::ConfirmationRequest {
            tool_name: func.name().to_string(),
            args: args.clone(),
            message: func.confirmation_message().map(str::to_string),
        };
        let decision = provider.confirm(request).await;
        if decision.confirmed {
            Ok(())
        } else {
            Err(ToolError::Declined(
                decision.hint.unwrap_or_else(|| "no reason given".into()),
            ))
        }
    }

    /// Returns the configured default timeout.
    pub fn default_timeout(&self) -> Duration {
        self.default_timeout
    }

    /// Register a tool that implements [`ToolFunction`].
    pub fn register(&mut self, tool: impl ToolFunction) {
        self.insert(ToolKind::Function(Arc::new(tool)));
    }

    /// Register a regular function tool (pre-wrapped in Arc).
    pub fn register_function(&mut self, tool: Arc<dyn ToolFunction>) {
        self.insert(ToolKind::Function(tool));
    }

    /// Register a streaming tool.
    pub fn register_streaming(&mut self, tool: Arc<dyn super::StreamingTool>) {
        self.insert(ToolKind::Streaming(tool));
    }

    /// Register an input-streaming tool.
    pub fn register_input_streaming(&mut self, tool: Arc<dyn super::InputStreamingTool>) {
        self.insert(ToolKind::InputStream(tool));
    }

    /// Register `tool` under its name, replacing a tool of the same name.
    fn insert(&mut self, tool: ToolKind) {
        let name = match &tool {
            ToolKind::Function(f) => f.name(),
            ToolKind::Streaming(s) => s.name(),
            ToolKind::InputStream(i) => i.name(),
        }
        .to_string();
        self.tools.insert(name, tool);
        self.cached_declarations.take();
    }

    /// Add every tool of `other` whose name this dispatcher does not already
    /// have. This dispatcher's own tools, timeout and confirmation provider
    /// win.
    pub fn merge(&mut self, other: ToolDispatcher) {
        for (name, tool) in other.tools {
            self.tools.entry(name).or_insert(tool);
        }
        self.cached_declarations.take();
    }

    /// The function tools that ask for confirmation before they run
    /// (`T::confirm(..)`), which need a confirmation provider to be gated.
    pub fn gated_tools(&self) -> impl Iterator<Item = &str> {
        self.tools.iter().filter_map(|(name, tool)| match tool {
            ToolKind::Function(f) if f.requires_confirmation() => Some(name.as_str()),
            _ => None,
        })
    }

    /// The names of the registered tools, in declaration order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.tools.keys().map(String::as_str)
    }

    /// Get a tool by name (for introspection/streaming tool spawning).
    pub fn get_tool(&self, name: &str) -> Option<&ToolKind> {
        self.tools.get(name)
    }

    /// Classify a tool by name.
    pub fn classify(&self, name: &str) -> Option<ToolClass> {
        self.tools.get(name).map(|t| match t {
            ToolKind::Function(_) => ToolClass::Regular,
            ToolKind::Streaming(_) => ToolClass::Streaming,
            ToolKind::InputStream(_) => ToolClass::InputStream,
        })
    }

    /// Call a regular function tool by name, using the default timeout.
    ///
    /// The tool gets a detached [`ToolContext`](super::ToolContext) (fresh
    /// state, no call id). Inside a session use
    /// [`call_function_in`](Self::call_function_in).
    pub async fn call_function(
        &self,
        name: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, ToolError> {
        self.call_function_with_timeout(name, args, self.default_timeout)
            .await
    }

    /// Call a regular function tool by name within a session: the tool
    /// receives `ctx` (see [`ToolContext`](super::ToolContext)). The default
    /// timeout covers confirmation and execution. Cancelling `ctx.cancel`
    /// drops either wait with [`ToolError::Cancelled`].
    pub async fn call_function_in(
        &self,
        name: &str,
        args: serde_json::Value,
        ctx: super::ToolContext,
    ) -> Result<serde_json::Value, ToolError> {
        self.call_function_with_context_timeout(name, args, ctx, self.default_timeout)
            .await
    }

    /// The regular function tool registered as `name`.
    fn function(&self, name: &str) -> Result<Arc<dyn super::ToolFunction>, ToolError> {
        match self.tools.get(name) {
            Some(ToolKind::Function(f)) => Ok(f.clone()),
            Some(_) => Err(ToolError::Other(format!(
                "{name} is not a regular function tool"
            ))),
            None => Err(ToolError::NotFound(name.to_string())),
        }
    }

    /// Call a regular function tool by name with an explicit timeout.
    ///
    /// The duration includes confirmation and execution. When it expires,
    /// the pending future is dropped and `ToolError::Timeout` is returned.
    pub async fn call_function_with_timeout(
        &self,
        name: &str,
        args: serde_json::Value,
        timeout: Duration,
    ) -> Result<serde_json::Value, ToolError> {
        self.call_function_with_context_timeout(name, args, super::ToolContext::detached(), timeout)
            .await
    }

    /// Call a regular function tool by name, racing against a cancellation token.
    ///
    /// The default timeout covers confirmation and execution. If the token
    /// is cancelled during either wait, its future is dropped and
    /// `ToolError::Cancelled` is returned.
    pub async fn call_function_with_cancel(
        &self,
        name: &str,
        args: serde_json::Value,
        cancel: CancellationToken,
    ) -> Result<serde_json::Value, ToolError> {
        self.call_function_in(
            name,
            args,
            super::ToolContext::detached().with_cancel(cancel),
        )
        .await
    }

    async fn call_function_with_context_timeout(
        &self,
        name: &str,
        args: serde_json::Value,
        ctx: super::ToolContext,
        timeout: Duration,
    ) -> Result<serde_json::Value, ToolError> {
        let func = self.function(name)?;
        let cancel = ctx.cancel.clone();
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err(ToolError::Cancelled),
            result = tokio::time::timeout(timeout, async {
                self.ensure_confirmed(&func, &args).await?;
                if cancel.is_cancelled() {
                    return Err(ToolError::Cancelled);
                }
                func.call_with_context(args, ctx).await
            }) => result.unwrap_or(Err(ToolError::Timeout(timeout))),
        }
    }

    /// Build a FunctionResponse from a FunctionCall result.
    pub fn build_response(
        call: &FunctionCall,
        result: Result<serde_json::Value, ToolError>,
    ) -> FunctionResponse {
        match result {
            Ok(value) => FunctionResponse {
                name: call.name.clone(),
                response: value,
                id: call.id.clone(),
                scheduling: None,
            },
            Err(e) => FunctionResponse {
                name: call.name.clone(),
                response: serde_json::json!({"error": e.to_string()}),
                id: call.id.clone(),
                scheduling: None,
            },
        }
    }

    /// Cancel a streaming tool by name.
    pub async fn cancel_streaming(&self, name: &str) {
        let mut active = self.active.lock().await;
        if let Some(tool) = active.remove(name) {
            tool.cancel.cancel();
            tool.task.abort();
        }
    }

    /// Store an active streaming tool (for cancellation tracking).
    pub(crate) async fn store_active(&self, id: String, tool: ActiveStreamingTool) {
        self.active.lock().await.insert(id, tool);
    }

    /// Cancel streaming tools by IDs.
    pub async fn cancel_by_ids(&self, ids: &[String]) {
        let mut active = self.active.lock().await;
        for id in ids {
            if let Some(tool) = active.remove(id.as_str()) {
                tool.cancel.cancel();
                tool.task.abort();
            }
        }
    }

    /// Generate Tool declarations for the setup message.
    ///
    /// Declarations are ordered by tool name and cached until the next
    /// `register*()` or [`merge`](Self::merge).
    pub fn to_tool_declarations(&self) -> Vec<Tool> {
        self.cached_declarations
            .get_or_init(|| {
                let declarations: Vec<FunctionDeclaration> = self
                    .tools
                    .values()
                    .map(|t| {
                        let (name, desc, params) = match t {
                            ToolKind::Function(f) => (f.name(), f.description(), f.parameters()),
                            ToolKind::Streaming(s) => (s.name(), s.description(), s.parameters()),
                            ToolKind::InputStream(i) => (i.name(), i.description(), i.parameters()),
                        };
                        FunctionDeclaration {
                            name: name.to_string(),
                            description: desc.to_string(),
                            parameters: params,
                            behavior: None,
                        }
                    })
                    .collect();

                if declarations.is_empty() {
                    vec![]
                } else {
                    vec![Tool::functions(declarations)]
                }
            })
            .clone()
    }

    /// Number of registered tools.
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// Whether no tools are registered.
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

impl Default for ToolDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl gemini_genai_rs::prelude::ToolProvider for ToolDispatcher {
    fn declarations(&self) -> Vec<gemini_genai_rs::prelude::Tool> {
        self.to_tool_declarations()
    }
}

#[cfg(test)]
mod confirmation_tests {
    use super::*;
    use crate::confirmation::StaticConfirmation;
    use crate::tool::{PolicyTool, SimpleTool, policy::ToolPolicy};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A counting tool wrapped in a confirm policy.
    fn confirm_tool(runs: Arc<AtomicUsize>) -> Arc<dyn ToolFunction> {
        let inner: Arc<dyn ToolFunction> = Arc::new(SimpleTool::new(
            "danger",
            "does something sensitive",
            None,
            move |_| {
                let runs = runs.clone();
                async move {
                    runs.fetch_add(1, Ordering::SeqCst);
                    Ok(json!({ "ok": true }))
                }
            },
        ));
        Arc::new(PolicyTool::new(
            inner,
            ToolPolicy::new().with_confirm(Some("delete production data?".into())),
        ))
    }

    #[tokio::test]
    async fn denied_confirmation_blocks_execution() {
        let runs = Arc::new(AtomicUsize::new(0));
        let mut d = ToolDispatcher::new();
        d.register_function(confirm_tool(runs.clone()));
        d.set_confirmation_provider(StaticConfirmation::deny_all("blocked by policy"));

        let result = d.call_function("danger", json!({})).await;
        assert!(
            matches!(&result, Err(ToolError::Declined(reason)) if reason == "blocked by policy"),
            "the model must learn why: {result:?}"
        );
        assert_eq!(
            runs.load(Ordering::SeqCst),
            0,
            "tool must not run when denied"
        );
    }

    #[tokio::test]
    async fn approved_confirmation_runs() {
        let runs = Arc::new(AtomicUsize::new(0));
        let mut d = ToolDispatcher::new();
        d.register_function(confirm_tool(runs.clone()));
        d.set_confirmation_provider(StaticConfirmation::allow_all());

        let out = d.call_function("danger", json!({})).await.unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    fn awaiting_confirmation(timeout: Duration) -> (ToolDispatcher, Arc<AtomicUsize>) {
        let runs = Arc::new(AtomicUsize::new(0));
        let mut dispatcher = ToolDispatcher::new().with_timeout(timeout);
        dispatcher.register_function(confirm_tool(runs.clone()));
        dispatcher.set_confirmation_provider(Arc::new(
            |_: crate::confirmation::ConfirmationRequest| {
                std::future::pending::<crate::confirmation::ToolConfirmation>()
            },
        ));
        (dispatcher, runs)
    }

    #[tokio::test]
    async fn session_cancellation_stops_pending_confirmation() {
        let (dispatcher, runs) = awaiting_confirmation(Duration::from_secs(30));
        let cancel = CancellationToken::new();
        let ctx = crate::tool::ToolContext::detached().with_cancel(cancel.clone());
        let mut call =
            tokio_test::task::spawn(dispatcher.call_function_in("danger", json!({}), ctx));
        tokio_test::assert_pending!(call.poll());

        cancel.cancel();

        assert!(matches!(
            tokio_test::assert_ready!(call.poll()),
            Err(ToolError::Cancelled)
        ));
        assert_eq!(runs.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn token_cancellation_stops_pending_confirmation() {
        let (dispatcher, runs) = awaiting_confirmation(Duration::from_secs(30));
        let cancel = CancellationToken::new();
        let mut call = tokio_test::task::spawn(dispatcher.call_function_with_cancel(
            "danger",
            json!({}),
            cancel.clone(),
        ));
        tokio_test::assert_pending!(call.poll());

        cancel.cancel();

        assert!(matches!(
            tokio_test::assert_ready!(call.poll()),
            Err(ToolError::Cancelled)
        ));
        assert_eq!(runs.load(Ordering::SeqCst), 0);
    }

    async fn assert_confirmation_timeout(
        call: impl std::future::Future<Output = Result<serde_json::Value, ToolError>>,
        timeout: Duration,
    ) {
        let mut call = tokio_test::task::spawn(call);
        tokio_test::assert_pending!(call.poll());
        tokio::time::advance(timeout).await;
        assert!(matches!(
            tokio_test::assert_ready!(call.poll()),
            Err(ToolError::Timeout(elapsed)) if elapsed == timeout
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn session_timeout_includes_pending_confirmation() {
        let timeout = Duration::from_secs(5);
        let (dispatcher, runs) = awaiting_confirmation(timeout);
        assert_confirmation_timeout(
            dispatcher.call_function_in("danger", json!({}), crate::tool::ToolContext::detached()),
            timeout,
        )
        .await;
        assert_eq!(runs.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn cancellable_call_timeout_includes_pending_confirmation() {
        let timeout = Duration::from_secs(5);
        let (dispatcher, runs) = awaiting_confirmation(timeout);
        assert_confirmation_timeout(
            dispatcher.call_function_with_cancel("danger", json!({}), CancellationToken::new()),
            timeout,
        )
        .await;
        assert_eq!(runs.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn explicit_timeout_includes_pending_confirmation() {
        let timeout = Duration::from_secs(5);
        let (dispatcher, runs) = awaiting_confirmation(Duration::from_secs(30));
        assert_confirmation_timeout(
            dispatcher.call_function_with_timeout("danger", json!({}), timeout),
            timeout,
        )
        .await;
        assert_eq!(runs.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn cancellation_during_approval_prevents_execution() {
        let runs = Arc::new(AtomicUsize::new(0));
        let cancel = CancellationToken::new();
        let approval_cancel = cancel.clone();
        let mut dispatcher = ToolDispatcher::new();
        dispatcher.register_function(confirm_tool(runs.clone()));
        dispatcher.set_confirmation_provider(Arc::new(
            move |_: crate::confirmation::ConfirmationRequest| {
                let cancel = approval_cancel.clone();
                async move {
                    cancel.cancel();
                    crate::confirmation::ToolConfirmation::confirmed()
                }
            },
        ));

        let result = dispatcher
            .call_function_in(
                "danger",
                json!({}),
                crate::tool::ToolContext::detached().with_cancel(cancel),
            )
            .await;
        assert!(matches!(result, Err(ToolError::Cancelled)));
        assert_eq!(runs.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn pending_confirmation_can_approve_execution() {
        let runs = Arc::new(AtomicUsize::new(0));
        let approve = Arc::new(tokio::sync::Notify::new());
        let approval = approve.clone();
        let mut dispatcher = ToolDispatcher::new();
        dispatcher.register_function(confirm_tool(runs.clone()));
        dispatcher.set_confirmation_provider(Arc::new(
            move |_: crate::confirmation::ConfirmationRequest| {
                let approve = approval.clone();
                async move {
                    approve.notified().await;
                    crate::confirmation::ToolConfirmation::confirmed()
                }
            },
        ));
        let mut call = tokio_test::task::spawn(dispatcher.call_function_with_cancel(
            "danger",
            json!({}),
            CancellationToken::new(),
        ));
        tokio_test::assert_pending!(call.poll());
        assert_eq!(runs.load(Ordering::SeqCst), 0);

        approve.notify_one();

        let result = tokio_test::assert_ready!(call.poll()).unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn approval_and_execution_share_one_timeout_budget() {
        let mut dispatcher = ToolDispatcher::new();
        dispatcher.register(PolicyTool::new(
            Arc::new(SimpleTool::new(
                "danger",
                "slow operation",
                None,
                |_| async {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    Ok(json!({ "ok": true }))
                },
            )),
            ToolPolicy::new().with_confirm(None),
        ));
        dispatcher.set_confirmation_provider(Arc::new(
            |_: crate::confirmation::ConfirmationRequest| async {
                tokio::time::sleep(Duration::from_secs(4)).await;
                crate::confirmation::ToolConfirmation::confirmed()
            },
        ));

        let timeout = Duration::from_secs(5);
        let started = tokio::time::Instant::now();
        let result = dispatcher
            .call_function_with_timeout("danger", json!({}), timeout)
            .await;
        assert!(matches!(result, Err(ToolError::Timeout(elapsed)) if elapsed == timeout));
        assert_eq!(started.elapsed(), timeout);
    }

    #[tokio::test]
    async fn no_provider_runs_optin() {
        // Enforcement is opt-in: a confirm-gated tool runs when no provider is set.
        let runs = Arc::new(AtomicUsize::new(0));
        let mut d = ToolDispatcher::new();
        d.register_function(confirm_tool(runs.clone()));

        let out = d.call_function("danger", json!({})).await.unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn provider_sees_request_and_ignores_non_gated_tools() {
        // A non-confirm tool is never sent to the (deny-all) provider.
        let mut d = ToolDispatcher::new();
        d.register(SimpleTool::new(
            "plain",
            "no confirmation",
            None,
            |_| async move { Ok(json!({ "ran": true })) },
        ));
        d.set_confirmation_provider(StaticConfirmation::deny_all("should not be consulted"));

        let out = d.call_function("plain", json!({})).await.unwrap();
        assert_eq!(out["ran"], true);
    }

    #[tokio::test]
    async fn nested_policy_wrapper_does_not_bypass_confirmation() {
        // T::cached(T::confirm(tool)): an outer cache PolicyTool (confirm=false)
        // wraps an inner confirm PolicyTool. The gate must still fire.
        let runs = Arc::new(AtomicUsize::new(0));
        let inner_confirm = confirm_tool(runs.clone()); // Arc<PolicyTool{confirm}>
        let outer_cached: Arc<dyn ToolFunction> = Arc::new(PolicyTool::new(
            inner_confirm,
            ToolPolicy::new().with_cache(),
        ));
        assert!(
            outer_cached.requires_confirmation(),
            "must propagate through nesting"
        );

        let mut d = ToolDispatcher::new();
        d.register_function(outer_cached);
        d.set_confirmation_provider(StaticConfirmation::deny_all("blocked"));

        let result = d.call_function("danger", json!({})).await;
        assert!(matches!(result, Err(ToolError::Declined(_))));
        assert_eq!(
            runs.load(Ordering::SeqCst),
            0,
            "nested confirm must not run when denied"
        );
    }

    #[tokio::test]
    async fn closure_provider_can_gate_by_name() {
        let runs = Arc::new(AtomicUsize::new(0));
        let mut d = ToolDispatcher::new();
        d.register_function(confirm_tool(runs.clone()));
        d.set_confirmation_provider(Arc::new(
            |req: crate::confirmation::ConfirmationRequest| async move {
                if req.tool_name == "danger" {
                    crate::confirmation::ToolConfirmation::denied("name-gated")
                } else {
                    crate::confirmation::ToolConfirmation::confirmed()
                }
            },
        ));

        assert!(d.call_function("danger", json!({})).await.is_err());
        assert_eq!(runs.load(Ordering::SeqCst), 0);
    }
}

#[cfg(test)]
mod declaration_tests {
    use super::*;
    use crate::tool::SimpleTool;

    fn named(name: &'static str) -> Arc<dyn ToolFunction> {
        Arc::new(SimpleTool::new(name, name, None, |_| async {
            Ok(serde_json::json!({}))
        }))
    }

    fn declared(d: &ToolDispatcher) -> Vec<String> {
        d.to_tool_declarations()
            .iter()
            .flat_map(|t| t.function_declarations.iter().flatten())
            .map(|f| f.name.clone())
            .collect()
    }

    /// Registering after the declarations were read must change them; the
    /// cache used to be computed once and never cleared.
    #[test]
    fn registering_a_tool_refreshes_the_declarations() {
        let mut d = ToolDispatcher::new();
        d.register_function(named("a"));
        assert_eq!(declared(&d), ["a"]);
        d.register_function(named("b"));
        assert_eq!(declared(&d), ["a", "b"]);
    }

    /// Declarations are ordered by name, whatever the registration order, so a
    /// setup message is byte-identical from run to run.
    #[test]
    fn declarations_are_deterministic() {
        let mut d = ToolDispatcher::new();
        for name in ["zeta", "alpha", "mid"] {
            d.register_function(named(name));
        }
        assert_eq!(declared(&d), ["alpha", "mid", "zeta"]);
        assert_eq!(d.names().collect::<Vec<_>>(), ["alpha", "mid", "zeta"]);
    }

    #[test]
    fn merge_keeps_both_and_this_dispatcher_wins_a_clash() {
        let mut mine = ToolDispatcher::new().with_timeout(Duration::from_secs(3));
        mine.register_function(Arc::new(SimpleTool::new(
            "shared",
            "mine",
            None,
            |_| async { Ok(serde_json::json!({})) },
        )));
        let mut theirs = ToolDispatcher::new();
        theirs.register_function(named("shared"));
        theirs.register_function(named("extra"));
        mine.merge(theirs);
        assert_eq!(declared(&mine), ["extra", "shared"]);
        match mine.get_tool("shared") {
            Some(ToolKind::Function(f)) => assert_eq!(f.description(), "mine"),
            _ => panic!("shared must stay a function tool"),
        }
        assert_eq!(mine.default_timeout(), Duration::from_secs(3));
    }
}
