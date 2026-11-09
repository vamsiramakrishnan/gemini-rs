//! Wire-level regressions for the rsipstack 0.8 migration. No provider is used.

use super::*;
use rsipstack::rsip::{HeadersExt as _, Method, Response};

pub(super) const DEADLINE: Duration = Duration::from_secs(3);
pub(super) const OFFER: &str = "v=0\r\no=probe 1 1 IN IP4 127.0.0.1\r\ns=call\r\n\
    c=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio 40000 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\n";

pub(super) struct Peer {
    socket: UdpSocket,
    target: SocketAddr,
}

impl Peer {
    pub(super) async fn new(agent: &SipAgent) -> Self {
        Self {
            socket: UdpSocket::bind("127.0.0.1:0").await.unwrap(),
            target: SocketAddr::from(([127, 0, 0, 1], agent.sip_port())),
        }
    }

    pub(super) fn request(
        &self,
        method: &str,
        seq: u32,
        branch: &str,
        to: Option<&str>,
        body: &str,
    ) -> String {
        let addr = self.socket.local_addr().unwrap();
        let to = to
            .map(str::to_owned)
            .unwrap_or_else(|| format!("<sip:gemini@{}>", self.target));
        format!(
            "{method} sip:gemini@{} SIP/2.0\r\n\
             Via: SIP/2.0/UDP {addr};branch=z9hG4bK{branch}\r\n\
             Max-Forwards: 70\r\n\
             From: <sip:probe@{addr}>;tag=probe\r\n\
             To: {to}\r\n\
             Call-ID: migration@{addr}\r\n\
             CSeq: {seq} {method}\r\n\
             Contact: <sip:probe@{addr}>\r\n\
             Content-Type: application/sdp\r\n\
             Content-Length: {}\r\n\r\n{body}",
            self.target,
            body.len()
        )
    }

    pub(super) async fn send(&self, request: &str) {
        self.socket
            .send_to(request.as_bytes(), self.target)
            .await
            .unwrap();
    }

    pub(super) async fn response(&self, status: u16, method: Method) -> Response {
        tokio::time::timeout(DEADLINE, async {
            let mut buf = [0u8; 4096];
            loop {
                let (len, _) = self.socket.recv_from(&mut buf).await.unwrap();
                let response = Response::try_from(&buf[..len]).expect("valid SIP response");
                if u16::from(response.status_code.clone()) == status
                    && response.cseq_header().unwrap().method().unwrap() == method
                {
                    return response;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("missing {status} response to {method}"))
    }
}

pub(super) fn drive(
    mut agent: SipAgent,
) -> (mpsc::UnboundedReceiver<IncomingCall>, JoinHandle<()>) {
    let (sender, receiver) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        while let Some(call) = agent.next_call().await {
            if sender.send(call).is_err() {
                break;
            }
        }
        agent.shutdown();
    });
    (receiver, task)
}

pub(super) async fn incoming(calls: &mut mpsc::UnboundedReceiver<IncomingCall>) -> IncomingCall {
    tokio::time::timeout(DEADLINE, calls.recv())
        .await
        .unwrap()
        .expect("incoming call")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn extension_method_replies_preserve_the_method_and_later_invites() {
    let agent = SipAgent::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let stop = agent.cancel.clone().drop_guard();
    let peer = Peer::new(&agent).await;
    let (mut calls, task) = drive(agent);

    peer.send(&peer.request("X-PROBE", 1, "extension", None, ""))
        .await;
    let response = peer.response(405, Method::Other("X-PROBE".into())).await;
    assert_eq!(response.cseq_header().unwrap().seq().unwrap(), 1);
    assert!(matches!(
        calls.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));

    peer.send(&peer.request("INVITE", 2, "invite", None, OFFER))
        .await;
    let call = incoming(&mut calls).await;
    assert_eq!(call.offer.g711_payload_type(), Some(rtp::PT_PCMU));
    assert!(call.from.contains("probe"));
    peer.response(180, Method::Invite).await;
    call.reject();
    peer.response(603, Method::Invite).await;

    drop(stop);
    tokio::time::timeout(DEADLINE, task).await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn strict_parsing_rejects_bad_framing_and_preserves_udp_body_without_length() {
    let agent = SipAgent::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let stop = agent.cancel.clone().drop_guard();
    let peer = Peer::new(&agent).await;
    let (mut calls, task) = drive(agent);

    let invite = peer.request("INVITE", 1, "bad", None, OFFER);
    for invalid in [
        invite.replace(
            &format!("Content-Length: {}", OFFER.len()),
            "Content-Length: 9999",
        ),
        invite.replace("Content-Length:", "l: 0\r\nContent-Length:"),
    ] {
        peer.send(&invalid).await;
        peer.response(400, Method::Invite).await;
        assert!(matches!(
            calls.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    // Core-header parse failures may be discarded without a response when
    // routing cannot be trusted. An OPTIONS barrier proves the agent keeps
    // serving without surfacing any of these malformed INVITEs.
    for invalid in [
        invite.replace("CSeq: 1 INVITE", "CSeq: 1 OPTIONS"),
        invite.replace("Max-Forwards: 70", "Max-Forwards: 999"),
        invite.replace(
            &format!("Contact: <sip:probe@{}>", peer.socket.local_addr().unwrap()),
            "Contact: <sip:>",
        ),
    ] {
        assert!(rsipstack::rsip::Request::try_from(invalid.as_str()).is_err());
        peer.send(&invalid).await;
    }
    peer.send(&peer.request("OPTIONS", 1, "barrier", None, ""))
        .await;
    peer.response(200, Method::Options).await;
    assert!(matches!(
        calls.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));

    // This agent only binds UDP: RFC 3261 permits an omitted length there.
    let valid = peer
        .request("INVITE", 2, "valid", None, OFFER)
        .replace(&format!("Content-Length: {}\r\n", OFFER.len()), "");
    peer.send(&valid).await;
    let call = incoming(&mut calls).await;
    assert_eq!(call.offer.port, 40000);
    call.reject();
    peer.response(603, Method::Invite).await;

    drop(stop);
    tokio::time::timeout(DEADLINE, task).await.unwrap().unwrap();
}
