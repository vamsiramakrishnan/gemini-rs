# Skills and task activations

Use one `Live` session as the speaking assistant. Install reusable skills for billing, diagnostics, FAQ retrieval, or human handoff. Each activation gets its own identity, input revision, accepted state, operations, and optional governed conversation. Starting another capability suspends the previous foreground task. Completing a child task resumes its parent.

The Studio gallery includes support, call screening, clinic intake, collections, telecom support, returns, and restaurant assistants. Each combines several independently usable skills. Their business tools return controlled demo responses. Replace those bindings before using them for actual customer operations. The original single-workflow examples remain available under **Original workflow references**.

## Try it in Studio

Open `/studio`, choose **Support assistant** from the file menu, and open
**Tasks** to exercise the runtime without a provider. Start billing with
`{"account_id":"DEMO-42"}`, run account verification, then start an FAQ task
with **Return to the current task afterward** selected. Completing the FAQ
returns to billing with verification preserved. Calling `submit_adjustment`
creates a proposal; **Approve** executes it and **Deny** leaves it unexecuted.

Use **Tests** to run the included scenarios. Use **Run** for a configured Live
provider session. Enable audio, start the session, and select **Use microphone**
when ready to speak, or type a message. The microphone permission request occurs
only after that click. HTTPS or localhost is required for browser microphone
access. Switching tabs preserves the session; **Stop** ends it and releases
audio capture.

## Author a capability

A `SkillSpec` contains a name and version, a short description, activation instructions, typed inputs and outputs, private state defaults, and tools. Attach `flow` or `conversation` when the task needs governed stages. A capability that only retrieves an FAQ answer can use tools without a workflow.

```json
{
  "name": "faq",
  "version": "1.0.0",
  "description": "Answer product questions",
  "instruction": "Retrieve the relevant help article and cite it.",
  "inputs": {"question": {"type": "string"}},
  "outputs": {"result": {"type": "object"}},
  "tools": [{
    "name": "search_faq",
    "description": "Retrieve a help article",
    "effect": {"kind": "read"},
    "response": {"answer": "Adjustments appear on the next invoice."},
    "save_response_as": "result"
  }]
}
```

Input fields without defaults are required. Unknown input fields and mismatched JSON types fail before activation. Outputs project only the declared fields from accepted task state. Missing required outputs prevent completion. Object and array contracts check their JSON kind; use typed tools to validate their internal structure.

Every tool declares its effect. `read` permits operations that do not commit an external mutation. `commit` requires `idempotency_argument`, naming the string argument the external adapter uses as the complete operation key. Commit admission creates an immutable proposal. The trusted application sends a decision about that operation; the model cannot approve its own proposal.

Compilation adds that required string argument to the commit tool's provider declaration and invocation binding. An authored schema declaring it with another type fails validation. The runtime rejects an empty operation key.

```json
"effect": {"kind": "commit", "idempotency_argument": "request_id"}
```

## Install in a voice session

Start with the fluent prelude and import task authoring from its named module.

```rust,no_run
use gemini_adk_fluent_rs::prelude::*;
use gemini_adk_fluent_rs::spec::{SkillSpec, SpecResources};

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let skill: SkillSpec = serde_json::from_str(include_str!("faq.json"))?;
let handle = Live::builder()
    .instruction("Help the customer with the available capabilities.")
    .voice(Voice::Kore)
    .skills([skill.compile(&SpecResources::default())?])
    .connect_from_env()
    .await?;
# let _ = handle; Ok(()) }
```

For a complete JSON application, put definitions in `SessionSpec.skills` and call `SessionSpec::apply`. Root instruction, greeting, voice, and audio tuning describe the speaking session. Skill definitions own task state, tools, and governance. Root state defaults and separate root tool, extraction, phase, or flow pipelines are rejected. Share context explicitly through activation inputs or contextual tools; root state is not transferred implicitly into tasks.

Custom implementations bind through `SpecResources::implement_skill`. Use contextual tools and read or write `ToolContext.state`; it is the private invocation snapshot. The runtime forwards task ownership and cancellation in that context. Do not capture another session's mutable `State` inside an implementation. Each invocation rebuilds declared tools and commit guards against its private snapshot.

Session-level tool middleware is unsupported in task mode and fails configuration. Put task-specific checks in the owned tool adapter and retain the runtime's admission and approval boundaries.

Task skills accept deterministic stages, guards, digressions, repair, explicit tool effects, and the service fields described below. Conversation frame extractors, resolver slots, verbatim transcript matching, and stage-specific voice timing still fail validation. Use skill `extract` declarations for collected speech fields, or the single-conversation path for those remaining conversation features.

Task tool execution uses task-owned asynchronous delivery. Skill tool `background` and `scheduling` overrides fail validation. Install the complete catalog in one `.skills(...)` or `.tasks(...)` call; repeated installation is a configuration error.

Skill redaction policies also fail validation until input steering, approval arguments, operation results, and task exports share a complete redaction path. Invocation-local state isolation alone is not a data-retention guarantee.

## Extraction, derived state, and memory

Skills use the same `extract`, `computed`, `watch`, `patterns`, and `memory` declarations as single-workflow specs. Every activation has separate transcript windows, computed registries, watcher state, and temporal detectors. Supply `SpecResources.extraction_llm` for extraction and `SpecResources.memory` for memory. Missing resources fail before connection. Studio and generated Rust projects detect resources inside skills automatically.

For example, the screening skill declares a sticky spam verdict:

```json
"extract": [{
  "name": "screening",
  "instruction": "Classify sales, robocall, or solicitation attempts.",
  "schema": {"type":"object","properties":{"is_spam":{"type":"boolean"}}},
  "promote": [{"field":"is_spam","policy":"true_only"}]
}]
```

Input belongs to the task active when its first fragment arrives. Starting a child never imports the parent's transcript. When the model routes an initially unowned utterance into a new task, the Live adapter explicitly assigns that input once. A UI task switch does not transfer earlier speech.

Extraction runs away from the Live control lane. Accepted results apply only to their original task and revision. A suspended task can accept its result without steering the foreground task. Cancellation or revision rejects late projections and extraction results. Tool execution and services that write state run serially within one task so a tool snapshot cannot erase a newer extraction. Other tasks continue while that work waits.

Computed values and watcher effects settle after accepted changes. Turn patterns count that task's turns; sustained patterns use active-task time and pause across suspension and executing work. New business facts revoke an awaiting approval before execution. Extraction cannot undo an already dispatched external action. Extraction errors appear in task status. Phase-triggered extraction and detached extractor completion callbacks remain unsupported in tasks.

Memory uses the existing `gemini-memory-rs` engine. Configure one `SessionMemoryBinding` for the authenticated subject and pass it through `SpecResources.memory`. `SessionSpec::apply` installs its teardown once. Applications installing compiled skills directly must also call `MemoryBinding::install_task_lifecycle` on their `Live` builder.

The authenticated subject's memory backend is shared, while each skill declares its own projected slots. Caller-supplied activation fields do not select the backend user or establish authorization. `recall_context` reads memory; `manage_memory` requires application approval and a `request_id`. Accepted turns and watcher `remember` effects use awaited, owned memory jobs. A later cancellation cannot roll back ingestion that has already started. Memory-tool receipts report pending durability until session teardown finishes the engine. Studio's engine is scoped to the current connection; durable cross-session memory requires the application's configured backend.

Memory slots preserve values supplied explicitly by the application or tools. Slots still owned by a previous memory projection can refresh or disappear when memory changes. Resuming a task refreshes its memory before admitting tools, approvals, or completion; stale memory-derived context stays out of the model's instructions while that refresh settles. Forgetting removes matching facts from current slots and recall, then from canonical storage at reconciliation. It does not erase transcripts, operation receipts, or append-only evidence and audit events. Memory effect deduplication belongs to the current binding and does not provide crash recovery for external business operations.

## Observe and test a journey

Live and offline drivers use the same L1 `TaskRuntime`. `TaskSessionSnapshot` reports the foreground activation, pinned skill versions, task progress, pending services, service errors, operation proposals, and operation outcomes. A task's `FlowSnapshot` reports its currently driving conversation layer. These observations are not durable restoration checkpoints.

While an invocation is pending, its task's flow progress is logically paused. The runtime queues observed turn boundaries and applies them after the operation is resolved; speech and other independent tasks can continue. This prevents a slow tool from consuming repair turns before its result is available.

Live approval waits for any current input turn to finish and its services to settle. Denial and cancellation remain available. A model completion request made during its owned input turn is deferred until service results are accepted, then checked against the current task revision and governance. Switching or revising the task discards that request.

Add `task_scenarios` to a session spec. Each scenario has a name and `steps`. A step can apply a trusted command, advance a turn, finish a deferred controlled invocation, or assert task observations. For example:

```json
{
  "name": "FAQ answer",
  "steps": [
    {"event":"command","command":{"action":"start","skill":"faq","input":{"question":"When does the adjustment appear?"}}},
    {"event":"command","command":{"action":"invoke","task":"task-1","tool":"search_faq","args":{}}},
    {"event":"command","command":{"action":"complete","task":"task-1","output":{}}},
    {"event":"expect","tasks":[{"task":"task-1","status":"completed"}]}
  ]
}
```

Call `SessionSpec::run_task_scenarios` for CI, or `trace_tasks` to inspect every event. Offline task replay executes declared mock responses and state effects. It does not invoke attached implementations, HTTP bindings, or MCP servers. Test actual service adapters separately.

Use an `observe` step to supply speech and controlled extraction results through the actual trigger, promotion, computed, watcher, and temporal pipeline:

```json
{"event":"observe","task":"task-1","revision":1,
 "user":"Actually I am calling to sell a subscription.",
 "extraction":{"screening":{"is_spam":true}}}
```

Missing or unused extraction fixtures fail the scenario. `defer: true` holds a service job; `finish_service` completes its reported `service-N` operation so tests can exercise cancellation or revision first. `advance_time` advances the replay clock in milliseconds. A `memory` step seeds declared slot projections in the controlled memory adapter. It is not evidence that ingestion or storage works. `trace_tasks_with_resources` can test a configured memory engine separately.

Studio's **Tasks** panel provides **Test a conversation turn** for these controlled observations. **Tests** runs each example's embedded journeys. Re-run the entire gallery, asset mirror, HTTP testing and export roundtrip with:

```bash
python3 scripts/verify-use-cases.py --adk /path/to/adk --base-url http://127.0.0.1:25127
```

`SessionSpec::run_tests` also runs embedded skill workflow tests, with names such as `billing/verify account`. HTTP testing, generated project tests, and the CLI use this same aggregation. Use task scenarios to test admission, approval, cancellation, and operation outcomes.

Admission, approval, cancellation, and retries are distinct behavior. Cancellation invalidates task work and suppresses stale state updates. It does not establish that an external commit was undone. An uncertain commit outcome requires trusted reconciliation before retrying that operation key.

A trusted successful reconciliation reconstructs the tool's declared `set_state` and `save_response_as` effects from the verified receipt without calling the external adapter again. Custom implementation-only state writes require an application-specific receipt adapter when constructing `CompiledSkill` directly.

## Layers and discovery

L2 owns `SkillSpec`, field contracts, resource binding, and lowering into runtime definitions. L1 owns task identity, revisions, state acceptance, admission, approvals, operation receipts, and task-local `FlowStack` instances. L0 continues carrying Gemini session traffic.

The installed runtime catalog is local executable data, not another discovery service. Existing skill-registry APIs continue to discover `AgentConfig` or A2A capabilities; they do not automatically load `SkillSpec` bundles. Store serialized definitions through the existing bundle workflow and explicitly resolve/compile them at installation.
