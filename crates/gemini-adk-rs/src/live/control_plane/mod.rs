//! Control plane submodules — extracted from `processor.rs` for readability.
//!
//! All functions here are internal to the processor and not part of the public API.

mod extractors;
mod lifecycle;
mod main_loop;
mod tool_gate;
mod tool_handler;

pub(super) use main_loop::run_control_lane;

/// Dispatch an async callback respecting its [`ExecutionMode`](super::ExecutionMode).
///
/// - [`Blocking`](super::ExecutionMode::Blocking): awaits the callback inline.
/// - [`Concurrent`](super::ExecutionMode::Concurrent): spawns as a detached tokio task.
macro_rules! dispatch_callback {
    ($mode:expr_2021, $cb:expr_2021) => {
        match $mode {
            $crate::live::ExecutionMode::Blocking => {
                $cb.await;
            }
            $crate::live::ExecutionMode::Concurrent => {
                let f = $cb;
                tokio::spawn(async move {
                    f.await;
                });
            }
        }
    };
}

pub(super) use dispatch_callback;
