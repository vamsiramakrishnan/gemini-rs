# Code Mode

Code mode gives the model one more tool, `run_code`. The model writes a
short Python program whose functions are the session's own tools, and reads
back only the program's final value. The rest of the turn is unchanged: the
model still speaks, still calls tools directly, and every call a program
makes is governed exactly like a direct call.

Use it where function calling makes the model do arithmetic in its head:

- sums, counts and comparisons over a result with many rows;
- dates ("the second Friday of next month");
- chains of dependent reads (list prescriptions, pick one, check its refills).

Keep direct calls for a single lookup and for every commit. A commit should
be one visible call, not a line inside a program.

## Turning it on

With the fluent builder, name the tools a program may call:

```rust,ignore
use gemini_adk_fluent_rs::code_mode::CodeMode;
use gemini_adk_fluent_rs::prelude::*;

let session = Live::builder()
    .tool(lookup_payment_history())
    .tool(check_availability())
    .tool(book_table())
    .code_mode(
        CodeMode::new()
            .function_returning(
                "lookup_payment_history",
                r#"{"payments": [{"paid": str, "due": str, "amount": float}]}"#,
            )
            .function("check_availability"),
    )
    .connect_from_env()
    .await?;
```

In a spec, add a `code` section. Without `functions`, a program can call
every declared tool except commits (a stage's `commit`, a flow's `commit`)
and background tools:

```json
{
  "tools": [
    { "name": "lookup_payment_history",
      "returns": { "type": "object", "properties": { "payments": { "type": "array",
        "items": { "type": "object", "properties": {
          "paid": { "type": "string" }, "due": { "type": "string" },
          "amount": { "type": "number" } } } } } } },
    { "name": "book_table" }
  ],
  "code": { "limits": { "calls": 10 } }
}
```

| Field | Default | Meaning |
|---|---|---|
| `tool` | `run_code` | The code tool's name |
| `functions` | every tool but commits and background tools | The tools a program can call |
| `direct` | `true` | Keep the functions declared as tools of their own too |
| `only_in_code` | none | Tools declared only through the code tool, whatever `direct` says; they join `functions` |
| `limits` | see [Limits](#limits) | Bounds on each program |

`check` reports a function that is not a declared tool, or a code tool name
that clashes with one, as an error, and a commit or background tool among the
functions as a warning. Codegen emits the `.code_mode(..)` call.

## What the model sees

The code tool's description says when to use it and lists each function as
a Python stub generated from the tool's declaration. Required parameters come
first, in the order the declaration requires them, then optional ones:

```python
def check_availability(date: str, party_size: int, seating: str | None = None) -> dict:
    """Open table times at Trattoria Rustica.
    date: YYYY-MM-DD
    Returns {"date": str, "times": [str]}"""
```

Positional arguments are named in that order, so `check_availability("2026-10-12", 4)`
and `check_availability(date="2026-10-12", party_size=4)` reach the tool
alike. The `Returns` line comes from `function_returning`, a spec tool's
`returns` schema, or, for a mock tool, the shape of its canned `response`.
Without it the model guesses the result's keys, and a wrong guess costs a
failed program.

A tool whose result the model should never read whole, such as a long
history, belongs in `only_in_code` (`CodeMode::only_in_code` in Rust). Given
the payment history as a direct tool as well, Gemini 3.8 Live often fetched
all 120 rows with a direct call and then pasted them into a program, which
costs the tokens code mode exists to save.

Under [`ContextUpdate` steering](steering-modes.md) the code tool is declared
only while the flow offers a function it exposes, and its stubs list only
those functions, so a program is never told about a tool the current step
does not allow. With `direct(false)` (`"direct": false`) the functions are
not declared on their own at all; the model reaches them only through
programs.

The model sometimes copies the stubs into its program (`def
check_availability(...): pass`), which would shadow the real function so
that every call returns `None`. A definition that does nothing, or an import
of only these names, is removed before the program runs; any other
definition of one of them is refused with the reason.

The description's wording matters, and it is tuned on Gemini 3.8 Live. An
earlier wording, "for a single lookup, call the tool directly", led the model
to fetch 120 payment rows with a direct call (6,286 prompt tokens) and then
paste them into a program. It also printed results "to look at them" and
pasted the printout into a second program. The description now says to call
the function inside the program whenever the answer needs arithmetic over
its result, to do the whole request in one program, and to rely on the
docstrings instead of printing. With it, the model wrote one program for the
120-row question in every run of the smoke test, at 1,748 prompt tokens.

## Every call is governed

A program cannot reach around the session's governance. Each call it makes
pauses the interpreter and goes down the same path as a direct call:

1. the active phase's tool list;
2. the [decision gate](decisions.md), asking the questions that govern the
   tool about the turn in progress;
3. the flow gate, extracting the turn in progress first when a guard is only
   waiting on it;
4. middleware (`before_tool` can veto), confirmation, and the tool itself;
5. the flow advance on the result, so the program's next call sees the step
   it opened.

A refused or failed call raises `RuntimeError` in the program with the reason,
which the program can catch. Each call, refused ones included, is recorded as
a `LiveEvent::ToolExecution` with its full result, and the transcript keeps
it for extraction and decisions; the model reads only the program's final
value. A barge-in cancels the program: no response is sent, as for a direct
call. The `on_tool_call` override does not see a program's calls.

A program can read state only through its calls and cannot write state or
move the flow except through the receipts of the calls it makes. A flow guard
reads evidence the model cannot assert, and a program cannot assert it either.

## Limits

| Limit | Default | Over it |
|---|---|---|
| `calls` | 20 | The call past the limit ends the program with the reason |
| `cpu` | 2 s | Interpreter time, excluding calls: `TimeoutError` |
| `wall` | 30 s | The whole program, calls included: ended before its next call |
| `output_chars` | 4,000 | A larger result is refused with a note to return less; printed output is cut |
| `memory` | 64 MiB | Enforced by `MontyPool` only |

The spec spells them `calls`, `cpu_ms`, `wall_ms`, `output_chars` and
`memory_mb`.

## Sandboxes

Programs run in [Monty](https://github.com/pydantic/monty), a Python
interpreter written in Rust for code written by models. It has no file
system, network or clock access; `datetime`, `math`, `json` and `re` import.

| Sandbox | Feature | Isolation |
|---|---|---|
| `InProcess` (default) | `code-mode` (default) | A blocking thread of this process |
| `MontyPool` | `code-pool` | A pool of Monty worker processes |

In-process, Monty cannot bound memory. Measured with Monty 1.1.0: a program
of `len('a' * 10**10)` either aborts the process (with an address-space limit
of 4 GB) or allocates 10 GB and runs for 37 s past a 2 s limit. On a voice
server that is every call on the machine. Use the in-process sandbox for
development, tests and trusted deployments.

`MontyPool` runs each program in a worker process with a memory cap and hard
deadlines. The same program comes back as a `MemoryError` that ends only its
worker; the pool replaces it and the next program runs. Workers are the
`monty` binary (`cargo install monty-runtime`, or `pip install
pydantic-monty-runtime`):

```rust,ignore
use std::sync::Arc;
use gemini_adk_fluent_rs::code_mode::{CodeMode, MontyPool};

let pool = MontyPool::spawn("/usr/local/bin/monty", 8).await?;
let code = CodeMode::new().function("lookup_payment_history").sandbox(Arc::new(pool));
```

For a spec, set `SpecResources::code_sandbox`. `adk spec run` (built with
`code-pool`) uses a pool when `MONTY_BIN` names the binary.

## Skills

A skill can have its own `code` section. Its code tool is declared as
`{skill}__run_code` while the skill is foreground and offers a function the
code tool exposes, and its stubs use the skill's local tool names:
`lookup(...)`, not `billing__lookup(...)`.

```json
{ "name": "billing", "version": "1.0",
  "tools": [ { "name": "lookup", "effect": { "kind": "read" } },
             { "name": "adjust", "effect": { "kind": "commit", "idempotency_argument": "request_id" } } ],
  "code": {} }
```

Each call a program makes is an invocation of the task: admitted by the
skill's flow, executed, and completed before the program reads the result,
so its receipt moves the task on. Without `functions`, a program calls the
skill's read tools. A commit tool is refused in `code.functions`: it waits
for approval in the application, and a program cannot wait for it.

The task lane runs a skill's program to its end before it handles the next
event, its calls included, bounded by the `wall` limit. Keep skill programs
to reads that return quickly.

## What it measured

<!-- SDK_RESULTS -->

## Gemini's own code execution

Gemini's built-in `codeExecution` tool runs Python in Google's sandbox, where
it cannot call your tools. Gemini 3.8 Live, 3.8 Live Extended Thinking and 2.5
native audio refuse it at setup on Google AI, and the SDK leaves it off for
them; see [Gemini 3.8 Live](gemini-3-8-live.md#code-execution).
