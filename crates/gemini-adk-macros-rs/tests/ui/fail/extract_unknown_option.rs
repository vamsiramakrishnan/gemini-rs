//! `#[derive(Extract)]` rejects an unknown container `#[extract(..)]` option.

use gemini_adk_macros_rs::Extract;

#[derive(Extract)]
#[extract(window_size = 5)]
#[allow(dead_code)]
struct Order {
    #[recognize(integer)]
    quantity: Option<i64>,
}

fn main() {}
