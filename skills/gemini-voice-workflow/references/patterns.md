# Stage patterns

Each pattern is a fragment of `conversation.stages` (plus the tools,
extractors and policies it needs) that has been run through `adk spec check`
and `adk spec test`. Rename the ids and keys to fit the call; keep the shape.

Contents: [Verify identity](#verify-identity) ·
[Gates that survive corrections](#gates-that-survive-corrections) ·
[Collect, read back, commit after a yes](#collect-read-back-commit-after-a-yes) ·
[Hand off to a person](#hand-off-to-a-person) ·
[Disclosure](#disclosure) · [Slow tools](#slow-tools) ·
[Side questions](#side-questions)

## Verify identity

Nothing about the caller is read out until a tool has confirmed who they are,
and nothing unlocks while that tool is still an unimplemented stub.

```json
{
  "id": "verify",
  "say": "Ask for the caller's name and date of birth and check them with verify_patient. Do not repeat the date back.",
  "collect": ["patient_name", "date_of_birth"],
  "allow": ["verify_patient"],
  "done": { "eq": ["identity", { "verified": true }] },
  "repair": { "escalate_after_tool_failures": 2, "escalate_to": "handoff_unverified" },
  "next": [{ "to": "appointments", "when": { "eq": ["identity", { "verified": true }] } }]
},
{
  "id": "appointments",
  "say": "Read out the caller's upcoming appointments.",
  "allow": ["list_appointments"],
  "commit": { "tool": "list_appointments", "when": { "eq": ["identity", { "verified": true }] } },
  "next": [{ "to": "find_time", "when": { "called_ok": "list_appointments" } }]
},
{ "id": "handoff_unverified", "say": "Say a member of staff will call back.", "terminal": true }
```

```json
"tools": [
  { "name": "verify_patient",
    "description": "Check the caller's name and date of birth against the patient record. Returns {\"verified\": true} only on an exact match.",
    "parameters": { "type": "object", "properties": { "name": { "type": "string" }, "date_of_birth": { "type": "string" } }, "required": ["name", "date_of_birth"] },
    "response": { "verified": false },
    "save_response_as": "identity" },
  { "name": "list_appointments", "description": "List the verified caller's upcoming appointments." }
],
"conversation": { "policies": [{ "kind": "redact", "keys": ["patient_name", "date_of_birth"] }] }
```

Why each piece is there:

- **The verdict is the tool's response, compared exactly.** The mock
  `response` is a failing answer and codegen's stub returns it until the real
  tool is written, so an unimplemented verifier verifies nobody. Gating on
  "the tool ran" (`called_ok`) or on a `set_state` flag would verify every
  caller until then. The real tool returns `{"verified": true}` only on a
  match. Don't bind it straight to HTTP unless the endpoint's body is exactly
  that on success, since an HTTP call counts as succeeded whatever its status.
- **`done` on the verdict.** Without it the stage completes as soon as the
  slots are captured, verified or not. Its only exit then waits on the
  verdict, and the call is left with no active stage.
- **`commit` on the tool that reveals data**, not just `allow`. An `allow`
  list only restricts while its stage is active; after an escalation or a
  dead end nothing is active and every ungated tool is callable. A commit
  guard holds everywhere.
- **Its own handoff stage.** Each stage that escalates gets its own
  `escalate_to` target; see [Hand off](#hand-off-to-a-person).
- **`redact`** keeps the name and date of birth out of the journal and
  snapshots.

Later commit guards should not repeat `{"eq": ["identity", ...]}`: see
[Gates that survive corrections](#gates-that-survive-corrections).

## Gates that survive corrections

When the caller corrects a slot, the runtime clears every state key read by
the commit guards of the stages after it, so those confirmations are asked
for again. That is what you want for the confirmation flag. It also clears
any other state check in those guards, such as a saved verification verdict,
and the commit can then never pass.

So in a commit guard that comes after a correctable stage, use a state key
only for the confirmation itself, and express "this already happened" with
`called_ok` (a tool that could only run once the prerequisite held) or with
`done` of a stage that has no `repair` (an escalation also completes a
stage):

```json
"commit": { "tool": "book_cleaning", "when": { "all": [
  { "called_ok": "list_appointments" },
  { "is_true": "book_cleaning_confirmed" } ] } }
```

`list_appointments` is itself gated on the verdict, so its success proves the
caller was verified, and a correction does not clear it.

## Collect, read back, commit after a yes

```json
{
  "id": "confirm",
  "say": "Read back the party size, time and name, ask the caller to confirm, then book. Say the table is booked only after book_table has returned.",
  "ground": "Party of {party_size} at {slot}, under {guest_name}.",
  "allow": ["book_table"],
  "commit": { "tool": "book_table", "when": { "is_true": "book_table_confirmed" } },
  "next": [{ "to": "done", "when": { "called_ok": "book_table" } }]
}
```

The confirmation key comes from the caller's words, through the signals
extractor:

```json
{
  "name": "caller_signals",
  "instruction": "Judge only the caller's last turn; the turn before it is context. Set a field to true only when the caller clearly said so in their own words. Otherwise leave it null.",
  "window": 2,
  "schema": { "type": "object", "properties": {
    "book_table_confirmed": { "type": "boolean", "description": "The caller agreed to the booking after hearing the details read back. Choosing a time or giving a detail is not agreeing." } } },
  "promote": [{ "field": "book_table_confirmed", "policy": "true_only" }]
}
```

`"window": 2` is the agent's read-back and the caller's reply. With the
default of 3, a "yes" from before a correction can still be in the window
and set the confirmation again after the runtime cleared it.

The caller often says yes and the model calls the commit tool in the same
turn, before that turn is extracted. The runtime handles it: when a tool is
refused only because its commit guard has not held yet, it extracts the turn
in progress and decides again.

Models do announce bookings that never happened, especially after a refused
call. The `say` above asks for the booking to be reported only once the tool
has returned, and the terminal stage should read its details from the tool's
result (`save_response_as`), not restate the request.

`ground` puts the collected values in front of the model so the read-back is
accurate. If the caller corrects a slot collected earlier, the runtime clears
`book_table_confirmed` and the read-back happens again. The slots themselves
need their own extractor (`promote` with `"policy": "overwrite"` so a
correction replaces the old value); see the worked example in
spec-language.md.

Add `{"kind": "commit", "tool": "book_table", "idempotency_key":
"{guest_name}-{slot}"}` to `policies` when a retry must not book twice.

## Hand off to a person

Two routes, usually both: the caller asks for a person, and a stage stalls.

**On request**, use a digression whose one stage allows only a transfer tool
and waits for it:

```json
"tools": [{ "name": "handoff_to_staff", "description": "Transfer the caller to a member of staff." }],
"conversation": {
  "overlays": [{
    "name": "handoff",
    "trigger": { "is_true": "intent:human_agent" },
    "stages": [{
      "id": "transfer",
      "say": "Tell the caller you are passing them to a member of staff, then call handoff_to_staff.",
      "allow": ["handoff_to_staff"],
      "done": { "called_ok": "handoff_to_staff" }
    }],
    "resume": "terminate"
  }]
}
```

with a `caller_signals` field promoted to `intent:human_agent`:

```json
{ "field": "intent_human_agent", "to": "intent:human_agent", "policy": "true_only" }
```

While a digression drives, its own stages decide which tools are admitted;
the main flow's commit guards do not apply. A digression whose stage is
`terminal` (and the built-in `{"kind": "safety_handoff"}` policy, which
lowers to one) restricts nothing on the turn it is entered, so every tool,
booking included, is callable for that turn. The stage above keeps the
transfer tool as the only one admitted until it runs, then the conversation
ends and every tool is denied. A digression without `require` waits for its
terminal stages, or with none marked terminal for its last stages, so the
model offering the transfer before making it doesn't end the call. `adk spec plan`'s `escalation` answer writes
this shape.

**On stalls**, repair escalation to a terminal stage. A turn counts toward
`reprompt_after` and `escalate_after` when the caller spoke in it (or the
model reprompted a silent caller). The greeting and the model's reply to a
tool result don't count. Leave room for the stage's normal work: a stage
collecting three slots can take three answers before anything is wrong, so
`escalate_after` of 5 or more.

```json
"repair": { "reprompt_after": 2, "escalate_after": 4, "escalate_to": "handoff_collect" }
...
{ "id": "handoff_collect", "say": "Apologise and tell the caller a member of staff will call them back.", "terminal": true }
```

Give every escalating stage its own target. A stage that two stages escalate
to waits for both: when only one escalates, the handoff is never reached and
the call is left with no active stage. Three stages that can stall need
three handoff stages (they can say the same thing). A terminal stage leaves
no stage active once reached, so every tool without a commit guard is
callable afterwards; that is another reason to gate data and changes with
`commit`.

The runtime does not transfer or hang up. The app implements
`handoff_to_staff` and watches `flow:terminated` or the handoff stages. Say
in `brief.md` what the app should do then.

## Disclosure

```json
{ "id": "greet", "verbatim": "This call may be recorded. You are speaking with an automated assistant.", ... }
```

Put it on the first stage. The first stage's `say` and verbatim text reach
the model before the greeting, so a `greeting` such as "Say the disclosure
line, then ask how you can help" works. A verbatim stage does not complete,
and does not escalate, until its text has been said within one turn; extra
words around it are fine. Add `"timing": {"interruptible": false}` to the
stage if the caller must not be able to talk over it.

## Slow tools

```json
"timing": { "filler_after_ms": 1200 }
```

on the stage that calls a tool taking more than a second: the agent is cued
to say a filler line instead of going silent. For tools that take many
seconds, declare the tool with `"background": true` so the model keeps
talking while it runs (Google AI, and Gemini 3.8 Live on Vertex AI).

## Side questions

A caller asking opening hours mid-booking does not need a digression: put the
facts in the `instruction` and the model answers in place. Use a digression
(`overlays`) only when the side path has its own stages or tools. Its trigger
needs a writer, and a `previous`/`restart` digression re-enters while the
trigger stays true, so prefer `"resume": "terminate"` for signal-triggered
exits like cancelling.
