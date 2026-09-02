//! Integration tests for the `#[derive(Frame)]` macro.
//!
//! Proc-macro crates can only be tested through a downstream crate, so these
//! live in `tests/` and exercise the generated code against the real
//! `gemini-adk-rs` crate graph.

use gemini_adk_rs::Frame;
use gemini_adk_rs::frame::{ConfirmPolicy, SlotRecognizer, SlotValidator}; // brings both the `Frame` trait and the `#[derive(Frame)]` macro

#[derive(Frame)]
#[frame(name = "booking")]
struct Booking {
    #[slot(
        prompt = "For how many people?",
        confirm = "low_confidence",
        min = 1,
        max = 12
    )]
    #[recognize(integer_near = ["people", "guests", "party"])]
    party_size: u8,
    #[slot(
        prompt = "What day and time?",
        reprompt = "When would you like to come in?"
    )]
    #[slot(state = "when")]
    #[recognize(datetime)]
    slot: String,
    #[slot(prompt = "Name for the reservation?", pii)]
    name: String,
}

#[test]
fn derives_frame_spec_with_metadata() {
    let spec = Booking::frame();
    assert_eq!(spec.name, "booking");
    // Slot keys, in declaration order, honoring the `state` override.
    assert_eq!(spec.slot_keys(), vec!["party_size", "when", "name"]);

    let party = spec.slot("party_size").unwrap();
    assert_eq!(party.prompt.as_deref(), Some("For how many people?"));
    assert_eq!(party.confirm, ConfirmPolicy::LowConfidence);
    assert!(!party.pii);

    let when = spec.slot("slot").unwrap();
    assert_eq!(when.state_key, "when");
    assert_eq!(
        when.reprompt.as_deref(),
        Some("When would you like to come in?")
    );

    let name = spec.slot("name").unwrap();
    assert!(name.pii);
    assert_eq!(name.confirm, ConfirmPolicy::Never); // default

    // Recognizers parsed from `#[recognize(..)]`.
    assert_eq!(
        party.recognizer,
        Some(SlotRecognizer::IntegerNear(vec![
            "people".into(),
            "guests".into(),
            "party".into()
        ]))
    );
    assert_eq!(when.recognizer, Some(SlotRecognizer::DateTime));
    assert_eq!(name.recognizer, None); // no #[recognize]

    // `min`/`max` lower to a Range validator.
    assert_eq!(
        party.validate,
        Some(SlotValidator::Range {
            min: Some(1.0),
            max: Some(12.0)
        })
    );

    // The recognizer-bearing slots lower to an extractor.
    assert!(Booking::frame().to_extract().is_some());
}

#[derive(Frame)]
struct Bare {
    a: u8,
    b: u8,
}

#[test]
fn default_name_and_bare_slots() {
    let spec = Bare::frame();
    assert_eq!(spec.name, "bare"); // snake_case of the struct name
    assert_eq!(spec.slot_keys(), vec!["a", "b"]);
}
