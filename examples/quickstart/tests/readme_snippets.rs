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
    // `gemini-llm` is a default feature, so the manifest need not name it;
    // `voice-io` is opt-in and the voice program will not compile without it.
    for needle in [
        "gemini-adk-fluent-rs",
        "voice-io",
        "tokio",
        "macros",
        "rt-multi-thread",
    ] {
        assert!(
            manifest.contains(needle),
            "README Cargo.toml block no longer names `{needle}` — \
             the quickstart will not compile without it"
        );
    }
}
