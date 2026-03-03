# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Debt collection cookbook: FDCPA-compliant agent with compliance gates, emotional monitoring, and payment negotiation
- Cookbook L2 migration: all UI apps migrated to fluent `adk-rs-fluent` API
- Guardrails cookbook: policy monitoring with corrective injection for live conversations
- Support assistant cookbook: multi-agent handoff with billing and technical support flows
- Playbook cookbook: state machine with text agent evaluation for customer support
- All Config cookbook: configuration playground showcasing every Gemini Live option
- Documentation infrastructure: `#![warn(missing_docs)]`, per-crate READMEs, GitHub Pages deployment
- GitHub Actions CI for documentation checks on PRs
- Comprehensive doc comments on all public API items across all three crates

### Changed
- Migrated all cookbooks from L1 runtime API to L2 fluent API with `RegexExtractor` and `PhaseMachine`
- Cleaned up dead code after L2 migration

## [0.1.0] - 2026-03-03

### Added
- Initial release of three-crate workspace
- **rs-genai** (L0): Wire protocol, WebSocket transport, `Codec`/`Transport`/`AuthProvider` traits, `SessionWriter`/`SessionReader`, structured errors, `Role` enum, `Content`/`Part` builders
- **rs-adk** (L1): Agent runtime with three-lane processor (fast/control/telemetry), `State` with prefix scoping (`session:`, `derived:`, `turn:`, `app:`, `user:`), `PhaseMachine` for conversation flow control, `ToolDispatcher` with `SimpleTool`/`TypedTool`, `ComputedRegistry` for derived state, `WatcherRegistry` for state change watchers, `TemporalRegistry` for temporal pattern detection, `SessionSignals` with atomic counters, `SessionTelemetry`, `BackgroundToolTracker`
- **adk-rs-fluent** (L2): Fluent builder API, S-C-T-P-M-A operator algebra for agent composition, `Middleware` trait and `MiddlewareChain`, pre-built patterns and contract validation
- Cookbook UI framework: multi-app Axum WebSocket tester with devtools panel
- Standalone cookbooks: `text-chat`, `voice-chat`, `tool-calling`, `transcription`
- Agents cookbook: `weather-agent` and `research-pipeline` examples
- Support for both Google AI (API key) and Vertex AI (OAuth token) authentication
- Voice Activity Detection (VAD) with configurable settings
- Audio buffer management for bidirectional streaming
- `ConnectBuilder` for ergonomic session construction with generic `Transport` and `Codec`
