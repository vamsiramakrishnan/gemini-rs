# Batch plan: gaps #4/#5/#9 + cookbook & docs overhaul

Status: in progress · 2026-06-07

Decisions (from the user):
- **Cookbooks**: *Consolidate* — collapse single-operator Crawl demos (01–10) into ~4
  combined examples; drop the tiny reference-card trio (31/32/33) whose content lives
  in docs. Net ~40 → ~27 substantive examples.
- **#9 prelude**: *Hard carve* — shrink `prelude` to a kernel, move the rest to
  sub-modules; every cookbook + doc snippet conforms. (Breaking; pre-1.0 so allowed.)
- **E2E**: build + `cargo test` + `cargo build -p example-cookbook` + `mdbook build docs`
  + a link check, **and** `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`.
  No live Gemini run (no API key/socket in worktrees).
- **Docs**: *Full overhaul* — expand thin chapters, add Glossary + Troubleshooting/FAQ
  + Advanced/RFC index, dedup README→book, archive stale root docs, fix nav, add
  link-check to Pages CI.

## Why this isn't a pure all-at-once fan-out

Hard-carve makes #9 the foundation: a breaking prelude touches every `prelude::*`
consumer. Shared manifests (`examples/cookbook/Cargo.toml` `[[bin]]`, `docs/src/SUMMARY.md`,
`examples/INDEX.md`, `docs/src/cookbooks.md`, README) are edited by many logical units.
So the work is two waves:

- **Wave 1 — foundation (sequential, on `claude/gemini-rs-roadmap-6NaoX`, commit+push,
  no PRs — matching this session's workflow).** Must keep the workspace green at each
  commit. Too coupled/high-blast-radius to hand to isolated workers.
- **Wave 2 — fan-out (parallel background workers in worktrees, off the post-Wave-1
  HEAD).** File-disjoint, lower-risk, benefits from parallelism.

## Wave 1 — foundation (coordinator, sequential)

### F1 — #9 prelude hard carve + workspace migration
New L2 surface (frozen spec all later units target):
- `prelude` = **kernel**: `Live`, `AgentBuilder` (+ `Agent` alias), `GeminiModel`, `Voice`;
  compose namespaces `S C T P M A E G` + `Ctx`; operators (`>> | * /`) + `until`;
  `State`, `StateKey`; `Flow`, `Guard`, `Enforcement`, `FlowMonitor`;
  `AgentError`/`AgentResult`/`ToolError`; tool kernel `SimpleTool`,`TypedTool`,
  `ToolFunction`,`ToolDispatcher`, `#[tool]`, `Extract`, `Frame`; `GeminiModel`,
  `Content`/`Part`/`Role` (the L0 message builders people actually use).
- Sub-modules (curated, no `*` dumping):
  - `live` — full control plane (callbacks, persistence, steering, repair, contracts,
    transcripts, soft-turn, extraction triggers).
  - `text` — text-agent combinators.
  - `conversation` — ConversationSpec/CompiledConversation/FlowStack/…
  - `agents` — agent traits/builders; **L1 `Agent` trait re-exported here as `AgentTrait`**
    (resolves the L1↔L2 `Agent` collision; documented).
  - `tools`, `state`, `flow`, `compose`, `wire` (L0) as focused re-export modules.
- L1 (`gemini-adk-rs`) root: trim duplicate/aspirational re-exports; keep a lean set,
  move the rest behind `pub mod` paths. L0 prelude: leave feature-gated API groups, drop
  redundancy.
- **Migrate the entire workspace** to the new surface so it compiles green: 3 crates'
  internal+tests, all examples, `apps/*`, `tools/*`. (This is the lynchpin commit-set.)

### F2 — cookbook consolidation (≈40 → ≈27)
- Collapse `01_simple_agent` … `10_guards` into ~4 combined "foundations" examples
  (builders+tools+callbacks; sequential+parallel+loop; S/P/T/C; guards+eval).
- Delete the reference-card trio `31_connect_from_env`, `32_live_callbacks`,
  `33_tool_macro` (content lives in docs).
- Renumber/rename remaining; update `examples/cookbook/Cargo.toml` `[[bin]]`,
  `examples/cookbook/README.md`, `examples/INDEX.md`, `docs/src/cookbooks.md`.
- Migrate every surviving cookbook to the F1 prelude.

### F3 — docs structure & meta
- `docs/src/SUMMARY.md` nav rework; create new chapters (Glossary,
  Troubleshooting/FAQ, Advanced/RFC index) with full content; `book.toml` touch-ups.
- README dedup → point to book with deep links; keep positioning + FAQ pointer.
- Archive stale root docs (`docs/exploration.md`, `docs/design-implementation-plan.md`,
  the giant `2026-03-03-*.md`) into `docs/archive/` with an index note.
- `.github/workflows/docs.yml`: add a Markdown link-check step; keep the rustdoc
  `-D warnings` gate.

## Wave 2 — fan-out (parallel background workers, off post-Wave-1 HEAD)

Each worker owns a **disjoint** file set; e2e = `mdbook build docs` + link-check +
`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` (+ `cargo build`/`test` for
code units). Workers push their worktree branch; the coordinator integrates (no PRs,
per session rule).

- **U-4 (#4)** — extract a staged `SessionPlan` / `build_runtime` / `spawn_lanes`
  pipeline from L1 `connect()` (`live/builder.rs`) + L2 `build_and_connect()`
  (`live/connect.rs`), behavior-preserving, harness-tested.
- **U-5 (#5)** — add a `Delivery` policy (`Lossless`/`LossyDropOldest`/`CoalesceByKey`/
  `LatestOnly`) to the processor router (`live/processor.rs`) with per-event-class config
  + `try_send` paths + tests; document it in `live-callbacks.md` (coordinated: U-5 owns
  the new fast-lane section).
- **D1** — expand `flow.md` + `orchestration.md` (thin → full).
- **D2** — `composition.md` + `text-agents.md` + `middleware.md`.
- **D3** — `live-sessions.md` + `steering-modes.md` (callbacks owned by U-5).
- **D4** — `phases.md` + `phase-transitions-deep-dive.md` + `watchers.md`.
- **D5** — `state.md` + `extraction.md` + `session-persistence.md`.
- **D6** — `tools.md` + `tool-policies.md` + `mcp-tools.md`.
- **D7** — sync `introduction.md` + `best-practices.md` + `architecture.md` +
  `auth-and-connecting.md` + `migration.md` + `setup-and-running.md` to the new prelude.

## E2E recipe (every unit)
1. `cargo build --workspace` and `cargo test --workspace` (code units).
2. Cookbook units: `cargo build -p example-cookbook`.
3. Docs units: `mdbook build docs` (install mdbook 0.4.40 + mdbook-pagetoc) + a
   Markdown link check.
4. All: `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`.
5. `cargo clippy --workspace --all-targets -- -D warnings` + `cargo fmt --all -- --check`.
