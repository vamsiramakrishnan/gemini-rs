//! [`MockLlm`]: a scripted, inspectable model for tests.

use std::collections::VecDeque;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;

use super::{BaseLlm, LlmError, LlmRequest, LlmResponse, LlmStream};

type ReplyFn = dyn Fn(&LlmRequest) -> Result<LlmResponse, LlmError> + Send + Sync;

enum Replies {
    Repeat(LlmResponse),
    Script(VecDeque<Result<LlmResponse, LlmError>>),
    Function(Box<ReplyFn>),
}

struct Inner {
    replies: Mutex<Replies>,
    requests: Mutex<Vec<LlmRequest>>,
}

/// A [`BaseLlm`] that replies from a script or a closure and records every
/// request, for tests that must not reach a provider.
///
/// It replies three ways, one per kind of test:
///
/// | Constructor | Replies with | Use it to test |
/// |---|---|---|
/// | [`MockLlm::text`] | the same text, every call | wiring, prompts, state flow |
/// | [`MockLlm::script`] | each response in turn, then an error | tool rounds, retries, multi-turn |
/// | [`MockLlm::from_fn`] | whatever the closure returns | replies that depend on the request |
///
/// Every call's [`LlmRequest`] is recorded, so a test can assert on what the
/// agent actually sent — instructions, history, tool declarations, sampling —
/// not only on what came back.
///
/// `MockLlm` is a cheap handle: clones share the script and the recording.
/// Give one clone to the agent and keep one to inspect.
///
/// ```
/// use gemini_adk_rs::llm::{BaseLlm, LlmRequest, LlmResponse, MockLlm};
///
/// # tokio_test::block_on(async {
/// let llm = MockLlm::script([
///     LlmResponse::tool_call("get_weather", serde_json::json!({ "city": "Paris" })),
///     LlmResponse::from_text("It is sunny in Paris."),
/// ]);
///
/// let first = llm.generate(LlmRequest::from_text("Weather in Paris?")).await.unwrap();
/// assert_eq!(first.function_calls()[0].name, "get_weather");
///
/// let second = llm.generate(LlmRequest::from_text("…")).await.unwrap();
/// assert_eq!(second.text(), "It is sunny in Paris.");
///
/// // The script is spent: a third call is a test failure, not a silent reply.
/// assert!(llm.generate(LlmRequest::from_text("again")).await.is_err());
/// assert_eq!(llm.requests().len(), 3);
/// # });
/// ```
#[derive(Clone)]
pub struct MockLlm {
    model_id: String,
    inner: Arc<Inner>,
}

impl MockLlm {
    fn with_replies(replies: Replies) -> Self {
        Self {
            model_id: "mock".into(),
            inner: Arc::new(Inner {
                replies: Mutex::new(replies),
                requests: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Reply with `text` to every request.
    pub fn text(text: impl Into<String>) -> Self {
        Self::with_replies(Replies::Repeat(LlmResponse::from_text(text)))
    }

    /// Reply with each response in order.
    ///
    /// A call after the script is spent returns an error naming the call, so
    /// an agent that makes one model call more than the test expected fails
    /// instead of receiving a plausible reply.
    pub fn script(responses: impl IntoIterator<Item = LlmResponse>) -> Self {
        Self::with_replies(Replies::Script(responses.into_iter().map(Ok).collect()))
    }

    /// Reply by calling `reply` with the request.
    ///
    /// This is the programmable mock: branch on the conversation so far, echo
    /// the prompt, or return an error for a particular input.
    pub fn from_fn<F>(reply: F) -> Self
    where
        F: Fn(&LlmRequest) -> Result<LlmResponse, LlmError> + Send + Sync + 'static,
    {
        Self::with_replies(Replies::Function(Box::new(reply)))
    }

    /// Append a failure to a script, to test how an agent handles a provider
    /// error at that point in the conversation.
    ///
    /// # Panics
    ///
    /// On a mock built with [`text`](Self::text) or [`from_fn`](Self::from_fn):
    /// those have no sequence to append to, and a closure can return the error
    /// itself.
    pub fn then_fail(self, error: LlmError) -> Self {
        match &mut *self.inner.replies.lock() {
            Replies::Script(queue) => queue.push_back(Err(error)),
            _ => panic!("MockLlm::then_fail applies to MockLlm::script only"),
        }
        self
    }

    /// Report `model_id` from [`BaseLlm::model_id`] instead of `"mock"`.
    pub fn with_model_id(mut self, model_id: impl Into<String>) -> Self {
        self.model_id = model_id.into();
        self
    }

    /// Every request received so far, oldest first.
    pub fn requests(&self) -> Vec<LlmRequest> {
        self.inner.requests.lock().clone()
    }

    /// The most recent request, if any.
    pub fn last_request(&self) -> Option<LlmRequest> {
        self.inner.requests.lock().last().cloned()
    }

    /// How many times the model has been called.
    pub fn call_count(&self) -> usize {
        self.inner.requests.lock().len()
    }
}

impl fmt::Debug for MockLlm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let replies = match &*self.inner.replies.lock() {
            Replies::Repeat(_) => "repeat".to_string(),
            Replies::Script(queue) => format!("script({} left)", queue.len()),
            Replies::Function(_) => "fn".to_string(),
        };
        f.debug_struct("MockLlm")
            .field("model_id", &self.model_id)
            .field("replies", &replies)
            .field("calls", &self.call_count())
            .finish()
    }
}

#[async_trait]
impl BaseLlm for MockLlm {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    /// Streams a text reply one word at a time (usage and the finish reason
    /// ride on the last chunk), so tests exercise the multi-chunk path; a
    /// reply with tool calls arrives as one chunk.
    async fn generate_stream(&self, request: LlmRequest) -> Result<LlmStream, LlmError> {
        use futures_util::StreamExt;

        let reply = self.generate(request).await?;
        let words: Vec<String> = reply
            .text()
            .split_inclusive(' ')
            .map(str::to_owned)
            .collect();
        if !reply.function_calls().is_empty() || words.len() < 2 {
            return Ok(futures_util::stream::once(async move { Ok(reply) }).boxed());
        }
        let last = words.len() - 1;
        let chunks: Vec<Result<LlmResponse, LlmError>> = words
            .into_iter()
            .enumerate()
            .map(|(i, word)| {
                let mut chunk = LlmResponse::from_text(word);
                chunk.finish_reason = None;
                if i == last {
                    chunk.finish_reason = reply.finish_reason.clone();
                    chunk.usage = reply.usage;
                }
                Ok(chunk)
            })
            .collect();
        Ok(futures_util::stream::iter(chunks).boxed())
    }

    async fn generate(&self, request: LlmRequest) -> Result<LlmResponse, LlmError> {
        let call = {
            let mut requests = self.inner.requests.lock();
            requests.push(request.clone());
            requests.len()
        };
        match &mut *self.inner.replies.lock() {
            Replies::Repeat(response) => Ok(response.clone()),
            Replies::Script(queue) => queue.pop_front().unwrap_or_else(|| {
                Err(LlmError::Other(format!(
                    "MockLlm script exhausted: call {call} has no scripted reply \
                     (the agent called the model more times than the test expected)"
                )))
            }),
            Replies::Function(reply) => reply(&request),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn text_replies_the_same_to_every_call() {
        let llm = MockLlm::text("hi");
        for _ in 0..3 {
            let reply = llm.generate(LlmRequest::from_text("x")).await.unwrap();
            assert_eq!(reply.text(), "hi");
        }
        assert_eq!(llm.call_count(), 3);
    }

    #[tokio::test]
    async fn script_replies_in_order_then_fails_loudly() {
        let llm = MockLlm::script([LlmResponse::from_text("one"), LlmResponse::from_text("two")]);
        assert_eq!(
            llm.generate(LlmRequest::default()).await.unwrap().text(),
            "one"
        );
        assert_eq!(
            llm.generate(LlmRequest::default()).await.unwrap().text(),
            "two"
        );
        let spent = llm.generate(LlmRequest::default()).await.unwrap_err();
        assert!(spent.to_string().contains("call 3"), "{spent}");
    }

    #[tokio::test]
    async fn then_fail_injects_a_provider_error_at_that_step() {
        let llm = MockLlm::script([LlmResponse::from_text("ok")]).then_fail(LlmError::RateLimited);
        assert!(llm.generate(LlmRequest::default()).await.is_ok());
        assert!(matches!(
            llm.generate(LlmRequest::default()).await,
            Err(LlmError::RateLimited)
        ));
    }

    #[tokio::test]
    async fn from_fn_sees_the_request() {
        let llm = MockLlm::from_fn(|req| {
            Ok(LlmResponse::from_text(format!(
                "{} turns",
                req.contents.len()
            )))
        });
        let reply = llm.generate(LlmRequest::from_text("x")).await.unwrap();
        assert_eq!(reply.text(), "1 turns");
    }

    #[tokio::test]
    async fn clones_share_the_script_and_the_recording() {
        let llm = MockLlm::script([LlmResponse::from_text("a"), LlmResponse::from_text("b")]);
        let handed_to_agent = llm.clone();
        handed_to_agent
            .generate(LlmRequest::from_text("first"))
            .await
            .unwrap();
        assert_eq!(
            llm.generate(LlmRequest::default()).await.unwrap().text(),
            "b"
        );
        assert_eq!(llm.call_count(), 2);
        assert_eq!(llm.requests()[0].contents.len(), 1);
    }

    #[tokio::test]
    async fn with_model_id_keeps_the_replies() {
        let llm = MockLlm::text("hi").with_model_id("gemini-test");
        assert_eq!(llm.model_id(), "gemini-test");
        assert_eq!(
            llm.generate(LlmRequest::default()).await.unwrap().text(),
            "hi"
        );
    }

    #[test]
    #[should_panic(expected = "script only")]
    fn then_fail_on_a_repeating_mock_is_a_test_bug() {
        let _ = MockLlm::text("hi").then_fail(LlmError::RateLimited);
    }
}
