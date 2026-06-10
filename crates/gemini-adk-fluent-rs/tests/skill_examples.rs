//! Validates the JSON examples shipped in the `conversation-from-script` skill
//! (`.claude/skills/conversation-from-script/SKILL.md`) so the authoring guidance
//! cannot drift from what the compiler/runtime actually accept.

use gemini_adk_fluent_rs::prelude::*;

/// The `ConversationSpec` JSON from the skill.
const SPEC_JSON: &str = r#"
{
  "name": "booking",
  "stages": [
    {
      "id": "collect",
      "say": "Help the user book a table.",
      "collect": ["party_size", "slot"],
      "next": [{ "to": "confirm", "when": { "captured": ["party_size", "slot"] } }],
      "repair": { "reprompt_after": 2, "escalate_after": 4, "escalate_to": "handoff" }
    },
    {
      "id": "confirm",
      "ground": "Party of {party_size} at {slot}.",
      "allow": ["book"],
      "commit": { "tool": "book", "when": { "is_true": "user_confirmed" } },
      "next": [{ "to": "done", "when": { "called_ok": "book" } }]
    },
    { "id": "done", "terminal": true },
    { "id": "handoff", "terminal": true }
  ],
  "require": ["done"],
  "overlays": [
    {
      "name": "faq",
      "trigger": { "is_true": "intent:faq" },
      "stages": [
        { "id": "answer", "done": { "is_true": "faq_answered" },
          "next": [{ "to": "faq_end", "when": { "is_true": "faq_answered" } }] },
        { "id": "faq_end", "terminal": true }
      ],
      "resume": "previous"
    }
  ],
  "policies": [
    { "kind": "redact", "keys": ["card_number"] },
    { "kind": "safety_handoff", "intents": ["self_harm", "abuse"] }
  ]
}
"#;

/// The happy-path `Scenario` JSON from the skill.
const SCENARIO_JSON: &str = r#"
{
  "name": "happy_path",
  "steps": [
    { "expect_active": ["collect"] },
    { "expect_denied": "book" },
    { "set": { "key": "party_size", "value": 4 } },
    { "set": { "key": "slot", "value": "tomorrow 7pm" } },
    "turn",
    { "expect_active": ["confirm"] },
    { "set": { "key": "user_confirmed", "value": true } },
    "turn",
    { "expect_allowed": "book" },
    { "tool_ok": "book" },
    "expect_complete"
  ]
}
"#;

#[tokio::test]
async fn skill_spec_and_scenario_are_valid() {
    let spec: ConversationSpec = serde_json::from_str(SPEC_JSON).expect("spec parses");
    let convo = Conversation::from_spec(spec).expect("spec compiles");

    // The compiled artifact matches what the skill documents.
    assert!(convo.flow().tool_policy().tools.contains("book"));
    assert!(convo.overlays().iter().any(|o| o.name == "faq"));
    assert!(convo.overlays().iter().any(|o| o.name == "safety")); // from safety_handoff
    assert!(convo.redacted_fields().contains("card_number"));

    let scenario: Scenario = serde_json::from_str(SCENARIO_JSON).expect("scenario parses");
    scenario
        .run(&convo, FlowMode::Enforce)
        .await
        .expect("happy-path scenario passes");
}
