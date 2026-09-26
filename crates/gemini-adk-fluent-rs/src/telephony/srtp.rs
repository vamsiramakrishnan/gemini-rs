//! SRTP (RFC 3711) for SIP media, keyed by SDES (RFC 4568).
//!
//! *(feature `sip`)* A SIP trunk or softphone that offers `RTP/SAVP` with an
//! `a=crypto` line sends its media encrypted and authenticated. This module
//! is the packet layer: [`SrtpSession::protect`] turns an RTP packet into an
//! SRTP packet, and [`SrtpSession::unprotect`] checks and decrypts one.
//! [`CryptoAttribute`] parses and prints the SDP line that carries the keys.
//!
//! Supported: the `AES_CM_128_HMAC_SHA1_80` and `AES_CM_128_HMAC_SHA1_32`
//! suites (the ones every SDES endpoint implements), a key derivation rate
//! of zero, and no MKI. SRTCP is not implemented, since the agent does not
//! send RTCP; an offer is answered for RTP only.
//!
//! SDES sends the keys in the SDP body, so they are only as secret as the
//! signalling. Use SIP over TLS when the keys must not be readable on the
//! network path.

use std::collections::HashMap;
use std::fmt;

use aes::Aes128;
use aes::cipher::{BlockEncrypt, KeyInit, KeyIvInit, StreamCipher, generic_array::GenericArray};
use base64::Engine as _;
use hmac::{Hmac, Mac};
use sha1::Sha1;

type Aes128Ctr = ctr::Ctr128BE<Aes128>;
type HmacSha1 = Hmac<Sha1>;

const MASTER_KEY_LEN: usize = 16;
const MASTER_SALT_LEN: usize = 14;
const AUTH_KEY_LEN: usize = 20;
/// Packets this far behind the newest one are rejected as possible replays.
const REPLAY_WINDOW: u64 = 64;

/// An SRTP crypto suite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SrtpSuite {
    /// AES-128 counter mode, HMAC-SHA1 with an 80-bit tag.
    AesCm128HmacSha1_80,
    /// AES-128 counter mode, HMAC-SHA1 with a 32-bit tag.
    AesCm128HmacSha1_32,
}

impl SrtpSuite {
    /// The suite's SDP name.
    pub fn name(self) -> &'static str {
        match self {
            Self::AesCm128HmacSha1_80 => "AES_CM_128_HMAC_SHA1_80",
            Self::AesCm128HmacSha1_32 => "AES_CM_128_HMAC_SHA1_32",
        }
    }

    /// The suite named `name` in SDP, when supported.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "AES_CM_128_HMAC_SHA1_80" => Some(Self::AesCm128HmacSha1_80),
            "AES_CM_128_HMAC_SHA1_32" => Some(Self::AesCm128HmacSha1_32),
            _ => None,
        }
    }

    /// Bytes of authentication tag appended to each packet.
    pub fn tag_len(self) -> usize {
        match self {
            Self::AesCm128HmacSha1_80 => 10,
            Self::AesCm128HmacSha1_32 => 4,
        }
    }
}

/// A master key and salt, from which the session keys are derived.
#[derive(Clone, PartialEq, Eq)]
pub struct MasterKey {
    key: [u8; MASTER_KEY_LEN],
    salt: [u8; MASTER_SALT_LEN],
}

impl MasterKey {
    /// A key and salt from their bytes.
    pub fn new(key: [u8; MASTER_KEY_LEN], salt: [u8; MASTER_SALT_LEN]) -> Self {
        Self { key, salt }
    }

    /// A fresh random key and salt from the operating system's generator.
    pub fn generate() -> std::io::Result<Self> {
        let mut bytes = [0u8; MASTER_KEY_LEN + MASTER_SALT_LEN];
        getrandom::getrandom(&mut bytes).map_err(std::io::Error::other)?;
        Ok(Self::from_bytes(&bytes).expect("the length is right"))
    }

    fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != MASTER_KEY_LEN + MASTER_SALT_LEN {
            return None;
        }
        let mut key = [0u8; MASTER_KEY_LEN];
        let mut salt = [0u8; MASTER_SALT_LEN];
        key.copy_from_slice(&bytes[..MASTER_KEY_LEN]);
        salt.copy_from_slice(&bytes[MASTER_KEY_LEN..]);
        Some(Self { key, salt })
    }

    /// The SDES `inline:` key parameter: base64 of key then salt.
    pub fn to_inline(&self) -> String {
        let mut bytes = Vec::with_capacity(MASTER_KEY_LEN + MASTER_SALT_LEN);
        bytes.extend_from_slice(&self.key);
        bytes.extend_from_slice(&self.salt);
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    /// Parse an SDES `inline:` key parameter (with or without the prefix).
    /// A lifetime (`|2^31`) is accepted and ignored; an MKI (`|1:4`) is not
    /// supported and fails the parse.
    pub fn from_inline(inline: &str) -> Option<Self> {
        let inline = inline.strip_prefix("inline:").unwrap_or(inline);
        let mut parts = inline.split('|');
        let key = parts.next()?;
        for part in parts {
            if part.contains(':') {
                return None; // an MKI
            }
        }
        let bytes = base64::engine::general_purpose::STANDARD.decode(key).ok()?;
        Self::from_bytes(&bytes)
    }
}

impl fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MasterKey([redacted])")
    }
}

/// An SDES `a=crypto` attribute (RFC 4568).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CryptoAttribute {
    /// The attribute's tag, echoed in the answer.
    pub tag: u32,
    /// The crypto suite.
    pub suite: SrtpSuite,
    /// The sender's master key.
    pub key: MasterKey,
}

impl CryptoAttribute {
    /// Parse the value of an `a=crypto:` line, e.g.
    /// `1 AES_CM_128_HMAC_SHA1_80 inline:WVNfX19zZW...`. `None` for an
    /// unsupported suite, key form or session parameter.
    pub fn parse(value: &str) -> Option<Self> {
        let value = value.strip_prefix("a=crypto:").unwrap_or(value);
        let mut parts = value.split_whitespace();
        let tag = parts.next()?.parse().ok()?;
        let suite = SrtpSuite::from_name(parts.next()?)?;
        let key_params = parts.next()?;
        // One key only: several `;`-joined keys need an MKI to tell apart.
        if key_params.contains(';') {
            return None;
        }
        let key = MasterKey::from_inline(key_params)?;
        // Session parameters (UNENCRYPTED_SRTP, KDR=.., ...) change the
        // transform; none is supported.
        if parts.next().is_some() {
            return None;
        }
        Some(Self { tag, suite, key })
    }

    /// The attribute's value, for an `a=crypto:` line.
    pub fn to_value(&self) -> String {
        format!(
            "{} {} inline:{}",
            self.tag,
            self.suite.name(),
            self.key.to_inline()
        )
    }
}

/// Why a packet failed [`SrtpSession::unprotect`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SrtpError {
    /// Too short, or not an RTP version 2 header.
    Malformed,
    /// The authentication tag does not match: altered, or keyed differently.
    AuthenticationFailed,
    /// This packet was already received, or is too old to tell.
    Replayed,
}

impl fmt::Display for SrtpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Malformed => "malformed SRTP packet",
            Self::AuthenticationFailed => "SRTP authentication failed",
            Self::Replayed => "replayed SRTP packet",
        })
    }
}

impl std::error::Error for SrtpError {}

/// Keys derived from a master key (RFC 3711 §4.3, key derivation rate 0).
struct SessionKeys {
    cipher_key: [u8; MASTER_KEY_LEN],
    salt: [u8; MASTER_SALT_LEN],
    auth_key: [u8; AUTH_KEY_LEN],
}

impl SessionKeys {
    fn derive(master: &MasterKey) -> Self {
        let mut cipher_key = [0u8; MASTER_KEY_LEN];
        let mut salt = [0u8; MASTER_SALT_LEN];
        let mut auth_key = [0u8; AUTH_KEY_LEN];
        derive(master, 0x00, &mut cipher_key);
        derive(master, 0x01, &mut auth_key);
        derive(master, 0x02, &mut salt);
        Self {
            cipher_key,
            salt,
            auth_key,
        }
    }
}

/// The AES-CM PRF over `x = label XOR master salt`, index 0.
fn derive(master: &MasterKey, label: u8, out: &mut [u8]) {
    let mut iv = [0u8; 16];
    iv[..MASTER_SALT_LEN].copy_from_slice(&master.salt);
    iv[7] ^= label;
    out.fill(0);
    Aes128Ctr::new(&master.key.into(), &iv.into()).apply_keystream(out);
}

/// Receive-side state of one SSRC.
#[derive(Default)]
struct Inbound {
    roc: u32,
    highest_seq: u16,
    /// Newest accepted packet index, and which of the 64 before it arrived.
    newest: u64,
    seen: u64,
}

/// Send-side state of one SSRC.
#[derive(Default)]
struct Outbound {
    roc: u32,
    last_seq: Option<u16>,
}

/// One direction of an SRTP stream: protect what you send with your key,
/// unprotect what you receive with the peer's.
pub struct SrtpSession {
    suite: SrtpSuite,
    keys: SessionKeys,
    cipher: Aes128,
    outbound: HashMap<u32, Outbound>,
    inbound: HashMap<u32, Inbound>,
}

impl fmt::Debug for SrtpSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SrtpSession")
            .field("suite", &self.suite)
            .finish_non_exhaustive()
    }
}

impl SrtpSession {
    /// A session keyed by `key`.
    pub fn new(suite: SrtpSuite, key: &MasterKey) -> Self {
        let keys = SessionKeys::derive(key);
        let cipher = Aes128::new(&keys.cipher_key.into());
        Self {
            suite,
            keys,
            cipher,
            outbound: HashMap::new(),
            inbound: HashMap::new(),
        }
    }

    /// Encrypt and authenticate an RTP packet. `None` when `rtp` is not an
    /// RTP packet.
    pub fn protect(&mut self, rtp: &[u8]) -> Option<Vec<u8>> {
        let header = header_len(rtp)?;
        let (seq, ssrc) = seq_ssrc(rtp);
        let state = self.outbound.entry(ssrc).or_default();
        if let Some(last) = state.last_seq
            && seq < last
            && last - seq > 0x8000
        {
            state.roc = state.roc.wrapping_add(1);
        }
        state.last_seq = Some(seq);
        let roc = state.roc;
        let index = (u64::from(roc) << 16) | u64::from(seq);

        let mut packet = rtp.to_vec();
        self.keystream(ssrc, index, &mut packet[header..]);
        let tag = self.tag(&packet, roc);
        packet.extend_from_slice(&tag[..self.suite.tag_len()]);
        Some(packet)
    }

    /// Authenticate and decrypt an SRTP packet back to RTP.
    pub fn unprotect(&mut self, srtp: &[u8]) -> Result<Vec<u8>, SrtpError> {
        let tag_len = self.suite.tag_len();
        if srtp.len() < tag_len {
            return Err(SrtpError::Malformed);
        }
        let (authenticated, tag) = srtp.split_at(srtp.len() - tag_len);
        let header = header_len(authenticated).ok_or(SrtpError::Malformed)?;
        let (seq, ssrc) = seq_ssrc(authenticated);

        let (roc, index) = match self.inbound.get(&ssrc) {
            Some(state) => {
                let roc = estimate_roc(state.roc, state.highest_seq, seq);
                let index = (u64::from(roc) << 16) | u64::from(seq);
                if is_replay(state, index) {
                    return Err(SrtpError::Replayed);
                }
                (roc, index)
            }
            None => (0, u64::from(seq)),
        };

        let mut mac =
            <HmacSha1 as Mac>::new_from_slice(&self.keys.auth_key).expect("any key length");
        mac.update(authenticated);
        mac.update(&roc.to_be_bytes());
        mac.verify_truncated_left(tag)
            .map_err(|_| SrtpError::AuthenticationFailed)?;

        let mut packet = authenticated.to_vec();
        self.keystream(ssrc, index, &mut packet[header..]);
        self.accept(ssrc, roc, seq, index);
        Ok(packet)
    }

    /// Record an authenticated packet: advance the rollover counter and the
    /// replay window.
    fn accept(&mut self, ssrc: u32, roc: u32, seq: u16, index: u64) {
        let state = self.inbound.entry(ssrc).or_insert_with(|| Inbound {
            roc,
            highest_seq: seq,
            newest: index,
            seen: 0,
        });
        if index > state.newest {
            let shift = index - state.newest;
            state.seen = if shift >= REPLAY_WINDOW {
                0
            } else {
                state.seen << shift
            };
            state.newest = index;
            state.roc = roc;
            state.highest_seq = seq;
        }
        let behind = state.newest - index;
        state.seen |= 1 << behind;
    }

    /// XOR the AES-CM keystream for packet `index` of `ssrc` into `data`.
    fn keystream(&self, ssrc: u32, index: u64, data: &mut [u8]) {
        let mut counter = [0u8; 16];
        counter[..MASTER_SALT_LEN].copy_from_slice(&self.keys.salt);
        for (i, b) in ssrc.to_be_bytes().iter().enumerate() {
            counter[4 + i] ^= b;
        }
        for (i, b) in index.to_be_bytes()[2..].iter().enumerate() {
            counter[8 + i] ^= b;
        }
        for chunk in data.chunks_mut(16) {
            let mut block = GenericArray::from(counter);
            self.cipher.encrypt_block(&mut block);
            for (d, k) in chunk.iter_mut().zip(block.iter()) {
                *d ^= k;
            }
            let next = u16::from_be_bytes([counter[14], counter[15]]).wrapping_add(1);
            counter[14..].copy_from_slice(&next.to_be_bytes());
        }
    }

    fn tag(&self, authenticated: &[u8], roc: u32) -> [u8; 20] {
        let mut mac =
            <HmacSha1 as Mac>::new_from_slice(&self.keys.auth_key).expect("any key length");
        mac.update(authenticated);
        mac.update(&roc.to_be_bytes());
        mac.finalize().into_bytes().into()
    }
}

/// The RTP header's length, including CSRCs and an extension.
fn header_len(packet: &[u8]) -> Option<usize> {
    if packet.len() < 12 || packet[0] >> 6 != 2 {
        return None;
    }
    let mut len = 12 + usize::from(packet[0] & 0x0F) * 4;
    if packet[0] & 0x10 != 0 {
        let words = packet.get(len + 2..len + 4)?;
        len += 4 + usize::from(u16::from_be_bytes([words[0], words[1]])) * 4;
    }
    (len <= packet.len()).then_some(len)
}

fn seq_ssrc(packet: &[u8]) -> (u16, u32) {
    (
        u16::from_be_bytes([packet[2], packet[3]]),
        u32::from_be_bytes([packet[8], packet[9], packet[10], packet[11]]),
    )
}

/// The rollover counter a packet with `seq` most likely belongs to
/// (RFC 3711 §3.3.1).
fn estimate_roc(roc: u32, highest: u16, seq: u16) -> u32 {
    if highest < 0x8000 {
        if seq > highest && seq - highest > 0x8000 {
            roc.wrapping_sub(1)
        } else {
            roc
        }
    } else if highest - 0x8000 > seq {
        roc.wrapping_add(1)
    } else {
        roc
    }
}

fn is_replay(state: &Inbound, index: u64) -> bool {
    if index > state.newest {
        return false;
    }
    let behind = state.newest - index;
    behind >= REPLAY_WINDOW || state.seen & (1 << behind) != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        let s: String = s.split_whitespace().collect();
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    fn rfc_master() -> MasterKey {
        let bytes = hex("E1F97A0D3E018BE0D64FA32C06DE4139 0EC675AD498AFEEBB6960B3AABE6");
        MasterKey::from_bytes(&bytes).unwrap()
    }

    /// RFC 3711 Appendix B.3.
    #[test]
    fn key_derivation_matches_rfc_3711() {
        let keys = SessionKeys::derive(&rfc_master());
        assert_eq!(
            keys.cipher_key.to_vec(),
            hex("C61E7A93744F39EE10734AFE3FF7A087")
        );
        assert_eq!(keys.salt.to_vec(), hex("30CBBC08863D8C85D49DB34A9AE1"));
        assert_eq!(
            keys.auth_key.to_vec(),
            hex("CEBE321F6FF7716B6FD4AB49AF256A156D38BAA4")
        );
    }

    /// RFC 3711 Appendix B.2: the AES-CM keystream for SSRC 0, index 0.
    #[test]
    fn keystream_matches_rfc_3711() {
        let key: [u8; 16] = hex("2B7E151628AED2A6ABF7158809CF4F3C").try_into().unwrap();
        let salt: [u8; 14] = hex("F0F1F2F3F4F5F6F7F8F9FAFBFCFD").try_into().unwrap();
        let session = SrtpSession {
            suite: SrtpSuite::AesCm128HmacSha1_80,
            keys: SessionKeys {
                cipher_key: key,
                salt,
                auth_key: [0; AUTH_KEY_LEN],
            },
            cipher: Aes128::new(&key.into()),
            outbound: HashMap::new(),
            inbound: HashMap::new(),
        };
        let mut stream = [0u8; 48];
        session.keystream(0, 0, &mut stream);
        assert_eq!(
            stream.to_vec(),
            hex("E03EAD0935C95E80E166B16DD92B4EB4 \
                 D23513162B02D0F72A43A2FE4A5F97AB \
                 41E95B3BB0A2E8DD477901E4FCA894C0")
        );
    }

    /// The reference packet of libsrtp's test driver: SSRC 0xCAFEBABE,
    /// sequence 0x1234, sixteen bytes of 0xAB.
    #[test]
    fn protects_the_reference_packet() {
        let plain = hex("800f1234 decafbad cafebabe abababab abababab abababab abababab");
        let expected = hex(
            "800f1234 decafbad cafebabe 4e55dc4c e79978d8 8ca4d215 949d2402 \
             b78d6acc 99ea179b 8dbb",
        );
        let mut sender = SrtpSession::new(SrtpSuite::AesCm128HmacSha1_80, &rfc_master());
        assert_eq!(sender.protect(&plain).unwrap(), expected);

        let mut receiver = SrtpSession::new(SrtpSuite::AesCm128HmacSha1_80, &rfc_master());
        assert_eq!(receiver.unprotect(&expected).unwrap(), plain);
    }

    fn rtp(seq: u16, payload: &[u8]) -> Vec<u8> {
        let mut packet = vec![0x80, 0x00];
        packet.extend_from_slice(&seq.to_be_bytes());
        packet.extend_from_slice(&[0, 0, 0, 160]);
        packet.extend_from_slice(&0x1234_5678u32.to_be_bytes());
        packet.extend_from_slice(payload);
        packet
    }

    #[test]
    fn a_tampered_or_replayed_packet_is_refused() {
        let key = MasterKey::generate().unwrap();
        for suite in [
            SrtpSuite::AesCm128HmacSha1_80,
            SrtpSuite::AesCm128HmacSha1_32,
        ] {
            let mut tx = SrtpSession::new(suite, &key);
            let mut rx = SrtpSession::new(suite, &key);
            let packet = tx.protect(&rtp(7, b"hello")).unwrap();
            assert_eq!(packet.len(), 12 + 5 + suite.tag_len());
            assert_ne!(&packet[12..17], b"hello", "the payload is encrypted");

            let mut tampered = packet.clone();
            tampered[13] ^= 1;
            assert_eq!(
                rx.unprotect(&tampered),
                Err(SrtpError::AuthenticationFailed)
            );

            assert_eq!(rx.unprotect(&packet).unwrap(), rtp(7, b"hello"));
            assert_eq!(rx.unprotect(&packet), Err(SrtpError::Replayed));

            let other =
                SrtpSession::new(suite, &MasterKey::generate().unwrap()).protect(&rtp(8, b"x"));
            assert_eq!(
                rx.unprotect(&other.unwrap()),
                Err(SrtpError::AuthenticationFailed),
                "a packet under another key"
            );
        }
    }

    #[test]
    fn the_rollover_counter_follows_a_sequence_wrap() {
        let key = MasterKey::generate().unwrap();
        let suite = SrtpSuite::AesCm128HmacSha1_80;
        let mut tx = SrtpSession::new(suite, &key);
        let mut rx = SrtpSession::new(suite, &key);
        let mut seq: u16 = 65_530;
        for n in 0..20u8 {
            let packet = tx.protect(&rtp(seq, &[n; 4])).unwrap();
            assert_eq!(
                rx.unprotect(&packet).unwrap(),
                rtp(seq, &[n; 4]),
                "seq {seq}"
            );
            seq = seq.wrapping_add(1);
        }
        assert_eq!(tx.outbound[&0x1234_5678].roc, 1);
        assert_eq!(rx.inbound[&0x1234_5678].roc, 1);

        // A packet from before the wrap that arrives after it still
        // authenticates, under the previous rollover count.
        let mut tx = SrtpSession::new(suite, &key);
        let mut rx = SrtpSession::new(suite, &key);
        let early = tx.protect(&rtp(65_534, b"a")).unwrap();
        let late = tx.protect(&rtp(65_535, b"b")).unwrap();
        let wrapped = tx.protect(&rtp(0, b"c")).unwrap();
        assert!(rx.unprotect(&early).is_ok());
        assert!(rx.unprotect(&wrapped).is_ok());
        assert_eq!(rx.unprotect(&late).unwrap(), rtp(65_535, b"b"));
        assert_eq!(rx.unprotect(&late), Err(SrtpError::Replayed));
    }

    #[test]
    fn a_crypto_line_round_trips() {
        let line = "1 AES_CM_128_HMAC_SHA1_80 inline:4fl6DT4Bi+DWT6MsBt5BOQ7Gda1Jiv7rtpYLOqvm|2^20";
        let attribute = CryptoAttribute::parse(line).unwrap();
        assert_eq!(attribute.tag, 1);
        assert_eq!(attribute.suite, SrtpSuite::AesCm128HmacSha1_80);
        assert_eq!(attribute.key, rfc_master());
        assert_eq!(
            CryptoAttribute::parse(&attribute.to_value()).unwrap(),
            attribute
        );
        assert!(
            !format!("{attribute:?}").contains("4fl6DT4B"),
            "keys never print"
        );

        assert_eq!(
            CryptoAttribute::parse(
                "1 F8_128_HMAC_SHA1_80 inline:4fl6DT4Bi+DWT6MsBt5BOQ7Gda1Jiv7rtpYLOqvm"
            ),
            None
        );
        assert_eq!(
            CryptoAttribute::parse(
                "1 AES_CM_128_HMAC_SHA1_80 inline:4fl6DT4Bi+DWT6MsBt5BOQ7Gda1Jiv7rtpYLOqvm|2^20|1:4"
            ),
            None,
            "an MKI"
        );
        assert_eq!(
            CryptoAttribute::parse(
                "1 AES_CM_128_HMAC_SHA1_80 inline:4fl6DT4Bi+DWT6MsBt5BOQ7Gda1Jiv7rtpYLOqvm KDR=1"
            ),
            None,
            "a session parameter"
        );
    }
}
