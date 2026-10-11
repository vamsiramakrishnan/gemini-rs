# Code mode against function calls on Gemini 3.8 Live

Can a voice agent do better if the model writes a small Python program that
calls the tools, instead of calling them one at a time? This harness runs the
same tasks three ways on Gemini 3.8 Live and records whether the answer and
the calls were right, gate refusals, claims of a booking that did not happen,
latency, time to the first action, round trips and prompt tokens.

| Arm | What the model gets |
|---|---|
| `tools` | Five function declarations |
| `server` | The same, plus Gemini's own `codeExecution`. Every Live model tried refuses it at setup (1007) |
| `code` | One tool, `run_code(code)`: Python that calls the five tools as functions, run in-process by [Monty](https://crates.io/crates/monty), pausing at each call so the host runs it through the same tools and the same commit gate |

The `sdk-*` arms run the same tasks through the SDK as shipped: a `Live`
session with the five tools registered, the booking gated by a governed flow
(`never(book_table).until(confirmed)`, the caller's yes set as state from the
line that gives it), and [code mode](../../docs/user-guide/code-mode.md):

| Arm | What the model gets |
|---|---|
| `sdk-tools` | The five tools |
| `sdk-code` | `run_code` over the four reads (`direct(false)`), and `book_table` as a direct call |
| `sdk-hybrid` | `run_code` beside all five tools: the SDK default |
| `sdk-tuned` | The hybrid, with `lookup_payment_history` (up to 120 rows) marked `only_in_code` |

The tasks (ground truth is computed, not hand-written) cover dates to work
out, fan-out over days, aggregation over 6, 22 and 120 rows, a dependent
chain, and bookings that must wait for the caller's yes.

The findings, and what they mean for the SDK, are in
[docs/plans/2026-10-11-code-execution.md](../../docs/plans/2026-10-11-code-execution.md).
In short: 49 of 50 right in code mode against 46 of 50 with function calls,
the whole difference at 120 rows (5/5 against 2/5, with 77% fewer prompt
tokens and 8 s less); no difference on small tasks, where code mode is a
little slower to its first action.

## Run it

Outside the workspace, so that it never runs in CI: every run opens live
sessions.

```text
cd evals/code-mode
GEMINI_API_KEY=… cargo run --release -- --runs 5 --arms tools,code --json results.json
python3 summarize.py results.json
```

| Flag | Default | Meaning |
|---|---|---|
| `--runs N` | `3` | Runs per task and arm |
| `--arms` | `tools,server,code` | Which arms, `sdk-tools`, `sdk-code` and `sdk-hybrid` included |
| `--tasks` | all | Task id prefixes, such as `t1,t9` |
| `--model` | `gemini-3.8-live` | Live model |
| `--json PATH` | none | Write every run, with the programs the model wrote |
| `--voice` | off | SDK arms only: speak each caller line (Gemini TTS, streamed in real time through an open mic) and grade the model's spoken answer by its transcription |

`results/` holds the runs behind the numbers in the report (2026-10-11).
