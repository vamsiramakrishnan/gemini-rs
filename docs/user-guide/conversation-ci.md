# Conversation CI

**Unit tests for voice-agent conversations** — deterministic, model-free, and
fast enough to run on every pull request.

A governed conversation is a serializable [`ConversationSpec`](./flow.md): it
compiles to an enforced Flow DAG, and a model-free [`Sim`](./flow.md) drives that
same control-plane code with scripted inputs — no audio, no model, no network.
Conversation CI turns that into a gate: a corpus of specs and scenarios that runs
in milliseconds and fails the build on any regression.

## What it checks

For a directory of conversations, `adk flow ci` runs two checks across the whole
corpus:

1. **Compile** — every spec lowers to a valid governed flow. Catches authoring
   breakage statically: an unreachable stage, an **unguarded commit** tool, a
   dangling tool name, an ordering cycle. Reports the structured error.
2. **Behavior** — every scenario runs deterministically against its spec.
   Catches behavior regressions: the wrong stage active, a tool admitted when it
   should be gated, the conversation failing to complete.

Both run with **no model in the loop**, so they are 100% reproducible — unlike
LLM-as-judge testing, which is flaky enough that vendors recommend retrying each
test several times.

## Corpus layout

Put specs and scenarios in a directory (searched recursively). A spec named
`<name>.spec.json` is tested by `<name>.scenario.json` and any
`<name>.<label>.scenario.json` in the same directory; a spec with no scenarios is
still compile-checked.

```
conversations/
  booking.spec.json
  booking.happy.scenario.json
  booking.no_book_without_confirm.scenario.json
  refund.spec.json
  refund.scenario.json
```

## Run it

```bash
adk flow ci conversations
```

```
  conversations/booking.spec.json — compiled ✓
      happy_path ... PASS
      cannot_book_without_confirmation ... PASS

specs: 1 ok / 0 failed   scenarios: 2 passed / 0 failed
```

Exits non-zero if any spec fails to compile or any scenario fails, so it gates
CI. Use `--json` for a machine-readable report (specs, per-scenario results, and
a summary) to feed dashboards or annotations.

When a scenario fails, the message names the exact step and reason — e.g.
`[happy_path] step 8 (ExpectAllowed("book")): expected 'book' allowed, but
denied: needs user_confirmed` — so a conversation-behavior change that would
otherwise only surface in a live call is caught precisely, in milliseconds.

## In GitHub Actions

Use the bundled composite action:

```yaml
jobs:
  conversation-ci:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      - uses: dtolnay/rust-toolchain@stable
      - uses: vamsiramakrishnan/gemini-rs/.github/actions/conversation-ci@main
        with:
          dir: conversations
```

Or run the CLI directly:

```yaml
      - run: cargo install gemini-adk-cli-rs --locked
      - run: adk flow ci conversations
```

## Authoring scenarios

Scenarios are model-free: each step substitutes for a piece of a live turn.

| In a live call | Scenario step |
|---|---|
| Caller speaks; a slot is recognized | `{"set": {"key": "party_size", "value": 4}}` or `{"user": "party of four"}` (runs the real recognizers) |
| A turn boundary | `"turn"` |
| A tool completes | `{"tool_ok": "book"}` |
| Tool latency / silence | `{"schedule_tool": {"tool": "x", "after": 2}}` |

Assertions: `{"expect_active": [ids]}`, `{"expect_allowed": "tool"}`,
`{"expect_denied": "tool"}`, `{"expect_slot": {"key": ..., "value": ...}}`,
`"expect_complete"`.

The highest-value scenario is usually a **governance** one — assert a committing
tool is `expect_denied` before its guard holds and `expect_allowed` after. That
is the bug class (e.g. "booked without confirming") that is catastrophic in
production and that only deterministic simulation can catch reliably.

## The regression flywheel

Conversation CI pairs with [record & replay](./record-replay.md): when a
production session misbehaves, its recorded trace becomes a new scenario in the
corpus, so the bug becomes a permanent regression test and cannot return.
