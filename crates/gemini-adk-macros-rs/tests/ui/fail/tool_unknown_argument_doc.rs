//! A `# Arguments` item must name a real parameter, so the documentation the
//! model reads cannot drift from the signature.

use gemini_adk_macros_rs::tool;

/// Look up a city.
///
/// # Arguments
///
/// * `town` - The town to look up.
#[tool]
async fn lookup(city: String) -> String {
    city
}

fn main() {}
