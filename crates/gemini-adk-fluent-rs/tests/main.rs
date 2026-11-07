//! One test binary for this crate's integration tests: each module is
//! one former `tests/*.rs` file. Linking one binary instead of one per
//! file is most of the build-time saving in `cargo test`.

mod common;

mod debt_collection_eval;
mod decision_eval;
mod decision_latency;
mod docsrs_features;
mod extraction_latency;
mod integration;
mod policies;
mod scenario_from_recording;
mod scripted_session;
mod session_lifecycle;
mod skill_examples;
mod skills_code_live;
mod skills_live;
mod spec_round_trip_props;
mod task_services_session;
mod task_session;
mod verbatim;
mod voice_timing;
