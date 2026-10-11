# Code execution in a voice agent: does it beat function calls?

Status: experiment done; integration proposed. Date: 2026-10-11.
Harness and raw results: [`evals/code-mode`](../../evals/code-mode/README.md).

## The question

Instead of the model calling tools one at a time, and the runtime turning
results into state and transitions, could the model write a small program
that does the work, and would that improve the functional requirements
(right answers, governance held) and the non-functional ones (latency,
tokens, reliability)?

## Three arms

Each run opens a fresh Gemini 3.8 Live session, text in and out, with the
same instruction ("today is Sunday, 11 October 2026"), the same five tools
(table availability and booking, payment history, prescriptions and
refills) and the same commit gate: `book_table` is refused until the caller
has said yes to the details read back.

| Arm | What the model gets |
|---|---|
| Tools | The five tools as function declarations, as today |
| Server code | The same, plus Gemini's own `codeExecution` (Python in Google's sandbox, which cannot reach our tools) |
| Code mode | One tool, `run_code(code)`. The model writes Python that calls the five tools as functions. [Monty](https://crates.io/crates/monty), a Python interpreter written in Rust for code written by models, runs it in-process and pauses at each call; the host runs the call through the same tools and gate, and resumes the program with the result or a `RuntimeError` |

## Result 1: Gemini's code execution is not available on Live

Gemini 3.8 Live, 3.8 Live Extended Thinking and 2.5 native audio all close
the session at setup when the `codeExecution` tool is declared (Google AI,
1007, "Code Execution tool is not supported for this model"). The SDK's
`.code_execution()` and `T::code_execution()` therefore ended a Live session
before it started. The setup message now leaves the tool off for those
models on Google AI, and connect warns (`ignored_settings` lists
`tools.codeExecution`). Text agents are unaffected. The rest of this note
compares the other two arms.

## Result 2: code mode against function calls

Ten tasks with computed ground truth, five runs each per arm (100 sessions).

| Task | Kind | Right: code / tools | Latency p50 (ms): code / tools | Prompt tokens p50: code / tools |
|---|---|---|---|---|
| t1, t2 | A date to work out ("the second Friday of next month") | 10/10 / 10/10 | 8,238 / 7,386 and 6,650 / 5,770 | 1,376 / 1,298 and 1,335 / 1,282 |
| t3 | Four days to check, pick the earliest | 4/5 / 4/5 | 7,255 / 7,855 | 1,391 / 1,670 |
| t10 | Fourteen days to check (the first one qualified) | 5/5 / 5/5 | 6,806 / 7,465 | 1,421 / 1,316 |
| t4 | Sum and lateness over 6 payments | 5/5 / 5/5 | 8,457 / 8,864 | 1,246 / 1,456 |
| t7 | Sum to the cent and lateness over 22 payments | 5/5 / 5/5 | 8,582 / 9,280 | 1,289 / 2,105 |
| t9 | Total, late count and largest over 120 payments | **5/5 / 2/5** | **12,110 / 19,995** | **1,361 / 5,796** |
| t5 | Chain: list prescriptions, pick the cholesterol one, check refills | 5/5 / 5/5 | 10,489 / 13,030 | 1,573 / 1,600 |
| t6 | Booking, confirmed on the second line | 5/5 / 5/5 | 16,712 / 15,733 | 1,732 / 1,598 |
| t8 | Booking, caller says "don't read it back, just book it" | 5/5 / 5/5 | 19,502 / 16,087 | 1,647 / 1,607 |
| **All** | | **49/50 / 46/50** | **8,699 / 9,186** | **1,386 / 1,599** |

The t3 misses in both arms read "after 6pm" differently (one took 6:00 pm
as after 6). Spoken numbers and dates ("twenty-two", "eight o'clock",
"October twelfth") are graded as numbers.

### Functional requirements

- **Correctness improves only where the data is large.** Gemini 3.8 Live
  worked out every date, summed 22 amounts to the cent and followed every
  chain without code. At 120 rows it gave three wrong totals out of five
  ($2,697.58, $2,629.70 and $2,618.70 against $2,588.70), spoken as
  confidently as the right ones. The program summed them right every time.
- **Governance holds.** Every call a program makes pauses the interpreter
  and goes through the same gate as a direct call, so a program cannot reach
  around it. Neither arm tried to book before the caller's yes, even when
  the caller asked to skip it: in 20 commit runs, the gate refused nothing,
  and no answer claimed a booking that had not happened.
- **Programs fail and recover.** 4 of the 50 code-mode runs had a program
  error (importing the functions, mocking them); each was followed by a
  corrected program.

### Non-functional requirements

- **Tokens.** Code mode returns only what the program ends with, so a large
  result never enters the conversation: 39% fewer prompt tokens at 22 rows,
  77% fewer at 120, and the saving compounds over the rest of the call. On
  small tasks the program text costs 2 to 8% more.
- **Latency.** Faster when the work is heavy (120 rows: 12.1 s against 20.0
  s; the chain: 10.5 against 13.0 s). Slower when it is light: about 0.9 s
  on a single lookup, 1 to 3.4 s on a booking, where the model split the
  work into several programs.
- **Time to the first action.** Writing a program takes longer than emitting
  a call: the first action came 200 to 700 ms later in every task. On a
  voice line that is time the caller waits.
- **Round trips.** About the same overall (1.48 against 1.40 per task):
  fewer for chains and fan-out, more for bookings.

### Verdict

Code mode is worth having as one tool among others, not as the way every
tool is called:

- **Use it** for reads over data the model would otherwise have to add up,
  sort or join in its head, and for chains of dependent reads.
- **Keep direct calls** for a single lookup and for every commit. They are
  faster to the first action, and a commit should be one visible call
  rather than a line inside a program.

## Phase and state updates in code

Two different things can be meant, and they get opposite answers.

**The model writes code that sets state or moves the flow: no.** The guards
in a governed flow read evidence the model cannot assert: tool receipts,
extracted slots, decision-model answers. A program that can set
`verified = True` or advance a step would let the model vouch for itself,
and a caller could talk it into doing so. A program may read state, and its
calls produce receipts that set state as any call does.

**The spec author writes code: yes, and it needs no model.** The same
sandbox can run author-written Python for:

- **Computed state**: derived values from state and receipts.
- **Guards** beyond the JSON atoms, such as binding a commit's arguments to
  what was checked. The pharmacy fixture submitted a refill for a different
  prescription than the one checked until its flow required the medication
  first.
- **Projecting tool results**: shrinking a result to what the model needs
  before it enters the conversation. This is code mode's token saving
  without asking the model to write code.

These are deterministic, testable offline, and add no model latency. This
experiment did not measure them, since no model is involved.

## How it would fit the SDK

- **A tool kind, not a mode.** A spec names the functions code mode exposes,
  for example `{"code": {"functions": ["lookup_payment_history",
  "check_availability"], "limits": {"calls": 20, "ms": 2000}}}`, and the
  runtime declares `run_code` with Python stubs generated from those tools'
  declarations. Under `ContextUpdate` steering, a stage that allows
  `run_code` declares it, and one that does not leaves it out.
- **Every call is a tool call.** Each function a program calls goes down the
  existing path: flow admission, the decision gate, middleware, the
  journal, `set_state` and `save_response_as`. Commit tools are left out of
  the functions by default.
- **What the model reads** is the program's final value; the journal keeps
  each call and its full result.
- **Packaging is the open decision.** Monty 1.1.0 declares Rust 1.96 but
  needs a newer standard library (1.99 builds it), and the workspace's
  floor is 1.93. The options:
  - Raise the workspace's minimum Rust, which affects every user.
  - Add a separate optional crate with its own minimum.
  - Run Monty out of process (`monty-pool`, which also isolates a crash
    inside the interpreter, as Monty's own docs recommend for untrusted
    code) and depend on nothing at compile time.
- The existing `code_executors::CodeExecutor` trait runs code and returns
  its output, with no calls back into the host, so it does not fit code
  mode as it stands.

## Not covered yet

- Spoken callers: numbers heard by speech recognition, and the audio a
  caller waits through before the first action.
- Code mode inside skills, where each skill would expose its own functions.
- Author-written guards and projections, measured against the same tasks.
