//! Drift guard for the vendored web assets.
//!
//! `adk web` embeds `assets/web/` (rust-embed folders must live inside the
//! crate, or `cargo publish` ships a tarball where the derive resolves to an
//! empty set — the v1.0.0 release failed its publish verify exactly this way).
//! The canonical sources stay in `apps/gemini-adk-web-rs/static/`; this test
//! fails the build the moment the two trees differ.
//!
//! To resync after editing the web app:
//!     rm -rf tools/gemini-adk-cli-rs/assets/web
//!     cp -r apps/gemini-adk-web-rs/static tools/gemini-adk-cli-rs/assets/web

use std::collections::BTreeMap;
use std::path::Path;

fn collect(dir: &Path, root: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
    for entry in std::fs::read_dir(dir).expect("readable asset dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            collect(&path, root, out);
        } else {
            let rel = path
                .strip_prefix(root)
                .expect("path under root")
                .to_string_lossy()
                .replace('\\', "/");
            out.insert(rel, std::fs::read(&path).expect("readable asset file"));
        }
    }
}

#[test]
fn vendored_web_assets_match_source() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = manifest_dir.join("../../apps/gemini-adk-web-rs/static");
    if !source.exists() {
        // Published-package context: the workspace app is not in the tarball,
        // so there is nothing to compare against. The vendored copy is the
        // only copy, and that is exactly the point.
        eprintln!("source static dir absent (packaged build) — skipping drift check");
        return;
    }
    let vendored = manifest_dir.join("assets/web");

    let mut src = BTreeMap::new();
    let mut ven = BTreeMap::new();
    collect(&source, &source, &mut src);
    collect(&vendored, &vendored, &mut ven);

    let missing: Vec<_> = src.keys().filter(|k| !ven.contains_key(*k)).collect();
    let extra: Vec<_> = ven.keys().filter(|k| !src.contains_key(*k)).collect();
    let changed: Vec<_> = src
        .iter()
        .filter(|(k, v)| ven.get(*k).is_some_and(|b| b != *v))
        .map(|(k, _)| k)
        .collect();

    assert!(
        missing.is_empty() && extra.is_empty() && changed.is_empty(),
        "vendored assets/web drifted from apps/gemini-adk-web-rs/static\n\
         missing from vendored: {missing:?}\nextra in vendored: {extra:?}\nchanged: {changed:?}\n\
         resync with:\n  rm -rf tools/gemini-adk-cli-rs/assets/web\n  \
         cp -r apps/gemini-adk-web-rs/static tools/gemini-adk-cli-rs/assets/web"
    );
}
