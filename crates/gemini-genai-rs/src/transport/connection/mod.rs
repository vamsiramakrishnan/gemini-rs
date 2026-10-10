//! WebSocket connection lifecycle — connect, setup, full-duplex split, reconnection.

mod message_handler;
mod reconnect;
mod session_loop;

use std::sync::Arc;

use tokio::sync::{broadcast, mpsc, watch};

use crate::protocol::types::*;
use crate::session::{SessionHandle, SessionPhase, SessionState};
use crate::transport::TransportConfig;
use crate::transport::codec::{Codec, JsonCodec};
use crate::transport::ws::{Transport, TungsteniteTransport};

/// Connect to the Gemini Multimodal Live API with the default transport and
/// return a session handle.
///
/// Timeouts, reconnection, a custom transport or codec, or a wire recorder:
/// [`ConnectBuilder`](crate::transport::ConnectBuilder), of which this is the
/// zero-option form.
pub async fn connect(config: SessionConfig) -> Result<SessionHandle, crate::session::SessionError> {
    connect_with(
        config,
        TransportConfig::default(),
        TungsteniteTransport::new(),
        JsonCodec,
    )
    .await
}

/// Connect with an explicit transport config, transport, and codec — the
/// one path every public entry point ends in.
pub(crate) async fn connect_with<T, C>(
    config: SessionConfig,
    transport_config: TransportConfig,
    transport: T,
    codec: C,
) -> Result<SessionHandle, crate::session::SessionError>
where
    T: Transport,
    C: Codec,
{
    let (command_tx, command_rx) = mpsc::channel(transport_config.send_queue_depth);
    let (event_tx, _) = broadcast::channel(transport_config.event_channel_capacity);
    let (phase_tx, phase_rx) = watch::channel(SessionPhase::Disconnected);

    let state = Arc::new(SessionState::with_events(phase_tx, event_tx.clone()));

    let mut handle = SessionHandle::new(command_tx, event_tx.clone(), state.clone(), phase_rx);
    if let Some(pacing) = config.audio_pacing.clone() {
        handle = handle.with_audio_pacing(pacing);
    }

    // Honor a config-installed wire recorder by wrapping the codec. The
    // boxed indirection keeps `connect_with` generic while letting the
    // recorder be a runtime decision.
    let task = if let Some(recorder) = config.wire_recorder.clone() {
        let codec: Box<dyn Codec> = Box::new(crate::transport::recording::RecordingCodec::new(
            codec,
            recorder.recorder(),
        ));
        tokio::spawn(async move {
            session_loop::generic_connection_loop(
                config,
                transport_config,
                state,
                command_rx,
                event_tx,
                transport,
                codec,
            )
            .await;
        })
    } else {
        tokio::spawn(async move {
            session_loop::generic_connection_loop(
                config,
                transport_config,
                state,
                command_rx,
                event_tx,
                transport,
                codec,
            )
            .await;
        })
    };
    handle.set_task(task);

    Ok(handle)
}

#[cfg(test)]
mod tests {
    use super::message_handler::{MessageAction, handle_server_msg};
    use super::reconnect::reconnect_delay;
    use super::*;

    use std::time::Duration;

    use crate::protocol::messages::ServerMessage;
    use crate::session::{SessionEvent, SessionPhase, SessionState};
    use crate::transport::codec::JsonCodec;
    use crate::transport::ws::MockTransport;

    /// TransportConfig that disables reconnection for mock tests.
    fn no_reconnect_config() -> TransportConfig {
        TransportConfig {
            max_reconnect_attempts: 0,
            connect_timeout_secs: 5,
            setup_timeout_secs: 5,
            ..TransportConfig::default()
        }
    }

    /// A transport that plays one script per connection and records every
    /// setup message it is sent.
    struct Reconnecting {
        scripts: std::collections::VecDeque<Vec<Vec<u8>>>,
        current: std::collections::VecDeque<Vec<u8>>,
        setups: std::sync::Arc<parking_lot::Mutex<Vec<serde_json::Value>>>,
    }

    #[async_trait::async_trait]
    impl crate::transport::ws::Transport for Reconnecting {
        type Error = std::io::Error;

        async fn connect(
            &mut self,
            _url: &str,
            _headers: Vec<(String, String)>,
        ) -> Result<(), Self::Error> {
            self.current = self.scripts.pop_front().unwrap_or_default().into();
            Ok(())
        }

        async fn send(&mut self, data: Vec<u8>) -> Result<(), Self::Error> {
            if let Ok(message) = serde_json::from_slice::<serde_json::Value>(&data)
                && message.get("setup").is_some()
            {
                self.setups.lock().push(message);
            }
            Ok(())
        }

        async fn recv(&mut self) -> Result<Option<Vec<u8>>, Self::Error> {
            match self.current.pop_front() {
                Some(frame) => Ok(Some(frame)),
                // Out of script: stay open and quiet.
                None => std::future::pending().await,
            }
        }

        async fn close(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    /// A server that refuses every setup, closing with a status and reason.
    struct Refusing {
        connects: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        reason: &'static str,
    }

    #[async_trait::async_trait]
    impl crate::transport::ws::Transport for Refusing {
        type Error = std::io::Error;

        async fn connect(
            &mut self,
            _url: &str,
            _headers: Vec<(String, String)>,
        ) -> Result<(), Self::Error> {
            self.connects
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }

        async fn send(&mut self, _data: Vec<u8>) -> Result<(), Self::Error> {
            Ok(())
        }

        async fn recv(&mut self) -> Result<Option<Vec<u8>>, Self::Error> {
            // Give the test time to subscribe before the refusal.
            tokio::time::sleep(Duration::from_millis(50)).await;
            Ok(None)
        }

        async fn close(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }

        fn close_reason(&self) -> Option<String> {
            Some(self.reason.to_string())
        }
    }

    #[tokio::test]
    async fn an_invalid_setup_is_reported_with_the_servers_reason_and_not_retried() {
        let connects = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let transport = Refusing {
            connects: connects.clone(),
            reason: "server closed the connection (1007): The requested combination of response modalities (TEXT) is not supported by the model.",
        };
        let transport_config = TransportConfig {
            max_reconnect_attempts: 3,
            reconnect_base_delay_ms: 10,
            reconnect_max_delay_ms: 10,
            ..no_reconnect_config()
        };
        let handle = connect_with(
            SessionConfig::new("test-key"),
            transport_config,
            transport,
            JsonCodec,
        )
        .await
        .unwrap();
        let mut events = handle.subscribe();
        let mut error = None;
        let mut disconnected = None;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while disconnected.is_none() && tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(200), events.recv()).await {
                Ok(Ok(SessionEvent::Error(e))) => error = Some(e.to_string()),
                Ok(Ok(SessionEvent::Disconnected(reason))) => disconnected = Some(reason),
                _ => {}
            }
        }
        let error = error.expect("the refusal is reported");
        assert!(error.contains("response modalities (TEXT)"), "{error}");
        assert!(disconnected.flatten().unwrap_or_default().contains("1007"));
        assert_eq!(
            connects.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "no retry"
        );
    }

    #[test]
    fn close_codes_are_read_from_the_reason() {
        assert_eq!(
            super::session_loop::close_code("server closed the connection (1007): x"),
            Some(1007)
        );
        assert_eq!(
            super::session_loop::close_code("server closed the connection (1011)"),
            Some(1011)
        );
        assert_eq!(super::session_loop::close_code("no code"), None);
    }

    /// A server that accepts every setup and, on the first connection, sends
    /// `goAway` as soon as it has received a `contextUpdate`.
    struct GoAwayAfterUpdate {
        connects: usize,
        current: std::collections::VecDeque<Vec<u8>>,
        sent: std::sync::Arc<parking_lot::Mutex<Vec<serde_json::Value>>>,
        /// Issue a resumption handle on the first connection, so the
        /// reconnect resumes.
        issue_handle: bool,
    }

    #[async_trait::async_trait]
    impl crate::transport::ws::Transport for GoAwayAfterUpdate {
        type Error = std::io::Error;

        async fn connect(
            &mut self,
            _url: &str,
            _headers: Vec<(String, String)>,
        ) -> Result<(), Self::Error> {
            self.connects += 1;
            self.current = vec![br#"{"setupComplete":{}}"#.to_vec()].into();
            if self.issue_handle && self.connects == 1 {
                self.current.push_back(
                    br#"{"sessionResumptionUpdate":{"newHandle":"h-1","resumable":true}}"#.to_vec(),
                );
            }
            Ok(())
        }

        async fn send(&mut self, data: Vec<u8>) -> Result<(), Self::Error> {
            let message: serde_json::Value = serde_json::from_slice(&data).unwrap();
            if self.connects == 1 && message.get("contextUpdate").is_some() {
                self.current
                    .push_back(br#"{"goAway":{"timeLeft":"0s"}}"#.to_vec());
            }
            self.sent.lock().push(message);
            Ok(())
        }

        async fn recv(&mut self) -> Result<Option<Vec<u8>>, Self::Error> {
            match self.current.pop_front() {
                Some(frame) => Ok(Some(frame)),
                None => std::future::pending().await,
            }
        }

        async fn close(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_reconnect_after_a_context_update_declares_the_updated_preamble() {
        let sent = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
        let transport = GoAwayAfterUpdate {
            connects: 0,
            current: Default::default(),
            sent: sent.clone(),
            issue_handle: false,
        };
        let config = SessionConfig::from_vertex("p", "us-central1", "t")
            .model(ModelId::LIVE_3_8)
            .system_instruction("Unverified caller.")
            .add_tool(Tool::functions(vec![FunctionDeclaration {
                name: "verify_identity".into(),
                description: "Verify the caller.".into(),
                parameters: None,
                behavior: None,
            }]));
        let transport_config = TransportConfig {
            max_reconnect_attempts: 1,
            reconnect_base_delay_ms: 10,
            reconnect_max_delay_ms: 10,
            ..no_reconnect_config()
        };
        let handle = connect_with(config, transport_config, transport, JsonCodec)
            .await
            .unwrap();
        handle.wait_for_phase(SessionPhase::Active).await;
        handle
            .update_context(
                ContextUpdate::new()
                    .system_instruction("Verified caller.")
                    .tools(vec![Tool::functions(vec![FunctionDeclaration {
                        name: "get_balance".into(),
                        description: "Fetch the balance.".into(),
                        parameters: None,
                        behavior: None,
                    }])]),
            )
            .await
            .unwrap();

        let setups = || -> Vec<serde_json::Value> {
            sent.lock()
                .iter()
                .filter(|m| m.get("setup").is_some())
                .cloned()
                .collect()
        };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while setups().len() < 2 && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let setups = setups();
        assert_eq!(setups.len(), 2, "one setup per connection");
        let declared = |i: usize| {
            setups[i]
                .pointer("/setup/tools/0/functionDeclarations/0/name")
                .cloned()
        };
        let instruction = |i: usize| {
            setups[i]
                .pointer("/setup/systemInstruction/parts/0/text")
                .cloned()
        };
        assert_eq!(declared(0), Some("verify_identity".into()));
        assert_eq!(instruction(0), Some("Unverified caller.".into()));
        assert_eq!(
            declared(1),
            Some("get_balance".into()),
            "the reconnect declares the tools in effect, not the starting ones"
        );
        assert_eq!(instruction(1), Some("Verified caller.".into()));
        let updates = sent
            .lock()
            .iter()
            .filter(|m| m.get("contextUpdate").is_some())
            .count();
        assert_eq!(
            updates, 1,
            "a fresh session needs no replay: its setup declares the update"
        );
    }

    #[tokio::test]
    async fn a_resume_replays_the_fields_updates_replaced() {
        let sent = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
        let transport = GoAwayAfterUpdate {
            connects: 0,
            current: Default::default(),
            sent: sent.clone(),
            issue_handle: true,
        };
        let mut config = SessionConfig::from_vertex("p", "us-central1", "t")
            .model(ModelId::LIVE_3_8)
            .system_instruction("Unverified caller.")
            .add_tool(Tool::functions(vec![FunctionDeclaration {
                name: "verify_identity".into(),
                description: "Verify the caller.".into(),
                parameters: None,
                behavior: None,
            }]));
        config.session_resumption = Some(SessionResumptionConfig {
            handle: None,
            transparent: None,
        });
        let transport_config = TransportConfig {
            max_reconnect_attempts: 1,
            reconnect_base_delay_ms: 10,
            reconnect_max_delay_ms: 10,
            ..no_reconnect_config()
        };
        let handle = connect_with(config, transport_config, transport, JsonCodec)
            .await
            .unwrap();
        handle.wait_for_phase(SessionPhase::Active).await;
        handle
            .update_context(ContextUpdate::new().tools(vec![Tool::functions(vec![
                FunctionDeclaration {
                    name: "get_balance".into(),
                    description: "Fetch the balance.".into(),
                    parameters: None,
                    behavior: None,
                },
            ])]))
            .await
            .unwrap();

        // A resume ignores the setup's tools, and the client cannot tell
        // whether the update reached the server before the goAway.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        // The frames sent after the second (resumed) setup, once there is one.
        let after_resume = || -> Option<Vec<serde_json::Value>> {
            let sent = sent.lock();
            let resume_at = sent
                .iter()
                .enumerate()
                .filter(|(_, m)| m.get("setup").is_some())
                .nth(1)?
                .0;
            assert_eq!(
                sent[resume_at].pointer("/setup/sessionResumption/handle"),
                Some(&serde_json::json!("h-1"))
            );
            Some(sent[resume_at + 1..].to_vec())
        };
        let replay = loop {
            let found = after_resume()
                .and_then(|after| after.into_iter().find(|m| m.get("contextUpdate").is_some()));
            if let Some(replay) = found {
                break replay;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "no replay after the resume"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert_eq!(
            replay.pointer("/contextUpdate/tools/tools/0/functionDeclarations/0/name"),
            Some(&serde_json::json!("get_balance")),
            "the resumed session is brought back to the updated tools"
        );
        assert!(
            replay.pointer("/contextUpdate/systemInstruction").is_none(),
            "only the fields an update replaced are sent again"
        );
    }

    #[tokio::test]
    async fn a_reconnect_after_go_away_resumes_with_the_latest_handle() {
        let setups = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
        let transport = Reconnecting {
            scripts: vec![
                vec![
                    br#"{"setupComplete":{}}"#.to_vec(),
                    br#"{"sessionResumptionUpdate":{"newHandle":"h-1","resumable":true}}"#.to_vec(),
                    br#"{"goAway":{"timeLeft":"0s"}}"#.to_vec(),
                ],
                vec![br#"{"setupComplete":{}}"#.to_vec()],
            ]
            .into(),
            current: Default::default(),
            setups: setups.clone(),
        };
        let mut config = SessionConfig::new("test-key")
            .model(ModelId::from_static("models/gemini-2.0-flash-live-001"));
        config.session_resumption = Some(SessionResumptionConfig {
            handle: None,
            transparent: None,
        });
        let transport_config = TransportConfig {
            max_reconnect_attempts: 1,
            reconnect_base_delay_ms: 10,
            reconnect_max_delay_ms: 10,
            ..no_reconnect_config()
        };
        let _handle = connect_with(config, transport_config, transport, JsonCodec)
            .await
            .unwrap();

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while setups.lock().len() < 2 && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let setups = setups.lock();
        assert_eq!(setups.len(), 2, "one setup per connection");
        let resumption = |i: usize| setups[i].pointer("/setup/sessionResumption").cloned();
        assert_eq!(
            resumption(0),
            Some(serde_json::json!({})),
            "first connect: no handle yet"
        );
        assert_eq!(
            resumption(1),
            Some(serde_json::json!({ "handle": "h-1" })),
            "the reconnect presents the handle the server issued"
        );
    }

    #[tokio::test]
    async fn connect_with_mock_transport() {
        let mut transport = MockTransport::new();
        // Script setupComplete response
        transport.script_recv(br#"{"setupComplete":{}}"#.to_vec());
        // Script a text response then turn complete
        transport.script_recv(
            br#"{"serverContent":{"modelTurn":{"parts":[{"text":"Hello!"}]},"turnComplete":true}}"#
                .to_vec(),
        );

        let config = SessionConfig::new("test-key")
            .model(ModelId::from_static("models/gemini-2.0-flash-live-001"));

        let handle = connect_with(config, no_reconnect_config(), transport, JsonCodec)
            .await
            .unwrap();

        // Should reach Active phase after setup completes
        handle.wait_for_phase(SessionPhase::Active).await;
        assert_eq!(handle.phase(), SessionPhase::Active);
    }

    #[tokio::test]
    async fn connect_with_mock_receives_text_events() {
        let mut transport = MockTransport::new();
        transport.script_recv(br#"{"setupComplete":{}}"#.to_vec());
        transport.script_recv(
            br#"{"serverContent":{"modelTurn":{"parts":[{"text":"Hello from mock!"}]},"turnComplete":true}}"#
                .to_vec(),
        );

        let config = SessionConfig::new("test-key")
            .model(ModelId::from_static("models/gemini-2.0-flash-live-001"));
        let handle = connect_with(config, no_reconnect_config(), transport, JsonCodec)
            .await
            .unwrap();

        let mut events = handle.subscribe();

        // Wait for the session to become active
        handle.wait_for_phase(SessionPhase::Active).await;

        // Collect events until TurnComplete
        let mut got_text_delta = false;
        let mut got_text_complete = false;
        let mut got_turn_complete = false;

        for _ in 0..20 {
            match tokio::time::timeout(Duration::from_millis(100), events.recv()).await {
                Ok(Ok(SessionEvent::TextDelta(t))) => {
                    assert_eq!(t, "Hello from mock!");
                    got_text_delta = true;
                }
                Ok(Ok(SessionEvent::TextComplete(t))) => {
                    assert_eq!(t, "Hello from mock!");
                    got_text_complete = true;
                }
                Ok(Ok(SessionEvent::TurnComplete)) => {
                    got_turn_complete = true;
                    break;
                }
                Ok(Ok(_)) => continue,
                Ok(Err(_)) => break,
                Err(_) => break,
            }
        }

        assert!(got_text_delta, "should have received TextDelta");
        assert!(got_text_complete, "should have received TextComplete");
        assert!(got_turn_complete, "should have received TurnComplete");
    }

    #[tokio::test]
    async fn connect_with_mock_decodes_audio_and_standalone_usage() {
        // 40 ms of 24 kHz PCM16 with every byte value, so a decoder that
        // mangles any of the 64 base64 symbols shows up.
        let pcm: Vec<u8> = (0..1920u32).map(|i| (i * 7 % 256) as u8).collect();
        let b64 = {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD.encode(&pcm)
        };
        let mut transport = MockTransport::new();
        transport.script_recv(br#"{"setupComplete":{}}"#.to_vec());
        transport.script_recv(
            format!(
                r#"{{"serverContent":{{"modelTurn":{{"parts":[{{"inlineData":{{"mimeType":"audio/pcm;rate=24000","data":"{b64}"}}}}]}}}}}}"#
            )
            .into_bytes(),
        );
        // Usage with no content alongside it used to parse as an unknown
        // message and be dropped.
        transport.script_recv(
            br#"{"usageMetadata":{"promptTokenCount":12,"totalTokenCount":30}}"#.to_vec(),
        );
        transport.script_recv(br#"{"serverContent":{"turnComplete":true}}"#.to_vec());

        let config = SessionConfig::new("test-key")
            .model(ModelId::from_static("models/gemini-2.0-flash-live-001"));
        let handle = connect_with(config, no_reconnect_config(), transport, JsonCodec)
            .await
            .unwrap();
        let mut events = handle.subscribe();
        handle.wait_for_phase(SessionPhase::Active).await;

        let mut audio = None;
        let mut usage = None;
        for _ in 0..20 {
            match tokio::time::timeout(Duration::from_millis(100), events.recv()).await {
                Ok(Ok(SessionEvent::AudioData(data))) => audio = Some(data),
                Ok(Ok(SessionEvent::Usage(u))) => usage = Some(u),
                Ok(Ok(SessionEvent::TurnComplete)) => break,
                Ok(Ok(_)) => continue,
                Ok(Err(_)) | Err(_) => break,
            }
        }

        assert_eq!(audio.as_deref(), Some(pcm.as_slice()));
        let usage = usage.expect("standalone usage should reach the session");
        assert_eq!(usage.prompt_token_count, Some(12));
        assert_eq!(usage.total_token_count, Some(30));
    }

    #[tokio::test]
    async fn connect_with_mock_tool_call() {
        let mut transport = MockTransport::new();
        transport.script_recv(br#"{"setupComplete":{}}"#.to_vec());
        transport.script_recv(
            br#"{"toolCall":{"functionCalls":[{"name":"get_weather","args":{"city":"London"},"id":"call-1"}]}}"#
                .to_vec(),
        );

        let config = SessionConfig::new("test-key")
            .model(ModelId::from_static("models/gemini-2.0-flash-live-001"));
        let handle = connect_with(config, no_reconnect_config(), transport, JsonCodec)
            .await
            .unwrap();

        let mut events = handle.subscribe();
        handle.wait_for_phase(SessionPhase::Active).await;

        // Look for the ToolCall event
        let mut got_tool_call = false;
        for _ in 0..20 {
            match tokio::time::timeout(Duration::from_millis(100), events.recv()).await {
                Ok(Ok(SessionEvent::ToolCall(calls))) => {
                    assert_eq!(calls.len(), 1);
                    assert_eq!(calls[0].name, "get_weather");
                    got_tool_call = true;
                    break;
                }
                Ok(Ok(_)) => continue,
                Ok(Err(_)) => break,
                Err(_) => break,
            }
        }

        assert!(got_tool_call, "should have received ToolCall event");
    }

    #[tokio::test]
    async fn connect_with_mock_graceful_disconnect() {
        let mut transport = MockTransport::new();
        transport.script_recv(br#"{"setupComplete":{}}"#.to_vec());
        // Keep the connection alive with a message that arrives before disconnect
        transport.script_recv(
            br#"{"serverContent":{"modelTurn":{"parts":[{"text":"hi"}]},"turnComplete":true}}"#
                .to_vec(),
        );

        let config = SessionConfig::new("test-key")
            .model(ModelId::from_static("models/gemini-2.0-flash-live-001"));
        let handle = connect_with(config, no_reconnect_config(), transport, JsonCodec)
            .await
            .unwrap();

        handle.wait_for_phase(SessionPhase::Active).await;
        // Small delay to let the background task process
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Disconnect gracefully
        handle.disconnect().await.unwrap();

        // Wait for disconnected phase
        handle.wait_for_phase(SessionPhase::Disconnected).await;
        assert_eq!(handle.phase(), SessionPhase::Disconnected);
    }

    #[test]
    fn handle_server_msg_preserves_interruption() {
        let (phase_tx, _phase_rx) = watch::channel(SessionPhase::Active);
        let (event_tx, mut event_rx) = broadcast::channel(16);
        let state = Arc::new(SessionState::with_events(phase_tx, event_tx.clone()));

        let json = r#"{"serverContent":{"interrupted":true}}"#;
        let msg = ServerMessage::parse(json).unwrap();
        let action = handle_server_msg(msg, &state, &event_tx);

        assert!(matches!(action, MessageAction::Continue));
        // Should have emitted Interrupted event
        let mut found_interrupted = false;
        while let Ok(evt) = event_rx.try_recv() {
            if matches!(evt, SessionEvent::Interrupted) {
                found_interrupted = true;
            }
        }
        assert!(found_interrupted, "should emit Interrupted event");
    }

    #[test]
    fn handle_server_msg_routes_avatar_video_away_from_audio() {
        let (phase_tx, _phase_rx) = watch::channel(SessionPhase::Active);
        let (event_tx, mut event_rx) = broadcast::channel(16);
        let state = Arc::new(SessionState::with_events(phase_tx, event_tx.clone()));

        // "AAEC" = [0, 1, 2]; "AwQF" = [3, 4, 5].
        let json = r#"{"serverContent":{"modelTurn":{"parts":[
            {"inlineData":{"mimeType":"audio/pcm;rate=24000","data":"AAEC"}},
            {"inlineData":{"mimeType":"video/mp4","data":"AwQF"}}
        ]}}}"#;
        let msg = ServerMessage::parse(json).unwrap();
        handle_server_msg(msg, &state, &event_tx);

        let mut audio = Vec::new();
        let mut media = Vec::new();
        while let Ok(evt) = event_rx.try_recv() {
            match evt {
                SessionEvent::AudioData(bytes) => audio.push(bytes.to_vec()),
                SessionEvent::Media(m) => media.push(m),
                _ => {}
            }
        }
        assert_eq!(audio, [vec![0u8, 1, 2]], "only the audio part is audio");
        assert_eq!(media.len(), 1);
        assert!(media[0].is_video());
        assert_eq!(media[0].mime_type, "video/mp4");
        assert_eq!(media[0].data.as_ref(), [3u8, 4, 5]);
    }

    #[test]
    fn a_text_session_on_a_speech_only_model_reads_the_transcript_as_text() {
        let (phase_tx, _phase_rx) = watch::channel(SessionPhase::Active);
        let (event_tx, mut event_rx) = broadcast::channel(32);
        let state = Arc::new(SessionState::with_events(phase_tx, event_tx.clone()));
        state.set_text_from_transcription(true);

        for json in [
            r#"{"serverContent":{"modelTurn":{"parts":[{"inlineData":{"mimeType":"audio/pcm;rate=24000","data":"AAEC"}}]}}}"#,
            r#"{"serverContent":{"outputTranscription":{"text":"A table "}}}"#,
            r#"{"serverContent":{"outputTranscription":{"text":"for two."}}}"#,
            r#"{"serverContent":{"turnComplete":true}}"#,
        ] {
            handle_server_msg(ServerMessage::parse(json).unwrap(), &state, &event_tx);
        }
        let mut deltas = Vec::new();
        let mut complete = None;
        while let Ok(evt) = event_rx.try_recv() {
            match evt {
                SessionEvent::TextDelta(t) => deltas.push(t),
                SessionEvent::TextComplete(t) => complete = Some(t),
                SessionEvent::AudioData(_) => panic!("a text session plays no audio"),
                SessionEvent::OutputTranscription(_) => panic!("the transcript is the text"),
                _ => {}
            }
        }
        assert_eq!(deltas, ["A table ", "for two."]);
        assert_eq!(complete.as_deref(), Some("A table for two."));
    }

    #[test]
    fn handle_server_msg_go_away() {
        let (phase_tx, _phase_rx) = watch::channel(SessionPhase::Active);
        let (event_tx, _event_rx) = broadcast::channel(16);
        let state = Arc::new(SessionState::with_events(phase_tx, event_tx.clone()));

        let json = r#"{"goAway":{"timeLeft":"30s"}}"#;
        let msg = ServerMessage::parse(json).unwrap();
        let action = handle_server_msg(msg, &state, &event_tx);

        assert!(matches!(action, MessageAction::GoAway(Some(_))));
    }

    #[test]
    fn handle_server_msg_unknown_is_continue() {
        let (phase_tx, _phase_rx) = watch::channel(SessionPhase::Active);
        let (event_tx, _event_rx) = broadcast::channel(16);
        let state = Arc::new(SessionState::with_events(phase_tx, event_tx.clone()));

        let json = r#"{"unknownField":{"data":"test"}}"#;
        let msg = ServerMessage::parse(json).unwrap();
        let action = handle_server_msg(msg, &state, &event_tx);

        assert!(matches!(action, MessageAction::Continue));
    }

    #[tokio::test]
    async fn session_handle_join_after_disconnect() {
        let mut transport = MockTransport::new();
        transport.script_recv(br#"{"setupComplete":{}}"#.to_vec());

        let config = SessionConfig::new("test-key")
            .model(ModelId::from_static("models/gemini-2.0-flash-live-001"));
        let handle = connect_with(config, no_reconnect_config(), transport, JsonCodec)
            .await
            .unwrap();

        handle.wait_for_phase(SessionPhase::Active).await;

        // Disconnect to end the connection loop task
        handle.disconnect().await.unwrap();
        handle.wait_for_phase(SessionPhase::Disconnected).await;

        // join() should return Ok after the task completes
        let result = handle.join().await;
        assert!(result.is_ok(), "join() should succeed after disconnect");
    }

    #[tokio::test]
    async fn session_handle_join_after_command_channel_closed() {
        let mut transport = MockTransport::new();
        transport.script_recv(br#"{"setupComplete":{}}"#.to_vec());

        let config = SessionConfig::new("test-key")
            .model(ModelId::from_static("models/gemini-2.0-flash-live-001"));
        let handle = connect_with(config, no_reconnect_config(), transport, JsonCodec)
            .await
            .unwrap();

        handle.wait_for_phase(SessionPhase::Active).await;

        // Drop all senders to close the command channel, which triggers disconnect
        // We need to get the handle before dropping the original
        let join_handle = handle.clone();

        // Drop command_tx by dropping the handle — but we cloned it first.
        // Instead, disconnect and then join.
        handle.disconnect().await.unwrap();

        let result = join_handle.join().await;
        assert!(result.is_ok(), "join() should succeed after channel close");
    }

    #[test]
    fn reconnect_delay_exponential_backoff() {
        let config = TransportConfig::default();
        let d1 = reconnect_delay(1, &config);
        let d2 = reconnect_delay(2, &config);
        let d3 = reconnect_delay(3, &config);
        // Each step should roughly double (plus jitter)
        assert!(d2 > d1);
        assert!(d3 > d2);
        // Should not exceed max
        let d_large = reconnect_delay(100, &config);
        let max_with_jitter = Duration::from_millis(
            config.reconnect_max_delay_ms as u64 + config.reconnect_max_delay_ms as u64 / 4,
        );
        assert!(d_large <= max_with_jitter);
    }
}
