//! `#[derive(Extract)]` rejects an unknown `#[recognize(..)]` name.

use gemini_adk_macros_rs::Extract;

#[derive(Extract)]
#[allow(dead_code)]
struct Order {
    #[recognize(bogus)]
    quantity: Option<i64>,
}

fn main() {}
