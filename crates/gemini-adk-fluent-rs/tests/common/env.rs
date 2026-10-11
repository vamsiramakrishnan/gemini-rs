//! Credentials for the live suites.

/// A variable from the environment, else from `.env.local` at the
/// repository root (git-ignored). Values are never printed.
pub fn env_or_local(key: &str) -> Option<String> {
    if let Some(v) = std::env::var(key).ok().filter(|v| !v.trim().is_empty()) {
        return Some(v);
    }
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.env.local");
    let text = std::fs::read_to_string(path).ok()?;
    text.lines().find_map(|line| {
        let (k, v) = line.trim().split_once('=')?;
        (k.trim() == key).then(|| v.trim().trim_matches('"').trim_matches('\'').to_string())
    })
}
