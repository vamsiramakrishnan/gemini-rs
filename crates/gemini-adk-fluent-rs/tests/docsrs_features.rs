//! docs.rs is told which features to document by an explicit list in this
//! crate's manifest, because `all-features` would pull `voice-io` → `cpal` →
//! ALSA headers the docs.rs builder does not have. An explicit list drifts:
//! the next feature someone adds is documented nowhere unless they also
//! remember this table. This test is the reminder.

use std::collections::BTreeSet;

const MANIFEST: &str = include_str!("../Cargo.toml");

/// Features docs.rs must not enable, with the reason it must not.
const EXCLUDED: &[(&str, &str)] = &[("voice-io", "cpal needs libasound headers at build time")];

/// Names declared under `[features]`.
fn declared_features() -> BTreeSet<String> {
    section(MANIFEST, "[features]")
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            if l.starts_with('#') || l.is_empty() {
                return None;
            }
            let (name, _) = l.split_once('=')?;
            Some(name.trim().trim_matches('"').to_string())
        })
        .collect()
}

/// Names inside `features = [...]` under `[package.metadata.docs.rs]`.
fn docsrs_features() -> BTreeSet<String> {
    let sec = section(MANIFEST, "[package.metadata.docs.rs]");
    let line = sec
        .lines()
        .find(|l| l.trim_start().starts_with("features"))
        .expect("docs.rs metadata declares an explicit `features = [...]` list");
    let inner = &line[line.find('[').unwrap() + 1..line.rfind(']').unwrap()];
    inner
        .split(',')
        .map(|s| s.trim().trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// The body of one TOML table: from its header to the next `[` header.
fn section<'a>(toml: &'a str, header: &str) -> &'a str {
    let start = toml
        .find(header)
        .unwrap_or_else(|| panic!("{header} is missing"))
        + header.len();
    let rest = &toml[start..];
    let end = rest.find("\n[").map_or(rest.len(), |i| i + 1);
    &rest[..end]
}

#[test]
fn docsrs_documents_every_feature_it_can_build() {
    let declared = declared_features();
    let documented = docsrs_features();
    let excluded: BTreeSet<String> = EXCLUDED.iter().map(|(n, _)| n.to_string()).collect();

    for (name, why) in EXCLUDED {
        assert!(
            declared.contains(*name),
            "excluded feature `{name}` no longer exists ({why})"
        );
        assert!(
            !documented.contains(*name),
            "`{name}` is on the docs.rs list but cannot build there: {why}"
        );
    }

    let expected: BTreeSet<String> = declared.difference(&excluded).cloned().collect();
    let missing: Vec<_> = expected.difference(&documented).collect();
    let unknown: Vec<_> = documented.difference(&declared).collect();
    assert!(
        missing.is_empty(),
        "declared but not on the docs.rs list — add them or move them to EXCLUDED with a reason: {missing:?}"
    );
    assert!(
        unknown.is_empty(),
        "on the docs.rs list but not declared under [features]: {unknown:?}"
    );
}
