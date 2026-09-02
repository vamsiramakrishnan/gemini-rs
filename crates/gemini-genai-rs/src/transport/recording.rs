//! Wire-level recording — capture every byte that crosses the transport.
//!
//! [`RecordingCodec`] wraps any [`Codec`] and taps both directions of the wire:
//! every successful `encode_setup`/`encode_command` records the outbound bytes,
//! and every `decode_message` records the inbound bytes *before* decoding (so
//! even undecodable frames are captured). Each tap produces a [`WireEntry`]
//! with a monotonic sequence number, a direction, and an epoch-millis
//! timestamp, delivered synchronously to a [`WireRecorder`].
//!
//! Built-in recorders:
//!
//! - [`FileWireRecorder`] — appends JSONL to a file (base64 payloads).
//! - [`MemoryWireRecorder`] — collects entries in memory (tests, harnesses).
//!
//! # Installation
//!
//! The lowest-friction knob is [`crate::protocol::types::SessionConfig::record_wire`]:
//! `connect` and `ConnectBuilder` both honor it by wrapping whatever
//! codec is in use. [`crate::transport::ConnectBuilder::record_wire`] is the
//! builder-level equivalent.
//!
//! # Wire-log format (JSONL)
//!
//! One JSON object per line; `payload_b64` is the standard-base64 encoding of
//! the raw frame bytes:
//!
//! ```json
//! {"seq":1,"dir":"out","ts_ms":1718000000000,"payload_b64":"eyJzZXR1cCI6eyJtb2RlbCI6Li4ufX0="}
//! {"seq":2,"dir":"in","ts_ms":1718000000123,"payload_b64":"eyJzZXR1cENvbXBsZXRlIjp7fX0="}
//! ```
//!
//! Decoded, the first entry is the outbound `{"setup":{...}}` message and the
//! second the inbound `{"setupComplete":{}}` handshake. Logs in this format are
//! read back with [`read_wire_log`] and replayed with
//! [`crate::transport::replay::ReplayTransport`].

use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::protocol::messages::ServerMessage;
use crate::protocol::types::SessionConfig;
use crate::session::SessionCommand;

use super::codec::{Codec, CodecError};

/// Direction of a recorded wire frame, relative to the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum WireDirection {
    /// Client → server (encoded setup or command bytes).
    #[serde(rename = "out")]
    Outbound,
    /// Server → client (raw bytes handed to the decoder).
    #[serde(rename = "in")]
    Inbound,
}

/// One recorded wire frame: sequence, direction, timestamp, raw payload.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WireEntry {
    /// Monotonic per-recording sequence number, starting at 1.
    pub seq: u64,
    /// Frame direction relative to the client.
    pub dir: WireDirection,
    /// Wall-clock capture time as milliseconds since the Unix epoch.
    pub ts_ms: u64,
    /// Raw frame bytes (serialized as standard base64 under `payload_b64`).
    #[serde(rename = "payload_b64", with = "base64_bytes")]
    pub payload: Vec<u8>,
}

/// Serde codec for base64-encoded byte payloads.
mod base64_bytes {
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(bytes: &[u8], ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(&base64::engine::general_purpose::STANDARD.encode(bytes))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(de)?;
        base64::engine::general_purpose::STANDARD
            .decode(s.as_bytes())
            .map_err(serde::de::Error::custom)
    }
}

/// Synchronous sink for recorded wire frames.
///
/// `record` is called on the session loop's encode/decode path, so it must be
/// cheap and is infallible by contract: implementations log internal errors
/// (I/O failures, serialization issues) instead of surfacing them — recording
/// must never take a live session down.
pub trait WireRecorder: Send + Sync {
    /// Record one wire frame. Must not panic; log errors internally.
    fn record(&self, entry: WireEntry);
}

/// Cloneable, `Debug`-friendly handle to a shared [`WireRecorder`].
///
/// Exists so `Option<WireRecorderHandle>` can live on
/// [`SessionConfig`] (which derives
/// `Debug` + `Clone`) without requiring `Debug` from every recorder.
#[derive(Clone)]
pub struct WireRecorderHandle(Arc<dyn WireRecorder>);

impl WireRecorderHandle {
    /// Wrap a shared recorder.
    pub fn new(recorder: Arc<dyn WireRecorder>) -> Self {
        Self(recorder)
    }

    /// The underlying shared recorder.
    pub fn recorder(&self) -> Arc<dyn WireRecorder> {
        self.0.clone()
    }
}

impl From<Arc<dyn WireRecorder>> for WireRecorderHandle {
    fn from(recorder: Arc<dyn WireRecorder>) -> Self {
        Self::new(recorder)
    }
}

impl std::fmt::Debug for WireRecorderHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WireRecorderHandle(..)")
    }
}

fn epoch_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A [`Codec`] decorator that records every byte crossing the wire.
///
/// Outbound bytes are recorded after a successful encode (empty encodes —
/// e.g. [`SessionCommand::Disconnect`] — are skipped because they never hit
/// the wire). Inbound bytes are recorded *before* decoding, so frames that
/// fail to decode are still captured.
///
/// ```rust
/// use std::sync::Arc;
/// use gemini_genai_rs::transport::{JsonCodec, MemoryWireRecorder, RecordingCodec};
///
/// let recorder = Arc::new(MemoryWireRecorder::new());
/// let codec = RecordingCodec::new(JsonCodec, recorder.clone());
/// # let _ = codec;
/// ```
pub struct RecordingCodec<C> {
    inner: C,
    recorder: Arc<dyn WireRecorder>,
    seq: AtomicU64,
}

impl<C: Codec> RecordingCodec<C> {
    /// Wrap `inner`, delivering taps to `recorder`.
    pub fn new(inner: C, recorder: Arc<dyn WireRecorder>) -> Self {
        Self {
            inner,
            recorder,
            seq: AtomicU64::new(1),
        }
    }

    fn tap(&self, dir: WireDirection, payload: &[u8]) {
        let entry = WireEntry {
            seq: self.seq.fetch_add(1, Ordering::Relaxed),
            dir,
            ts_ms: epoch_millis(),
            payload: payload.to_vec(),
        };
        self.recorder.record(entry);
    }
}

impl<C: Codec> Codec for RecordingCodec<C> {
    fn encode_setup(&self, config: &SessionConfig) -> Result<Vec<u8>, CodecError> {
        let bytes = self.inner.encode_setup(config)?;
        if !bytes.is_empty() {
            self.tap(WireDirection::Outbound, &bytes);
        }
        Ok(bytes)
    }

    fn encode_command(
        &self,
        cmd: &SessionCommand,
        config: &SessionConfig,
    ) -> Result<Vec<u8>, CodecError> {
        let bytes = self.inner.encode_command(cmd, config)?;
        if !bytes.is_empty() {
            self.tap(WireDirection::Outbound, &bytes);
        }
        Ok(bytes)
    }

    fn decode_message(&self, data: &[u8]) -> Result<ServerMessage, CodecError> {
        self.tap(WireDirection::Inbound, data);
        self.inner.decode_message(data)
    }
}

/// Forwarding impl so the connection loop can
/// install a recorder dynamically without changing its generic signature.
impl Codec for Box<dyn Codec> {
    fn encode_setup(&self, config: &SessionConfig) -> Result<Vec<u8>, CodecError> {
        (**self).encode_setup(config)
    }

    fn encode_command(
        &self,
        cmd: &SessionCommand,
        config: &SessionConfig,
    ) -> Result<Vec<u8>, CodecError> {
        (**self).encode_command(cmd, config)
    }

    fn decode_message(&self, data: &[u8]) -> Result<ServerMessage, CodecError> {
        (**self).decode_message(data)
    }
}

// ---------------------------------------------------------------------------
// FileWireRecorder — durable JSONL backend
// ---------------------------------------------------------------------------

const FILE_FLUSH_INTERVAL: Duration = Duration::from_secs(1);

struct FileWireRecorderInner {
    writer: std::io::BufWriter<std::fs::File>,
    last_flush: Instant,
}

/// Durable [`WireRecorder`] writing one JSON object per line (JSONL).
///
/// Entries are buffered and flushed at least every second and on drop. Write
/// or serialization errors are logged via `tracing::warn!` — recording never
/// fails the session.
///
/// See the [module docs](self) for the on-disk format and an example entry.
pub struct FileWireRecorder {
    inner: parking_lot::Mutex<FileWireRecorderInner>,
}

impl FileWireRecorder {
    /// Create (truncating) the wire log at `path`.
    pub fn create(path: impl AsRef<std::path::Path>) -> std::io::Result<Self> {
        let file = std::fs::File::create(path)?;
        Ok(Self {
            inner: parking_lot::Mutex::new(FileWireRecorderInner {
                writer: std::io::BufWriter::new(file),
                last_flush: Instant::now(),
            }),
        })
    }

    /// Flush buffered entries to disk now.
    pub fn flush(&self) {
        let mut inner = self.inner.lock();
        if let Err(e) = inner.writer.flush() {
            tracing::warn!(error = %e, "FileWireRecorder flush failed");
        }
        inner.last_flush = Instant::now();
    }
}

impl WireRecorder for FileWireRecorder {
    fn record(&self, entry: WireEntry) {
        let line = match serde_json::to_string(&entry) {
            Ok(line) => line,
            Err(e) => {
                tracing::warn!(error = %e, "FileWireRecorder serialize failed");
                return;
            }
        };
        let mut inner = self.inner.lock();
        if let Err(e) = writeln!(inner.writer, "{line}") {
            tracing::warn!(error = %e, "FileWireRecorder write failed");
            return;
        }
        if inner.last_flush.elapsed() >= FILE_FLUSH_INTERVAL {
            if let Err(e) = inner.writer.flush() {
                tracing::warn!(error = %e, "FileWireRecorder flush failed");
            }
            inner.last_flush = Instant::now();
        }
    }
}

impl Drop for FileWireRecorder {
    fn drop(&mut self) {
        if let Err(e) = self.inner.lock().writer.flush() {
            tracing::warn!(error = %e, "FileWireRecorder final flush failed");
        }
    }
}

// ---------------------------------------------------------------------------
// MemoryWireRecorder — in-memory backend for tests and harnesses
// ---------------------------------------------------------------------------

/// In-memory [`WireRecorder`] for tests and replay harnesses.
#[derive(Default)]
pub struct MemoryWireRecorder {
    entries: parking_lot::Mutex<Vec<WireEntry>>,
}

impl MemoryWireRecorder {
    /// Create an empty recorder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot all recorded entries (in record order).
    pub fn entries(&self) -> Vec<WireEntry> {
        self.entries.lock().clone()
    }

    /// Number of recorded entries.
    pub fn len(&self) -> usize {
        self.entries.lock().len()
    }

    /// Whether nothing has been recorded yet.
    pub fn is_empty(&self) -> bool {
        self.entries.lock().is_empty()
    }
}

impl WireRecorder for MemoryWireRecorder {
    fn record(&self, entry: WireEntry) {
        self.entries.lock().push(entry);
    }
}

// ---------------------------------------------------------------------------
// Wire-log reading
// ---------------------------------------------------------------------------

/// Error reading a JSONL wire log.
#[derive(Debug, thiserror::Error)]
pub enum WireLogError {
    /// Failed to read the file.
    #[error("failed to read wire log: {0}")]
    Io(#[from] std::io::Error),
    /// A line failed to parse as a [`WireEntry`].
    #[error("invalid wire log entry on line {line}: {source}")]
    Parse {
        /// 1-based line number of the offending entry.
        line: usize,
        /// The underlying JSON error.
        source: serde_json::Error,
    },
}

/// Read a JSONL wire log written by [`FileWireRecorder`] back into entries.
///
/// Blank lines are skipped; any other malformed line is an error.
pub fn read_wire_log(path: impl AsRef<std::path::Path>) -> Result<Vec<WireEntry>, WireLogError> {
    let data = std::fs::read_to_string(path)?;
    parse_wire_log(&data)
}

/// Parse JSONL wire-log text (one [`WireEntry`] per non-blank line).
pub fn parse_wire_log(data: &str) -> Result<Vec<WireEntry>, WireLogError> {
    let mut entries = Vec::new();
    for (idx, line) in data.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let entry: WireEntry =
            serde_json::from_str(line).map_err(|source| WireLogError::Parse {
                line: idx + 1,
                source,
            })?;
        entries.push(entry);
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::types::ModelId;
    use crate::transport::codec::JsonCodec;

    fn test_config() -> SessionConfig {
        SessionConfig::new("test-key")
            .model(ModelId::from_static("models/gemini-2.0-flash-live-001"))
    }

    #[test]
    fn recording_codec_taps_outbound_and_inbound() {
        let recorder = Arc::new(MemoryWireRecorder::new());
        let codec = RecordingCodec::new(JsonCodec, recorder.clone());
        let config = test_config();

        let setup = codec.encode_setup(&config).unwrap();
        let cmd_bytes = codec
            .encode_command(&SessionCommand::SendText("hi".into()), &config)
            .unwrap();
        let inbound = br#"{"setupComplete":{}}"#;
        codec.decode_message(inbound).unwrap();

        let entries = recorder.entries();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].seq, 1);
        assert_eq!(entries[0].dir, WireDirection::Outbound);
        assert_eq!(entries[0].payload, setup);
        assert_eq!(entries[1].seq, 2);
        assert_eq!(entries[1].dir, WireDirection::Outbound);
        assert_eq!(entries[1].payload, cmd_bytes);
        assert_eq!(entries[2].seq, 3);
        assert_eq!(entries[2].dir, WireDirection::Inbound);
        assert_eq!(entries[2].payload, inbound.to_vec());
        assert!(entries.iter().all(|e| e.ts_ms > 0));
    }

    #[test]
    fn recording_codec_skips_empty_encodes() {
        let recorder = Arc::new(MemoryWireRecorder::new());
        let codec = RecordingCodec::new(JsonCodec, recorder.clone());
        let config = test_config();

        // Disconnect encodes to empty bytes and never hits the wire.
        let bytes = codec
            .encode_command(&SessionCommand::Disconnect, &config)
            .unwrap();
        assert!(bytes.is_empty());
        assert!(recorder.is_empty());
    }

    #[test]
    fn recording_codec_records_undecodable_inbound() {
        let recorder = Arc::new(MemoryWireRecorder::new());
        let codec = RecordingCodec::new(JsonCodec, recorder.clone());

        let bad: &[u8] = &[0xFF, 0xFE];
        assert!(codec.decode_message(bad).is_err());
        let entries = recorder.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].dir, WireDirection::Inbound);
        assert_eq!(entries[0].payload, bad.to_vec());
    }

    #[test]
    fn wire_entry_jsonl_round_trip() {
        let entry = WireEntry {
            seq: 7,
            dir: WireDirection::Inbound,
            ts_ms: 1_718_000_000_123,
            payload: br#"{"setupComplete":{}}"#.to_vec(),
        };
        let line = serde_json::to_string(&entry).unwrap();
        assert!(line.contains("\"dir\":\"in\""));
        assert!(line.contains("payload_b64"));
        let parsed = parse_wire_log(&format!("{line}\n\n{line}")).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0], entry);
    }

    #[test]
    fn file_wire_recorder_round_trip() {
        let dir = std::env::temp_dir().join(format!(
            "gemini-rs-wire-log-test-{}-{}",
            std::process::id(),
            epoch_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.wire.jsonl");

        {
            let recorder = FileWireRecorder::create(&path).unwrap();
            recorder.record(WireEntry {
                seq: 1,
                dir: WireDirection::Outbound,
                ts_ms: 42,
                payload: b"{\"setup\":{}}".to_vec(),
            });
            recorder.record(WireEntry {
                seq: 2,
                dir: WireDirection::Inbound,
                ts_ms: 43,
                payload: b"{\"setupComplete\":{}}".to_vec(),
            });
            // Drop flushes.
        }

        let entries = read_wire_log(&path).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].payload, b"{\"setup\":{}}".to_vec());
        assert_eq!(entries[1].dir, WireDirection::Inbound);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_wire_log_reports_bad_line() {
        let err = parse_wire_log("not json").unwrap_err();
        match err {
            WireLogError::Parse { line, .. } => assert_eq!(line, 1),
            other => panic!("expected Parse error, got {other:?}"),
        }
    }

    #[test]
    fn boxed_codec_forwards() {
        let codec: Box<dyn Codec> = Box::new(JsonCodec);
        let config = test_config();
        let bytes = codec.encode_setup(&config).unwrap();
        assert!(!bytes.is_empty());
        assert!(codec.decode_message(br#"{"setupComplete":{}}"#).is_ok());
    }
}
