//! In-process SIP — answer raw SIP calls with no carrier service in the path.
//!
//! *(feature `sip`)* Where [`super::twilio`] relies on a carrier
//! service to terminate the phone network and hand audio over a WebSocket,
//! this module terminates the call itself: SIP signalling via
//! [`rsipstack`](https://docs.rs/rsipstack) (the stack underneath the
//! `rustpbx` PBX), and G.711-over-RTP media built from this crate's own pure
//! layers ([`super::rtp`], [`super::sdp`], [`super::g711`]).
//! Any SIP endpoint — a softphone, an Asterisk/FreeSWITCH PBX, a provider's
//! SIP trunk — dials the agent directly, and each call attaches to a Live
//! session through the same [`voice::pump`](crate::voice::pump) as every
//! other audio surface.
//!
//! ```ignore
//! let mut agent = SipAgent::bind("0.0.0.0:5060".parse()?).await?;
//! while let Some(incoming) = agent.next_call().await {
//!     let session = Live::builder()
//!         .instruction("Answer the phone politely.")
//!         .greeting("Greet the caller.")
//!         .connect_from_env().await?;
//!     let call = incoming.answer(&session).await?;
//!     tokio::spawn(async move { call.ended().await; });
//! }
//! ```
//!
//! RFC 4733 telephone events (DTMF) are negotiated in the SDP answer when
//! the offer proposes them; keypresses land in session state under the
//! shared [`super::bridge`] keys, where flow guards read them —
//! identical to the Twilio path. What is deliberately *not* here yet: SIP
//! registration (the agent is a directly-dialed UAS) and SRTP. Media is
//! symmetric RTP: the agent sends to the offer's address but re-latches onto
//! the source of the first arriving packet, which keeps NATted softphones
//! working.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use rsipstack::EndpointBuilder;
use rsipstack::dialog::dialog::DialogState;
use rsipstack::dialog::dialog_layer::DialogLayer;
use rsipstack::dialog::invite_dialog::InviteDialog;
use rsipstack::transport::TransportLayer;
use rsipstack::transport::udp::UdpConnection;

use gemini_adk_rs::State;
use gemini_adk_rs::live::LiveHandle;

use super::bridge::{self, DtmfDeduper, FillerConfig};
use super::g711;
use super::rtp::{self, PT_PCMA, RtpSender, SAMPLES_PER_PACKET};
use super::sdp::{self, AudioOffer};
use crate::voice::{Playback, VoicePump, pump};

/// Errors from the SIP agent.
#[derive(Debug)]
pub enum SipError {
    /// Binding or socket I/O failed.
    Io(std::io::Error),
    /// The SIP stack reported an error.
    Sip(rsipstack::Error),
    /// The INVITE carried no answerable audio offer (no `m=audio`, port 0).
    NoAudioOffer,
    /// The offer had audio but no G.711 codec this agent can speak.
    NoCommonCodec,
}

impl std::fmt::Display for SipError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "sip io error: {e}"),
            Self::Sip(e) => write!(f, "sip stack error: {e:?}"),
            Self::NoAudioOffer => write!(f, "INVITE carried no answerable audio offer"),
            Self::NoCommonCodec => write!(f, "no common G.711 codec with the caller"),
        }
    }
}

impl std::error::Error for SipError {}

impl From<std::io::Error> for SipError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<rsipstack::Error> for SipError {
    fn from(e: rsipstack::Error) -> Self {
        Self::Sip(e)
    }
}

// ── Agent ────────────────────────────────────────────────────────────────────

/// A SIP user agent server: binds a UDP SIP port and yields incoming calls.
pub struct SipAgent {
    dialog_layer: Arc<DialogLayer>,
    incoming: rsipstack::transaction::TransactionReceiver,
    cancel: CancellationToken,
    local_ip: IpAddr,
    sip_port: u16,
}

impl SipAgent {
    /// Bind the SIP signalling port (conventionally 5060/udp) and start the
    /// endpoint's serve loop in the background.
    pub async fn bind(addr: SocketAddr) -> Result<SipAgent, SipError> {
        let cancel = CancellationToken::new();
        let transport_layer = TransportLayer::new(cancel.child_token());
        let udp = UdpConnection::create_connection(addr, None, Some(cancel.child_token()))
            .await
            .map_err(SipError::Sip)?;
        let sip_port = udp
            .get_addr()
            .addr
            .port
            .as_ref()
            .map(|p| u16::from(*p))
            .unwrap_or(addr.port());
        transport_layer.add_transport(udp.into());

        let endpoint = EndpointBuilder::new()
            .with_user_agent("gemini-rs")
            .with_cancel_token(cancel.child_token())
            .with_transport_layer(transport_layer)
            .build();
        endpoint
            .inner
            .transport_layer
            .serve_listens()
            .await
            .map_err(SipError::Sip)?;
        let inner = endpoint.inner.clone();
        tokio::spawn(async move {
            let _ = inner.serve().await;
        });

        let incoming = endpoint.incoming_transactions().map_err(SipError::Sip)?;
        let dialog_layer = Arc::new(DialogLayer::new(endpoint.inner.clone()));

        Ok(SipAgent {
            dialog_layer,
            incoming,
            cancel,
            local_ip: addr.ip(),
            sip_port,
        })
    }

    /// Wait for the next incoming call. In-dialog requests (BYE, re-INVITE,
    /// ACK) and non-call methods are handled internally; only new INVITEs
    /// surface. Returns `None` once the agent is shut down.
    pub async fn next_call(&mut self) -> Option<IncomingCall> {
        while let Some(mut tx) = self.incoming.recv().await {
            use rsipstack::rsip::{Method, StatusCode};
            match tx.original.method {
                Method::Invite => {
                    let offer = match sdp::parse_audio_offer(
                        String::from_utf8_lossy(&tx.original.body).as_ref(),
                    ) {
                        Some(offer) => offer,
                        None => {
                            let _ = tx.reply(StatusCode::NotAcceptableHere).await;
                            continue;
                        }
                    };
                    let (state_tx, state_rx) = mpsc::unbounded_channel();
                    let contact = format!(
                        "sip:gemini@{}:{};transport=udp",
                        advertised_ip(self.local_ip, &offer),
                        self.sip_port
                    );
                    let contact = match rsipstack::rsip::Uri::try_from(contact.as_str()) {
                        Ok(uri) => uri,
                        Err(_) => {
                            let _ = tx.reply(StatusCode::ServerInternalError).await;
                            continue;
                        }
                    };
                    let dialog = match self.dialog_layer.get_or_create_server_invite(
                        &tx,
                        state_tx,
                        None,
                        Some(contact),
                    ) {
                        Ok(dialog) => dialog,
                        Err(err) => {
                            tracing::warn!("rejecting INVITE: {err:?}");
                            let _ = tx.reply(StatusCode::ServerInternalError).await;
                            continue;
                        }
                    };
                    use rsipstack::rsip::HeadersExt as _;
                    let from = tx
                        .original
                        .from_header()
                        .map(std::string::ToString::to_string)
                        .unwrap_or_default();
                    let _ = dialog.ringing(None, None);
                    // Responses (180/200/603) are queued events on the INVITE
                    // transaction; pumping receive() is what puts them on the
                    // wire and later delivers the ACK.
                    tokio::spawn(async move { while tx.receive().await.is_some() {} });
                    return Some(IncomingCall {
                        dialog,
                        state_rx,
                        offer,
                        from,
                        local_ip: self.local_ip,
                        dialog_layer: self.dialog_layer.clone(),
                        filler: None,
                    });
                }
                Method::Ack | Method::Bye | Method::Cancel | Method::Info | Method::Update => {
                    // In-dialog requests: route to the owning dialog.
                    match self.dialog_layer.match_dialog(&tx) {
                        Some(mut dialog) => {
                            tokio::spawn(async move {
                                let _ = dialog.handle(&mut tx).await;
                            });
                        }
                        None => {
                            let _ = tx.reply(StatusCode::CallTransactionDoesNotExist).await;
                        }
                    }
                }
                Method::Options => {
                    let _ = tx.reply(StatusCode::OK).await;
                }
                _ => {
                    let _ = tx.reply(StatusCode::MethodNotAllowed).await;
                }
            }
        }
        None
    }

    /// The SIP port actually bound (useful with port 0).
    pub fn sip_port(&self) -> u16 {
        self.sip_port
    }

    /// Stop the endpoint and every call it produced.
    pub fn shutdown(&self) {
        self.cancel.cancel();
    }
}

// ── Incoming call ────────────────────────────────────────────────────────────

/// A ringing inbound call: answer it onto a session, or reject it.
pub struct IncomingCall {
    dialog: InviteDialog,
    state_rx: mpsc::UnboundedReceiver<DialogState>,
    /// The caller's parsed audio offer.
    pub offer: AudioOffer,
    /// The caller's `From` header, for screening/logging.
    pub from: String,
    local_ip: IpAddr,
    dialog_layer: Arc<DialogLayer>,
    filler: Option<FillerConfig>,
}

impl IncomingCall {
    /// Play a latency-masking filler clip when the model stays silent too
    /// long after the caller stops speaking — see
    /// [`bridge::spawn_latency_filler`]. The clip must be mono PCM16 at
    /// 8 kHz (the call's playback rate).
    pub fn filler(mut self, config: FillerConfig) -> Self {
        self.filler = Some(config);
        self
    }

    /// Answer the call onto a connected session: bind an RTP socket, send the
    /// SDP answer in the 200 OK, and start the media loop.
    ///
    /// When the offer proposes RFC 4733 telephone events, the answer accepts
    /// them and keypresses are written to session state via
    /// [`bridge::record_dtmf`]. The caller's `From` identity lands under
    /// [`bridge::KEY_CALLER`].
    pub async fn answer(self, handle: &LiveHandle) -> Result<SipCall, SipError> {
        let payload_type = self.offer.g711_payload_type().ok_or_else(|| {
            let _ = self.dialog.reject(None, None);
            SipError::NoCommonCodec
        })?;

        let media_ip = advertised_ip(self.local_ip, &self.offer);
        let rtp_socket = UdpSocket::bind((self.local_ip, 0)).await?;
        let rtp_port = rtp_socket.local_addr()?.port();
        let remote: SocketAddr = format!("{}:{}", self.offer.host, self.offer.port)
            .parse()
            .map_err(|_| SipError::NoAudioOffer)?;

        let telephone_event_pt = self.offer.telephone_event_pt;
        let answer = sdp::audio_answer(
            seed() as u64,
            &media_ip.to_string(),
            rtp_port,
            payload_type,
            telephone_event_pt,
        );
        self.dialog
            .accept(None, Some(answer.into_bytes()))
            .map_err(SipError::Sip)?;
        let _ = handle.state().set(bridge::KEY_CALLER, self.from.clone());

        let cancel = CancellationToken::new();
        let media = rtp_media(
            handle,
            Arc::new(rtp_socket),
            remote,
            payload_type,
            telephone_event_pt,
            self.filler,
            cancel.clone(),
        );

        // Tear the media down when the dialog terminates (BYE, error).
        let mut state_rx = self.state_rx;
        let media_cancel = cancel.clone();
        let dialog_id = self.dialog.id();
        let dialog_layer = self.dialog_layer;
        let ended = tokio::spawn(async move {
            while let Some(state) = state_rx.recv().await {
                if let DialogState::Terminated(_, _) = state {
                    break;
                }
            }
            media_cancel.cancel();
            dialog_layer.remove_dialog(&dialog_id);
        });

        Ok(SipCall {
            dialog: self.dialog,
            media,
            cancel,
            ended,
        })
    }

    /// Decline the call (486 Busy Here by default).
    pub fn reject(self) {
        let _ = self.dialog.reject(None, None);
    }
}

// ── Live call ────────────────────────────────────────────────────────────────

/// An answered SIP call with media flowing.
pub struct SipCall {
    dialog: InviteDialog,
    media: MediaTasks,
    cancel: CancellationToken,
    ended: JoinHandle<()>,
}

impl SipCall {
    /// Wait until the call ends (caller hung up, or [`hangup`](Self::hangup)).
    pub async fn ended(self) {
        let _ = self.ended.await;
        self.media.stop().await;
    }

    /// Hang up: send BYE and stop the media loop.
    pub async fn hangup(self) {
        let _ = self.dialog.bye().await;
        self.cancel.cancel();
        let _ = self.ended.await;
        self.media.stop().await;
    }
}

// ── Media loop ───────────────────────────────────────────────────────────────

struct MediaTasks {
    pump: VoicePump,
    inbound: JoinHandle<()>,
    outbound: JoinHandle<()>,
    filler: Option<JoinHandle<()>>,
}

impl MediaTasks {
    async fn stop(self) {
        self.inbound.abort();
        self.outbound.abort();
        if let Some(filler) = self.filler {
            filler.abort();
        }
        self.pump.abort();
        self.pump.join().await;
    }
}

/// Wire a session's voice pump to G.711-over-RTP on a UDP socket.
///
/// Symmetric RTP: packets go to `remote` until the first packet arrives,
/// whose source address then becomes the send target (NAT re-latch).
fn rtp_media(
    handle: &LiveHandle,
    socket: Arc<UdpSocket>,
    remote: SocketAddr,
    payload_type: u8,
    telephone_event_pt: Option<u8>,
    filler: Option<FillerConfig>,
    cancel: CancellationToken,
) -> MediaTasks {
    let (mic_tx, mic_rx) = mpsc::channel::<Vec<i16>>(64);
    let (speaker_tx, speaker_rx) = mpsc::channel::<Playback>(64);
    let voice_pump = pump(
        handle,
        mic_rx,
        super::TWILIO_HZ,
        speaker_tx.clone(),
        super::TWILIO_HZ,
    );
    let (peer_tx, peer_rx) = watch::channel(remote);

    let filler = filler.map(|config| bridge::spawn_latency_filler(handle, speaker_tx, config));

    let inbound = tokio::spawn(inbound_loop(
        socket.clone(),
        payload_type,
        telephone_event_pt,
        handle.state().clone(),
        mic_tx,
        peer_tx,
        cancel.clone(),
    ));
    let outbound = tokio::spawn(outbound_loop(
        socket,
        payload_type,
        speaker_rx,
        peer_rx,
        cancel,
    ));

    MediaTasks {
        pump: voice_pump,
        inbound,
        outbound,
        filler,
    }
}

async fn inbound_loop(
    socket: Arc<UdpSocket>,
    payload_type: u8,
    telephone_event_pt: Option<u8>,
    state: State,
    mic_tx: mpsc::Sender<Vec<i16>>,
    peer_tx: watch::Sender<SocketAddr>,
    cancel: CancellationToken,
) {
    let mut buf = [0u8; 2048];
    let mut latched = false;
    let mut dtmf = DtmfDeduper::default();
    loop {
        let (len, source) = tokio::select! {
            _ = cancel.cancelled() => break,
            received = socket.recv_from(&mut buf) => match received {
                Ok(pair) => pair,
                Err(_) => break,
            },
        };
        let Some(packet) = rtp::parse(&buf[..len]) else {
            continue; // stray non-RTP traffic on the media port
        };
        if telephone_event_pt == Some(packet.payload_type) {
            // RFC 4733 keypress: emit once per end-marked event.
            if let Some(event) = rtp::parse_telephone_event(&packet.payload)
                && dtmf.accept(event.end, packet.timestamp)
                && let Some(digit) = event.digit()
            {
                bridge::record_dtmf(&state, digit);
            }
            continue;
        }
        if packet.payload_type != payload_type {
            continue; // a payload type we did not negotiate
        }
        if !latched {
            let _ = peer_tx.send(source);
            latched = true;
        }
        let samples = if payload_type == PT_PCMA {
            g711::decode_alaw(&packet.payload)
        } else {
            g711::decode_ulaw(&packet.payload)
        };
        if mic_tx.send(samples).await.is_err() {
            break;
        }
    }
}

async fn outbound_loop(
    socket: Arc<UdpSocket>,
    payload_type: u8,
    mut speaker_rx: mpsc::Receiver<Playback>,
    peer_rx: watch::Receiver<SocketAddr>,
    cancel: CancellationToken,
) {
    let silence_byte: u8 = if payload_type == PT_PCMA { 0xD5 } else { 0xFF };
    let seed = seed();
    let mut sender = RtpSender::new(payload_type, seed, (seed >> 16) as u16, seed.rotate_left(8));
    let mut pending: std::collections::VecDeque<i16> = std::collections::VecDeque::new();
    let mut ticker = tokio::time::interval(Duration::from_millis(20));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            playback = speaker_rx.recv() => match playback {
                Some(Playback::Chunk(samples)) => pending.extend(samples),
                // Barge-in on raw RTP: we ARE the buffer — drop it.
                Some(Playback::Flush) => pending.clear(),
                None => break,
            },
            _ = ticker.tick() => {
                if pending.is_empty() {
                    sender.skip_silence(SAMPLES_PER_PACKET as u32);
                    continue;
                }
                let take = pending.len().min(SAMPLES_PER_PACKET);
                let mut payload = Vec::with_capacity(SAMPLES_PER_PACKET);
                for sample in pending.drain(..take) {
                    payload.push(if payload_type == PT_PCMA {
                        g711::linear_to_alaw(sample)
                    } else {
                        g711::linear_to_ulaw(sample)
                    });
                }
                // Constant 20 ms ptime: pad a short tail with silence.
                payload.resize(SAMPLES_PER_PACKET, silence_byte);
                let datagram = sender.packetize(&payload, SAMPLES_PER_PACKET as u32);
                let target = *peer_rx.borrow();
                if socket.send_to(&datagram, target).await.is_err() {
                    break;
                }
            }
        }
    }
}

/// The IP to advertise for media. A wildcard bind cannot go into SDP, so
/// discover the interface that routes toward the caller's offer address.
fn advertised_ip(local: IpAddr, offer: &AudioOffer) -> IpAddr {
    if !local.is_unspecified() {
        return local;
    }
    let probe = std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect((offer.host.as_str(), offer.port))?;
            s.local_addr()
        })
        .map(|a| a.ip());
    probe.unwrap_or(local)
}

/// A cheap non-cryptographic seed for SSRC/sequence/timestamp offsets.
fn seed() -> u32 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    nanos ^ (std::process::id().rotate_left(16))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn recv_status(socket: &UdpSocket, buf: &mut [u8]) -> Option<String> {
        let deadline = Duration::from_secs(3);
        let (len, _) = tokio::time::timeout(deadline, socket.recv_from(buf))
            .await
            .ok()?
            .ok()?;
        String::from_utf8_lossy(&buf[..len])
            .lines()
            .next()
            .map(str::to_string)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn answers_options_and_rings_then_rejects_an_invite() {
        let mut agent = SipAgent::bind("127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind agent");
        let agent_port = agent.sip_port();
        // Drive the agent loop concurrently: OPTIONS is answered inside it,
        // and the INVITE surfaces through the channel.
        let (call_tx, call_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            if let Some(incoming) = agent.next_call().await {
                let _ = call_tx.send(incoming);
            }
        });

        let uac = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let uac_port = uac.local_addr().unwrap().port();
        let target = format!("127.0.0.1:{agent_port}");
        let mut buf = [0u8; 2048];

        // OPTIONS gets a 200 without surfacing a call.
        let options = format!(
            "OPTIONS sip:gemini@{target} SIP/2.0\r\n\
             Via: SIP/2.0/UDP 127.0.0.1:{uac_port};branch=z9hG4bKopt1\r\n\
             Max-Forwards: 70\r\n\
             From: <sip:probe@127.0.0.1>;tag=opt\r\n\
             To: <sip:gemini@{target}>\r\n\
             Call-ID: options-1@127.0.0.1\r\n\
             CSeq: 1 OPTIONS\r\n\
             Content-Length: 0\r\n\r\n"
        );
        uac.send_to(options.as_bytes(), &target).await.unwrap();
        let status = recv_status(&uac, &mut buf).await.expect("OPTIONS response");
        assert!(
            status.contains("200"),
            "expected 200 to OPTIONS, got {status}"
        );

        // An INVITE with a G.711 offer surfaces as an IncomingCall (after
        // 100/180 provisional responses); rejecting it sends a final failure.
        let sdp_body = "v=0\r\n\
             o=probe 1 1 IN IP4 127.0.0.1\r\n\
             s=call\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\n\
             m=audio 40000 RTP/AVP 0\r\n\
             a=rtpmap:0 PCMU/8000\r\n";
        let invite = format!(
            "INVITE sip:gemini@{target} SIP/2.0\r\n\
             Via: SIP/2.0/UDP 127.0.0.1:{uac_port};branch=z9hG4bKinv1\r\n\
             Max-Forwards: 70\r\n\
             From: <sip:probe@127.0.0.1>;tag=inv\r\n\
             To: <sip:gemini@{target}>\r\n\
             Call-ID: invite-1@127.0.0.1\r\n\
             CSeq: 1 INVITE\r\n\
             Contact: <sip:probe@127.0.0.1:{uac_port}>\r\n\
             Content-Type: application/sdp\r\n\
             Content-Length: {}\r\n\r\n{sdp_body}",
            sdp_body.len()
        );
        uac.send_to(invite.as_bytes(), &target).await.unwrap();

        let incoming = tokio::time::timeout(Duration::from_secs(3), call_rx)
            .await
            .expect("call surfaces")
            .expect("agent still running");
        assert_eq!(incoming.offer.port, 40_000);
        assert_eq!(
            incoming.offer.g711_payload_type(),
            Some(super::super::rtp::PT_PCMU)
        );
        assert!(incoming.from.contains("probe"), "from: {}", incoming.from);
        incoming.reject();

        // Drain provisional responses until the final failure arrives.
        let mut saw_final = false;
        for _ in 0..6 {
            match recv_status(&uac, &mut buf).await {
                Some(status) => {
                    let code: u32 = status
                        .split_whitespace()
                        .nth(1)
                        .and_then(|c| c.parse().ok())
                        .unwrap_or(0);
                    if code >= 400 {
                        saw_final = true;
                        break;
                    }
                }
                None => break,
            }
        }
        assert!(
            saw_final,
            "expected a final failure response to the rejected INVITE"
        );
    }
}
