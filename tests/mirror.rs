// SPDX-License-Identifier: AGPL-3.0-or-later
//! Messages sent through one client are shown to the other apps, dressed as
//! received messages.

mod common;

use common::*;
use meshkeep::hub::Event;
use meshkeep::protocol::{self as p, cmd, resp};
use reqwest::redirect::Policy;
use serde_json::json;

async fn web_send(rig: &Rig, conv: &str, text: &str) {
    let c = reqwest::Client::builder()
        .redirect(Policy::none())
        .build()
        .unwrap();
    let r = c
        .post(format!("{}/login", rig.web()))
        .form(&[("password", PASSWORD)])
        .send()
        .await
        .unwrap();
    let cookie = r.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let r = c
        .post(format!("{}/api/send", rig.web()))
        .header("cookie", cookie)
        .json(&json!({ "conv": conv, "text": text }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
}

async fn status(rig: &mut Rig, want: &str) {
    rig.wait_event(|e| matches!(e, Event::MessageStatus { status, .. } if status == want))
        .await;
}

fn texts(frames: &[Vec<u8>]) -> Vec<String> {
    frames.iter().map(|f| msg_text(f)).collect()
}

#[tokio::test]
async fn web_sends_reach_every_app() {
    let mut rig = Rig::start().await;
    let alice = rig.alice.prefix();
    web_send(&rig, &format!("dm:{}", p::hex(&alice)), "on my way").await;
    status(&mut rig, "sent").await;
    web_send(&rig, "ch:1", "leaving now").await;
    status(&mut rig, "sent").await;

    for i in 0..2 {
        let mut app = rig.app(i).await;
        app.handshake(3).await;
        let got = app.sync_all().await;
        assert_eq!(texts(&got), ["You: on my way", "SimNode: leaving now"]);
        let dm = p::parse_inbound(&got[0]).unwrap();
        assert_eq!(got[0][0], resp::CONTACT_MSG_RECV_V3);
        assert_eq!(dm.prefix, Some(alice));
        assert_eq!(dm.txt_type, p::txt::PLAIN);
        assert_eq!(dm.path_len, p::MIRROR_PATH_LEN);
        let ch = p::parse_inbound(&got[1]).unwrap();
        assert_eq!(got[1][0], resp::CHANNEL_MSG_RECV_V3);
        assert_eq!(ch.channel, Some(1));
        assert_eq!(ch.sender_ts as u64, UNIX_BASE);
    }

    // The web view shows the sent messages once, not the mirrors.
    let conv = format!("dm:{}", p::hex(&alice));
    let msgs = rig.mk.store.messages(&conv, None, 10);
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].direction, "out");
    assert_eq!(rig.mk.store.messages("ch:1", None, 10).len(), 1);
    assert!(
        rig.mk
            .store
            .conversations()
            .iter()
            .all(|c| c.last_direction == "out")
    );
}

#[tokio::test]
async fn app_sends_reach_other_apps_only() {
    let mut rig = Rig::start().await;
    let mut phone = rig.app(0).await;
    let mut tablet = rig.app(1).await;
    phone.handshake(3).await;
    tablet.handshake(3).await;

    let dm = p::build_send_txt(&rig.bob.prefix(), 4242, 0, "see you there");
    assert_eq!(phone.request(&dm).await[0], resp::SENT);
    // An app retry of the same message is not mirrored twice.
    let retry = p::build_send_txt(&rig.bob.prefix(), 4242, 1, "see you there");
    assert_eq!(phone.request(&retry).await[0], resp::SENT);
    let ch = p::build_send_channel(1, 4243, "parked");
    assert_eq!(phone.request(&ch).await, vec![resp::OK]);

    let got = tablet.sync_all().await;
    assert_eq!(texts(&got), ["You: see you there", "SimNode: parked"]);
    assert_eq!(p::parse_inbound(&got[0]).unwrap().sender_ts, 4242);
    assert!(phone.sync_all().await.is_empty());

    // The sender's own reconnects still do not see them.
    drop(phone);
    rig.wait_event(|e| matches!(e, Event::Apps)).await;
    let mut phone = rig.app(0).await;
    assert!(phone.sync_all().await.is_empty());
}

#[tokio::test]
async fn legacy_apps_get_legacy_mirrors() {
    let mut rig = Rig::start().await;
    let mut tablet = rig.app(1).await;
    tablet.handshake(1).await;
    web_send(&rig, &format!("dm:{}", p::hex(&rig.alice.prefix())), "hi").await;
    status(&mut rig, "sent").await;
    web_send(&rig, "ch:1", "all").await;
    status(&mut rig, "sent").await;
    let got = tablet.sync_all().await;
    assert_eq!(got[0][0], resp::CONTACT_MSG_RECV);
    assert_eq!(got[1][0], resp::CHANNEL_MSG_RECV);
    assert_eq!(texts(&got), ["You: hi", "SimNode: all"]);
}

#[tokio::test]
async fn refused_sends_are_not_mirrored() {
    let mut rig = Rig::start().await;
    let mut phone = rig.app(0).await;
    phone.handshake(3).await;
    web_send(&rig, "dm:010203040506", "nobody").await;
    status(&mut rig, "failed").await;
    // Channel index the radio does not have.
    let bad = p::build_send_channel(200, 1, "void");
    assert_eq!(phone.request(&bad).await[0], resp::ERR);
    // Command text is not mirrored.
    let mut cli = p::build_send_txt(&rig.alice.prefix(), 1, 0, "get name");
    cli[1] = p::txt::CLI_DATA;
    phone.request(&cli).await;
    let mut tablet = rig.app(1).await;
    assert!(tablet.sync_all().await.is_empty());
}

#[tokio::test]
async fn mirroring_can_be_turned_off() {
    let mut rig = Rig::start_with(RigOpts {
        mirror_sent: false,
        ..Default::default()
    })
    .await;
    let mut phone = rig.app(0).await;
    phone.handshake(3).await;
    let dm = p::build_send_txt(&rig.bob.prefix(), 1, 0, "quiet");
    assert_eq!(phone.request(&dm).await[0], resp::SENT);
    web_send(&rig, "ch:1", "quiet too").await;
    status(&mut rig, "sent").await;
    let mut tablet = rig.app(1).await;
    assert!(tablet.sync_all().await.is_empty());
    assert!(phone.sync_all().await.is_empty());
    let _ = cmd::SYNC_NEXT_MESSAGE;
}
