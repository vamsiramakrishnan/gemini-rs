//! Control plane submodules — extracted from `processor.rs` for readability.
//!
//! All functions here are internal to the processor and not part of the public API.

mod extractors;
mod lifecycle;
mod main_loop;
mod tool_handler;

pub(super) use main_loop::run_control_lane;

/// Dispatch an async callback respecting its [`CallbackMode`].
///
/// - [`Blocking`](super::callbacks::CallbackMode::Blocking): awaits the callback inline.
/// - [`Concurrent`](super::callbacks::CallbackMode::Concurrent): spawns as a detached tokio task.
macro_rules! dispatch_callback {
    ($mode:expr, $cb:expr) => {
        match $mode {
            $crate::live::callbacks::CallbackMode::Blocking => {
                $cb.await;
            }
            $crate::live::callbacks::CallbackMode::Concurrent => {
                let f = $cb;
                tokio::spawn(async move {
                    f.await;
                });
            }
        }
    };
}

pub(super) use dispatch_callback;
