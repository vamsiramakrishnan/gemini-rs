# Sweep markers: Rust workspace inventory

Read-only sweep of `*.rs` files under `crates/`, `apps/`, `tools/`, `examples/` and `evals/`. 631 files scanned.

Excluded per the brief: `node_modules`, `target`, `apps/studio/dist`, `apps/gemini-adk-web-rs/static`, `tools/gemini-adk-cli-rs/assets`, `*.min.js`, `crates/gemini-adk-rs/src/agents/generated.rs`. The file list comes from `rg --files -t rust`, so `.gitignore` is honoured.

Tags in the lines: `[comment]` is a line comment or doc comment; `[string literal]` is text inside a string, often generated-code templates. Untagged hits are code.

## 1. `todo!(`, `unimplemented!(`, `panic!("not implemented`

Total: 8

- crates/gemini-adk-fluent-rs/src/live/mod.rs:418 — `[comment]     /// # fn my_tools(_: State) -> gemini_adk_fluent_rs::compose::tools::ToolComposite { t`
- crates/gemini-adk-fluent-rs/src/spec/codegen.rs:511 — `[string literal]         Guard::Custom(_) => "Guard::custom(|_| todo!(\"custom guard\"))".to_string(`
- crates/gemini-genai-rs/src/transport/auth/mod.rs:109 — `[comment] /// 'unimplemented!()' default with a compile-time guarantee.`
- tools/gemini-adk-transpiler-rs/src/codegen/adk.rs:396 — `[string literal]         out.push_str(&format!("        todo!(\"{struct_name}::run_live\")\n"));`
- tools/gemini-adk-transpiler-rs/src/codegen/adk.rs:946 — `[string literal]             out.push_str(&format!("        todo!(\"{struct_name}::run_live\")\n"));`
- tools/gemini-adk-transpiler-rs/src/codegen/adk.rs:1251 — `[string literal]             code.contains("todo!(\"SequentialAgent::run_live\")"),`
- tools/gemini-adk-transpiler-rs/src/codegen/adk.rs:1355 — `[string literal]         assert!(code.contains("todo!(\"ParallelAgent::run_live\")"));`
- tools/gemini-adk-transpiler-rs/src/codegen/adk.rs:1356 — `[string literal]         assert!(code.contains("todo!(\"LoopAgent::run_live\")"));`

## 2. `TODO`, `FIXME`, `XXX`, `HACK` (word match)

Total: 8

- apps/gemini-adk-web-rs/src/apps/guardrails.rs:39 — `[comment]     // PII: SSN pattern (XXX-XX-XXXX or XXX.XX.XXXX or XXXXXXXXX).`
- tools/gemini-adk-transpiler-rs/src/codegen/adk.rs:315 — `[string literal]         out.push_str("    // TODO: Implement Rust callback type\n");`
- tools/gemini-adk-transpiler-rs/src/codegen/adk.rs:1331 — `[string literal]             code.contains("// TODO: Implement Rust callback type"),`
- tools/gemini-adk-transpiler-rs/src/codegen/adk.rs:1332 — `[string literal]             "Should contain TODO comment"`
- tools/gemini-adk-transpiler-rs/src/codegen/adk.rs:1632 — `[string literal]             !code.contains("// TODO: Implement Rust callback type"),`
- tools/gemini-adk-transpiler-rs/src/codegen/adk.rs:1633 — `[string literal]             "Should NOT contain TODO for callbacks"`
- tools/gemini-adk-transpiler-rs/src/codegen/rest.rs:274 — `[string literal]         "    // TODO: {} requires manual implementation (special upload/download lo`
- tools/gemini-adk-transpiler-rs/src/codegen/rest.rs:410 — `[string literal]         assert!(code.contains("TODO: upload_file requires manual implementation"));`

## 3. `#[ignore]` attributes on tests

Total: 9

9 of these carry a reason string. Comment or doc mentions of `#[ignore]` (not attributes, not listed): 3.

- crates/gemini-adk-fluent-rs/tests/debt_collection_eval.rs:103 — `#[ignore = "live: costs money, needs GEMINI_API_KEY, takes minutes"]`
- crates/gemini-adk-fluent-rs/tests/decision_eval.rs:330 — `#[ignore = "calls Vercel AI Gateway; needs AI_GATEWAY_API_KEY and Jev access"]`
- crates/gemini-adk-fluent-rs/tests/decision_latency.rs:150 — `#[ignore = "calls Vercel AI Gateway; needs AI_GATEWAY_API_KEY and Jev access"]`
- crates/gemini-adk-fluent-rs/tests/extraction_latency.rs:179 — `#[ignore = "calls the Gemini API; needs GEMINI_API_KEY"]`
- crates/gemini-adk-fluent-rs/tests/skills_code_live.rs:248 — `#[ignore = "talks to the Live API; run with --ignored"]`
- crates/gemini-adk-fluent-rs/tests/skills_live.rs:225 — `#[ignore = "talks to the Live API; run with --ignored"]`
- crates/gemini-adk-fluent-rs/tests/spec_live_eval.rs:1474 — `#[ignore = "live: costs money, needs GEMINI_API_KEY, takes minutes"]`
- crates/gemini-memory-rs/tests/memory_paraphrase.rs:253 — `#[ignore = "needs an Embedder wired into the fixture: the acceptance test for a semantic layer, not `
- crates/gemini-memory-rs/tests/memory_retrieval.rs:304 — `#[ignore = "needs an Embedder in the fixture: see the note above — ranking alone cannot fix either h`

## 4. Feature-gated test modules and test files

Total: 44 (16 inner `#![cfg(feature` lines in files under tests/; 28 outer `#[cfg(...feature...)]` attached to a test module or test fn, or `cfg(all(test, feature`).

### 4a. Inner `#![cfg(feature` in files under tests/

Per-feature counts:

- gemini-llm: 16
- fluent: 2

- crates/gemini-memory-rs/tests/alias_terms_probe.rs:113 — `feature gemini-llm: #![cfg(feature = "gemini-llm")]`
- crates/gemini-memory-rs/tests/canonical_language_e2e.rs:18 — `feature gemini-llm: #![cfg(feature = "gemini-llm")]`
- crates/gemini-memory-rs/tests/code_switched_e2e.rs:15 — `feature gemini-llm: #![cfg(feature = "gemini-llm")]`
- crates/gemini-memory-rs/tests/contradiction_reconciliation.rs:57 — `feature gemini-llm: #![cfg(feature = "gemini-llm")]`
- crates/gemini-memory-rs/tests/filter_dsl_probe.rs:117 — `feature gemini-llm: #![cfg(feature = "gemini-llm")]`
- crates/gemini-memory-rs/tests/live_memory_e2e.rs:32 — `feature gemini-llm, fluent: #![cfg(all(feature = "gemini-llm", feature = "fluent"))]`
- crates/gemini-memory-rs/tests/live_session_e2e.rs:21 — `feature gemini-llm, fluent: #![cfg(all(feature = "gemini-llm", feature = "fluent"))]`
- crates/gemini-memory-rs/tests/memory_lifecycle_e2e.rs:11 — `feature gemini-llm: #![cfg(feature = "gemini-llm")]`
- crates/gemini-memory-rs/tests/memory_map_probe.rs:110 — `feature gemini-llm: #![cfg(feature = "gemini-llm")]`
- crates/gemini-memory-rs/tests/model_latency_probe.rs:12 — `feature gemini-llm: #![cfg(feature = "gemini-llm")]`
- crates/gemini-memory-rs/tests/quantization_probe.rs:152 — `feature gemini-llm: #![cfg(feature = "gemini-llm")]`
- crates/gemini-memory-rs/tests/query_rewrite_probe.rs:95 — `feature gemini-llm: #![cfg(feature = "gemini-llm")]`
- crates/gemini-memory-rs/tests/semantic_fusion_probe.rs:168 — `feature gemini-llm: #![cfg(feature = "gemini-llm")]`
- crates/gemini-memory-rs/tests/serving_latency_probe.rs:81 — `feature gemini-llm: #![cfg(feature = "gemini-llm")]`
- crates/gemini-memory-rs/tests/storage_encoding_probe.rs:102 — `feature gemini-llm: #![cfg(feature = "gemini-llm")]`
- crates/gemini-memory-rs/tests/voice_personas_e2e.rs:47 — `feature gemini-llm: #![cfg(feature = "gemini-llm")]`

### 4b. Outer `#[cfg(feature`, test items

Per-feature counts:

- code-mode: 7
- gemini-llm: 6
- vertex-ai-code-executor: 4
- templates: 2
- vad-wavekat: 2
- http-tools: 1
- gcs-store: 1
- dsp: 1
- database-sessions: 1
- otel-base: 1
- tls-native: 1
- tls-rustls: 1
- fluent: 1

- crates/gemini-adk-fluent-rs/src/spec/mod.rs:3688 — `feature http-tools (test attribute): #[cfg(feature = "http-tools")]`
- crates/gemini-adk-fluent-rs/src/spec/store.rs:972 — `feature gcs-store (test attribute): #[cfg(feature = "gcs-store")]`
- crates/gemini-adk-fluent-rs/src/voice/dsp/mod.rs:350 — `feature dsp (test attribute): #[cfg(feature = "dsp")]`
- crates/gemini-adk-rs/src/code_executors/vertex_ai.rs:602 — `feature vertex-ai-code-executor (test attribute): #[cfg(feature = "vertex-ai-code-executor")]`
- crates/gemini-adk-rs/src/code_executors/vertex_ai.rs:619 — `feature vertex-ai-code-executor (test attribute): #[cfg(feature = "vertex-ai-code-executor")]`
- crates/gemini-adk-rs/src/code_executors/vertex_ai.rs:634 — `feature vertex-ai-code-executor (test attribute): #[cfg(feature = "vertex-ai-code-executor")]`
- crates/gemini-adk-rs/src/code_executors/vertex_ai.rs:647 — `feature vertex-ai-code-executor (test attribute): #[cfg(not(feature = "vertex-ai-code-executor"))]`
- crates/gemini-adk-rs/src/instruction.rs:155 — `feature templates (test attribute): #[cfg(feature = "templates")]`
- crates/gemini-adk-rs/src/instruction.rs:168 — `feature templates (test attribute): #[cfg(feature = "templates")]`
- crates/gemini-adk-rs/src/live/control_plane/task_handler.rs:1003 — `feature code-mode (test attribute): #[cfg(feature = "code-mode")]`
- crates/gemini-adk-rs/src/live/control_plane/tool_handler.rs:2034 — `feature code-mode (test attribute): #[cfg(feature = "code-mode")]`
- crates/gemini-adk-rs/src/live/control_plane/tool_handler.rs:2074 — `feature code-mode (test attribute): #[cfg(feature = "code-mode")]`
- crates/gemini-adk-rs/src/live/control_plane/tool_handler.rs:2093 — `feature code-mode (test attribute): #[cfg(feature = "code-mode")]`
- crates/gemini-adk-rs/src/live/control_plane/tool_handler.rs:2115 — `feature code-mode (test attribute): #[cfg(feature = "code-mode")]`
- crates/gemini-adk-rs/src/live/control_plane/tool_handler.rs:2139 — `feature code-mode (test attribute): #[cfg(feature = "code-mode")]`
- crates/gemini-adk-rs/src/llm/gemini.rs:481 — `feature gemini-llm (test attribute): #[cfg(feature = "gemini-llm")]`
- crates/gemini-adk-rs/src/llm/gemini.rs:526 — `feature gemini-llm (test attribute): #[cfg(feature = "gemini-llm")]`
- crates/gemini-adk-rs/src/llm/gemini.rs:537 — `feature gemini-llm (test attribute): #[cfg(feature = "gemini-llm")]`
- crates/gemini-adk-rs/src/llm/gemini.rs:558 — `feature gemini-llm (test attribute): #[cfg(feature = "gemini-llm")]`
- crates/gemini-adk-rs/src/llm/gemini.rs:581 — `feature gemini-llm (test attribute): #[cfg(feature = "gemini-llm")]`
- crates/gemini-adk-rs/src/llm/gemini.rs:650 — `feature gemini-llm (test attribute): #[cfg(feature = "gemini-llm")]`
- crates/gemini-adk-rs/src/session/database.rs:507 — `feature database-sessions (cfg(test, feature)): #[cfg(all(test, feature = "database-sessions"))]`
- crates/gemini-adk-rs/src/tasks/runtime.rs:984 — `feature code-mode (cfg(test, feature)): #[cfg(all(test, feature = "code-mode"))]`
- crates/gemini-genai-rs/src/telemetry/mod.rs:462 — `feature otel-base (test attribute): #[cfg(feature = "otel-base")]`
- crates/gemini-genai-rs/src/transport/ws.rs:401 — `feature tls-native, tls-rustls (test attribute): #[cfg(not(any(feature = "tls-native", feature = "tl`
- crates/gemini-genai-rs/src/vad/mod.rs:441 — `feature vad-wavekat (test attribute): #[cfg(feature = "vad-wavekat")]`
- crates/gemini-genai-rs/src/vad/mod.rs:452 — `feature vad-wavekat (test attribute): #[cfg(feature = "vad-wavekat")]`
- crates/gemini-memory-rs/tests/governed_integration.rs:379 — `feature fluent (test attribute): #[cfg(feature = "fluent")]`

## 5. `#[allow(dead_code`, `#[allow(unused`, `#[allow(clippy::` (crate-level `#![allow(` included)

Total: 116. With a `reason =` argument: 15. With an explanatory comment on the same or previous line (no reason argument): 23. Without either: 78 (listed below). Excluded: 5 mentions inside string literals (code generators such as the transpiler), 1 in comments or doc comments.

With reason or comment, per crate:

- crates/gemini-adk-rs: 13
- crates/gemini-adk-fluent-rs: 11
- crates/gemini-memory-rs: 4
- tools/gemini-adk-cli-rs: 3
- crates/gemini-genai-rs: 2
- tools/gemini-adk-transpiler-rs: 2
- apps/gemini-adk-server-rs: 1
- apps/gemini-adk-web-rs: 1
- evals/dspbench: 1

Without reason or comment:

- apps/gemini-adk-server-rs/src/ws.rs:360 — `#[allow(dead_code)]`
- apps/gemini-adk-web-rs/src/apps/clinic.rs:24 — `#[allow(dead_code)]`
- apps/gemini-adk-web-rs/src/apps/clinic.rs:26 — `#[allow(dead_code)]`
- apps/gemini-adk-web-rs/src/apps/clinic.rs:28 — `#[allow(dead_code)]`
- apps/gemini-adk-web-rs/src/apps/clinic.rs:30 — `#[allow(dead_code)]`
- apps/gemini-adk-web-rs/src/apps/clinic.rs:32 — `#[allow(dead_code)]`
- apps/gemini-adk-web-rs/src/apps/clinic.rs:34 — `#[allow(dead_code)]`
- apps/gemini-adk-web-rs/src/apps/clinic.rs:36 — `#[allow(dead_code)]`
- apps/gemini-adk-web-rs/src/apps/clinic.rs:38 — `#[allow(dead_code)]`
- apps/gemini-adk-web-rs/src/apps/clinic.rs:40 — `#[allow(dead_code)]`
- apps/gemini-adk-web-rs/src/apps/clinic.rs:42 — `#[allow(dead_code)]`
- apps/gemini-adk-web-rs/src/apps/clinic.rs:44 — `#[allow(dead_code)]`
- apps/gemini-adk-web-rs/src/apps/mod.rs:2 — `#[allow(unused)]`
- apps/gemini-adk-web-rs/src/apps/mod.rs:4 — `#[allow(unused)]`
- apps/gemini-adk-web-rs/src/apps/mod.rs:6 — `#[allow(unused)]`
- apps/gemini-adk-web-rs/src/apps/mod.rs:12 — `#[allow(unused)]`
- apps/gemini-adk-web-rs/src/bridge.rs:47 — `#[allow(dead_code)]`
- crates/gemini-adk-fluent-rs/src/compose/context.rs:18 — `#[allow(clippy::type_complexity)]`
- crates/gemini-adk-fluent-rs/src/compose/middleware.rs:356 — `#[allow(clippy::type_complexity)]`
- crates/gemini-adk-fluent-rs/src/compose/middleware.rs:375 — `#[allow(clippy::type_complexity)]`
- crates/gemini-adk-fluent-rs/src/compose/middleware.rs:393 — `#[allow(clippy::type_complexity)]`
- crates/gemini-adk-fluent-rs/src/compose/middleware.rs:415 — `#[allow(clippy::type_complexity)]`
- crates/gemini-adk-fluent-rs/src/compose/middleware.rs:463 — `#[allow(dead_code)]`
- crates/gemini-adk-fluent-rs/src/compose/middleware.rs:670 — `#[allow(clippy::type_complexity)]`
- crates/gemini-adk-fluent-rs/src/compose/middleware.rs:871 — `#[allow(clippy::type_complexity)]`
- crates/gemini-adk-fluent-rs/src/compose/middleware.rs:893 — `#[allow(clippy::type_complexity)]`
- crates/gemini-adk-fluent-rs/src/compose/middleware.rs:915 — `#[allow(clippy::type_complexity)]`
- crates/gemini-adk-fluent-rs/src/compose/middleware.rs:937 — `#[allow(clippy::type_complexity)]`
- crates/gemini-adk-fluent-rs/src/compose/tools.rs:513 — `#[allow(clippy::type_complexity)]`
- crates/gemini-adk-fluent-rs/tests/common/evaluate.rs:34 — `#![allow(dead_code)]`
- crates/gemini-adk-fluent-rs/tests/common/mod.rs:3 — `#![allow(dead_code)]`
- crates/gemini-adk-fluent-rs/tests/common/report.rs:14 — `#![allow(dead_code)]`
- crates/gemini-adk-fluent-rs/tests/common/scenario.rs:37 — `#![allow(dead_code)]`
- crates/gemini-adk-fluent-rs/tests/common/voice.rs:30 — `#![allow(dead_code)]`
- crates/gemini-adk-macros-rs/src/lib.rs:703 — `#[allow(dead_code)]`
- crates/gemini-adk-macros-rs/src/lib.rs:900 — `#[allow(dead_code)]`
- crates/gemini-adk-macros-rs/tests/schema_snapshots.rs:64 — `#[allow(dead_code)]`
- crates/gemini-adk-macros-rs/tests/schema_snapshots.rs:74 — `#[allow(dead_code)]`
- crates/gemini-adk-macros-rs/tests/schema_snapshots.rs:121 — `#[allow(dead_code)]`
- crates/gemini-adk-macros-rs/tests/schema_snapshots.rs:136 — `#[allow(dead_code)]`
- crates/gemini-adk-macros-rs/tests/ui/fail/extract_unknown_option.rs:7 — `#[allow(dead_code)]`
- crates/gemini-adk-macros-rs/tests/ui/fail/extract_unknown_recognizer.rs:6 — `#[allow(dead_code)]`
- crates/gemini-adk-macros-rs/tests/ui/fail/frame_bad_confirm.rs:6 — `#[allow(dead_code)]`
- crates/gemini-adk-macros-rs/tests/ui/fail/frame_unknown_slot_option.rs:6 — `#[allow(dead_code)]`
- crates/gemini-adk-macros-rs/tests/ui/fail/tool_self_receiver.rs:5 — `#[allow(dead_code)]`
- crates/gemini-adk-macros-rs/tests/ui/pass/extract_basic.rs:7 — `#[allow(dead_code)]`
- crates/gemini-adk-macros-rs/tests/ui/pass/frame_basic.rs:7 — `#[allow(dead_code)]`
- crates/gemini-adk-rs/src/flow/decided.rs:201 — `#[allow(dead_code)]`
- crates/gemini-adk-rs/src/flow/decided.rs:208 — `#[allow(dead_code)]`
- crates/gemini-adk-rs/src/flow/decided.rs:221 — `#[allow(dead_code)]`
- crates/gemini-adk-rs/src/flow/mod.rs:666 — `#[allow(dead_code)]`
- crates/gemini-adk-rs/src/text/fn_agent.rs:10 — `#[allow(clippy::type_complexity)]`
- crates/gemini-adk-rs/src/text/mod.rs:365 — `#[allow(dead_code)]`
- crates/gemini-adk-rs/src/tool/schema.rs:159 — `#[allow(dead_code)]`
- crates/gemini-adk-rs/src/tool/schema.rs:167 — `#[allow(dead_code)]`
- crates/gemini-adk-rs/src/tool/schema.rs:173 — `#[allow(dead_code)]`
- crates/gemini-adk-rs/src/tool/simple.rs:14 — `#[allow(clippy::type_complexity)]`
- crates/gemini-adk-rs/src/tool/typed.rs:44 — `#[allow(clippy::type_complexity)]`
- crates/gemini-adk-rs/src/tool/typed.rs:106 — `#[allow(dead_code)]`
- crates/gemini-adk-rs/src/tool/typed.rs:113 — `#[allow(dead_code)]`
- crates/gemini-genai-rs/src/telemetry/mod.rs:346 — `#[allow(dead_code)]`
- crates/gemini-memory-rs/tests/common/corpus.rs:38 — `#![allow(dead_code)]`
- crates/gemini-memory-rs/tests/common/mod.rs:9 — `#![allow(dead_code)]`
- crates/gemini-memory-rs/tests/common/paraphrase.rs:26 — `#![allow(dead_code)]`
- crates/gemini-memory-rs/tests/common/rank.rs:12 — `#![allow(dead_code)]`
- crates/gemini-memory-rs/tests/common/views.rs:11 — `#![allow(dead_code, unused_imports)]`
- crates/gemini-memory-rs/tests/common/voice.rs:30 — `#![allow(dead_code)]`
- crates/gemini-memory-rs/tests/quantization_probe.rs:1037 — `#[allow(clippy::too_many_arguments)]`
- evals/code-mode/src/voice.rs:4 — `#![allow(dead_code)]`
- evals/dspbench/examples/aec_probe.rs:3 — `#[allow(dead_code)]`
- evals/dspbench/examples/real_eval.rs:19 — `#[allow(dead_code)]`
- examples/cookbook/src/30_production_pipeline.rs:17 — `#[allow(unused_imports)]`
- examples/cookbook/src/30_production_pipeline.rs:20 — `#[allow(unused_imports)]`
- tools/gemini-adk-cli-rs/src/commands/eval.rs:11 — `#[allow(dead_code)]`
- tools/gemini-adk-cli-rs/src/commands/eval.rs:23 — `#[allow(dead_code)]`
- tools/gemini-adk-cli-rs/src/commands/web.rs:42 — `#[allow(dead_code)]`
- tools/gemini-adk-cli-rs/src/manifest.rs:6 — `#[allow(dead_code)]`
- tools/gemini-adk-transpiler-rs/src/schema/mod.rs:9 — `#[allow(unused_imports)]`

