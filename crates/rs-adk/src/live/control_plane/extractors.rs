//! OOB extraction pipeline — run extractors concurrently and merge results into state.

use std::sync::Arc;

use crate::state::State;

use crate::live::callbacks::EventCallbacks;
use crate::live::extractor::TurnExtractor;
use crate::live::transcript::TranscriptBuffer;

use super::dispatch_callback;

/// Run a subset of extractors concurrently and merge results into state.
///
/// Shared between handle_turn_complete (EveryTurn/Interval),
/// handle_tool_calls (AfterToolCall), and phase transitions (OnPhaseChange).
pub(in crate::live) async fn run_extractors(
    extractors: &[Arc<dyn TurnExtractor>],
    transcript_buffer: &mut TranscriptBuffer,
    state: &State,
    callbacks: &EventCallbacks,
) {
    if extractors.is_empty() {
        return;
    }

    let extraction_futures: Vec<_> = extractors
        .iter()
        .filter_map(|extractor| {
            let window_size = extractor.window_size();
            let window: Vec<_> = transcript_buffer.window(window_size).to_vec();
            if window.is_empty() {
                return None;
            }
            // Check should_extract before launching async work
            if !extractor.should_extract(&window) {
                return None;
            }
            let ext = extractor.clone();
            Some(async move {
                match ext.extract(&window).await {
                    Ok(value) => Ok((ext.name().to_string(), value)),
                    Err(e) => {
                        #[cfg(feature = "tracing-support")]
                        tracing::warn!(extractor = ext.name(), "Extraction failed: {e}");
                        Err((ext.name().to_string(), e.to_string()))
                    }
                }
            })
        })
        .collect();

    let results = futures::future::join_all(extraction_futures).await;
    for result in results {
        match result {
            Ok((name, value)) => {
                state.set(&name, &value);
                // Auto-flatten: promote each top-level field.
                // Accumulative merge: null extraction values do NOT overwrite
                // previously extracted non-null values.  This prevents the
                // LLM from "forgetting" information gathered in earlier turns
                // when the current window lacks that data.
                if let Some(obj) = value.as_object() {
                    for (field, val) in obj {
                        if val.is_null() {
                            // Skip -- don't erase previously extracted data
                            continue;
                        }
                        state.set(field, val.clone());
                    }
                }
                if let Some(cb) = &callbacks.on_extracted {
                    dispatch_callback!(callbacks.on_extracted_mode, cb(name, value));
                }
            }
            Err((name, error)) => {
                if let Some(cb) = &callbacks.on_extraction_error {
                    dispatch_callback!(callbacks.on_extraction_error_mode, cb(name, error));
                }
            }
        }
    }
}

/// Run extractors using a window that optionally includes the current in-progress turn.
///
/// When `include_current` is true, uses `snapshot_window_with_current` to capture
/// the model's output before interruption truncation (for GenerationComplete extractors).
pub(in crate::live) async fn run_extractors_with_window(
    extractors: &[Arc<dyn TurnExtractor>],
    transcript_buffer: &mut TranscriptBuffer,
    state: &State,
    callbacks: &EventCallbacks,
    include_current: bool,
) {
    if extractors.is_empty() {
        return;
    }

    let extraction_futures: Vec<_> = extractors
        .iter()
        .filter_map(|extractor| {
            let window_size = extractor.window_size();
            let window = if include_current {
                transcript_buffer
                    .snapshot_window_with_current(window_size)
                    .turns()
                    .to_vec()
            } else {
                transcript_buffer.window(window_size).to_vec()
            };
            if window.is_empty() || !extractor.should_extract(&window) {
                return None;
            }
            let ext = extractor.clone();
            Some(async move {
                match ext.extract(&window).await {
                    Ok(value) => Ok((ext.name().to_string(), value)),
                    Err(e) => {
                        #[cfg(feature = "tracing-support")]
                        tracing::warn!(extractor = ext.name(), "Extraction failed: {e}");
                        Err((ext.name().to_string(), e.to_string()))
                    }
                }
            })
        })
        .collect();

    let results = futures::future::join_all(extraction_futures).await;
    for result in results {
        match result {
            Ok((name, value)) => {
                state.set(&name, &value);
                if let Some(obj) = value.as_object() {
                    for (field, val) in obj {
                        if val.is_null() {
                            continue;
                        }
                        state.set(field, val.clone());
                    }
                }
                if let Some(cb) = &callbacks.on_extracted {
                    dispatch_callback!(callbacks.on_extracted_mode, cb(name, value));
                }
            }
            Err((name, error)) => {
                if let Some(cb) = &callbacks.on_extraction_error {
                    dispatch_callback!(callbacks.on_extraction_error_mode, cb(name, error));
                }
            }
        }
    }
}
