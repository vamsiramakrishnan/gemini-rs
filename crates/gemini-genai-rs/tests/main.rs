//! One test binary for this crate's integration tests: each module is
//! one former `tests/*.rs` file. Linking one binary instead of one per
//! file is most of the build-time saving in `cargo test`.

mod golden_wire;
mod mock_session;
mod prelude_exports;
