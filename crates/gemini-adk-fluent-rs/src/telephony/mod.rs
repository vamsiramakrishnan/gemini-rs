//! # Telephony — answer the phone with a governed session
//!
//! The public phone network speaks G.711 at 8 kHz; the Live API speaks PCM16
//! at 16/24 kHz. Between them sits exactly the plumbing
//! [`voice::pump`](crate::voice::pump) was built to carry — this module adds
//! the telephone-side dialect:
//!
//! - [`g711`] — μ-law and A-law codecs (pure functions, ITU-T G.711).
//! - [`twilio`] — the Twilio Media Streams protocol and [`TwilioCall`]: attach
//!   a live phone call to a session over any WebSocket server, with barge-in
//!   mapped to Twilio's `clear` and DTMF digits landing in session state.
//!
//! `examples/telephony` is the runnable end: an axum server exposing the
//! TwiML webhook and the Media Streams WebSocket, one governed session per
//! call.

pub mod g711;
pub mod rtp;
pub mod sdp;
pub mod twilio;

#[cfg(feature = "sip")]
pub mod sip;

pub use twilio::{Inbound, StartMeta, TwilioCall, TwilioError, TWILIO_HZ};
