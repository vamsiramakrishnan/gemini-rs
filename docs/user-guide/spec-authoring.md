# Authoring a spec from a coding harness

A coding harness (Claude Code, Gemini CLI, an IDE agent) can write
`agent.json` for a user. To do that well it needs to know three things:

- what it is allowed to write;
- what is wrong with the document so far;
- which decisions belong to the user.

The authoring interface answers each of these as JSON. The library is
`gemini_adk_fluent_rs::spec::authoring`, and the CLI front end is a set of
`adk spec` subcommands:

| Command | Library | Output |
|---|---|---|
| `adk spec catalog` | `authoring::catalog()` | The vocabulary: voices, guard atoms, policies, tool bindings, resume policies, question rules, diagnostic codes |
| `adk spec check <spec> [--json]` | `authoring::check(&doc)` | Diagnostics, each with a JSON pointer and, when the repair is mechanical, a fix as JSON-patch operations. Exits non-zero on errors |
| `adk spec plan <spec> [--decisions f] [--json]` | `authoring::plan(&doc, &decisions)` | Open questions. Each option carries the patch that records it |
| `adk spec answer <spec> <answers> [--decisions f] [--write]` | `authoring::answer(&doc, &answers, &decisions)` | The answers applied, the updated decisions, a fresh check and plan |
| `adk spec patch <spec> <ops> [--write]` | `authoring::apply_patch(&doc, &ops)` | The patched spec and its check report |

The full document shape is the spec's JSON Schema (`adk spec schema`).

## The loop

```text
draft agent.json ─▶ check ─▶ apply one fix ─▶ check … ─▶ plan ─▶ ask the user ─▶ answer ─▶ plan … ─▶ ready
```

1. Write a first draft from the user's description. Use `catalog` for names
   and shapes rather than guessing them.
2. Run `adk spec check agent.json --json`. Apply one fix with
   `adk spec patch agent.json '<fix.patch>' --write`, then check again.
   All the fixes in a report are computed against the checked document,
   so two of them can touch the same array.
3. Run `adk spec plan agent.json --decisions decisions.json --json`. Ask
   the user the blocking questions. For an optional question, ask the user
   or take its `default`.
4. Run `adk spec answer agent.json '<answers>' --decisions decisions.json --write`.
   The command plans again before each answer, so every patch is computed
   against the current document. If any answer fails, nothing is written.
5. When `plan.ready` is true (no blocking questions, no check errors),
   continue with `adk spec test` and `adk spec codegen`.

## Check

```console
$ adk spec check agent.json
error    unknown_tool   /conversation/stages/1/allow/0
         tool 'book_tabel' is not declared — did you mean 'book_table'?
         fix: replace 'book_tabel' with 'book_table'
warning  unknown_field  /instructions
         'instructions' is not a field here and is ignored — did you mean 'instruction'?
         fix: rename 'instructions' to 'instruction'

1 error(s), 1 warning(s)
```

With `--json`, each diagnostic is an object:

```json
{
  "severity": "error",
  "code": "unknown_tool",
  "path": "/conversation/stages/1/allow/0",
  "message": "tool 'book_tabel' is not declared — did you mean 'book_table'?",
  "fix": {
    "description": "replace 'book_tabel' with 'book_table'",
    "patch": [{ "op": "replace", "path": "/conversation/stages/1/allow/0", "value": "book_table" }]
  }
}
```

| Code | Meaning | Fix |
|---|---|---|
| `invalid_json` | The text is not JSON | — |
| `not_an_object` | The document is not a JSON object | — |
| `invalid_spec` | The document does not deserialize as a spec | — |
| `unknown_field` | A field the spec does not have; serde ignores it, so the setting is lost | Rename to the closest field |
| `unknown_tool` | A conversation stage, commit, guard or commit policy names an undeclared tool | Replace with the closest declared tool, or declare it with no binding |
| `unwritten_key` | A guard reads a state key that nothing writes, so it can never become true | Read the closest written key, or add a caller signal (below) |
| `validation` | Anything else `SessionSpec::validate` reports, passed through as text | — |

`unknown_field` covers the top level, tools, the conversation, its stages
and its digressions. Keys that start with `$`, such as `$schema`, are
allowed. The pointer-level `unknown_tool` and `unwritten_key` checks cover
conversation specs. For a `flow` spec, the same problems are reported as
`validation` text without a path.

`unwritten_key` also covers digression triggers and safety-handoff intents.
Validation does not see those, because they are not part of the lowered
main flow. Nothing in the runtime writes `intent:{name}` flags, so a
trigger such as `{"is_true": "intent:cancel"}` needs a writer.

## Caller signals

A confirmation (`book_table_confirmed`) or an intent (`intent:human_agent`)
comes from what the caller says. Fixes and answers that need such a key
add it to one extractor, `caller_signals`. It has one boolean field per
signal, and each field is promoted to its key with the `true_only` policy:

```json
{
  "name": "caller_signals",
  "instruction": "Read the latest turns of the conversation. Set a field to true only when the caller clearly said so in their own words. Otherwise leave it out.",
  "schema": {
    "type": "object",
    "properties": {
      "book_table_confirmed": {
        "type": "boolean",
        "description": "The caller explicitly agreed to go ahead with book table after hearing the details."
      }
    }
  },
  "promote": [{ "field": "book_table_confirmed", "policy": "true_only" }]
}
```

An extractor needs an extraction model at run time
(`SpecResources::extraction_llm`). `adk spec run` creates one from the
environment. When the caller corrects a slot collected before the commit
stage, the runtime clears the confirmation key. See
[voice behavior](voice-behavior.md).

## Plan

Each question has a stable id, the question to put to the user, why it
matters, and options:

```json
{
  "id": "redact:card_number",
  "ask": "`card_number` looks sensitive. Should it be redacted?",
  "why": "A redacted key is written as `[redacted]` to the journal, persistence snapshots and extraction events. ...",
  "kind": "choice",
  "options": [
    { "value": "redact", "label": "Redact it",
      "patch": [{ "op": "add", "path": "/conversation/policies", "value": [{ "kind": "redact", "keys": ["card_number"] }] }] },
    { "value": "keep", "label": "Keep it in the clear (it is not sensitive, or a later system needs it)" }
  ],
  "default": "redact",
  "blocking": true,
  "affects": ["/conversation/policies"]
}
```

| Id | Asked when | Blocking |
|---|---|---|
| `name` | The app has no name | yes |
| `instruction` | The app has no base instruction | yes |
| `tool_description:<tool>` | A tool has no description | yes |
| `commit_gate:<tool>` | A stage allows a tool that looks state-changing (its name starts with a verb such as `book`, `pay` or `cancel`, or it has a non-GET HTTP binding), and no stage commits it | yes |
| `redact:<slot>` | A collected slot looks sensitive (`card`, `cvv`, `ssn`, `dob`, `passport` …) and no redact policy covers it | yes |
| `voice` | An audio app has no voice | no |
| `greeting` | An audio app has no greeting | no |
| `escalation` | The conversation has no digression, safety handoff or repair escalation | no |
| `disclosure` | An audio conversation has no verbatim text | no |
| `tool_binding:<tool>` | A tool has no HTTP or MCP binding and no canned response | no |

A text question has one option with `"needs_value": true`. Some choice
options need a value too, such as an HTTP binding's URL. In an option's
patch, a string equal to `"$answer"` is replaced by the answer's value.
A `default` never needs a value.

These rules are heuristics. `commit_gate` and `redact` look at names, so
they can miss a tool or slot, and they can ask about one that is fine.
Answering `no_confirmation` or `keep` records the decision without
changing the spec.

## Answer

Answers are a JSON array:

```json
[
  { "id": "name", "value": "trattoria-booking" },
  { "id": "commit_gate:book_table" },
  { "id": "voice", "choice": "Kore" },
  { "id": "disclosure", "choice": "read", "value": "This call is with an automated assistant and may be recorded." }
]
```

When `choice` is omitted, a text question takes its only option and a
choice question takes its default. An answer fails if:

- the question is not open;
- the choice is not one of the options;
- the option needs a value and none was given;
- the result no longer deserializes as a spec.

When any answer fails, nothing is applied.

## Decisions

`decisions.json` maps each answered question id to the option chosen:

```json
{ "commit_gate:book_table": "confirm", "escalation": "none", "voice": "Kore" }
```

`plan` skips the ids in it. Most answers change the spec, so their
questions stop applying anyway. The decisions file is what keeps a
question that changed nothing, such as `escalation: none`, from being
asked again. Keep it next to `agent.json`. To ask a question again,
delete its entry.

## Limits

- `--write` writes object keys in sorted order, not the file's original
  order.
- Redaction keeps a value out of the journal, snapshots and extraction
  events. It does not redact transcript text. See
  [hardening](hardening.md) before you treat a spec as data-retention safe.
- Question rules run on full specs. A bare flow (`{"steps": [...]}`) has
  no questions.
