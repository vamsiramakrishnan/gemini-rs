//! Shared harness for the live evaluation suites.

#![allow(
    dead_code,
    reason = "shared test helpers: each test binary uses a subset"
)]

pub mod env;
pub mod evaluate;
pub mod live;
pub mod report;
pub mod scenario;
pub mod voice;
