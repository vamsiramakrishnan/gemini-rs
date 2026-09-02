//! T — Tool composition.
//!
//! Compose tools in any order with `|`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use gemini_adk_rs::text::TextAgent;
use gemini_adk_rs::tool::{PolicyTool, SimpleTool, ToolFunction, ToolPolicy};
use gemini_genai_rs::prelude::{FunctionDeclaration, Tool};

/// A tool composite — one or more tool entries.
///
/// Built from the `T` namespace and composed with `|`. Any single
/// [`ToolFunction`] (a `SimpleTool`, a `TypedTool`, the value a `#[tool]`
/// function returns, or an `Arc<dyn ToolFunction>`) converts into a
/// one-entry composite via `From`, so `.tools(get_weather())` works without
/// the namespace.
#[derive(Clone)]
#[non_exhaustive]
pub struct ToolComposite {
    /// The tool entries in this composite.
    pub entries: Vec<ToolCompositeEntry>,
}

/// Async transformer applied to a tool result value.
pub type TransformFn = Arc<
    dyn Fn(serde_json::Value) -> Pin<Box<dyn Future<Output = serde_json::Value> + Send>>
        + Send
        + Sync,
>;

/// An entry in a tool composite.
#[derive(Clone)]
pub enum ToolCompositeEntry {
    /// A runtime tool function.
    Function(Arc<dyn ToolFunction>),
    /// A built-in Gemini tool declaration.
    BuiltIn(Tool),
    /// A text agent wrapped as a tool.
    Agent {
        /// Tool name exposed to the model.
        name: String,
        /// Tool description exposed to the model.
        description: String,
        /// The text agent to invoke.
        agent: Arc<dyn TextAgent>,
    },
    /// An MCP (Model Context Protocol) toolset connection.
    Mcp {
        /// Connection params (e.g. URL or command string).
        params: String,
    },
    /// A mock tool that returns a fixed response (useful for testing).
    Mock {
        /// Tool name.
        name: String,
        /// Tool description.
        description: String,
        /// Fixed response to return.
        response: serde_json::Value,
    },
    /// A schema-defined tool (placeholder/marker).
    Schema {
        /// Tool name.
        name: String,
        /// JSON Schema defining the tool's parameters.
        schema: serde_json::Value,
    },
    /// A tool wrapped with a result transformer.
    Transform {
        /// The inner tool entry.
        inner: Box<ToolCompositeEntry>,
        /// Transformer function applied to the tool result.
        transformer: TransformFn,
    },
}

impl ToolComposite {
    /// Create a composite containing a single runtime tool function.
    pub fn from_function(f: Arc<dyn ToolFunction>) -> Self {
        Self {
            entries: vec![ToolCompositeEntry::Function(f)],
        }
    }

    /// Create a composite containing a single built-in tool declaration.
    pub fn from_built_in(tool: Tool) -> Self {
        Self {
            entries: vec![ToolCompositeEntry::BuiltIn(tool)],
        }
    }

    /// Number of tool entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Apply a per-tool [`ToolPolicy`] transform to every function entry.
    ///
    /// Each [`ToolCompositeEntry::Function`] is wrapped in a [`PolicyTool`]
    /// carrying the policy. Successive modifiers nest (e.g. `T::cached(T::timeout(..))`
    /// applies both timeout and cache), since a `PolicyTool` is itself a
    /// [`ToolFunction`]. Other entry kinds are left untouched.
    fn map_function_policy(
        mut self,
        f: impl Fn(ToolPolicy) -> ToolPolicy + Send + Sync + 'static,
    ) -> Self {
        self.entries = self
            .entries
            .into_iter()
            .map(|entry| match entry {
                ToolCompositeEntry::Function(func) => {
                    let policy = f(ToolPolicy::new());
                    ToolCompositeEntry::Function(PolicyTool::wrap(func, policy))
                }
                other => other,
            })
            .collect();
        self
    }
}

/// A single tool is a one-entry composite, so `.tools(my_tool)` and
/// `.tool(my_tool)` accept the same values.
impl<F: ToolFunction + 'static> From<F> for ToolComposite {
    fn from(f: F) -> Self {
        Self::from_function(Arc::new(f))
    }
}

/// Compose two tool composites with `|`.
impl std::ops::BitOr for ToolComposite {
    type Output = ToolComposite;

    fn bitor(mut self, rhs: ToolComposite) -> Self::Output {
        self.entries.extend(rhs.entries);
        self
    }
}

/// The `T` namespace — static factory methods for tool composition.
pub struct T;

impl T {
    /// Register a function tool.
    pub fn function(f: Arc<dyn ToolFunction>) -> ToolComposite {
        ToolComposite::from_function(f)
    }

    /// Add Google Search built-in tool.
    pub fn google_search() -> ToolComposite {
        ToolComposite::from_built_in(Tool::google_search())
    }

    /// Add URL context built-in tool.
    pub fn url_context() -> ToolComposite {
        ToolComposite::from_built_in(Tool::url_context())
    }

    /// Add code execution built-in tool.
    pub fn code_execution() -> ToolComposite {
        ToolComposite::from_built_in(Tool::code_execution())
    }

    /// Create a simple tool from a name, description, and async closure.
    pub fn simple<F, Fut>(
        name: impl Into<String>,
        description: impl Into<String>,
        f: F,
    ) -> ToolComposite
    where
        F: Fn(serde_json::Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<serde_json::Value, gemini_adk_rs::ToolError>> + Send + 'static,
    {
        let tool = SimpleTool::new(name, description, None, f);
        ToolComposite::from_function(Arc::new(tool))
    }

    /// Alias for [`simple`](Self::simple) — matches upstream Python `T.fn()`.
    ///
    /// Named `fn_tool` because `fn` is a reserved keyword in Rust.
    pub fn fn_tool<F, Fut>(
        name: impl Into<String>,
        description: impl Into<String>,
        f: F,
    ) -> ToolComposite
    where
        F: Fn(serde_json::Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<serde_json::Value, gemini_adk_rs::ToolError>> + Send + 'static,
    {
        Self::simple(name, description, f)
    }

    /// Require user confirmation before each function tool in the composite runs.
    ///
    /// The confirmation flag is recorded on the tool's [`ToolPolicy`] and surfaced
    /// to the runtime via [`PolicyTool::requires_confirmation`] — it is never
    /// silently dropped. The `message` becomes the confirmation hint. Built-in and
    /// placeholder entries are left unchanged.
    pub fn confirm(tool: ToolComposite, message: &str) -> ToolComposite {
        let msg = if message.is_empty() {
            None
        } else {
            Some(message.to_string())
        };
        tool.map_function_policy(move |p| p.with_confirm(msg.clone()))
    }

    /// Bound each function tool in the composite by a timeout.
    ///
    /// At dispatch the tool's future is raced against the duration; on elapse the
    /// call returns [`ToolError::Timeout`](gemini_adk_rs::ToolError::Timeout).
    /// Built-in and placeholder entries are left unchanged.
    pub fn timeout(tool: ToolComposite, duration: std::time::Duration) -> ToolComposite {
        tool.map_function_policy(move |p| p.with_timeout(duration))
    }

    /// Memoize each function tool's successful results.
    ///
    /// Results are cached by `(tool name, canonical-JSON args)`; repeat calls with
    /// identical arguments return the cached value without re-invoking the tool.
    /// Errors are not cached. Built-in/placeholder entries are left unchanged.
    pub fn cached(tool: ToolComposite) -> ToolComposite {
        tool.map_function_policy(gemini_adk_rs::tool::ToolPolicy::with_cache)
    }

    /// Combine multiple tool functions into a single composite.
    pub fn toolset(tools: Vec<Arc<dyn ToolFunction>>) -> ToolComposite {
        ToolComposite {
            entries: tools
                .into_iter()
                .map(ToolCompositeEntry::Function)
                .collect(),
        }
    }

    /// Wrap a [`TextAgent`] as a tool (shorthand for creating an agent tool entry).
    ///
    /// When invoked, the agent runs via `BaseLlm::generate()` and returns its
    /// text output as the tool result. State is shared with the parent session.
    pub fn agent(
        name: impl Into<String>,
        description: impl Into<String>,
        agent: impl TextAgent + 'static,
    ) -> ToolComposite {
        ToolComposite {
            entries: vec![ToolCompositeEntry::Agent {
                name: name.into(),
                description: description.into(),
                agent: Arc::new(agent),
            }],
        }
    }

    /// Create an MCP (Model Context Protocol) toolset entry.
    ///
    /// `params` is the connection string (e.g. a URL or command) used to
    /// establish the MCP session at runtime.
    pub fn mcp(params: impl Into<String>) -> ToolComposite {
        ToolComposite {
            entries: vec![ToolCompositeEntry::Mcp {
                params: params.into(),
            }],
        }
    }

    /// Create a mock tool that returns a fixed response.
    ///
    /// Useful for testing and prototyping without real tool implementations.
    pub fn mock(
        name: impl Into<String>,
        description: impl Into<String>,
        response: serde_json::Value,
    ) -> ToolComposite {
        ToolComposite {
            entries: vec![ToolCompositeEntry::Mock {
                name: name.into(),
                description: description.into(),
                response,
            }],
        }
    }

    /// Create a schema-defined tool (placeholder/marker).
    ///
    /// The tool's parameters are defined by the given JSON Schema value.
    pub fn schema(name: impl Into<String>, schema: serde_json::Value) -> ToolComposite {
        ToolComposite {
            entries: vec![ToolCompositeEntry::Schema {
                name: name.into(),
                schema,
            }],
        }
    }

    /// Wrap each tool entry in a composite with a result transformer.
    ///
    /// The transformer function is applied to the tool's output value before
    /// it is returned to the model.
    pub fn transform<F, Fut>(tool: ToolComposite, f: F) -> ToolComposite
    where
        F: Fn(serde_json::Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = serde_json::Value> + Send + 'static,
    {
        let f: TransformFn = Arc::new(
            move |v: serde_json::Value| -> Pin<Box<dyn Future<Output = serde_json::Value> + Send>> {
                Box::pin(f(v))
            },
        );
        ToolComposite {
            entries: tool
                .entries
                .into_iter()
                .map(|entry| ToolCompositeEntry::Transform {
                    inner: Box::new(entry),
                    transformer: Arc::clone(&f),
                })
                .collect(),
        }
    }
}

// ── Resolution ────────────────────────────────────────────────────────────

/// A tool entry that needs asynchronous I/O (network or subprocess) to resolve,
/// and is therefore resolved at connect time rather than when the composite is
/// built. See [`crate::live::Live`] connection methods. A text
/// [`AgentBuilder`](crate::builder::AgentBuilder) rejects these at `build`.
#[derive(Clone, Debug)]
pub enum DeferredTool {
    /// MCP server connection — a stdio command line or an SSE/HTTP URL.
    Mcp {
        /// Connection string: an `http(s)://` URL (SSE) or a command line (stdio).
        params: String,
    },
}

/// The concrete outcome of classifying a single [`ToolCompositeEntry`].
///
/// This is the *single* exhaustive mapping from the composable tool algebra to
/// the runtime; both [`crate::builder::AgentBuilder`] and [`crate::live::Live`]
/// resolve through it, so no entry can be silently dropped.
pub(crate) enum ToolResolution {
    /// A runtime-executable tool function (register with a dispatcher).
    Runtime(Arc<dyn ToolFunction>),
    /// A built-in / declaration-only Gemini tool (add to the session config).
    BuiltIn(Tool),
    /// A text agent to expose as a tool (needs a shared session `State`).
    Agent {
        /// Tool name exposed to the model.
        name: String,
        /// Tool description exposed to the model.
        description: String,
        /// The text agent to invoke.
        agent: Arc<dyn TextAgent>,
    },
    /// A tool that can only be resolved with async I/O at connect time.
    Deferred(DeferredTool),
}

impl ToolCompositeEntry {
    #[cfg(test)]
    fn classify_name(self) -> String {
        match self.classify() {
            ToolResolution::Runtime(f) => f.name().to_string(),
            _ => String::new(),
        }
    }

    /// Classify this entry into its concrete [`ToolResolution`]. Exhaustive by
    /// construction — adding a variant forces every consumer to handle it.
    pub(crate) fn classify(self) -> ToolResolution {
        match self {
            ToolCompositeEntry::Function(f) => ToolResolution::Runtime(f),
            ToolCompositeEntry::BuiltIn(t) => ToolResolution::BuiltIn(t),
            ToolCompositeEntry::Agent {
                name,
                description,
                agent,
            } => ToolResolution::Agent {
                name,
                description,
                agent,
            },
            ToolCompositeEntry::Mock {
                name,
                description,
                response,
            } => ToolResolution::Runtime(Arc::new(SimpleTool::new(
                name,
                description,
                None,
                move |_args| {
                    let r = response.clone();
                    async move { Ok(r) }
                },
            ))),
            ToolCompositeEntry::Transform { inner, transformer } => match inner.classify() {
                ToolResolution::Runtime(f) => ToolResolution::Runtime(Arc::new(TransformTool {
                    inner: f,
                    transformer,
                })),
                // A transformer only applies to a runtime function; for any other
                // inner kind the transform is a no-op and the inner resolution
                // passes through unchanged.
                other => other,
            },
            ToolCompositeEntry::Schema { name, schema } => {
                // A declaration-only tool: the model is told the function exists
                // and the application services the call (e.g. via on_tool_call).
                ToolResolution::BuiltIn(Tool::functions(vec![FunctionDeclaration {
                    name,
                    description: String::new(),
                    parameters: Some(schema),
                    behavior: None,
                }]))
            }
            ToolCompositeEntry::Mcp { params } => {
                ToolResolution::Deferred(DeferredTool::Mcp { params })
            }
        }
    }
}

/// A [`ToolFunction`] that applies an async transformer to another tool's result.
struct TransformTool {
    inner: Arc<dyn ToolFunction>,
    #[allow(clippy::type_complexity)]
    transformer: Arc<
        dyn Fn(serde_json::Value) -> Pin<Box<dyn Future<Output = serde_json::Value> + Send>>
            + Send
            + Sync,
    >,
}

#[async_trait::async_trait]
impl ToolFunction for TransformTool {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn parameters(&self) -> Option<serde_json::Value> {
        self.inner.parameters()
    }

    async fn call(
        &self,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, gemini_adk_rs::error::ToolError> {
        let result = self.inner.call(args).await?;
        Ok((self.transformer)(result).await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Classify the single entry of a one-element composite.
    fn classify_one(c: ToolComposite) -> ToolResolution {
        c.entries.into_iter().next().unwrap().classify()
    }

    #[test]
    fn classify_maps_every_variant() {
        // Synchronous, runtime-executable.
        assert!(matches!(
            classify_one(T::mock("m", "d", serde_json::json!({"ok": true}))),
            ToolResolution::Runtime(_)
        ));
        assert!(matches!(
            classify_one(T::simple("s", "d", |a| async move { Ok(a) })),
            ToolResolution::Runtime(_)
        ));
        // Built-in / declaration-only.
        assert!(matches!(
            classify_one(T::google_search()),
            ToolResolution::BuiltIn(_)
        ));
        assert!(matches!(
            classify_one(T::schema("s", serde_json::json!({"type": "object"}))),
            ToolResolution::BuiltIn(_)
        ));
        // Async, connect-time.
        assert!(matches!(
            classify_one(T::mcp("node ./server.js")),
            ToolResolution::Deferred(DeferredTool::Mcp { .. })
        ));
    }

    #[test]
    fn a_single_tool_function_converts_into_a_composite() {
        let composite: ToolComposite =
            SimpleTool::new("one", "one", None, |_| async { Ok(serde_json::json!(1)) }).into();
        assert_eq!(composite.len(), 1);
        let arc: Arc<dyn ToolFunction> = Arc::new(SimpleTool::new("two", "two", None, |_| async {
            Ok(serde_json::json!(2))
        }));
        let composite: ToolComposite = arc.into();
        assert_eq!(composite.len(), 1);
        assert_eq!(composite.entries[0].clone().classify_name(), "two");
    }

    #[tokio::test]
    async fn mock_resolves_to_callable_runtime_tool() {
        let resolution = classify_one(T::mock(
            "weather",
            "Mock weather",
            serde_json::json!({"temp": 22}),
        ));
        let ToolResolution::Runtime(tool) = resolution else {
            panic!("mock should resolve to a runtime tool");
        };
        assert_eq!(tool.name(), "weather");
        let out = tool.call(serde_json::json!({})).await.unwrap();
        assert_eq!(out, serde_json::json!({"temp": 22}));
    }

    #[tokio::test]
    async fn transform_wraps_inner_runtime_result() {
        let composite = T::transform(
            T::mock("base", "d", serde_json::json!({"n": 1})),
            |mut v| async move {
                v["doubled"] = serde_json::json!(true);
                v
            },
        );
        let ToolResolution::Runtime(tool) = classify_one(composite) else {
            panic!("transform over a mock should resolve to a runtime tool");
        };
        assert_eq!(tool.name(), "base");
        let out = tool.call(serde_json::json!({})).await.unwrap();
        assert_eq!(out, serde_json::json!({"n": 1, "doubled": true}));
    }

    #[test]
    fn google_search_creates_composite() {
        let t = T::google_search();
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn url_context_creates_composite() {
        let t = T::url_context();
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn code_execution_creates_composite() {
        let t = T::code_execution();
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn compose_with_bitor() {
        let t = T::google_search() | T::url_context() | T::code_execution();
        assert_eq!(t.len(), 3);
    }

    #[test]
    fn simple_creates_tool() {
        let t = T::simple("greet", "Greets the user", |_args| async {
            Ok(serde_json::json!({"message": "hello"}))
        });
        assert_eq!(t.len(), 1);
        match &t.entries[0] {
            ToolCompositeEntry::Function(f) => assert_eq!(f.name(), "greet"),
            _ => panic!("expected Function entry"),
        }
    }

    #[tokio::test]
    async fn timeout_modifier_enforces_timeout() {
        use gemini_adk_rs::ToolError;
        use std::time::Duration;

        let t = T::timeout(
            T::simple("slow", "slow tool", |_| async move {
                tokio::time::sleep(Duration::from_secs(3600)).await;
                Ok(serde_json::json!({"ok": true}))
            }),
            Duration::from_millis(50),
        );
        match &t.entries[0] {
            ToolCompositeEntry::Function(f) => match f.call(serde_json::json!({})).await {
                Err(ToolError::Timeout(d)) => assert_eq!(d, Duration::from_millis(50)),
                other => panic!("expected Timeout, got {other:?}"),
            },
            _ => panic!("expected Function entry"),
        }
    }

    #[tokio::test]
    async fn cached_modifier_memoizes_results() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let counter = Arc::new(AtomicU32::new(0));
        let c = counter.clone();
        let t = T::cached(T::simple("count", "counts calls", move |_| {
            let c = c.clone();
            async move {
                let n = c.fetch_add(1, Ordering::SeqCst) + 1;
                Ok(serde_json::json!({"n": n}))
            }
        }));
        match &t.entries[0] {
            ToolCompositeEntry::Function(f) => {
                let first = f.call(serde_json::json!({"x": 1})).await.unwrap();
                let second = f.call(serde_json::json!({"x": 1})).await.unwrap();
                assert_eq!(first, second);
                assert_eq!(first["n"], 1);
                assert_eq!(counter.load(Ordering::SeqCst), 1);
            }
            _ => panic!("expected Function entry"),
        }
    }

    #[test]
    fn confirm_modifier_wraps_function() {
        // confirm() wraps the function (preserving its name) so the policy flag
        // travels to the runtime rather than being silently dropped.
        let t = T::confirm(
            T::simple("danger", "dangerous", |_| async move {
                Ok(serde_json::json!({}))
            }),
            "are you sure?",
        );
        match &t.entries[0] {
            ToolCompositeEntry::Function(f) => assert_eq!(f.name(), "danger"),
            _ => panic!("expected Function entry"),
        }
    }

    #[test]
    fn toolset_combines_functions() {
        let tool_a: Arc<dyn ToolFunction> =
            Arc::new(SimpleTool::new("a", "tool a", None, |_| async {
                Ok(serde_json::json!(null))
            }));
        let tool_b: Arc<dyn ToolFunction> =
            Arc::new(SimpleTool::new("b", "tool b", None, |_| async {
                Ok(serde_json::json!(null))
            }));
        let t = T::toolset(vec![tool_a, tool_b]);
        assert_eq!(t.len(), 2);
    }
}
