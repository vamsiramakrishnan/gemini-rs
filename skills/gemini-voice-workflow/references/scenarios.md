# Scenarios: proving the flow offline

A scenario is a script of caller events and assertions that runs against the
conversation with no model and no audio. `adk spec test agent.json` runs
every scenario in the spec's `scenarios` array, plus its embedded `tests`.

```json
"scenarios": [
  {
    "name": "never books without a yes",
    "steps": [
      { "set": { "key": "party_size", "value": 2 } },
      { "set": { "key": "slot", "value": "2026-10-15T20:00" } },
      "turn",
      { "expect_active": ["confirm"] },
      { "expect_denied": "book_table" }
    ]
  }
]
```

## Steps

| Step | What it does |
|---|---|
| `{"set": {"key", "value"}}` | Writes state, standing in for an extractor or a tool's `set_state` |
| `{"remove": {"key"}}` | Removes a key |
| `"turn"` | A turn boundary with no input |
| `{"user": "text"}` | The caller speaks, then a turn advances. Only frame recognizers read the text; with plain `collect` slots, use `set` |
| `{"tool_ok": "name"}` | The tool succeeds: `called_ok` holds. Does **not** check that the tool is admitted, and does not apply `set_state` or `save_response_as` |
| `{"tool_failed": "name"}` | The tool fails; counts toward `escalate_after_tool_failures` |
| `{"tool_result": {"tool", "ok"}}` | A completed call observed without advancing a turn |
| `{"schedule_tool": {"tool", "after"}}` | The tool succeeds after N turns |
| `"interrupt"` | The caller barges in; counts toward `escalate_after_interruptions` |
| `{"expect_active": [stages]}` | These stages are active |
| `{"expect_allowed": "tool"}` / `{"expect_denied": "tool"}` | The tool is admitted / refused right now |
| `{"expect_slot": {"key", "value"}}` | A state key has this value |
| `"expect_complete"` | Every `require`d stage is done |

## Timing rules that trip people up

- **`tool_ok` doesn't check admission.** It succeeds even for a denied
  tool, so a scenario can "book" through a closed gate and still reach
  `expect_complete`. Put `{"expect_allowed": "tool"}` before every `tool_ok`.
- **`tool_ok` doesn't write state.** If a stage waits on a key the tool's
  `set_state` or `save_response_as` writes, add the `set` yourself after
  `tool_ok`. Embedded `tests` (`{"tool": "name"}` events) do apply the
  declared tool's effects, including its mock `response`.
- **A verbatim stage waits for its line.** Set `verbatim:{stage}` to `true`
  to stand in for the agent having said it; until then the stage neither
  completes nor escalates.
- **A handoff digression starts on the next turn.** Set the intent, then a
  `"turn"`: the digression now drives. With the handoff pattern, only the
  transfer tool is admitted; after `tool_ok` on it and another `"turn"`,
  the conversation has ended and every tool is denied. (A `safety_handoff`
  policy instead admits every tool on that first turn.) `flow:terminated` is
  not visible to scenarios, so assert with `expect_denied`.
- **Repair escalation is counted in turns.** With `escalate_after: 4`, four
  `"turn"`s raise `repair:{stage}:escalate`. A terminal `escalate_to` stage
  completes as soon as it is reached, so assert the signal with
  `expect_slot` rather than `expect_active`, then assert what must stay
  closed (`expect_denied` on data and commit tools).
- **Check that each handoff is reachable on its own.** A stage that several
  stages escalate to waits for all of them; a scenario that escalates from
  one stage and then expects tools denied catches it.

## What to cover

For each promise in the brief, one scenario that shows it happening and,
for every gate, one that shows it holding:

1. **The happy path** to `expect_complete`.
2. **Each commit tool denied** before its confirmation key, in the stage that
   allows it.
3. **Each verification holding**: data-revealing tools denied while the key
   is unset, including after a failed check.
4. **Each handoff route**: the intent, and repair escalation after stalls or
   tool failures.
5. **Corrections**, when the brief mentions them: a slot changed after the
   read-back makes the commit tool denied again.

Name scenarios for the behaviour ("never reads appointments before
verification"), not the mechanics. When one fails, the failure line names the
step and what was found instead; fix the spec, not the expectation, unless the
expectation was wrong about the brief.
