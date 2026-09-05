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

/// The version a reader is told to depend on must accept the version shipped.
///
/// The other assertions here check that the manifest block *names* the right
/// crates and features, which is why `gemini-adk-fluent-rs = "1.0"` sat above
/// 2.0 programs through a whole major release: the snippet named the right
/// crate, so nothing complained, and a reader copying both halves got
/// `AgentBuilder::build(llm)?` — a `Result` in 2.0, an `Arc` in 1.0 — against a
/// 1.x dependency, which does not compile.
///
/// Compatibility, not equality: a caret requirement of `"2.0"` already admits
/// every 2.x, so `2.1.0` needs no doc edit and demanding `"2.1"` would fail
/// every minor release — including inside `release.sh`, which bumps the
/// manifest and then runs this suite. Only the major is compared, except below
/// 1.0 where cargo treats the minor as the breaking component.
#[test]
fn readme_documents_a_version_that_accepts_what_it_ships() {
    let workspace =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml"))
            .expect("workspace manifest");
    let shipped = workspace
        .lines()
        .find_map(|l| l.trim().strip_prefix("version = \""))
        .and_then(|v| v.split('"').next())
        .expect("[workspace.package] version");

    let readme = readme();
    for block in ["Cargo.toml", "Cargo.toml:voice"] {
        let manifest = fenced_block(&readme, block);
        let line = manifest
            .lines()
            .find(|l| l.contains("gemini-adk-fluent-rs"))
            .unwrap_or_else(|| panic!("README block `{block}` no longer pins the crate"));
        let documented = quoted_version(line).unwrap_or_else(|| {
            panic!("README block `{block}` names no version for the crate:\n  {line}")
        });
        assert!(
            caret_admits(&documented, shipped),
            "README block `{block}` documents `{documented}`, which does not admit the \
             version being shipped ({shipped}):\n  {line}"
        );
    }
}

/// The version a reader sees before any code: the README's `**vX.Y · MIT**`
/// line and the book's hero eyebrow. Both said 1.0 for a day after 2.0.0
/// shipped — the manifest check above could not see them, because they are
/// prose, not a dependency line. Here they must name the shipped major.minor.
#[test]
fn the_version_readers_see_first_is_the_one_that_shipped() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let workspace = std::fs::read_to_string(root.join("Cargo.toml")).expect("workspace manifest");
    let shipped = workspace
        .lines()
        .find_map(|l| l.trim().strip_prefix("version = \""))
        .and_then(|v| v.split('"').next())
        .expect("[workspace.package] version");
    let major_minor = shipped
        .rsplit_once('.')
        .map(|(mm, _)| mm)
        .unwrap_or(shipped);

    let readme = readme();
    let eyebrow = readme
        .lines()
        .find(|l| l.starts_with("**v"))
        .expect("README has a `**vX.Y · …**` line under the badges");
    assert!(
        eyebrow.starts_with(&format!("**v{major_minor} ·")),
        "README says {eyebrow:?} but the workspace ships {shipped}"
    );

    let hero = std::fs::read_to_string(root.join("apps/docs/src/content/docs/index.mdx"))
        .expect("site landing page");
    let hero_line = hero
        .lines()
        .find(|l| l.contains("hero-eyebrow"))
        .expect("site landing has a hero-eyebrow");
    assert!(
        hero_line.contains(&format!(">v{major_minor} ·")),
        "book hero says {:?} but the workspace ships {shipped}",
        hero_line.trim()
    );
}

/// The first double-quoted token on the line that starts with a digit — the
/// version, whether the line is `foo = "2.0"` or `foo = {{ version = "2.0", .. }}`.
fn quoted_version(line: &str) -> Option<String> {
    line.split('"')
        .skip(1)
        .step_by(2)
        .find(|t| t.starts_with(|c: char| c.is_ascii_digit()))
        .map(str::to_string)
}

/// Does a caret requirement of `req` accept version `shipped`?
///
/// Cargo's rule: the leftmost non-zero component is the breaking one, so `"1.2"`
/// admits any 1.x at or above 1.2, and `"0.6"` admits only 0.6.x. Comparing
/// that component is enough here — the docs name a floor, and a release only
/// ever moves the version up.
fn caret_admits(req: &str, shipped: &str) -> bool {
    fn parts(v: &str) -> (u64, u64) {
        let mut it = v.split(['.', '-']).filter_map(|p| p.parse::<u64>().ok());
        (it.next().unwrap_or(0), it.next().unwrap_or(0))
    }
    let ((rmaj, rmin), (smaj, smin)) = (parts(req), parts(shipped));
    if smaj > 0 {
        rmaj == smaj
    } else {
        rmaj == 0 && rmin == smin
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
