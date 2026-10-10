# The agent.json language

Contents: [Top level](#top-level) · [Stages](#stages) · [Guards](#guards) ·
[How a key gets written](#how-a-key-gets-written) · [Tools](#tools) ·
[Digressions and policies](#digressions-and-policies) ·
[Worked example](#worked-example)

`adk spec schema` prints the full JSON Schema; `adk spec catalog` lists the
names. This page is the part you need to write a voice agent.

## Top level

| Field | What it is |
|---|---|
| `name` | Short slug: `bright-smile-booking` |
| `description` | One line for people |
| `instruction` | The model's standing brief for every turn. Keep it about who the agent is and how it speaks ("one or two short spoken sentences"); per-stage behaviour goes in stages |
| `greeting` | An instruction for the first thing the agent says; without it the agent waits for the caller |
| `modality` | `"audio"` for voice |
| `voice` | A prebuilt voice from the catalog. Change it only when asked |
| `runtime` | `{"steering": "context_update"}` offers the model only the tools the active stage allows (Gemini 3.8 Live). Without it, other tools are still offered but refused |
| `tools` | Declared tools: what the model sees. See [Tools](#tools) |
| `extract` | Out-of-band extractors that fill state from what the caller says |
| `decisions` | Questions a decision model (Jev on Vercel AI Gateway) answers about the conversation, named by `decided` guards: confirmations, intents, picks among offered options, stage routing. See [Decisions](#decisions) |
| `conversation` | The stages, digressions and policies |
| `scenarios` | Offline tests; see scenarios.md |

## Stages

`conversation` is `{"name", "stages", "require", "overlays", "policies"}`.
A stage that no `next` (or `after`) leads to is active from the start;
usually that is only the first one. `next` edges move between stages;
`require` names the stages a complete call must reach.

| Stage field | Meaning |
|---|---|
| `id` | Stage name, used in `next`, `require` and scenarios |
| `say` | What the agent should do in this stage (its posture) |
| `ground` | Facts given to the model while active; `{key}` is filled from state: `"Party of {party_size} at {slot}."` |
| `collect` | Slots this stage gathers. The stage completes when all are set, unless `done` says otherwise |
| `allow` | Tools admitted while this stage is active. A stage with no `allow` (or `[]`) restricts nothing, so list the tools wherever the model may call them |
| `commit` | `{"tool", "when": guard}`: the tool is denied in every stage until the guard holds. Use it for every tool that changes something. Once a committed tool has succeeded it is denied for the rest of the call; a failed call can be retried, so a tool that must allow a second try reports a mismatch as an error |
| `next` | `[{"to": stage, "when": guard}]` transitions |
| `done` | Explicit completion guard, when `collect` or `next` is not the right signal |
| `terminal` | The call ends here (done, handoff, goodbye) |
| `repair` | `{"reprompt_after": 2, "escalate_after": 4, "escalate_to": "handoff_collect"}`: after that many turns without completing, raise `repair:{stage}:escalate` and move to `escalate_to`. Also `escalate_after_tool_failures`, `escalate_after_interruptions`. Give each stage its own `escalate_to` stage: a stage several stages escalate to waits for all of them |
| `verbatim` | Text read word for word. The stage does not complete (or escalate) until it has been said |
| `timing` | `{"filler_after_ms", "reprompt_after_ms", "reprompt", "interruptible", "end_of_speech_ms"}` |
| `resolve` | `[{"slot", "resolver"}]`: a declared tool fills the slot |

When the caller corrects a slot, the runtime clears every state key read by
the commit guards of later stages (except keys that are themselves
collected), so the read-back and the yes happen again. Keep other state
checks out of those guards: see "Gates that survive corrections" in
patterns.md. A correction also reopens the stages after the corrected slot,
even once the call has reached a terminal stage; committed tools that already
succeeded stay denied.

## Guards

`"always"`, `{"is_true": key}`, `{"is_set": key}`, `{"eq": [key, value]}`,
`{"captured": [keys]}`, `{"called_ok": tool}`, `{"done": stage}`,
`{"decided": question}`, `{"all": [...]}`, `{"any": [...]}`,
`{"not": guard}`.

`called_ok` and `done` read the conversation's progress, and `decided` the
decision model's answer for the caller's current turn (see
[Decisions](#decisions)); the rest read **state keys**, and a state key is
only ever true if something writes it.
`adk spec check` reports a guard key nothing writes (`unwritten_key`).

## How a key gets written

| Writer | Use for | Example |
|---|---|---|
| A stage's `collect` | Slots the caller gives | `"collect": ["party_size"]` |
| `extract` + `promote` | Filling slots and flags from speech | see the worked example |
| A decision's `writes` | A pick among offered options, into the slot it fills | see [Decisions](#decisions) |
| A tool's `set_state` | Facts a tool establishes | `"set_state": {"dob_verified": true}` |
| A tool's `save_response_as` | Keeping the tool's response | `"save_response_as": "availability"` |
| The runtime | `verbatim:{stage}`, `repair:{stage}:*`, `flow:*` | read only |

`collect` names slots; it does not listen for them. A collected slot needs
an `extract` entry whose `promote` rules write it (or a tool or resolver that
does). `adk spec check` counts collected slots as written, so it will not
warn you: a spec without extraction passes every check and scenario and then
never leaves its first stage on a real call. Confirmations and intents (`book_confirmed`,
`intent:human_agent`) come from one extractor, conventionally
`caller_signals`, with boolean fields and `"policy": "true_only"`; a key with
a colon is promoted with `"to"`. Extractors need an extraction model at run
time; `adk spec run` creates one from the environment.

## Decisions

When the person has an AI Gateway key with Jev access, confirmations,
intents, routing and picks among offered options can be questions a
decision model answers, instead of `caller_signals` flags. It answers in
about 250 ms (median through AI Gateway) with a probability, instead of a
language model's seconds. Use it when they ask for Jev or for faster
confirmations; it needs `AI_GATEWAY_API_KEY` at run time.

Declare each question once, then guard on it with `decided`:

```json
"decisions": {
  "book_confirmed": {
    "type": "boolean",
    "instructions": "In their last turn, did the caller agree to the booking that was read back?",
    "criteria": { "true": "the caller said yes in their own words",
                  "false": "they hesitated, changed a detail, or only picked an option" }
  },
  "wants_person": {
    "type": "boolean",
    "instructions": "In their last turn, did the caller ask to speak to a person?"
  },
  "picked_slot": {
    "type": "choice",
    "instructions": "Which of the offered times did the caller choose?",
    "options_from": "availability.slots",
    "none": "the caller has not chosen one of the offered times",
    "writes": "slot"
  }
}
```

```json
{ "id": "confirm", "commit": { "tool": "book_table", "when": { "decided": "book_confirmed" } } }
{ "name": "handoff", "trigger": { "decided": "wants_person" }, "stages": [ ... ] }
```

- `decided` forms: `"q"` (yes, picked, or scored), `{"q": false}` (no, or
  `none_of_these`), `{"q": "option"}`, `{"q": {"at_least": n}}`,
  `{"q": {"at_most": n}}`. `adk spec check` rejects one naming no question
  or expecting what its question cannot answer.
- An answer counts only for the caller turn it is about: no latching, no
  `true_only`, nothing to clear when the caller changes a detail. Unsure
  (between the thresholds, or the model too slow) satisfies nothing, so a
  commit fails closed.
- At the caller's turn end the runtime asks, in one request, the
  questions the driving flow's guards and the digressions' triggers name,
  plus any that `writes` a key an active stage reads; phase transitions
  and patterns are asked every turn. Before a tool call is admitted it asks
  only what governs that call (its commit guard, a digression that would
  admit it). Ask about "their last turn" for confirmations and intents.
- A `choice` with `options_from` picks among options in state; give it
  `none`, since a choice always picks something. `writes` puts the pick
  into the slot the stage collects. Slots with free values (names, dates)
  stay in `extract`.
- `at_least` sets the bar: 0.85 `P(true)` for a boolean by default, 0.6
  certainty for a choice or score.
- Scenarios script answers with `{"decide": {"book_confirmed": true}}`; a
  `user` step starts a new caller turn, so an earlier answer stops counting.
- Don't keep a `caller_signals` extract entry for the same signals.

## Tools

A declared tool is `{"name", "description", "parameters", ...}` plus at most
one way to run it:

| Binding | Fields | When |
|---|---|---|
| none (stub) | — | Not decided yet; `adk spec codegen` writes a typed stub |
| mock | `response`, `set_state` | Demos and offline runs |
| HTTP | `"http": {"method", "url", "headers", "body"}`, `{args.x}` and `{state.key}` interpolate | An existing API |
| MCP | `"mcp": "python tools/server.py"` or an http(s) URL | Code in another language, or codegen's tool server |

Two rules that matter for gates:

- `set_state` applies only when the call **succeeds**. A code or MCP tool
  that raises an error writes nothing, so it can safely mark a check as
  passed.
- An **HTTP** call that gets a non-2xx response is not a failure: it returns
  the body and `set_state` still applies. Never mark a verification as passed
  with `set_state` on an HTTP tool; implement it as code (or MCP) that raises
  on a mismatch.

Write `description` for the model: what the tool does and when to call it
("Call only after reading the details back and hearing a clear yes").

## Digressions and policies

A digression (`overlays`) is a sub-flow with a `trigger` guard that suspends
the main flow: `{"name", "trigger", "stages", "resume"}` with `resume`
`previous`, `restart` or `terminate`. While it drives, its own stages decide
which tools are admitted; the main flow's commit guards do not apply. Nothing in the runtime writes
`intent:*` flags, so a trigger needs a writer (a `caller_signals` field). A
`previous`/`restart` digression re-enters while its trigger still holds, so
clear the flag once handled; terminating ones need no clearing.

`policies`:

- `{"kind": "safety_handoff", "intents": ["human_agent"]}` ends the call when
  `intent:human_agent` becomes true. It lowers to a terminal digression,
  which admits every tool on the turn it is entered; prefer the handoff
  digression in patterns.md, which admits only the transfer tool.
- `{"kind": "redact", "keys": ["date_of_birth"]}` writes `[redacted]` to the
  journal, snapshots and extraction events. Transcripts are not covered.
- `{"kind": "commit", "tool", "idempotency_key", "compensate_with"}` makes a
  committing tool idempotent and names its undo.

## Worked example

A restaurant line: collect the booking, check availability, read back, book
only after a yes, hand off on request or after four stalled turns. It passes
`adk spec check` with no diagnostics, `plan` is ready (one optional question:
how `handoff_to_staff` is implemented), and its four scenarios pass.

```json
{
  "name": "trattoria-booking",
  "description": "Phone bookings for Trattoria Rustica.",
  "instruction": "You are the phone booking assistant for Trattoria Rustica. Keep every reply to one or two short spoken sentences. Never invent availability: use the tools.",
  "greeting": "Greet the caller as Trattoria Rustica's booking assistant and ask how you can help.",
  "modality": "audio",
  "voice": "Kore",
  "runtime": {
    "steering": "context_update"
  },
  "tools": [
    {
      "name": "check_availability",
      "description": "Find open tables for a party size and a requested date and time.",
      "parameters": {
        "type": "object",
        "properties": {
          "party_size": {
            "type": "integer"
          },
          "datetime": {
            "type": "string",
            "description": "Requested date and time, ISO 8601"
          }
        },
        "required": [
          "party_size",
          "datetime"
        ]
      },
      "response": {
        "options": [
          "2026-10-14T19:00",
          "2026-10-14T19:30"
        ]
      },
      "save_response_as": "availability"
    },
    {
      "name": "book_table",
      "description": "Book the table the caller confirmed. Call only after reading the details back and hearing a clear yes.",
      "parameters": {
        "type": "object",
        "properties": {
          "party_size": {
            "type": "integer"
          },
          "slot": {
            "type": "string"
          },
          "name": {
            "type": "string"
          }
        },
        "required": [
          "party_size",
          "slot",
          "name"
        ]
      },
      "response": {
        "reference": "TR-2044"
      },
      "save_response_as": "booking"
    },
    {
      "name": "handoff_to_staff",
      "description": "Transfer the caller to a member of staff."
    }
  ],
  "extract": [
    {
      "name": "booking_details",
      "instruction": "Extract the party size, the requested date and time, and the name for the booking, as the caller gave them.",
      "schema": {
        "type": "object",
        "properties": {
          "party_size": {
            "type": "integer"
          },
          "slot": {
            "type": "string"
          },
          "guest_name": {
            "type": "string"
          }
        }
      },
      "promote": [
        {
          "field": "party_size",
          "policy": "overwrite"
        },
        {
          "field": "slot",
          "policy": "overwrite"
        },
        {
          "field": "guest_name"
        }
      ]
    },
    {
      "name": "caller_signals",
      "instruction": "Read the latest turns of the conversation. Set a field to true only when the caller clearly said so in their own words. Otherwise leave it out.",
      "schema": {
        "type": "object",
        "properties": {
          "book_table_confirmed": {
            "type": "boolean",
            "description": "The caller agreed to the booking after hearing the details read back."
          },
          "intent_human_agent": {
            "type": "boolean",
            "description": "The caller asked to speak to a person instead of the assistant."
          }
        }
      },
      "promote": [
        {
          "field": "book_table_confirmed",
          "policy": "true_only"
        },
        {
          "field": "intent_human_agent",
          "to": "intent:human_agent",
          "policy": "true_only"
        }
      ]
    }
  ],
  "conversation": {
    "name": "booking",
    "stages": [
      {
        "id": "collect",
        "say": "Find out how many guests, the date and time they want, and the name for the booking.",
        "verbatim": "This call may be recorded.",
        "collect": [
          "party_size",
          "slot",
          "guest_name"
        ],
        "repair": {
          "reprompt_after": 2,
          "escalate_after": 4,
          "escalate_to": "handoff"
        },
        "next": [
          {
            "to": "offer",
            "when": {
              "captured": [
                "party_size",
                "slot",
                "guest_name"
              ]
            }
          }
        ]
      },
      {
        "id": "offer",
        "say": "Check availability for the requested time and offer the open options.",
        "allow": [
          "check_availability"
        ],
        "timing": {
          "filler_after_ms": 1200
        },
        "next": [
          {
            "to": "confirm",
            "when": {
              "called_ok": "check_availability"
            }
          }
        ]
      },
      {
        "id": "confirm",
        "say": "Read back the party size, time and name, ask the caller to confirm, then book.",
        "ground": "Party of {party_size} at {slot}, under {guest_name}.",
        "allow": [
          "book_table"
        ],
        "commit": {
          "tool": "book_table",
          "when": {
            "is_true": "book_table_confirmed"
          }
        },
        "next": [
          {
            "to": "done",
            "when": {
              "called_ok": "book_table"
            }
          }
        ]
      },
      {
        "id": "done",
        "say": "Give the booking reference and say goodbye.",
        "terminal": true
      },
      {
        "id": "handoff",
        "say": "Apologise and tell the caller a member of staff will call them back.",
        "terminal": true
      }
    ],
    "require": [
      "done"
    ],
    "overlays": [
      {
        "name": "handoff",
        "trigger": {
          "is_true": "intent:human_agent"
        },
        "stages": [
          {
            "id": "transfer",
            "say": "Tell the caller you are passing them to a member of staff, then call handoff_to_staff.",
            "allow": [
              "handoff_to_staff"
            ],
            "done": {
              "called_ok": "handoff_to_staff"
            }
          }
        ],
        "resume": "terminate"
      }
    ]
  },
  "scenarios": [
    {
      "name": "books after confirmation",
      "steps": [
        {
          "expect_active": [
            "collect"
          ]
        },
        {
          "set": {
            "key": "party_size",
            "value": 4
          }
        },
        {
          "set": {
            "key": "slot",
            "value": "2026-10-14T19:00"
          }
        },
        {
          "set": {
            "key": "guest_name",
            "value": "Rossi"
          }
        },
        {
          "set": {
            "key": "verbatim:collect",
            "value": true
          }
        },
        "turn",
        {
          "expect_active": [
            "offer"
          ]
        },
        {
          "expect_allowed": "check_availability"
        },
        {
          "tool_ok": "check_availability"
        },
        {
          "expect_active": [
            "confirm"
          ]
        },
        {
          "expect_denied": "book_table"
        },
        {
          "set": {
            "key": "book_table_confirmed",
            "value": true
          }
        },
        "turn",
        {
          "expect_allowed": "book_table"
        },
        {
          "tool_ok": "book_table"
        },
        "expect_complete"
      ]
    },
    {
      "name": "never books without a yes",
      "steps": [
        {
          "expect_denied": "book_table"
        },
        {
          "set": {
            "key": "party_size",
            "value": 2
          }
        },
        {
          "set": {
            "key": "slot",
            "value": "2026-10-15T20:00"
          }
        },
        {
          "set": {
            "key": "guest_name",
            "value": "Chen"
          }
        },
        {
          "set": {
            "key": "verbatim:collect",
            "value": true
          }
        },
        "turn",
        {
          "tool_ok": "check_availability"
        },
        {
          "expect_active": [
            "confirm"
          ]
        },
        {
          "expect_denied": "book_table"
        }
      ]
    },
    {
      "name": "asking for a person hands off and books nothing",
      "steps": [
        {
          "set": {
            "key": "intent:human_agent",
            "value": true
          }
        },
        "turn",
        {
          "expect_denied": "book_table"
        },
        {
          "expect_denied": "check_availability"
        },
        {
          "expect_allowed": "handoff_to_staff"
        },
        {
          "tool_ok": "handoff_to_staff"
        },
        "turn",
        {
          "expect_denied": "handoff_to_staff"
        },
        {
          "expect_denied": "book_table"
        }
      ]
    },
    {
      "name": "stalling escalates to handoff",
      "steps": [
        {
          "set": {
            "key": "verbatim:collect",
            "value": true
          }
        },
        "turn",
        "turn",
        "turn",
        "turn",
        {
          "expect_slot": {
            "key": "repair:collect:escalate",
            "value": true
          }
        },
        {
          "expect_denied": "book_table"
        }
      ]
    }
  ]
}
```
