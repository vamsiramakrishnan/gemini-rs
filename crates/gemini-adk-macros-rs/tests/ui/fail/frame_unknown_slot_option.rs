//! `#[derive(Frame)]` rejects an unknown `#[slot(..)]` option.

use gemini_adk_macros_rs::Frame;

#[derive(Frame)]
#[allow(dead_code, reason = "the trybuild fixture only has to compile")]
struct Booking {
    #[slot(prompt = "Name?", required)]
    name: String,
}

fn main() {}
