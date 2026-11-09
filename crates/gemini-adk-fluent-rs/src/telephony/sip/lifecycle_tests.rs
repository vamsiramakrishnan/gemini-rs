//! SIP lifecycle regressions that also fail with rsipstack 0.7.14.

use super::migration_tests::{DEADLINE, OFFER, Peer, drive, incoming};
use super::*;
use rsipstack::rsip::{HeadersExt as _, Method};

async fn state(call: &mut IncomingCall, predicate: impl Fn(&DialogState) -> bool) -> DialogState {
    tokio::time::timeout(DEADLINE, async {
        while let Some(state) = call.state_rx.recv().await {
            if predicate(&state) {
                return state;
            }
        }
        panic!("dialog state stream closed")
    })
    .await
    .expect("dialog transitions")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ack_confirms_and_bye_terminates_the_same_dialog() {
    use rsipstack::dialog::dialog::TerminatedReason;

    let agent = SipAgent::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let stop = agent.cancel.clone().drop_guard();
    let peer = Peer::new(&agent).await;
    let (mut calls, task) = drive(agent);

    peer.send(&peer.request("INVITE", 1, "invite", None, OFFER))
        .await;
    let mut call = incoming(&mut calls).await;
    let id = call.dialog.id();
    call.dialog
        .accept(None, Some(OFFER.as_bytes().to_vec()))
        .unwrap();
    let response = peer.response(200, Method::Invite).await;
    let to = response.to_header().unwrap().value();

    peer.send(&peer.request("ACK", 1, "ack", Some(to), ""))
        .await;
    let confirmed = state(&mut call, |s| matches!(s, DialogState::Confirmed(..))).await;
    assert!(matches!(confirmed, DialogState::Confirmed(confirmed_id, _) if confirmed_id == id));
    assert_eq!(call.dialog.last_remote_ack().unwrap().method, Method::Ack);

    peer.send(&peer.request("BYE", 2, "bye", Some(to), ""))
        .await;
    peer.response(200, Method::Bye).await;
    let ended = state(&mut call, |s| matches!(s, DialogState::Terminated(..))).await;
    assert!(
        matches!(ended, DialogState::Terminated(ended_id, TerminatedReason::UacBye) if ended_id == id)
    );
    assert!(matches!(
        calls.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));

    drop(stop);
    tokio::time::timeout(DEADLINE, task).await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_terminates_a_ringing_invite_and_retransmits_its_reply() {
    use rsipstack::dialog::dialog::TerminatedReason;

    let agent = SipAgent::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let stop = agent.cancel.clone().drop_guard();
    let peer = Peer::new(&agent).await;
    let (mut calls, task) = drive(agent);

    peer.send(&peer.request("INVITE", 1, "invite", None, OFFER))
        .await;
    let mut call = incoming(&mut calls).await;
    peer.response(180, Method::Invite).await;
    // CANCEL reuses the INVITE's branch, CSeq number and original To header.
    let cancel = peer.request("CANCEL", 1, "invite", None, "");
    peer.send(&cancel).await;
    peer.response(200, Method::Cancel).await;
    let terminated = peer.response(487, Method::Invite).await;
    let ended = state(&mut call, |s| matches!(s, DialogState::Terminated(..))).await;
    assert!(matches!(
        ended,
        DialogState::Terminated(_, TerminatedReason::UacCancel)
    ));

    peer.send(&cancel).await;
    peer.response(200, Method::Cancel).await;
    peer.send(&peer.request(
        "ACK",
        1,
        "invite",
        Some(terminated.to_header().unwrap().value()),
        "",
    ))
    .await;
    peer.send(&peer.request("CANCEL", 2, "unmatched", None, ""))
        .await;
    peer.response(481, Method::Cancel).await;

    drop(stop);
    tokio::time::timeout(DEADLINE, task).await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_ends_an_answered_call_and_the_pending_call_waiter() {
    let (transport, _) = crate::live::scripted::ScriptedServer::new().into_transport();
    let handle = crate::live::Live::builder()
        .connect_with_transport(transport)
        .await
        .unwrap();
    let agent = SipAgent::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let cancel = agent.cancel.clone();
    let _stop = cancel.clone().drop_guard();
    let peer = Peer::new(&agent).await;
    let (mut calls, task) = drive(agent);

    peer.send(&peer.request("INVITE", 1, "invite", None, OFFER))
        .await;
    let call = incoming(&mut calls).await.answer(&handle).await.unwrap();
    let response = peer.response(200, Method::Invite).await;
    peer.send(&peer.request(
        "ACK",
        1,
        "ack",
        Some(response.to_header().unwrap().value()),
        "",
    ))
    .await;

    cancel.cancel();
    tokio::time::timeout(DEADLINE, call.ended())
        .await
        .expect("shutdown ends the call and its media tasks");
    tokio::time::timeout(DEADLINE, task).await.unwrap().unwrap();
    assert!(calls.recv().await.is_none());
}
