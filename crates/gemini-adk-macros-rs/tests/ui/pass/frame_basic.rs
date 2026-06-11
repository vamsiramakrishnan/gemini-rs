//! Anchor: a well-formed `#[derive(Frame)]` struct expands and compiles.

use gemini_adk_macros_rs::Frame;

#[derive(Frame)]
#[frame(name = "booking")]
#[allow(dead_code)]
struct Booking {
    #[slot(
        prompt = "For how many people?",
        confirm = "low_confidence",
        min = 1,
        max = 12
    )]
    #[recognize(integer_near = ["people", "guests"])]
    party_size: u8,
    #[slot(prompt = "Name for the reservation?", pii, non_empty)]
    name: String,
}

fn main() {
    let spec = <Booking as gemini_adk_rs::frame::Frame>::frame();
    assert_eq!(spec.name, "booking");
}
