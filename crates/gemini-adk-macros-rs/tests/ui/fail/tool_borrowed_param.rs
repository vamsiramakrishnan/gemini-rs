//! Tool arguments are deserialized from the model's JSON, so they are owned.

use gemini_adk_macros_rs::tool;

/// Echo a word.
#[tool]
async fn echo(word: &str) -> String {
    word.to_string()
}

fn main() {}
