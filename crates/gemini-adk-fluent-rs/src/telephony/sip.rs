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
//! // `ignore`: `SipAgent` needs the `sip` feature and a bound UDP socket.
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
//! identical to the Twilio path. An `RTP/SAVP` offer with SDES keys is
//! answered with SRTP ([`super::srtp`]), and [`SipAgent::register`] registers
//! the agent with a PBX or trunk so it can be reached by address. Media is
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
use super::srtp::{CryptoAttribute, MasterKey, SrtpSession};
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
    /// The offer asked for SRTP with no crypto suite this agent supports.
    NoCommonCrypto,
    /// The registrar refused the registration (its final status code).
    RegistrationRejected(u16),
    /// A registrar or contact URI did not parse.
    InvalidUri(String),
}

impl std::fmt::Display for SipError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "sip io error: {e}"),
            Self::Sip(e) => write!(f, "sip stack error: {e:?}"),
            Self::NoAudioOffer => write!(f, "INVITE carried no answerable audio offer"),
            Self::NoCommonCodec => write!(f, "no common G.711 codec with the caller"),
            Self::NoCommonCrypto => write!(f, "no common SRTP crypto suite with the caller"),
            Self::RegistrationRejected(code) => {
                write!(f, "registrar refused the registration: {code}")
            }
            Self::InvalidUri(uri) => write!(f, "invalid SIP URI: {uri}"),
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

    /// Register this agent with a registrar, so calls to the account's
    /// address reach it.
    ///
    /// The first REGISTER (answering a digest challenge with the account's
    /// credentials) happens before this returns, so a wrong password or an
    /// unreachable registrar fails here. After that, the returned
    /// [`SipRegistration`] refreshes the binding at three quarters of the
    /// granted lifetime, and retries after a failure, until it is
    /// [unregistered](SipRegistration::unregister) or the agent shuts down.
    ///
    /// ```ignore
    /// let agent = SipAgent::bind("0.0.0.0:5060".parse()?).await?;
    /// let registration = agent
    ///     .register(SipAccount::new("sip:pbx.example.com", "agent", "secret"))
    ///     .await?;
    /// // ... take calls with agent.next_call() ...
    /// registration.unregister().await?;
    /// ```
    pub async fn register(&self, account: SipAccount) -> Result<SipRegistration, SipError> {
        use rsipstack::dialog::authenticate::Credential;
        use rsipstack::dialog::registration::Registration;

        let registrar = rsipstack::rsip::Uri::try_from(account.registrar.as_str())
            .map_err(|_| SipError::InvalidUri(account.registrar.clone()))?;
        let mut registration = Registration::new(
            self.dialog_layer.endpoint.clone(),
            Some(Credential {
                username: account.username.clone(),
                password: account.password.clone(),
                realm: account.realm.clone(),
            }),
        );
        if let Some(contact) = &account.contact {
            let uri = rsipstack::rsip::Uri::try_from(contact.as_str())
                .map_err(|_| SipError::InvalidUri(contact.clone()))?;
            registration.contact = Some(rsipstack::rsip::typed::Contact {
                display_name: None,
                uri,
                params: vec![],
            });
        }

        let requested = account.expires;
        let granted = register_once(&mut registration, &registrar, requested).await?;
        let (state_tx, state_rx) =
            watch::channel(RegistrationState::Registered { expires: granted });
        let cancel = self.cancel.child_token();
        let stop = cancel.clone();
        let task = tokio::spawn(async move {
            let mut next = refresh_after(granted);
            loop {
                tokio::select! {
                    _ = stop.cancelled() => break,
                    _ = tokio::time::sleep(next) => {}
                }
                match register_once(&mut registration, &registrar, requested).await {
                    Ok(granted) => {
                        next = refresh_after(granted);
                        let _ = state_tx.send(RegistrationState::Registered { expires: granted });
                    }
                    Err(error) => {
                        tracing::warn!("SIP re-registration failed: {error}");
                        next = RETRY_AFTER;
                        let _ = state_tx.send(RegistrationState::Retrying {
                            error: error.to_string(),
                        });
                    }
                }
            }
            // Remove the binding, unless the endpoint itself is gone.
            let removed = tokio::time::timeout(
                UNREGISTER_TIMEOUT,
                register_once(&mut registration, &registrar, Duration::ZERO),
            )
            .await
            .unwrap_or_else(|_| Err(SipError::Io(std::io::ErrorKind::TimedOut.into())));
            let _ = state_tx.send(RegistrationState::Unregistered);
            removed.map(|_| ())
        });
        Ok(SipRegistration {
            state: state_rx,
            cancel: cancel.drop_guard(),
            task,
        })
    }
}

// ── Registration ─────────────────────────────────────────────────────────────

/// How long after a failed refresh to try again.
const RETRY_AFTER: Duration = Duration::from_secs(30);
/// How long un-registering may take before it is abandoned.
const UNREGISTER_TIMEOUT: Duration = Duration::from_secs(5);

/// A SIP account on a registrar (a PBX or a SIP trunk provider).
#[derive(Clone)]
pub struct SipAccount {
    /// The registrar's URI, e.g. `sip:pbx.example.com`.
    pub registrar: String,
    /// The account's user name, also the user part of the registered address.
    pub username: String,
    /// The account's password, for digest authentication.
    pub password: String,
    /// The authentication realm, when the registrar needs it named.
    pub realm: Option<String>,
    /// The Contact URI to register. By default it is built from the agent's
    /// address, corrected by the address the registrar reports seeing
    /// (`received`/`rport`), which keeps an agent behind NAT reachable.
    pub contact: Option<String>,
    /// The binding lifetime to ask for (the registrar may grant less).
    pub expires: Duration,
}

impl SipAccount {
    /// An account with a one-hour binding.
    pub fn new(
        registrar: impl Into<String>,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self {
            registrar: registrar.into(),
            username: username.into(),
            password: password.into(),
            realm: None,
            contact: None,
            expires: Duration::from_secs(3600),
        }
    }

    /// Name the authentication realm.
    pub fn realm(mut self, realm: impl Into<String>) -> Self {
        self.realm = Some(realm.into());
        self
    }

    /// Register this Contact URI instead of the derived one.
    pub fn contact(mut self, contact: impl Into<String>) -> Self {
        self.contact = Some(contact.into());
        self
    }

    /// Ask for a binding lifetime (rounded down to whole seconds).
    pub fn expires(mut self, expires: Duration) -> Self {
        self.expires = expires;
        self
    }
}

impl std::fmt::Debug for SipAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SipAccount")
            .field("registrar", &self.registrar)
            .field("username", &self.username)
            .field("password", &"[redacted]")
            .field("realm", &self.realm)
            .field("contact", &self.contact)
            .field("expires", &self.expires)
            .finish()
    }
}

/// Where a [`SipRegistration`] stands.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RegistrationState {
    /// The registrar holds a binding for this long from the last refresh.
    Registered {
        /// The lifetime the registrar granted.
        expires: Duration,
    },
    /// The last refresh failed; it is retried shortly. The previous binding
    /// may still be live until it expires.
    Retrying {
        /// Why the refresh failed.
        error: String,
    },
    /// The binding was removed, or the agent shut down.
    Unregistered,
}

/// A live registration, kept fresh in the background. See
/// [`SipAgent::register`].
pub struct SipRegistration {
    state: watch::Receiver<RegistrationState>,
    // Dropped without `unregister`, the registration still stops refreshing
    // and removes its binding, in the background.
    cancel: tokio_util::sync::DropGuard,
    task: JoinHandle<Result<(), SipError>>,
}

impl SipRegistration {
    /// The registration's current state.
    pub fn state(&self) -> RegistrationState {
        self.state.borrow().clone()
    }

    /// A receiver that sees every state change, e.g. to alert when a refresh
    /// starts failing.
    pub fn watch(&self) -> watch::Receiver<RegistrationState> {
        self.state.clone()
    }

    /// Stop refreshing and remove the binding from the registrar
    /// (a REGISTER with a zero lifetime).
    pub async fn unregister(self) -> Result<(), SipError> {
        drop(self.cancel);
        match self.task.await {
            Ok(result) => result,
            Err(_) => Ok(()),
        }
    }
}

/// One REGISTER exchange; the lifetime granted on success.
async fn register_once(
    registration: &mut rsipstack::dialog::registration::Registration,
    registrar: &rsipstack::rsip::Uri,
    expires: Duration,
) -> Result<Duration, SipError> {
    let requested = u32::try_from(expires.as_secs()).unwrap_or(u32::MAX);
    let response = registration
        .register(registrar.clone(), Some(requested))
        .await?;
    let code = u16::from(response.status_code.clone());
    if !(200..300).contains(&code) {
        return Err(SipError::RegistrationRejected(code));
    }
    Ok(granted_expires(&response).unwrap_or(expires))
}

/// The lifetime a 2xx to REGISTER grants: the `expires` parameter of the
/// Contact, else the `Expires` header.
fn granted_expires(response: &rsipstack::rsip::Response) -> Option<Duration> {
    use rsipstack::rsip::Header;
    let mut header = None;
    for h in response.headers.iter() {
        match h {
            Header::Contact(contact) => {
                let text = contact.to_string().to_ascii_lowercase();
                if let Some(value) = text
                    .split(';')
                    .find_map(|p| p.trim().strip_prefix("expires="))
                {
                    let digits: String = value.chars().take_while(char::is_ascii_digit).collect();
                    if let Ok(secs) = digits.parse() {
                        return Some(Duration::from_secs(secs));
                    }
                }
            }
            Header::Expires(expires) => {
                header = expires.value().trim().parse().ok().map(Duration::from_secs);
            }
            _ => {}
        }
    }
    header
}

/// When to refresh a binding granted for `expires`: at three quarters of
/// it, but not sooner than a few seconds.
fn refresh_after(expires: Duration) -> Duration {
    (expires * 3 / 4).max(Duration::from_secs(5))
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
        let (answer, srtp) = if self.offer.secure {
            // SRTP (SDES): decrypt with the caller's key, encrypt with ours,
            // answering the first offered suite we support.
            let Some(theirs) = self
                .offer
                .crypto
                .iter()
                .find_map(|c| CryptoAttribute::parse(c))
            else {
                let _ = self
                    .dialog
                    .reject(Some(rsipstack::rsip::StatusCode::NotAcceptableHere), None);
                return Err(SipError::NoCommonCrypto);
            };
            let ours = CryptoAttribute {
                tag: theirs.tag,
                suite: theirs.suite,
                key: MasterKey::generate()?,
            };
            let answer = sdp::secure_audio_answer(
                seed() as u64,
                &media_ip.to_string(),
                rtp_port,
                payload_type,
                telephone_event_pt,
                &ours.to_value(),
            );
            let srtp = SrtpPair {
                inbound: SrtpSession::new(theirs.suite, &theirs.key),
                outbound: SrtpSession::new(ours.suite, &ours.key),
            };
            (answer, Some(srtp))
        } else {
            let answer = sdp::audio_answer(
                seed() as u64,
                &media_ip.to_string(),
                rtp_port,
                payload_type,
                telephone_event_pt,
            );
            (answer, None)
        };
        self.dialog
            .accept(None, Some(answer.into_bytes()))
            .map_err(SipError::Sip)?;
        let _ = handle.state().set(bridge::KEY_CALLER, self.from.clone());

        let cancel = CancellationToken::new();
        let media = rtp_media(
            handle,
            Arc::new(rtp_socket),
            remote,
            MediaFormat {
                payload_type,
                telephone_event_pt,
            },
            self.filler,
            srtp,
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

/// The negotiated payload types of a call.
#[derive(Clone, Copy)]
struct MediaFormat {
    /// The G.711 codec.
    payload_type: u8,
    /// RFC 4733 telephone events, when negotiated.
    telephone_event_pt: Option<u8>,
}

/// The two SRTP directions of a secure call.
struct SrtpPair {
    /// Keyed by the caller: unprotects what arrives.
    inbound: SrtpSession,
    /// Keyed by us: protects what we send.
    outbound: SrtpSession,
}

/// Wire a session's voice pump to G.711-over-RTP on a UDP socket.
///
/// Symmetric RTP: packets go to `remote` until the first packet arrives,
/// whose source address then becomes the send target (NAT re-latch).
fn rtp_media(
    handle: &LiveHandle,
    socket: Arc<UdpSocket>,
    remote: SocketAddr,
    format: MediaFormat,
    filler: Option<FillerConfig>,
    srtp: Option<SrtpPair>,
    cancel: CancellationToken,
) -> MediaTasks {
    let (srtp_in, srtp_out) = match srtp {
        Some(pair) => (Some(pair.inbound), Some(pair.outbound)),
        None => (None, None),
    };
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
        format,
        handle.state().clone(),
        mic_tx,
        peer_tx,
        srtp_in,
        cancel.clone(),
    ));
    let outbound = tokio::spawn(outbound_loop(
        socket,
        format.payload_type,
        speaker_rx,
        peer_rx,
        srtp_out,
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
    format: MediaFormat,
    state: State,
    mic_tx: mpsc::Sender<Vec<i16>>,
    peer_tx: watch::Sender<SocketAddr>,
    mut srtp: Option<SrtpSession>,
    cancel: CancellationToken,
) {
    let MediaFormat {
        payload_type,
        telephone_event_pt,
    } = format;
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
        let decrypted;
        let datagram = match srtp.as_mut() {
            // A packet that fails authentication is dropped unread, and
            // does not re-latch the peer address.
            Some(srtp) => match srtp.unprotect(&buf[..len]) {
                Ok(plain) => {
                    decrypted = plain;
                    &decrypted[..]
                }
                Err(_) => continue,
            },
            None => &buf[..len],
        };
        let Some(packet) = rtp::parse(datagram) else {
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
    mut srtp: Option<SrtpSession>,
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
                let mut datagram = sender.packetize(&payload, SAMPLES_PER_PACKET as u32);
                if let Some(srtp) = srtp.as_mut() {
                    match srtp.protect(&datagram) {
                        Some(protected) => datagram = protected,
                        None => continue,
                    }
                }
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

    /// A throwaway password, random per test run.
    fn test_password() -> String {
        MasterKey::generate().unwrap().to_inline()
    }

    /// A registrar that challenges a REGISTER without credentials (401) and
    /// grants one with them for 60 s. Every request it sees is forwarded.
    async fn fake_registrar() -> (SocketAddr, mpsc::UnboundedReceiver<String>) {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = socket.local_addr().unwrap();
        let (seen_tx, seen_rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            while let Ok((len, from)) = socket.recv_from(&mut buf).await {
                let request = String::from_utf8_lossy(&buf[..len]).to_string();
                let echoed: Vec<&str> = request
                    .lines()
                    .filter(|l| {
                        let l = l.to_ascii_lowercase();
                        ["via:", "from:", "call-id:", "cseq:"]
                            .iter()
                            .any(|h| l.starts_with(h))
                    })
                    .collect();
                let to = request
                    .lines()
                    .find(|l| l.to_ascii_lowercase().starts_with("to:"))
                    .unwrap_or("To: <sip:unknown@invalid>");
                let authorized = request.to_ascii_lowercase().contains("\nauthorization:");
                let (status, extra) = if authorized {
                    let contact = request
                        .lines()
                        .find(|l| l.to_ascii_lowercase().starts_with("contact:"))
                        .unwrap_or("Contact: <sip:alice@127.0.0.1>");
                    ("200 OK", format!("{contact};expires=60\r\n"))
                } else {
                    (
                        "401 Unauthorized",
                        "WWW-Authenticate: Digest realm=\"test\", nonce=\"n1\", algorithm=MD5\r\n"
                            .to_string(),
                    )
                };
                let response = format!(
                    "SIP/2.0 {status}\r\n{}\r\n{to};tag=reg\r\n{extra}Content-Length: 0\r\n\r\n",
                    echoed.join("\r\n")
                );
                let _ = socket.send_to(response.as_bytes(), from).await;
                let _ = seen_tx.send(request);
            }
        });
        (addr, seen_rx)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn registers_with_digest_auth_and_unregisters() {
        let password = test_password();
        let (registrar, mut seen) = fake_registrar().await;
        let agent = SipAgent::bind("127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind agent");

        let registration = tokio::time::timeout(
            Duration::from_secs(5),
            agent.register(
                SipAccount::new(format!("sip:{registrar}"), "alice", &password)
                    .expires(Duration::from_secs(300)),
            ),
        )
        .await
        .expect("registration finishes")
        .expect("registrar accepts");
        assert_eq!(
            registration.state(),
            RegistrationState::Registered {
                expires: Duration::from_secs(60)
            },
            "the lifetime the registrar granted, not the one asked for"
        );

        let first = seen.recv().await.unwrap();
        assert!(first.starts_with("REGISTER "), "{first}");
        assert!(!first.to_ascii_lowercase().contains("authorization:"));
        let answered = loop {
            let request = seen.recv().await.unwrap();
            if request.to_ascii_lowercase().contains("authorization:") {
                break request;
            }
        };
        for part in [
            "username=\"alice\"",
            "realm=\"test\"",
            "nonce=\"n1\"",
            "response=",
        ] {
            assert!(answered.contains(part), "missing {part} in {answered}");
        }
        assert!(
            !answered.contains(&password),
            "the password never goes on the wire"
        );

        tokio::time::timeout(Duration::from_secs(5), registration.unregister())
            .await
            .expect("unregister finishes")
            .expect("registrar accepts the removal");
        let mut removed = false;
        while let Ok(request) = seen.try_recv() {
            removed |= request
                .lines()
                .any(|l| l.eq_ignore_ascii_case("expires: 0"));
        }
        assert!(
            removed,
            "a REGISTER with a zero lifetime removes the binding"
        );
        agent.shutdown();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_refused_registration_fails_at_once() {
        // A registrar that refuses everyone.
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let registrar = socket.local_addr().unwrap();
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            while let Ok((len, from)) = socket.recv_from(&mut buf).await {
                let request = String::from_utf8_lossy(&buf[..len]).to_string();
                let echoed: Vec<&str> = request
                    .lines()
                    .filter(|l| {
                        let l = l.to_ascii_lowercase();
                        ["via:", "from:", "to:", "call-id:", "cseq:"]
                            .iter()
                            .any(|h| l.starts_with(h))
                    })
                    .collect();
                let response = format!(
                    "SIP/2.0 403 Forbidden\r\n{}\r\nContent-Length: 0\r\n\r\n",
                    echoed.join("\r\n")
                );
                let _ = socket.send_to(response.as_bytes(), from).await;
            }
        });
        let agent = SipAgent::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            agent.register(SipAccount::new(
                format!("sip:{registrar}"),
                "mallory",
                test_password(),
            )),
        )
        .await
        .expect("finishes");
        assert!(
            matches!(result, Err(SipError::RegistrationRejected(403))),
            "{:?}",
            result.err()
        );
        agent.shutdown();
    }

    /// Read datagrams until a SIP response with status `code` arrives.
    async fn recv_response(socket: &UdpSocket, code: &str) -> String {
        let mut buf = [0u8; 4096];
        loop {
            let (len, _) = tokio::time::timeout(Duration::from_secs(3), socket.recv_from(&mut buf))
                .await
                .expect("a response")
                .unwrap();
            let text = String::from_utf8_lossy(&buf[..len]).to_string();
            if text.lines().next().is_some_and(|l| l.contains(code)) {
                return text;
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_srtp_offer_gets_encrypted_media_both_ways() {
        use super::super::srtp::SrtpSuite;
        use base64::Engine as _;

        // The model says 200 ms of a tone once the call is up.
        let tone: Vec<u8> = (0..4800i16)
            .flat_map(|i| (if i % 48 < 24 { 6000i16 } else { -6000 }).to_le_bytes())
            .collect();
        let (transport, control) = crate::live::scripted::ScriptedServer::new()
            .frame(serde_json::json!({
                "serverContent": { "modelTurn": { "parts": [{ "inlineData": {
                    "mimeType": "audio/pcm;rate=24000",
                    "data": base64::engine::general_purpose::STANDARD.encode(&tone),
                } }] } }
            }))
            .into_transport();
        let handle = crate::live::Live::builder()
            .connect_with_transport(transport)
            .await
            .unwrap();

        let mut agent = SipAgent::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let target = format!("127.0.0.1:{}", agent.sip_port());
        let (call_tx, call_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            if let Some(incoming) = agent.next_call().await {
                let _ = call_tx.send(incoming);
            }
            // Keep the agent (and its endpoint) alive for the call.
            std::future::pending::<()>().await;
        });

        // The caller: a SIP socket, an RTP socket, and its own SRTP key.
        let uac = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let uac_port = uac.local_addr().unwrap().port();
        let media = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let media_port = media.local_addr().unwrap().port();
        let caller_key = MasterKey::generate().unwrap();
        let sdp_body = format!(
            "v=0\r\no=probe 1 1 IN IP4 127.0.0.1\r\ns=call\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\n\
             m=audio {media_port} RTP/SAVP 0\r\na=rtpmap:0 PCMU/8000\r\n\
             a=crypto:1 AES_CM_128_HMAC_SHA1_80 inline:{}\r\n",
            caller_key.to_inline()
        );
        let invite = format!(
            "INVITE sip:gemini@{target} SIP/2.0\r\n\
             Via: SIP/2.0/UDP 127.0.0.1:{uac_port};branch=z9hG4bKsrtp1\r\n\
             Max-Forwards: 70\r\n\
             From: <sip:probe@127.0.0.1>;tag=srtp\r\n\
             To: <sip:gemini@{target}>\r\n\
             Call-ID: srtp-1@127.0.0.1\r\n\
             CSeq: 1 INVITE\r\n\
             Contact: <sip:probe@127.0.0.1:{uac_port}>\r\n\
             Content-Type: application/sdp\r\n\
             Content-Length: {}\r\n\r\n{sdp_body}",
            sdp_body.len()
        );
        uac.send_to(invite.as_bytes(), &target).await.unwrap();
        let incoming = tokio::time::timeout(Duration::from_secs(3), call_rx)
            .await
            .unwrap()
            .unwrap();
        assert!(incoming.offer.secure);
        let _call = incoming.answer(&handle).await.expect("answers with SRTP");

        let ok = recv_response(&uac, "200").await;
        let answer_sdp = ok.split("\r\n\r\n").nth(1).unwrap();
        let answer = sdp::parse_audio_offer(answer_sdp).expect("an SDP answer");
        assert!(answer.secure, "{answer_sdp}");
        let agent_crypto = CryptoAttribute::parse(&answer.crypto[0]).unwrap();
        assert_eq!(agent_crypto.tag, 1);
        assert_eq!(agent_crypto.suite, SrtpSuite::AesCm128HmacSha1_80);
        assert_ne!(
            agent_crypto.key, caller_key,
            "the agent sends under its own key"
        );
        let agent_rtp = format!("{}:{}", answer.host, answer.port);

        let speak = |key: &MasterKey, first: u16| {
            let mut session = SrtpSession::new(SrtpSuite::AesCm128HmacSha1_80, key);
            (first..first + 15)
                .map(|seq| {
                    session
                        .protect(&rtp::build(&rtp::RtpPacket {
                            payload_type: 0,
                            marker: seq == first,
                            sequence: seq,
                            timestamp: u32::from(seq) * 160,
                            ssrc: 0xCA11_E500,
                            payload: vec![0x10; 160],
                        }))
                        .unwrap()
                })
                .collect::<Vec<_>>()
        };
        let heard = || {
            control
                .outbound_frames()
                .iter()
                .filter_map(|frame| serde_json::from_slice::<serde_json::Value>(frame).ok())
                .any(|message| message.pointer("/realtimeInput/audio").is_some())
        };

        // Caller → agent under a key the agent was not given: dropped.
        for packet in speak(&MasterKey::generate().unwrap(), 100) {
            media.send_to(&packet, &agent_rtp).await.unwrap();
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            !heard(),
            "packets that fail authentication never reach the model"
        );

        // Under the offered key: decrypted speech reaches the model.
        for packet in speak(&caller_key, 200) {
            media.send_to(&packet, &agent_rtp).await.unwrap();
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(heard(), "decrypted caller audio reached the model");

        // Agent → caller: the model's audio arrives as SRTP under the
        // agent's key, and decrypts to non-silent G.711.
        control.release();
        let mut from_agent = SrtpSession::new(agent_crypto.suite, &agent_crypto.key);
        let mut buf = [0u8; 2048];
        let (len, _) = tokio::time::timeout(Duration::from_secs(3), media.recv_from(&mut buf))
            .await
            .expect("the agent sends media")
            .unwrap();
        let plain = from_agent
            .unprotect(&buf[..len])
            .expect("authenticates under the answered key");
        let packet = rtp::parse(&plain).unwrap();
        assert_eq!(packet.payload_type, 0);
        assert!(packet.payload.iter().any(|b| *b != 0xFF), "not silence");
    }

    #[test]
    fn an_account_never_prints_its_password() {
        let password = test_password();
        let account = SipAccount::new("sip:pbx.example.com", "alice", &password);
        assert!(!format!("{account:?}").contains(&password));
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
