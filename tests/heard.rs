// SPDX-License-Identifier: AGPL-3.0-or-later
//! Repeats of sent channel messages, counted from the radio's RX log.

mod common;

use common::*;
use meshkeep::hub::Event;
use meshkeep::protocol as p;

async fn heard(rig: &mut Rig) -> (i64, u32) {
    let e = rig
        .wait_event(|e| matches!(e, Event::MessageHeard { .. }))
        .await;
    let Event::MessageHeard { id, heard } = e else {
        unreachable!()
    };
    (id, heard)
}

#[tokio::test]
async fn repeats_of_an_app_channel_send_are_counted() {
    let mut rig = Rig::start().await;
    let mut a = rig.app(0).await;
    a.handshake(3).await;
    let send = p::build_send_channel(1, 5000, "hello hills");
    assert_eq!(a.request(&send).await, vec![p::resp::OK]);
    // Somebody else's message in the same second is not ours.
    rig.sim
        .hear_channel_packet(1, 5000, "Carol: hello hills", &[0x10]);
    rig.sim.echo_last_channel_send(&[0xaa]);
    let (id, n) = heard(&mut rig).await;
    assert_eq!(n, 1);
    rig.sim.echo_last_channel_send(&[0xaa, 0xbb]);
    assert_eq!(heard(&mut rig).await, (id, 2));
    let m = rig.mk.store.message(id).unwrap();
    assert_eq!((m.text.as_str(), m.heard), ("hello hills", 2));
    // The app still gets the RX log pushes as before.
    assert_eq!(a.push(p::push::LOG_RX_DATA).await[0], p::push::LOG_RX_DATA);
}

#[tokio::test]
async fn repeats_of_a_web_channel_send_reach_the_browser() {
    let mut rig = Rig::start().await;
    let mut a = rig.app(0).await;
    a.handshake(3).await;
    let c = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
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
        .header("cookie", &cookie)
        .json(&serde_json::json!({ "conv": "ch:0", "text": "testing" }))
        .send()
        .await
        .unwrap();
    let id = r.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();
    rig.wait_event(|e| matches!(e, Event::MessageStatus { status, .. } if status == "sent"))
        .await;
    rig.sim.echo_last_channel_send(&[0x01]);
    assert_eq!(heard(&mut rig).await, (id, 1));
    let msgs: serde_json::Value = c
        .get(format!("{}/api/messages?conv=ch:0", rig.web()))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(msgs[0]["heard"], 1);
}

#[tokio::test]
async fn other_timestamps_are_not_counted() {
    let mut rig = Rig::start().await;
    let mut a = rig.app(0).await;
    let send = p::build_send_channel(1, 6000, "one");
    assert_eq!(a.request(&send).await, vec![p::resp::OK]);
    // Our own name, but a different send (another second).
    rig.sim
        .hear_channel_packet(1, 6001, "SimNode: one", &[0x10]);
    rig.sim.echo_last_channel_send(&[0x20]);
    let (id, n) = heard(&mut rig).await;
    assert_eq!(n, 1);
    assert_eq!(rig.mk.store.message(id).unwrap().heard, 1);
}
