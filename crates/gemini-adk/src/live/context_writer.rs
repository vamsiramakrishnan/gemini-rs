//! Deferred context delivery — flush pending context alongside user content.
//!
//! When the control plane produces model-role context turns (tool advisory,
//! repair nudge, steering modifiers, phase instructions, on_enter_context),
//! they can be queued in a [`PendingContext`] buffer instead of sent immediately.
//!
//! [`DeferredWriter`] wraps any [`SessionWriter`] and transparently drains the
//! pending queue before forwarding user-initiated sends (`send_audio`,
//! `send_text`, `send_video`).  This ensures context arrives in the same burst
//! as user content rather than as isolated WebSocket frames that can confuse
//! the model or clash with concurrent user input.
//!
//! # Architecture
//!
//! ```text
//!   Control lane (lifecycle)         User code (LiveHandle)
//!          |                                |
//!   push context to                  send_audio / send_text
//!   PendingContext                          |
//!          |                         DeferredWriter
//!          v                          1. drain PendingContext
//!   +---------------+                2. send_client_content(drained, false)
//!   | PendingContext | <-- drain ---  3. forward original send
//!   +---------------+
//! ```
//!
//! The queue uses `parking_lot::Mutex` for fast, uncontested locking — the
//! control lane pushes once per turn, and user sends drain before each frame.

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;

use gemini_live::prelude::{Content, FunctionResponse};
use gemini_live::session::{SessionError, SessionWriter};

/// Thread-safe buffer for pending context turns awaiting delivery.
///
/// Context is queued by the control plane (lifecycle steps 7d/7e/7f/12/13)
/// and drained by [`DeferredWriter`] before the next user interaction.
///
/// # Thread safety
///
/// Uses `parking_lot::Mutex` — fast uncontested locking, no poisoning.
/// The control lane pushes once per turn; user sends drain once per frame.
/// Contention is near-zero.
pub struct PendingContext {
    buffer: Mutex<Vec<Content>>,
    /// Whether a prompt (turnComplete:true) should be sent after flushing.
    prompt: Mutex<bool>,
}

impl PendingContext {
    /// Create an empty pending context buffer.
    pub fn new() -> Self {
        Self {
            buffer: Mutex::new(Vec::new()),
            prompt: Mutex::new(false),
        }
    }

    /// Push a single context turn into the buffer.
    pub fn push(&self, content: Content) {
        self.buffer.lock().push(content);
    }

    /// Push multiple context turns into the buffer.
    pub fn extend(&self, contents: Vec<Content>) {
        if !contents.is_empty() {
            self.buffer.lock().extend(contents);
        }
    }

    /// Mark that a prompt (turnComplete:true) should follow the next flush.
    pub fn set_prompt(&self) {
        *self.prompt.lock() = true;
    }

    /// Drain all pending context, returning the contents and whether to prompt.
    ///
    /// After this call, the buffer is empty and the prompt flag is cleared.
    pub fn drain(&self) -> (Vec<Content>, bool) {
        let contents = {
            let mut buf = self.buffer.lock();
            std::mem::take(&mut *buf)
        };
        let prompt = {
            let mut p = self.prompt.lock();
            std::mem::replace(&mut *p, false)
        };
        (contents, prompt)
    }

    /// Check if the buffer is empty (no pending context or prompt).
    pub fn is_empty(&self) -> bool {
        self.buffer.lock().is_empty() && !*self.prompt.lock()
    }
}

impl Default for PendingContext {
    fn default() -> Self {
        Self::new()
    }
}

/// A [`SessionWriter`] wrapper that flushes pending context before user content.
///
/// Wraps an inner writer and drains a shared [`PendingContext`] buffer before
/// forwarding `send_audio`, `send_text`, or `send_video` calls.  This ensures
/// model-role context turns arrive in the same burst as user content.
///
/// # When context is flushed
///
/// - **`send_audio`**: Context is flushed as `send_client_content(drained, false)`
///   immediately before the audio frame.  Audio goes via `realtimeInput` (different
///   wire message), so they are two frames — but sent back-to-back with no gap.
///
/// - **`send_text`**: Context is flushed, then user text is sent.  Both go via
///   `clientContent`, but as separate messages since the user text needs
///   `turn_complete: true` to trigger a model response.
///
/// - **`send_video`**: Same as audio — flush then forward.
///
/// # When context is NOT flushed
///
/// `send_tool_response`, `update_instruction`, `send_client_content`,
/// `signal_activity_start/end`, and `disconnect` do NOT trigger a flush.
/// These are either internal SDK operations or explicit user control — flushing
/// context before them would be surprising.
pub struct DeferredWriter {
    inner: Arc<dyn SessionWriter>,
    pending: Arc<PendingContext>,
}

impl DeferredWriter {
    /// Create a new deferred writer wrapping the given writer.
    pub fn new(inner: Arc<dyn SessionWriter>, pending: Arc<PendingContext>) -> Self {
        Self { inner, pending }
    }

    /// Flush any pending context to the wire.
    ///
    /// Sends all queued context turns as a single `send_client_content` call,
    /// then sends a prompt frame if one was requested.
    async fn flush(&self) -> Result<(), SessionError> {
        let (contents, prompt) = self.pending.drain();
        if !contents.is_empty() {
            self.inner.send_client_content(contents, false).await?;
        }
        if prompt {
            self.inner.send_client_content(vec![], true).await?;
        }
        Ok(())
    }

    /// Get a reference to the shared pending context buffer.
    pub fn pending(&self) -> &Arc<PendingContext> {
        &self.pending
    }
}

#[async_trait]
impl SessionWriter for DeferredWriter {
    async fn send_audio(&self, data: Vec<u8>) -> Result<(), SessionError> {
        self.flush().await?;
        self.inner.send_audio(data).await
    }

    async fn send_text(&self, text: String) -> Result<(), SessionError> {
        self.flush().await?;
        self.inner.send_text(text).await
    }

    async fn send_tool_response(
        &self,
        responses: Vec<FunctionResponse>,
    ) -> Result<(), SessionError> {
        // Tool responses are SDK-internal — don't flush context here.
        self.inner.send_tool_response(responses).await
    }

    async fn send_client_content(
        &self,
        turns: Vec<Content>,
        turn_complete: bool,
    ) -> Result<(), SessionError> {
        // Explicit client content calls pass through unchanged.
        // The caller knows what they're doing.
        self.inner.send_client_content(turns, turn_complete).await
    }

    async fn send_video(&self, jpeg_data: Vec<u8>) -> Result<(), SessionError> {
        self.flush().await?;
        self.inner.send_video(jpeg_data).await
    }

    async fn update_instruction(&self, instruction: String) -> Result<(), SessionError> {
        // Instruction updates are SDK-internal — don't flush context here.
        self.inner.update_instruction(instruction).await
    }

    async fn signal_activity_start(&self) -> Result<(), SessionError> {
        self.inner.signal_activity_start().await
    }

    async fn signal_activity_end(&self) -> Result<(), SessionError> {
        self.inner.signal_activity_end().await
    }

    async fn disconnect(&self) -> Result<(), SessionError> {
        // Flush any remaining context before disconnecting so it's not lost.
        let _ = self.flush().await;
        self.inner.disconnect().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Minimal writer that counts calls by type.
    struct CountingWriter {
        audio_count: AtomicUsize,
        text_count: AtomicUsize,
        client_content_count: AtomicUsize,
        video_count: AtomicUsize,
    }

    impl CountingWriter {
        fn new() -> Self {
            Self {
                audio_count: AtomicUsize::new(0),
                text_count: AtomicUsize::new(0),
                client_content_count: AtomicUsize::new(0),
                video_count: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl SessionWriter for CountingWriter {
        async fn send_audio(&self, _: Vec<u8>) -> Result<(), SessionError> {
            self.audio_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn send_text(&self, _: String) -> Result<(), SessionError> {
            self.text_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn send_tool_response(&self, _: Vec<FunctionResponse>) -> Result<(), SessionError> {
            Ok(())
        }
        async fn send_client_content(&self, _: Vec<Content>, _: bool) -> Result<(), SessionError> {
            self.client_content_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn send_video(&self, _: Vec<u8>) -> Result<(), SessionError> {
            self.video_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn update_instruction(&self, _: String) -> Result<(), SessionError> {
            Ok(())
        }
        async fn signal_activity_start(&self) -> Result<(), SessionError> {
            Ok(())
        }
        async fn signal_activity_end(&self) -> Result<(), SessionError> {
            Ok(())
        }
        async fn disconnect(&self) -> Result<(), SessionError> {
            Ok(())
        }
    }

    #[test]
    fn pending_context_push_and_drain() {
        let pc = PendingContext::new();
        assert!(pc.is_empty());

        pc.push(Content::model("context 1"));
        pc.push(Content::model("context 2"));
        assert!(!pc.is_empty());

        let (contents, prompt) = pc.drain();
        assert_eq!(contents.len(), 2);
        assert!(!prompt);
        assert!(pc.is_empty());
    }

    #[test]
    fn pending_context_extend() {
        let pc = PendingContext::new();
        pc.extend(vec![
            Content::model("a"),
            Content::model("b"),
            Content::model("c"),
        ]);
        let (contents, _) = pc.drain();
        assert_eq!(contents.len(), 3);
    }

    #[test]
    fn pending_context_prompt_flag() {
        let pc = PendingContext::new();
        pc.push(Content::model("ctx"));
        pc.set_prompt();
        assert!(!pc.is_empty());

        let (contents, prompt) = pc.drain();
        assert_eq!(contents.len(), 1);
        assert!(prompt);
        assert!(pc.is_empty());
    }

    #[test]
    fn pending_context_drain_clears() {
        let pc = PendingContext::new();
        pc.push(Content::model("a"));
        pc.set_prompt();
        let _ = pc.drain();

        // Second drain should be empty
        let (contents, prompt) = pc.drain();
        assert!(contents.is_empty());
        assert!(!prompt);
    }

    #[tokio::test]
    async fn deferred_writer_flushes_on_send_audio() {
        let inner = Arc::new(CountingWriter::new());
        let pending = Arc::new(PendingContext::new());
        let writer = DeferredWriter::new(inner.clone(), pending.clone());

        pending.push(Content::model("steering context"));
        pending.push(Content::model("phase instruction"));

        writer.send_audio(vec![0u8; 100]).await.unwrap();

        // Should have flushed: 1 client_content + 1 audio
        assert_eq!(inner.client_content_count.load(Ordering::SeqCst), 1);
        assert_eq!(inner.audio_count.load(Ordering::SeqCst), 1);
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn deferred_writer_flushes_on_send_text() {
        let inner = Arc::new(CountingWriter::new());
        let pending = Arc::new(PendingContext::new());
        let writer = DeferredWriter::new(inner.clone(), pending.clone());

        pending.push(Content::model("context"));

        writer.send_text("hello".into()).await.unwrap();

        assert_eq!(inner.client_content_count.load(Ordering::SeqCst), 1);
        assert_eq!(inner.text_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn deferred_writer_flushes_on_send_video() {
        let inner = Arc::new(CountingWriter::new());
        let pending = Arc::new(PendingContext::new());
        let writer = DeferredWriter::new(inner.clone(), pending.clone());

        pending.push(Content::model("context"));

        writer.send_video(vec![0xFFu8; 50]).await.unwrap();

        assert_eq!(inner.client_content_count.load(Ordering::SeqCst), 1);
        assert_eq!(inner.video_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn deferred_writer_no_flush_when_empty() {
        let inner = Arc::new(CountingWriter::new());
        let pending = Arc::new(PendingContext::new());
        let writer = DeferredWriter::new(inner.clone(), pending.clone());

        // No pending context — should just send audio, no client_content
        writer.send_audio(vec![0u8; 100]).await.unwrap();

        assert_eq!(inner.client_content_count.load(Ordering::SeqCst), 0);
        assert_eq!(inner.audio_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn deferred_writer_flushes_prompt_after_context() {
        let inner = Arc::new(CountingWriter::new());
        let pending = Arc::new(PendingContext::new());
        let writer = DeferredWriter::new(inner.clone(), pending.clone());

        pending.push(Content::model("repair nudge"));
        pending.set_prompt();

        writer.send_audio(vec![0u8; 100]).await.unwrap();

        // 1 client_content for context + 1 for prompt + 1 audio
        assert_eq!(inner.client_content_count.load(Ordering::SeqCst), 2);
        assert_eq!(inner.audio_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn deferred_writer_does_not_flush_on_tool_response() {
        let inner = Arc::new(CountingWriter::new());
        let pending = Arc::new(PendingContext::new());
        let writer = DeferredWriter::new(inner.clone(), pending.clone());

        pending.push(Content::model("context"));

        writer.send_tool_response(vec![]).await.unwrap();

        // Tool response should NOT flush — context still pending
        assert_eq!(inner.client_content_count.load(Ordering::SeqCst), 0);
        assert!(!pending.is_empty());
    }

    #[tokio::test]
    async fn deferred_writer_client_content_passes_through() {
        let inner = Arc::new(CountingWriter::new());
        let pending = Arc::new(PendingContext::new());
        let writer = DeferredWriter::new(inner.clone(), pending.clone());

        pending.push(Content::model("queued context"));

        // Explicit client_content should pass through without flushing
        writer
            .send_client_content(vec![Content::user("explicit")], true)
            .await
            .unwrap();

        assert_eq!(inner.client_content_count.load(Ordering::SeqCst), 1);
        // Queued context still pending
        assert!(!pending.is_empty());
    }
}
