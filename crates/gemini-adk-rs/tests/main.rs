//! One test binary for this crate's integration tests: each module is
//! one former `tests/*.rs` file. Linking one binary instead of one per
//! file is most of the build-time saving in `cargo test`.

mod flow_guard_props;
mod integration;
mod replay_closed_loop;
mod replay_tape;
mod teardown_hooks;
