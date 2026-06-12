//! Compile-fail UI tests for macro diagnostics, plus compile-pass anchors.
//!
//! Each `tests/ui/fail/*.rs` fixture must fail to compile with exactly the
//! diagnostics recorded in its committed `.stderr` snapshot; each
//! `tests/ui/pass/*.rs` fixture must compile and run. The snapshots are
//! rustc-version sensitive — regenerate with:
//!
//! ```text
//! TRYBUILD=overwrite cargo test -p gemini-adk-macros-rs --test ui
//! ```

#[test]
fn ui() {
    let t = trybuild::TestCases::new();
    t.pass("tests/ui/pass/*.rs");
    t.compile_fail("tests/ui/fail/*.rs");
}
