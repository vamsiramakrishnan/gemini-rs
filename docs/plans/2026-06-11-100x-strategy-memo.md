# The 100x Strategy Memo — gemini-rs

> Synthesized 2026-06-11 from four code audits (realtime hot path, concurrency
> correctness, API/DX, determinism/replay) and seven sourced research streams
> (Rust voice ecosystem, LiveKit/Pipecat, OpenAI/Google ADK/Vapi/Retell/vocode,
> Rasa CALM/Parlant, NeMo Colang/enterprise platforms, workflow-enforcement +
> simulation-testing academia/industry, consolidated field memo). Sources and
> full reports live in the session that produced this; load-bearing claims are
> dated June 2026 and falsifiable.

## The verdict

**gemini-rs holds a genuinely unshipped combination wrapped in the field's
weakest distribution position.**

The combination — across ~15 systems surveyed, nobody else has all four:

| Capability | Who else has it (June 2026) |
|---|---|
| Enforced (not advisory) conversation governance | Rasa CALM (proprietary, ~$35k/yr, cascaded ASR→TTS) and NVIDIA Colang (interpreted DSL, no compile step, no explain, avatar-niche) |
| Native speech-to-speech realtime substrate | LiveKit / Pipecat / Vapi / OpenAI — all ungoverned (imperative code or prompt-embedded state machines) |
| **Model-free deterministic conversation simulation** | **Nobody.** Every commercial tester (Vapi, Retell, Cekura, Hamming, Coval, LangSmith, Azure Foundry) uses LLM-simulated users. Sierra's own τ²-bench: 90% pass@1 decays to ~57% pass^8. Vapi's concession: retry each test 5×. |
| Open source + single-binary Rust runtime | Nobody (LiveKit/Pipecat/Ultravox have zero Rust agent support; adk-rust has breadth but no governance) |

The unique closed loop: **the artifact you test is the artifact that runs.**
Each third exists somewhere (CALM = the spec, NeMo/Pipecat ≈ enforcement,
Dialogflow CX ≈ CI test cases); nobody has the loop.

The distribution position: one provider, one transport, one language,
3 GitHub stars — against that provider's own free SDK.

## Why the thesis is right (external evidence)

1. **The graveyard pattern.** Every *design-time* deterministic authoring tool
   died as LLMs arrived: Bot Framework Composer archived, LUIS/QnA Maker
   retired, Vapi Workflows "no longer recommended for new builds," Pipecat
   static flows deprecated, vocode abandoned. What survived (Rasa CALM, NVIDIA
   Colang, Dialogflow CX's flow half) is exactly one architecture:
   **deterministic enforcement wrapped around an LLM at runtime.** gemini-rs is
   on the surviving side of that line, with two additions none of the survivors
   have (model-free sim; speech-to-speech-native substrate).
2. **Academia converged on the architecture in 2025-26:** AgentSpec (ICSE 2026,
   runtime action interception), IBM ToolGuard (EMNLP 2025, "compile policy
   docs → deterministic pre-tool guards"), PCAS (Feb 2026, literally "Policy
   Compiler for Agentic Systems"). Boruna is commercializing "workflows
   compiled to VM bytecode + hash-chained evidence" for regulated industries.
   The ideas are public; the advantage is the working voice-native
   implementation. **The clock is running.**
3. **The Live API judo.** The prevailing hard-gating mechanism elsewhere
   (per-node tool re-registration — Pipecat Flows, Retell, Vapi) *cannot work*
   on Live sessions: tool declarations are fixed at connect time. The
   gemini-rs approach — declare once, gate admission at runtime through the
   flow monitor — is the only enforcement architecture native to
   speech-to-speech sessions.
4. **Pricing/distribution validation:** Rasa proves enterprises pay ~$35k/yr
   for enforced governance. Parlant proves an open-source governance pitch can
   do 2k→18k stars in a year — with *advisory* enforcement.

## The window

This is a **design moat, not a data moat**: estimate **2-3 quarters** before
Pipecat Flows v2 or LiveKit Workflows ships the 80% version with 100x the
audience. Pipecat Flows is two-thirds of the way there architecturally.
Watch-items: Gradium/gradbot (Kyutai spinout, $70M seed, Rust, coming from the
model side), Boruna (compliance side), adk-rust (took the Rust multi-provider
breadth lane 2026-06-07, no governance story).

## The sequencing

**Doing the endgames before the funnel hardens the walls around an empty room.**

### Phase 0 — The floor (days)
- The five concurrency bugs from the audit (see Milestone 6 in ROADMAP).
- `#[non_exhaustive]` on `SessionEvent`/`LiveEvent`/`GeminiModel`/`Voice` —
  every new Gemini model/event is a semver break until this lands.
- Hot-path elegance: kill the double-parse (string-contains scan + full serde
  per message), fix the 64-deep control channel that can stall audio, wire the
  orphaned `TokenBucket`.

### Phase 1 — The determinism spine (the keystone, ~6 weeks full)
`RecordingCodec` (~80 LOC on the existing `Codec` trait) + durable
`JournalSink` (the journal is capped at 1024 mutations today — a 2-hour call
loses 98% of history) + injectable clock + recorded LLM/resolver outputs →
**any production session replays deterministically through the real control
plane** (verified: Sim already shares real `FlowStack`/extractor code).
Surface: `adk record` / `adk replay session.log`. Every incident becomes a
regression test; every recorded session becomes an eval.

### Phase 2 — The funnel
- **Python bindings** (PyO3) over the Rust core — the Pydantic/Polars/
  tokenizers play. The entire voice-AI population is in Python; bindings are
  the adoption funnel, not TAM dilution.
- **DECISION (2026-06-11): the OpenAI Realtime L0 is deliberately NOT
  pursued.** We stay Gemini-native — the identity is "the deepest, most
  rigorous Gemini Live runtime" rather than a thinner multi-provider layer.
  The control plane remains provider-agnostic by construction, so this option
  stays open at ~4-6 eng-months if the calculus changes; the research backing
  it is preserved above for that day.

### Phase 3 — Conversation CI (the most evidenced bet, ~2-3 eng-months)
Package `adk flow simulate` + scenario corpus + `why_blocked()` diffs as a
GitHub-Action conformance suite. Pitch writes itself: their tests pass 57% of
the time when run eight times (τ²-bench pass^8); ours pass 100% in
milliseconds, free. Attacks a funded category (Coval/Cekura/Hamming scored
43-49/100 on evaluation accuracy in independent testing).

### Phase 4 — The Rust-only endgames (no incumbent can follow)
- **Single-binary telephony**: integrate/embed `rustpbx` (642★, full SIP +
  RTP proxy + voice-AI hooks, 800 concurrent calls at 6ms/280MB benchmark)
  rather than building SIP from scratch — the governed agent brain *in* the
  media path.
- **WASM edge governance**: the compiler+Sim are model-free and deterministic
  — compile to WASM for in-browser authoring/validation and Workers/on-device
  deployment. Python frameworks structurally cannot follow.
- On-device turn-detection: Pipecat's smart-turn-v3 is BSD-2, 8M params,
  12ms CPU; Kyutai STT has semantic VAD; sherpa-onnx has official Rust
  bindings. Integration, not research.

### Throughout — proof artifacts (distribution attacks)
Published reproducible p99 mic-to-model jitter benchmark vs LiveKit/Pipecat
(hot-path audit verdict: already near-optimal, 4 small fixes from "fastest in
any language"); the time-travel debugger UI over journal × wire-log in the
existing web devtools; a strict canned-response mode (Parlant's one hard
guarantee, genuinely enforceable here per-phase) for the zero-hallucination
compliance pitch.

## One-line positioning

**Rasa CALM's enforcement guarantees, on a native speech-to-speech substrate,
with the deterministic testing story nobody has — open source, in Rust.**
