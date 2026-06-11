//! `#[derive(Frame)]` rejects an invalid `confirm` policy value.

use gemini_adk_macros_rs::Frame;

#[derive(Frame)]
#[allow(dead_code)]
struct Booking {
    #[slot(prompt = "Name?", confirm = "sometimes")]
    name: String,
}

fn main() {}
