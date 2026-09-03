//! The README's Quickstart programs must be these crate's binaries, verbatim.
//!
//! Each README code fence sits under an HTML marker comment naming the file
//! it mirrors: `<!-- quickstart:src/bin/hello_text.rs -->`. This test fails
//! whenever the fence and the compiled file differ, so the README cannot
//! print a program that does not build.

use std::path::Path;

fn readme() -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../README.md");
    std::fs::read_to_string(root).expect("workspace README.md")
}

/// Extract the fenced block that immediately follows `<!-- quickstart:NAME -->`.
fn fenced_block(readme: &str, name: &str) -> String {
    let marker = format!("<!-- quickstart:{name} -->");
    let after = readme
        .split(&marker)
        .nth(1)
        .unwrap_or_else(|| panic!("README is missing the marker {marker}"));
    let open = after.find("```").expect("opening fence after marker") + 3;
    let after = &after[open..];
    let body_start = after.find('\n').expect("newline after fence info") + 1;
    let body = &after[body_start..];
    let close = body.find("```").expect("closing fence");
    body[..close].to_string()
}

#[test]
fn readme_quickstart_programs_are_the_compiled_binaries() {
    let readme = readme();
    for file in ["src/bin/hello_text.rs", "src/bin/hello_voice.rs"] {
        let printed = fenced_block(&readme, file);
        let compiled = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(file))
            .expect("quickstart binary source");
        assert_eq!(
            printed.trim_end(),
            compiled.trim_end(),
            "README block `{file}` differs from the compiled file — edit both together"
        );
    }
}

#[test]
fn readme_quickstart_manifest_names_the_required_dependencies() {
    let readme = readme();
    let manifest = fenced_block(&readme, "Cargo.toml");
    // `gemini-llm` is a default feature, so the base manifest need not name it;
    // `voice-io` is opt-in and lives in the voice-only block, checked below.
    for needle in ["gemini-adk-fluent-rs", "tokio", "macros", "rt-multi-thread"] {
        assert!(
            manifest.contains(needle),
            "README Cargo.toml block no longer names `{needle}` — \
             the quickstart will not compile without it"
        );
    }
}

/// The version a reader is told to depend on must be the version being shipped.
///
/// The other assertions here check that the manifest block *names* the right
/// crates and features, which is why `gemini-adk-fluent-rs = "1.0"` sat above
/// 2.0 programs through a whole major release: the snippet named the right
/// crate, so nothing complained, and a reader copying both halves got
/// `AgentBuilder::build(llm)?` — a `Result` in 2.0, an `Arc` in 1.0 — against a
/// 1.x dependency, which does not compile.
///
/// Only the major (and, before 1.0, the minor) is checked. A caret requirement
/// of `"2.0"` admits every 2.x, so the docs need not be touched for a patch
/// release; they must be touched for a breaking one.
#[test]
fn readme_documents_the_version_it_ships() {
    let workspace =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml"))
            .expect("workspace manifest");
    let shipped = workspace
        .lines()
        .find_map(|l| l.trim().strip_prefix("version = \""))
        .and_then(|v| v.split('"').next())
        .expect("[workspace.package] version");

    // "2.0.0" → "2.0"; "0.6.3" → "0.6". The compatibility range a caret
    // requirement in the docs has to name.
    let mut parts = shipped.split('.');
    let (major, minor) = (parts.next().expect("major"), parts.next().expect("minor"));
    let expected = format!("{major}.{minor}");

    let readme = readme();
    for block in ["Cargo.toml", "Cargo.toml:voice"] {
        let manifest = fenced_block(&readme, block);
        let line = manifest
            .lines()
            .find(|l| l.contains("gemini-adk-fluent-rs"))
            .unwrap_or_else(|| panic!("README block `{block}` no longer pins the crate"));
        assert!(
            line.contains(&format!("\"{expected}\"")),
            "README block `{block}` tells the reader to depend on a version that \
             is not the one being shipped ({shipped}). Expected `\"{expected}\"` in:\n  {line}"
        );
    }
}

/// The text path is advertised as needing no audio stack, and the crate backs
/// that up by gating `voice-io` behind an opt-in `voice` feature. If the
/// README's base manifest ever enables `voice-io` again, that promise silently
/// becomes false on every headless Linux box — so pin it here.
#[test]
fn readme_text_path_manifest_pulls_in_no_audio_stack() {
    let readme = readme();
    let base = fenced_block(&readme, "Cargo.toml");
    assert!(
        !base.contains("voice-io"),
        "the base README manifest must stay audio-free — `voice-io` belongs in \
         the voice-only block, or the text quickstart needs ALSA headers it \
         claims not to need"
    );

    let voice = fenced_block(&readme, "Cargo.toml:voice");
    assert!(
        voice.contains("voice-io"),
        "the voice README block must add `voice-io`"
    );

    let manifest = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"),
    )
    .expect("quickstart manifest");
    assert!(
        manifest.contains("voice = [\"gemini-adk-fluent-rs/voice-io\"]"),
        "`voice-io` must stay behind this crate's opt-in `voice` feature"
    );
    assert!(
        !manifest.contains("\"voice-io\",\n]"),
        "`voice-io` must not be a default feature of the quickstart crate"
    );
}
