# Duplicate / convention sweep (read-only inventory)

Scope: gemini-genai-rs, gemini-adk-rs, gemini-adk-fluent-rs, gemini-memory-rs, gemini-adk-macros-rs, gemini-adk-server-rs, gemini-adk-web-rs, gemini-adk-api-rs, gemini-adk-cli-rs (src only; `agents/generated.rs` skipped). Line numbers are the definition line. "identical" = same body; "same intent" = same job, different body or signature.

## 1. Duplicate helper functions

### 1a. Identical bodies
- `now_iso8601`: crates/gemini-adk-rs/src/session/types.rs:115 == apps/gemini-adk-server-rs/src/types.rs:144 (both return `"{unix_secs}Z"`, which is not ISO 8601; a real RFC 3339 formatter exists at crates/gemini-adk-fluent-rs/src/spec/store.rs:693 `now_rfc3339`)
- `resolve_voice`: crates/gemini-adk-fluent-rs/src/spec/mod.rs:2932 == apps/gemini-adk-web-rs/src/apps/mod.rs:196
- `default_agent_type` ("llm"): crates/gemini-adk-rs/src/agent_config.rs:112 == apps/gemini-adk-server-rs/src/agents.rs:25
- `now_secs` (same crate): crates/gemini-adk-rs/src/artifacts/mod.rs:127 == crates/gemini-adk-rs/src/memory/mod.rs:83

### 1b. Same intent, different body
- Levenshtein distance: crates/gemini-adk-rs/src/evaluation/response_evaluator.rs:166 `levenshtein_distance` (full DP matrix) ~ crates/gemini-adk-fluent-rs/src/spec/mod.rs:2944 `levenshtein` (two-row)
- Strip markdown code fence before JSON parse: crates/gemini-adk-rs/src/live/extractor.rs:146 `strip_code_fences` ~ crates/gemini-adk-rs/src/text/run.rs:293 `strip_code_fence` ~ crates/gemini-memory-rs/src/llm.rs:370 `parse_json` (fence strip inlined)
- String truncate: crates/gemini-adk-rs/src/tape.rs:376 (char-safe, no marker) ~ crates/gemini-adk-rs/src/decision/gateway.rs:245 (char-safe, appends "…") ~ tools/gemini-adk-cli-rs/src/commands/web.rs:409 (byte slice `&s[..max-3]`, appends "..."; not char-boundary safe)
- Template `{key}` interpolation: crates/gemini-adk-rs/src/orchestration/mod.rs:105 `interpolate(template, state)` ~ crates/gemini-adk-fluent-rs/src/spec/mod.rs:2673 `interpolate(template, args, state)` (same scanner loop; fluent adds `args.`/`state.` prefixes)
- Extraction LLM construction from env: apps/gemini-adk-server-rs/src/runtime/mod.rs:469 `build_extraction_llm` (async, Result<_, String>, handles Vertex token) ~ apps/gemini-adk-web-rs/src/apps/mod.rs:167 `build_extraction_llm` (sync, reads `GEMINI_EXTRACTION_LOCATION`)
- Telemetry subscriber init: apps/gemini-adk-server-rs/src/bin/adk-runtime.rs:35 `init_telemetry` ~ apps/gemini-adk-web-rs/src/main.rs:87 `init_telemetry` (same TelemetryConfig::from_env / EnvFilter / OTLP+GCP feature blocks; web adds WebSocketSpanLayer)
- Demo web-server route handlers, same five handlers twice: apps/gemini-adk-web-rs/src/main.rs:171 `favicon`, :178 `landing_page`, :182 `app_page`, :190 `list_apps`, :194 `ws_upgrade` ~ tools/gemini-adk-cli-rs/src/commands/web.rs:361 `favicon`, :343 `landing_page`, :350 `app_page`, :357 `list_apps`, :393 `ws_upgrade` (differ in asset source: include_str vs rust-embed `Assets`; favicon emoji differs; cli `app_page` does not check the registry)
- Environment credential / endpoint resolution (variable chains differ):
  - crates/gemini-genai-rs/src/protocol/types/config.rs:802 `ApiEndpoint::from_env` (+ `non_empty_env` :831; key chain GEMINI_API_KEY, GOOGLE_GENAI_API_KEY, GOOGLE_API_KEY; vertex truthy = "true"|"1")
  - crates/gemini-adk-rs/src/llm/gemini.rs:148-166 (chain GOOGLE_GENAI_API_KEY, GEMINI_API_KEY, GOOGLE_API_KEY) and :242 `GeminiLlm::from_env`
  - crates/gemini-adk-rs/src/decision/gateway.rs:82 `from_env`
  - crates/gemini-adk-rs/src/utils/variant.rs:18 `get_google_llm_variant` / :29 `classify` (vertex truthy = "true"|"1")
  - crates/gemini-adk-fluent-rs/src/live/connect.rs:485 `resolve_endpoint_from_env` (re-reads GOOGLE_CLOUD_PROJECT/LOCATION after `ApiEndpoint::from_env`)
  - apps/gemini-adk-web-rs/src/apps/mod.rs:112 `build_session_config` (vertex truthy = "true" only; key chain GOOGLE_GENAI_API_KEY, GEMINI_API_KEY, no GOOGLE_API_KEY; location also reads GEMINI_LIVE_MODEL_LOCATION)
  - apps/gemini-adk-server-rs/src/runtime/mod.rs:479-481 (inline `env` closure, vertex truthy = "true" only)
  - tools/gemini-adk-cli-rs/src/commands/web.rs:135 (key chain GOOGLE_GENAI_API_KEY, GEMINI_API_KEY, GOOGLE_API_KEY) and tools/gemini-adk-cli-rs/src/commands/doctor.rs:38-43 (project/location)
- PCM16 little-endian bytes <-> i16: crates/gemini-genai-rs/src/buffer/convert.rs:10 `i16_to_bytes` and :17 `bytes_to_i16` (bytemuck) ~ inline copies at crates/gemini-adk-rs/src/live/input_vad.rs:77-82 and crates/gemini-adk-rs/src/live/handle.rs:242-249 (`as_chunks::<2>` + `from_le_bytes` / `flat_map(to_le_bytes)`); test-only copy at crates/gemini-adk-rs/src/live/input_vad.rs:158 `bytes`
- i16 <-> f32 (/32768, clamp) inline in one crate: crates/gemini-adk-fluent-rs/src/voice/dsp/mod.rs:135,140,279,322-325 and crates/gemini-adk-fluent-rs/src/voice/dsp/aec.rs:186 and crates/gemini-adk-fluent-rs/src/voice/denoise.rs:119 (no shared helper)
- Epoch time helpers (three spellings of "now"): crates/gemini-genai-rs/src/transport/recording.rs:133 `epoch_millis`; adk-rs `now_secs` (1a); fluent store.rs:693 `now_rfc3339`

### 1c. Test fixtures defined more than once (all inside `#[cfg(test)]` unless marked pub)
- `MockWriter` (SessionWriter no-op): crates/gemini-adk-rs/src/test_helpers.rs:8 (shared, pub(crate)) vs private copies at crates/gemini-adk-rs/src/live/effect_executor.rs:151 (records writes), crates/gemini-adk-rs/src/live/temporal.rs:404, crates/gemini-adk-rs/src/llm_agent.rs:553; `NoopWriter` at crates/gemini-adk-rs/src/live/control_plane/tool_handler.rs:814
- Canned-response `BaseLlm` mocks: crates/gemini-adk-rs/src/llm/mock.rs:64 `MockLlm` (the pub scripted one) vs private `MockLlm` at crates/gemini-adk-rs/src/live/extractor.rs:720, crates/gemini-adk-rs/src/llm/registry.rs:71, crates/gemini-adk-fluent-rs/src/builder.rs:952; `MockLlmA`/`MockLlmB` crates/gemini-adk-rs/src/text/llm.rs:816,827; `FakeLlm` crates/gemini-adk-fluent-rs/src/live/mod.rs:689; `DummyLlm` crates/gemini-adk-rs/src/evaluation/llm_as_judge.rs:202; `Canned` crates/gemini-memory-rs/src/llm.rs:441
- `EchoLlm`: crates/gemini-adk-rs/src/orchestration/mod.rs:346 vs crates/gemini-adk-fluent-rs/src/builder.rs:1161
- `NoopJudge`: crates/gemini-adk-fluent-rs/src/compose/eval.rs:449 vs crates/gemini-adk-fluent-rs/src/compose/guards.rs:461
- `NoopAgent`: crates/gemini-adk-rs/src/llm_agent.rs:538 vs crates/gemini-adk-rs/src/runner.rs:214 (identical shape)
- Not duplicated: `MockTransport` crates/gemini-genai-rs/src/transport/ws.rs:259 (single, pub); `ScriptedServer` crates/gemini-adk-fluent-rs/src/live/scripted.rs:56 (single); `MockDecisionModel` crates/gemini-adk-rs/src/decision/mod.rs:523 (single)

### 1d. Same name, NOT a duplicate (collisions)
`main` (4 bins); `run` (CLI subcommands); `build`, `parse`, `check`, `plan`, `answer`, `catalog`, `replay` (fluent vs cli, different jobs); `classify` (adk-rs/utils/variant.rs:29 vs memory-rs/reconcile/consolidate.rs:72); `strip_comment` (adk-rs/code_mode/stubs.rs:317 python `#` vs memory-rs/okf/yaml.rs:333 quote-aware yaml); `parse_json` (adk-rs/artifacts/gcs_service.rs:360 HTTP response vs memory-rs/llm.rs:370); `str_array` (fluent codegen.rs:1113 vs macros lib.rs:1035); `runtime` (adk-rs vs macros); `any_of`/`append`/`apply`; `list_bundles` (server runtime/mod.rs:544 vs web studio.rs:251, different stores/JSON); `handle_ws` (web ws_handler.rs:12 is a thin wrapper over server ws.rs:420); `default_model` differs in value: apps/gemini-adk-server-rs/src/agents.rs:193 = "gemini-2.0-flash" vs tools/gemini-adk-cli-rs/src/manifest.rs:42 = "gemini-flash-latest" (also tools/gemini-adk-cli-rs/src/main.rs:20 default "gemini-flash-latest").

No hits in scope for: mulaw/alaw (single impl crates/gemini-adk-fluent-rs/src/telephony/g711.rs), resample (single impl crates/gemini-adk-fluent-rs/src/voice/mod.rs:257), audio base64 helpers (only inline `STANDARD.encode/decode` calls, no named helper), retry/backoff (single impl crates/gemini-genai-rs/src/client/http.rs:322,385,390; separate retry policy types in adk-rs/middleware/retry.rs:46 and llm/mod.rs:372), websocket frame helpers, `redact` (adk-rs state.rs:515-568 and live/redaction.rs:98-143 only).

## 2. Duplicate public types

### 2a. Same concept modelled more than once
- Eval-set config: `TestConfig` crates/gemini-adk-rs/src/evaluation/test_config.rs:18 (`criteria: HashMap`) vs private `TestConfig` tools/gemini-adk-cli-rs/src/commands/eval.rs:28 (`pass_threshold` + `criteria: Vec<Criterion>`); CLI does not use the SDK type and the schemas differ
- Eval case: `EvalCase` crates/gemini-adk-rs/src/evaluation/eval_case.rs:35 vs crates/gemini-adk-fluent-rs/src/compose/eval.rs:195 vs private tools/gemini-adk-cli-rs/src/commands/eval.rs:17
- Code execution result: `CodeExecutionResult` crates/gemini-genai-rs/src/protocol/types/content.rs:85 (`outcome`/`output`, wire type) vs crates/gemini-adk-rs/src/code_executors/types.rs:27 (`stdout`/`stderr`/files)
- Agent definition file: `AgentConfig` crates/gemini-adk-rs/src/agent_config.rs:31 (model Option, nested sub_agents, ToolConfig) vs `AgentEntry` apps/gemini-adk-server-rs/src/agents.rs:9 + private `TomlManifest` agents.rs:179 vs `AgentManifest` tools/gemini-adk-cli-rs/src/manifest.rs:7 (agent.toml; adds temperature, thinking, greeting, voice, output_modality; model default differs)
- Partial: `AgentEvent` crates/gemini-adk-rs/src/context.rs:20 (enum) vs apps/gemini-adk-server-rs/src/types.rs:43 (struct `{event_type, data}`, serialized view of the same events)

### 2b. Same name, different concept (collision only)
- `SessionError`: crates/gemini-genai-rs/src/session/errors.rs:11 (Live connection) vs crates/gemini-adk-rs/src/session/mod.rs:35 (session store)
- `MemoryError`: crates/gemini-adk-rs/src/memory/mod.rs:47 (memory service) vs crates/gemini-memory-rs/src/core/error.rs:13 (memory engine)
- `SessionConfig`: crates/gemini-genai-rs/src/protocol/types/config.rs:918 (Live setup) vs crates/gemini-memory-rs/src/core/policy.rs:203 (logical-session timing)
- `ServerMessage`: crates/gemini-genai-rs/src/protocol/messages/server.rs:269 (Gemini wire) vs apps/gemini-adk-server-rs/src/ws.rs:61 (browser protocol)
- `RunRequest`: crates/gemini-adk-rs/src/text/run.rs:26 vs apps/gemini-adk-server-rs/src/types.rs:10 (HTTP body)
- `ToolConfig`: crates/gemini-genai-rs/src/protocol/types/tools.rs:163 (function-calling mode) vs crates/gemini-adk-rs/src/agent_config.rs:118 (agent tool decl)
- `Question`: crates/gemini-adk-rs/src/decision/mod.rs:82 vs crates/gemini-adk-fluent-rs/src/spec/authoring.rs:1296
- `Answer`: crates/gemini-adk-rs/src/decision/mod.rs:197 vs crates/gemini-adk-fluent-rs/src/spec/authoring.rs:1873
- `Decisions`: crates/gemini-adk-rs/src/decision/service.rs:249 (struct) vs crates/gemini-adk-fluent-rs/src/spec/authoring.rs:1265 (type alias BTreeMap)
- `Conversation`: crates/gemini-adk-rs/src/decision/service.rs:123 vs crates/gemini-adk-fluent-rs/src/conversation.rs:534
- `Field`: crates/gemini-adk-rs/src/extract/mod.rs:320 vs crates/gemini-memory-rs/src/bm25/schema.rs:20
- `Resolver`: crates/gemini-adk-rs/src/orchestration/mod.rs:138 vs crates/gemini-memory-rs/src/reconcile/resolver.rs:19

No cross-crate duplicates found for `AudioFormat`, `ToolCall`, `Phase`, `GuardSpec`.

## 3. Error conventions

### Per-crate definitions
- gemini-genai-rs: 19 error types, all `#[derive(thiserror::Error)]`: TuningsError tunings/mod.rs:112, EndpointEnvError protocol/types/config.rs:840, FilesError files/mod.rs:86, SessionError/WebSocketError/SetupError/AuthError session/errors.rs:11,68,89,114, ModelsError models/mod.rs:71, GenerateError generate/mod.rs:115, TungsteniteError transport/ws.rs:108, MockTransportError transport/ws.rs:304, WireLogError transport/recording.rs:348, CodecError transport/codec.rs:9, ReplayTransportError transport/replay.rs:40, EmbedError embed/mod.rs:93, CachesError caches/mod.rs:98, HttpError client/http.rs:39, TokensError tokens/mod.rs:25, BatchesError batches/mod.rs:117
- gemini-adk-rs: 20 thiserror types (AgentError error.rs:25, ConfigError error.rs:102, ToolError error.rs:134, LlmError llm/mod.rs:307, StateError state.rs:352, SessionError session/mod.rs:35, WorkflowError workflow.rs:70, AgentConfigError agent_config.rs:138, MemoryError memory/mod.rs:47, ArtifactError artifacts/mod.rs:79, McpError tools/mcp/session_manager.rs:470, PersistenceError live/persistence.rs:33, TaskError tasks/types.rs:333 (struct), CredentialError credentials.rs:24, CredentialExchangeError auth/exchanger.rs:14, CodeExecutorError code_executors/base.rs:6, EvalError evaluation/evaluator.rs:10, OptimizerError optimization/optimizer.rs:8, PlannerError planners/mod.rs:19, ProcessorError processors/mod.rs:13, SkillRegistryError skills.rs:142) plus one manual: `FlowError` flow/mod.rs:2137 (derive Serialize, manual `Display` :2164; `std::error::Error` only on `FlowErrors` :2207)
- gemini-adk-fluent-rs: 7 manual (hand-written `Display` + empty `impl std::error::Error`): ConversationError conversation.rs:324, StoreError spec/store.rs:90, PatchError spec/authoring.rs:106, AnswerError spec/authoring.rs:1919, TwilioError telephony/twilio.rs:168, SrtpError telephony/srtp.rs:181, SipError telephony/sip.rs:65; 1 thiserror: VoiceIoError voice/devices.rs:17 (thiserror is an optional dep gated on `voice-io`)
- gemini-memory-rs: MemoryError core/error.rs:13 (thiserror; manual `From<io::Error>` :94 and `From<serde_json::Error>` :100), MemorySlotError runtime/turn_extractor.rs:110 (thiserror), YamlError okf/yaml.rs:163 (manual Display)
- gemini-adk-macros-rs: no error types; proc-macro uses `syn::Result`
- apps/gemini-adk-server-rs: AppError ws.rs:351 (thiserror; re-used by the CLI), StreamTokenError runtime/auth.rs:129 (pub(crate) enum, no Display/Error impl); also stringly `Result<_, String>` in runtime/config.rs (3 fns) and runtime/mod.rs (2 fns, incl. build_extraction_llm :469)
- apps/gemini-adk-web-rs: no error types; uses `Result<_, String>` (apps/mod.rs `build_session_config`)
- apps/gemini-adk-api-rs: no error types (single main.rs)
- tools/gemini-adk-cli-rs: no error types of its own; all command entry points return `Result<_, Box<dyn std::error::Error>>`; `commands/web.rs` uses the server's `AppError`

### Conversion into AgentError
`AgentError` is defined in crates/gemini-adk-rs/src/error.rs:25 and only re-exported by the fluent layer (crates/gemini-adk-fluent-rs/src/lib.rs:219). There is no `impl From<...> for AgentError` anywhere in crates/gemini-adk-fluent-rs/src.
- Via `#[from]` in adk-rs error.rs: SessionError (adk-rs), ToolError, LlmError, StateError; manual `From<ConfigError>` error.rs:125
- No conversion at all (callers hand-format `AgentError::Config(String)` / `Other(String)`; e.g. crates/gemini-adk-fluent-rs/src/live/connect.rs:209,368,456,466,495,502,518,544,550,557,580 and compose/middleware.rs:386-959): genai errors (SessionError, EndpointEnvError, HttpError, ...), AgentConfigError, WorkflowError, FlowError/FlowErrors, McpError, ArtifactError, MemoryError (both), and all seven fluent-layer error types (ConversationError, StoreError, PatchError, AnswerError, TwilioError, SrtpError, SipError)
- ToolError has `ToolError::from_error(impl Into<Box<dyn Error+Send+Sync>>)` crates/gemini-adk-rs/src/error.rs:182, used by the `#[tool]` macro helper crates/gemini-adk-rs/src/lib.rs:203-208

### anyhow / Box<dyn Error> in public library signatures
- anyhow: no dependency in any workspace crate's Cargo.toml; only mentioned in docs/tests (crates/gemini-adk-rs/src/error.rs:170, crates/gemini-adk-macros-rs/src/lib.rs:124,1184)
- `Box<dyn std::error::Error>` in `pub fn` (library crates): crates/gemini-genai-rs/src/telemetry/mod.rs:150,180,229,282 (`install_metrics`, `build_otlp`, `init`, `build_gcp`); crates/gemini-adk-rs/src/telemetry/setup.rs:117 (`init`)
- Same in app/tool crates: apps/gemini-adk-server-rs/src/serve.rs:62 `run_server`; apps/gemini-adk-server-rs/src/runtime/mod.rs:518 `install_sdk_metrics`; tools/gemini-adk-cli-rs/src/commands/{deploy.rs:50, web.rs:273, doctor.rs:5, flow.rs:59,66,76,90,149,253, session.rs:281, api.rs:19, replay.rs:6} and manifest.rs:82 `load_manifest`
- Mixed Send+Sync: genai telemetry/mod.rs:229 `init` returns `Box<dyn Error>` (no Send+Sync) while :150,:180,:282 return `Box<dyn Error + Send + Sync>`; adk-rs telemetry/setup.rs:117 and server serve.rs:62 return the non-Send form
- `Result<_, String>` in `pub fn` (library crates, in scope): crates/gemini-adk-fluent-rs/src/compose/guards.rs (3), simulation.rs (3), spec/mod.rs (1), spec/skills.rs (1), tasks.rs (1); crates/gemini-adk-rs/src/{code_mode/pool.rs (2), code_mode/stubs.rs, decision/mod.rs, flow/mod.rs, flow/stack.rs, instruction.rs, orchestration/mod.rs, tools/bash_tool.rs} (1 each)

## 4. Naming consistency: SDK vs CLI vs Studio

Surfaces: SDK = crates/gemini-adk-fluent-rs/src/spec/mod.rs:837 `SessionSpec`, crates/gemini-adk-rs/src/flow/mod.rs:765 `Flow` (`steps`), crates/gemini-adk-fluent-rs/src/conversation.rs:305 `ConversationSpec` (`stages`, `overlays`); CLI = tools/gemini-adk-cli-rs/src/main.rs; Studio = apps/studio/src.

SessionSpec top-level fields (mod.rs:840-937): name, version, description, instruction, greeting, modality, voice, tools, mcp, extract, models, decisions, code, state, computed, memory, runtime, phases, initial_phase, watch, patterns, fragments, use_fragments, flow, conversation, skills, task_scenarios, scenarios, tests.

CLI subcommands: create, run, replay, web, api, eval, doctor, spec {codegen, test, call, run, schema, graph, catalog, check, plan, answer, patch}, bundle {push, list, get, label}, flow {inspect, graph, schema, validate, simulate, replay, ...}, session {replay, scenario}, deploy. Flags: --model, --api-key, --save-session, --session-id, --replay, --contract, --host, --port, --allow-origins, --log-level, --reload, --a2a, --trace-to-cloud, --session-service-uri, --artifact-storage-uri, --config-file, --print-detailed-results, --store, --project, --region, --service-name, --bundles, --serve, --image, --source, --tokens-secret, --twilio-secret, --service-account, --max-sessions, --min-instances, --dry-run, --lang, --out, --sdk-path, --force, --json, --decisions, --write, --name, -m/--message, --label, --journal.

Studio labels: tabs Problems, Tests, "Step through", Tasks, Run, Code, Versions (App.tsx:18-25); sections Agent, Skills, Conversation, Flow, Tools, State, Computed, Extraction, Memory, Phases, Watchers, Patterns, Runtime, "MCP servers", "Flow tests", Scenarios, "Task scenarios", Fragments (components/Inspector.tsx:24-41); node kinds step/stage/overlay (spec/graph.ts:12); FlowState labels Active, Missing, Blocked, Digression (panels/FlowState.tsx:13-26).

Mismatches (same concept, different name on two or more surfaces):
1. Whole document: SDK `SessionSpec` (JSON file, doc comment "A complete Live session as one JSON document"); CLI "spec" / "Path to the spec (agent.json)" (main.rs:229) but `flow` commands say "Path to a ConversationSpec JSON file" (main.rs:399); Studio calls it "agent" (File menu "New agent", "Open JSON…", section "Agent", components/Toolbar.tsx:152-154) and "bundle"/"Versions" for the stored form.
2. Stored versions: CLI `bundle` (push/list/get/label, `--store`) vs Studio tab "Versions" (App.tsx:25) and "Save a version" (Toolbar.tsx:175); SDK `bundle` (apps/gemini-adk-server-rs/src/runtime, spec/store.rs).
3. Flow node: SDK `Flow.steps` / `Step` (flow/mod.rs:767) vs `ConversationSpec.stages` / `StageSpec` (conversation.rs:310,218); Studio shows both ("Step"/"Stage", Inspector.tsx:200) and hints.ts:1 says "a step or stage"; CLI help text says "steps" (flow replay/explain) and "conversation spec".
4. Sub-flow: SDK `OverlaySpec` / `overlays` ("digression (overlay)", conversation.rs:282) vs Studio `FlowState` label "Digression" (panels/FlowState.tsx:15) and canvas badge "overlay" (components/Canvas.tsx:68).
5. `extract`: SDK field `extract` / `ExtractSpec` vs Studio section "Extraction" (Inspector.tsx:31).
6. `watch`: SDK field `watch` / `WatchSpec` vs Studio section "Watchers" (Inspector.tsx:34).
7. `mcp`: SDK field `mcp` vs Studio section "MCP servers" (Inspector.tsx:37).
8. `tests`: SDK field `tests` / `SpecTest` vs Studio section "Flow tests" (Inspector.tsx:38) vs Studio dock tab "Tests"; CLI `spec test` runs "tests and scenarios".
9. Validation: SDK `SpecValidation` / `validate`; CLI has three names for it: `spec check`, `flow validate`, `spec test` ("Validate the spec and run its tests"); Studio calls the output "Problems" (App.tsx:19).
10. Simulation: SDK `Scenario` / `simulate`; CLI `flow simulate` and `spec test`; Studio "Step through" (App.tsx:21) and "Test a conversation turn" (panels/Tasks.tsx:84).
11. Replay: CLI has four: top-level `replay` (wire + `--contract`), `run --replay`, `session replay` (wire log, `--journal`), `flow replay` (journal); Studio has none (Run panel only).
12. Duplicate verbs across `spec` and `flow` groups: `graph` (main.rs:266,402) and `schema` (main.rs:263,407) exist in both; Studio exposes neither under that name (Canvas, Code).
13. Cloud location: SDK/env `GOOGLE_CLOUD_LOCATION` / `location` (config.rs, llm/gemini.rs:165) vs CLI `deploy --region` (main.rs:173).
14. Agent directory: CLI `run`/`web`/`api` take `agent_dir` (agent.toml manifest, tools/gemini-adk-cli-rs/src/manifest.rs:7) while `spec ...`/`flow ...` take `spec` (agent.json); SDK has both `AgentConfig` (agent_config.rs:31) and `SessionSpec`; Studio only edits the `SessionSpec` form.
15. Eval config: CLI `eval --config-file` / struct `TestConfig` (commands/eval.rs:28) vs SDK `TestConfig` loaded from `test_config.json` (evaluation/test_config.rs:15); Studio "Tests" is a different feature (flow tests), so "test" names three things (eval config, SpecTest, Studio Tests tab).
16. Task scenarios: SDK `task_scenarios` / `TaskScenario` vs Studio section "Task scenarios" (matches) but Studio dock tab is "Tasks" and panel header "Task preview" (panels/Tasks.tsx:60), CLI has no matching command.
17. Skill inputs/outputs: Studio hints read `spec.inputs`/`spec.outputs` at the document top level (apps/studio/src/spec/hints.ts:24-25) but SDK defines them on `SkillSpec` (crates/gemini-adk-fluent-rs/src/spec/skills.rs:43,46), not on `SessionSpec`.
18. `decisions`: SDK `SessionSpec.decisions` (BTreeMap<String, DecisionSpec>, mod.rs:877) and authoring `Decisions` alias (authoring.rs:1265) vs CLI `--decisions` (a file of question id to choice) are different things sharing a name; Studio has no "Decisions" section and no plan/answer UI.
19. Confirm: SDK `confirm_tools` (Flow.confirm_tools flow/mod.rs:773) and `commit: CommitSpec` on a stage (conversation.rs:247) vs Studio chip title "confirm before" (components/Canvas.tsx:88) with CSS class `chip-commit`.
