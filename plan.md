# adk-rs-fluent ↔ adk-fluent Naming Parity Plan

## Summary

Align the Rust `adk-rs-fluent` (L2) crate with the upstream Python `adk-fluent` library naming conventions, add missing composition namespaces, and update examples.

---

## 1. Core Builder Naming Changes

### 1a. `AgentBuilder` → `Agent` (primary builder)

| Current Rust | Upstream Python | Action |
|---|---|---|
| `AgentBuilder::new("name")` | `Agent("name")` | Add `pub type Agent = AgentBuilder` alias; keep `AgentBuilder` as deprecated re-export |

### 1b. Add workflow builders to match upstream

| Upstream Python | Current Rust Equivalent | Action |
|---|---|---|
| `Pipeline("name")` | `Composable::Pipeline` (no builder) | Add `Pipeline` struct builder with `.step()`, `.sub_agent()`, `.describe()`, `.build()` |
| `FanOut("name")` | `Composable::FanOut` (no builder) | Add `FanOut` struct builder with `.branch()`, `.sub_agent()`, `.describe()`, `.build()` |
| `Loop("name")` | `Composable::Loop` (no builder) | Add `Loop` struct builder with `.step()`, `.max_iterations()`, `.describe()`, `.build()` |

### 1c. Agent builder method renames/additions

| Upstream Python | Current Rust | Action |
|---|---|---|
| `.instruct(text)` | `.instruction(text)` | Add `.instruct()` as alias |
| `.describe(text)` | `.description(text)` | Add `.describe()` as alias |
| `.tool(fn)` | N/A (only `.google_search()` etc.) | Add `.tool(impl ToolFunction)` method |
| `.tools(composite)` | N/A | Add `.tools(ToolComposite)` method |
| `.guard(g_composite)` | N/A | Add `.guard()` method |
| `.context(c_transform)` | N/A | Add `.context(ContextPolicy)` method |
| `.planner(p)` | N/A | Add `.planner()` method |
| `.code_executor(e)` | N/A | Add `.code_executor()` method |
| `.sub_agent(builder)` | `.sub_agent(builder)` | Already matches ✓ |
| `.agent_tool(agent)` | N/A (only on Live) | Add to AgentBuilder |
| `.show()` / `.hide()` | N/A | Add visibility methods |
| `.memory(mode)` | N/A | Add memory tools integration |
| `.skill(...)` | N/A | Add A2A skill declaration |
| `.publish(port)` | N/A | Add A2A server publishing |
| `.ask(prompt)` / `.ask_async(prompt)` | `.run(&state)` (via build+run) | Add `.ask()` convenience |
| `.eval(prompt, expect)` | N/A | Add inline evaluation |
| `.eval_suite()` | N/A | Add eval suite builder |

---

## 2. Composition Namespace Additions

### 2a. E (Evaluation) Module — NEW

Upstream has `E` namespace for evaluation composition. Add `compose/eval.rs`:

```
E::suite(agent) → EvalSuite
E::trajectory() → ECriterion
E::response_match(threshold) → ECriterion
E::safety() → ECriterion
E::semantic_match() → ECriterion
E::compare(agent_a, agent_b) → ComparisonSuite
EvalSuite::case(prompt, expect) → Self
EvalSuite::criteria(criteria) → Self
EvalSuite::run() → EvalReport
```

### 2b. G (Guards) Module — NEW

Upstream has `G` namespace for guard composition. Add `compose/guards.rs`:

```
G::pii(action) → GGuard
G::length(max) → GGuard
G::budget(max_tokens) → GGuard
G::regex(pattern) → GGuard
G::output(schema) → GGuard
G::custom(fn) → GGuard
GComposite via | operator
```

### 2c. C (Context) Module — Additional Methods

| Upstream Python | Current Rust | Action |
|---|---|---|
| `C.from_agents(names)` | N/A | Add |
| `C.exclude_agents(names)` | N/A | Add |
| `C.template(tpl)` | N/A | Add |
| `C.select(indices)` | N/A | Add |
| `C.recent(n)` | `C::window(n)` | Add as alias |
| `C.compact()` | N/A | Add |
| `C.budget(tokens)` | N/A | Add |
| `C.priority(keys)` | N/A | Add |
| `C.fit(max_tokens)` | N/A | Add |
| `C.fresh(max_age)` | N/A | Add |
| `C.redact(patterns)` | N/A | Add |
| `C.summarize(model)` | N/A | Add |
| `C.relevant(query)` | N/A | Add |
| `C.when(pred, spec)` | N/A | Add |
| `C.rolling(window)` | N/A | Add |
| `C.none()` | `C::empty()` | Add as alias |

### 2d. P (Prompt) Module — Additional Methods

| Upstream Python | Current Rust | Action |
|---|---|---|
| `P.section(name, text)` | N/A | Add |
| `P.template(tpl)` | N/A | Add |
| `P.reorder(order)` | N/A | Add |
| `P.only(sections)` | N/A | Add |
| `P.without(sections)` | N/A | Add |
| `P.compress(model)` | N/A | Add |
| `P.adapt(model)` | N/A | Add |
| `P.scaffolded(template)` | N/A | Add |
| `P.versioned(versions)` | N/A | Add |

### 2e. S (State) Module — Additional Methods

| Upstream Python | Current Rust | Action |
|---|---|---|
| `S.default(key=value)` | `S::defaults(json)` | Already matches (slightly different API) ✓ |
| `S.merge(*keys, into, fn)` | `S::merge(keys, into)` | Add `fn` parameter |
| `StateDelta` | N/A | Add type |
| `StateReplacement` | N/A | Add type |

### 2f. M (Middleware) Module — Additional Methods

| Upstream Python | Current Rust | Action |
|---|---|---|
| `M.scope(name, m)` | N/A | Add scoped middleware |
| `M.cost()` | `M::cost()` | Already matches ✓ |
| `M.structured_log()` | N/A | Add |
| `M.dispatch_log()` | N/A | Add |
| `M.topology_log()` | N/A | Add |
| `RetryMiddleware` | `M::retry()` | Already matches ✓ |
| `LatencyMiddleware` | `M::latency()` | Already matches ✓ |
| `A2ARetryMiddleware` | N/A | Add |
| `A2ACircuitBreakerMiddleware` | N/A | Add |
| `A2ATimeoutMiddleware` | N/A | Add |

### 2g. T (Tools) Module — Additional Methods

| Upstream Python | Current Rust | Action |
|---|---|---|
| `T.fn(callable)` | `T::simple(name, desc, f)` | Add `T::fn_tool()` alias |
| `T.search(registry)` | N/A | Add |
| `T.confirm(tool)` | N/A | Add confirmation wrapper |
| `T.timeout(tool, dur)` | N/A | Add timeout wrapper |
| `T.cached(tool)` | N/A | Add cached wrapper |
| `T.transform(tool, f)` | N/A | Add transform wrapper |

---

## 3. Pattern Functions

### 3a. Existing patterns — keep with name alignment

| Upstream Python | Current Rust | Status |
|---|---|---|
| `review_loop()` | `review_loop()` | ✓ Matches |
| `cascade()` | `cascade()` | ✓ Matches |
| `fan_out_merge()` | `fan_out_merge()` | ✓ Matches |
| `supervised()` | `supervised()` | ✓ Matches |
| `map_over()` | `map_over()` | ✓ Matches |

### 3b. New patterns from upstream

| Upstream Python | Action |
|---|---|
| `chain(*steps)` | Add — convenience for `>>` chaining |
| `conditional(pred, if_true, if_false)` | Add — state-based routing |
| `map_reduce(mapper, reducer)` | Add |
| `a2a_cascade(*endpoints)` | Add |
| `a2a_fanout(*endpoints)` | Add |
| `a2a_delegate(coordinator, **remotes)` | Add |

---

## 4. A2A Module

| Upstream Python | Current Rust | Action |
|---|---|---|
| `RemoteAgent(name, url)` | N/A in L2 (exists in L1 as `RemoteA2aAgent`) | Add `RemoteAgent` builder in L2 with `.timeout()`, `.describe()` |
| `A2AServer(agent)` | N/A | Add `A2AServer` builder with `.port()`, `.host()`, `.health_check()`, `.build()` |
| `AgentRegistry` | N/A | Add registry for agent discovery |
| `SkillDeclaration` | N/A | Add skill metadata type |

---

## 5. Testing Module Additions

| Upstream Python | Current Rust | Action |
|---|---|---|
| `check_contracts()` | `check_contracts()` | ✓ Matches |
| `infer_data_flow()` | N/A | Add data flow inference |
| `DataFlowSuggestion` | N/A | Add type |
| `MockBackend` | N/A | Add mock backend for testing |
| `AgentHarness` | N/A | Add test harness |
| `diagnose()` | N/A | Add diagnostic utility |

---

## 6. Runtime/Execution

| Upstream Python | Current Rust | Action |
|---|---|---|
| `App(agent)` | N/A (use Live::builder) | Add `App` runner for text agents |
| `Runner` / `InMemoryRunner` | N/A | Add runner abstraction |
| `Source` / `Inbox` | N/A | Add streaming source types |
| `StreamRunner` | N/A | Add stream execution |

---

## 7. Prelude Updates

Update `prelude` to re-export new types:

```rust
// Current prelude re-exports + new additions:
pub use crate::Agent;           // type alias for AgentBuilder
pub use crate::Pipeline;        // new workflow builder
pub use crate::FanOut;          // new workflow builder
pub use crate::Loop;            // new workflow builder
pub use crate::compose::eval::E;
pub use crate::compose::guards::G;
pub use crate::a2a::{RemoteAgent, A2AServer, AgentRegistry, SkillDeclaration};
pub use crate::patterns::{chain, conditional, map_reduce, a2a_cascade, a2a_fanout, a2a_delegate};
```

---

## 8. Examples Alignment

### 8a. Existing examples — update naming

| Current Example | Changes Needed |
|---|---|
| `examples/agents/` | Update `AgentBuilder::new()` → `Agent::new()`, use `.instruct()` alias |
| `examples/text-chat/` | Update imports, use new fluent aliases |
| `examples/tool-calling/` | Update to use `T::` module composition |
| `examples/voice-chat/` | Minimal changes (Live builder) |
| `examples/transcription/` | Minimal changes |
| `apps/adk-web/` | Minimal changes |

### 8b. New examples to add (matching upstream)

| Upstream Example | New Example |
|---|---|
| `examples/operator_composition/` | `examples/operator-composition/` |
| `examples/context_engineering/` | `examples/context-engineering/` |
| `examples/state_transforms/` | `examples/state-transforms/` |
| `examples/g_module_guards/` | `examples/guards/` |
| `examples/a2a_remote_delegation/` | `examples/a2a/` |
| `examples/dispatch_join/` | `examples/dispatch-join/` |
| `examples/inline_testing/` | `examples/testing/` |
| `examples/middleware/` | `examples/middleware/` |
| `examples/map_over/` | `examples/map-over/` |
| `examples/loop_until/` | `examples/loop-patterns/` |

---

## 9. Implementation Order (Priority)

1. **Phase 1 — Core naming** (high impact, low risk)
   - Add `Agent` type alias
   - Add `.instruct()`, `.describe()` aliases on AgentBuilder
   - Add `Pipeline`, `FanOut`, `Loop` builders
   - Update prelude

2. **Phase 2 — Composition namespaces** (high impact)
   - Add `E` (eval) module
   - Add `G` (guards) module
   - Extend `C`, `P`, `S`, `M`, `T` with missing methods

3. **Phase 3 — A2A and patterns** (medium impact)
   - Add `RemoteAgent`, `A2AServer` builders
   - Add `chain()`, `conditional()`, `map_reduce()`, `a2a_*` patterns

4. **Phase 4 — Builder enhancements** (medium impact)
   - Add `.tool()`, `.tools()`, `.guard()`, `.context()`, `.planner()`, `.code_executor()`
   - Add `.ask()`, `.eval()`, `.eval_suite()` convenience methods

5. **Phase 5 — Testing and runtime** (lower priority)
   - Add test harness, mock backend, diagnosis
   - Add runner abstractions

6. **Phase 6 — Examples** (final)
   - Update existing examples
   - Add new examples
