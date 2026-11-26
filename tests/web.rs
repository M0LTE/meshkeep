// SPDX-License-Identifier: AGPL-3.0-or-later
//! Web interface: sign-in, history, the send path and live events.

mod common;

use common::*;
use futures_util::StreamExt;
use meshkeep::hub::{ACK_GRACE, Event};
use meshkeep::protocol as p;
use reqwest::redirect::Policy;
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(Policy::none())
        .build()
        .unwrap()
}

async fn login(rig: &Rig) -> String {
    let r = http()
        .post(format!("{}/login", rig.web()))
        .form(&[("password", PASSWORD)])
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 303);
    let c = r.headers()["set-cookie"].to_str().unwrap();
    c.split(';').next().unwrap().to_string()
}

async fn get_json(rig: &Rig, cookie: &str, path: &str) -> Value {
    http()
        .get(format!("{}{path}", rig.web()))
        .header("cookie", cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn post_send(rig: &Rig, cookie: &str, conv: &str, text: &str) -> reqwest::Response {
    http()
        .post(format!("{}/api/send", rig.web()))
        .header("cookie", cookie)
        .json(&json!({ "conv": conv, "text": text }))
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn sign_in_is_required() {
    let rig = Rig::start().await;
    let c = http();
    let r = c
        .get(format!("{}/api/state", rig.web()))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    let r = c.get(format!("{}/", rig.web())).send().await.unwrap();
    assert_eq!(r.status(), 303);
    assert_eq!(r.headers()["location"], "/login");
    let r = c
        .post(format!("{}/login", rig.web()))
        .form(&[("password", "nope")])
        .send();
    // A wrong password is answered after a delay on the (fake) clock.
    let now = {
        use meshkeep::clock::Clock;
        rig.clock.now_ms()
    };
    let clock = rig.clock.clone();
    let advance = async move {
        clock.wait_for_sleeper(|d| d == now + 1000).await;
        clock.advance(std::time::Duration::from_secs(1));
    };
    let (r, _) = guard(futures_util::future::join(r, advance)).await;
    assert_eq!(r.unwrap().status(), 401);

    let cookie = login(&rig).await;
    let st = get_json(&rig, &cookie, "/api/state").await;
    assert_eq!(st["status"]["radio_connected"], true);
    assert_eq!(st["status"]["name"], "SimNode");
    assert_eq!(st["contacts"].as_array().unwrap().len(), 2);
    assert_eq!(st["channels"][1]["name"], "Hikers");

    // Signing out ends the session.
    http()
        .post(format!("{}/logout", rig.web()))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    let r = c
        .get(format!("{}/api/state", rig.web()))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
}

#[tokio::test]
async fn web_dm_goes_out_and_is_confirmed() {
    let mut rig = Rig::start().await;
    let cookie = login(&rig).await;
    let conv = format!("dm:{}", p::hex(&rig.alice.prefix()));
    let r = post_send(&rig, &cookie, &conv, "  hello from the web ").await;
    assert_eq!(r.status(), 200);
    let id = r.json::<Value>().await.unwrap()["id"].as_i64().unwrap();
    rig.wait_event(|e| matches!(e, Event::MessageStatus { status, .. } if status == "sent"))
        .await;
    assert_eq!(
        rig.sim.sent(),
        vec![(
            rig.alice.prefix().to_vec(),
            "hello from the web".to_string()
        )]
    );
    rig.sim.confirm(rig.sim.last_ack().unwrap());
    rig.wait_event(|e| matches!(e, Event::MessageStatus { status, .. } if status == "delivered"))
        .await;
    let msgs = get_json(&rig, &cookie, &format!("/api/messages?conv={conv}")).await;
    assert_eq!(msgs[0]["id"], id);
    assert_eq!(msgs[0]["status"], "delivered");
    assert_eq!(msgs[0]["direction"], "out");
}

#[tokio::test]
async fn unacked_web_dm_becomes_unconfirmed() {
    let mut rig = Rig::start().await;
    let cookie = login(&rig).await;
    let conv = format!("dm:{}", p::hex(&rig.bob.prefix()));
    post_send(&rig, &cookie, &conv, "anyone there").await;
    rig.wait_event(|e| matches!(e, Event::MessageStatus { status, .. } if status == "sent"))
        .await;
    // The simulator estimates 8 s; meshkeep adds its grace period.
    let now = {
        use meshkeep::clock::Clock;
        rig.clock.now_ms()
    };
    let due = now + 8000 + ACK_GRACE.as_millis() as u64;
    guard(rig.clock.wait_for_sleeper(|d| d == due)).await;
    rig.clock
        .advance(std::time::Duration::from_millis(due - now));
    rig.wait_event(|e| matches!(e, Event::MessageStatus { status, .. } if status == "unconfirmed"))
        .await;
}

#[tokio::test]
async fn web_channel_message_and_errors() {
    let mut rig = Rig::start().await;
    let cookie = login(&rig).await;
    let r = post_send(&rig, &cookie, "ch:1", "trail is clear").await;
    assert_eq!(r.status(), 200);
    rig.wait_event(|e| matches!(e, Event::MessageStatus { status, .. } if status == "sent"))
        .await;
    let last = rig.sim.commands().last().unwrap().clone();
    assert_eq!(
        p::parse_send_channel_cmd(&last).unwrap().3,
        "trail is clear"
    );

    let long = "x".repeat(200);
    let r = post_send(&rig, &cookie, "ch:1", &long).await;
    assert_eq!(r.status(), 400);
    assert!(
        r.json::<Value>().await.unwrap()["error"]
            .as_str()
            .unwrap()
            .contains("too long")
    );
    assert_eq!(
        post_send(&rig, &cookie, "nonsense", "x").await.status(),
        400
    );

    // A DM to someone the radio does not know fails cleanly.
    post_send(&rig, &cookie, "dm:010203040506", "who").await;
    rig.wait_event(|e| matches!(e, Event::MessageStatus { status, .. } if status == "failed"))
        .await;
}

#[tokio::test]
async fn browsers_get_live_events() {
    let rig = Rig::start().await;
    let cookie = login(&rig).await;
    let mut req = format!("ws://{}/api/events", rig.mk.web.unwrap())
        .into_client_request()
        .unwrap();
    req.headers_mut().insert("cookie", cookie.parse().unwrap());
    let (mut ws, _) = guard(tokio_tungstenite::connect_async(req)).await.unwrap();
    rig.sim.receive_channel(1, 5, "Dave: live");
    let text = guard(async {
        loop {
            let m = ws.next().await.unwrap().unwrap();
            if let Ok(t) = m.to_text()
                && t.contains("\"type\":\"message\"")
            {
                return t.to_string();
            }
        }
    })
    .await;
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["message"]["text"], "live");
    assert_eq!(v["message"]["sender"], "Dave");
    assert_eq!(v["message"]["conv"], "ch:1");

    let st = get_json(&rig, &cookie, "/api/state").await;
    assert_eq!(st["conversations"][0]["conv"], "ch:1");
}

#[tokio::test]
async fn web_sends_wait_their_turn_behind_apps() {
    let mut rig = Rig::start().await;
    let cookie = login(&rig).await;
    let mut a = rig.app(0).await;
    rig.sim.set_hold(true);
    a.send(&[p::cmd::GET_BATT_AND_STORAGE]).await;
    guard(rig.sim.wait_until(|s| s.held() == 1)).await;
    let conv = format!("dm:{}", p::hex(&rig.alice.prefix()));
    post_send(&rig, &cookie, &conv, "queued").await;
    assert_eq!(rig.sim.held(), 1);
    rig.sim.release();
    assert_eq!(a.response().await[0], p::resp::BATT_AND_STORAGE);
    guard(rig.sim.wait_until(|s| s.held() == 1)).await;
    rig.sim.release();
    rig.sim.set_hold(false);
    rig.wait_event(|e| matches!(e, Event::MessageStatus { status, .. } if status == "sent"))
        .await;
    assert_eq!(rig.sim.max_held(), 1);
}

#[tokio::test]
async fn cross_site_requests_are_refused() {
    let rig = Rig::start().await;
    let cookie = login(&rig).await;
    let conv = format!("dm:{}", p::hex(&rig.alice.prefix()));
    let send = |origin: String| {
        http()
            .post(format!("{}/api/send", rig.web()))
            .header("cookie", &cookie)
            .header("origin", origin)
            .json(&json!({ "conv": conv, "text": "hi" }))
            .send()
    };
    assert_eq!(
        send("http://evil.example".into()).await.unwrap().status(),
        403
    );
    assert_eq!(send(rig.web()).await.unwrap().status(), 200);

    // Logging in from another site is refused too.
    let r = http()
        .post(format!("{}/login", rig.web()))
        .header("origin", "http://evil.example")
        .form(&[("password", PASSWORD)])
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);

    let mut req = format!("ws://{}/api/events", rig.mk.web.unwrap())
        .into_client_request()
        .unwrap();
    req.headers_mut().insert("cookie", cookie.parse().unwrap());
    req.headers_mut()
        .insert("origin", "http://evil.example".parse().unwrap());
    assert!(guard(tokio_tungstenite::connect_async(req)).await.is_err());
}

#[tokio::test]
async fn without_sign_in_only_local_host_names_are_served() {
    let rig = Rig::start_with(RigOpts {
        auth: false,
        ..Default::default()
    })
    .await;
    let port = rig.mk.web.unwrap().port();
    let get = |host: String| {
        http()
            .get(format!("{}/api/state", rig.web()))
            .header("host", host)
            .send()
    };
    assert_eq!(
        get(format!("127.0.0.1:{port}")).await.unwrap().status(),
        200
    );
    assert_eq!(
        get(format!("meshkeep.lan:{port}")).await.unwrap().status(),
        200
    );
    assert_eq!(
        get(format!("rebind.example.com:{port}"))
            .await
            .unwrap()
            .status(),
        403
    );
}

#[tokio::test]
async fn wrong_passwords_are_throttled_across_all_requests() {
    use meshkeep::clock::Clock;
    use std::time::Duration;
    let rig = Rig::start().await;
    let attempt = || {
        http()
            .post(format!("{}/login", rig.web()))
            .form(&[("password", "guess")])
            .send()
    };
    let start = rig.clock.now_ms();
    let first = tokio::spawn(attempt());
    let second = tokio::spawn(attempt());
    // Only one attempt is ever waiting out its delay.
    guard(rig.clock.wait_for_sleeper(|d| d == start + 1000)).await;
    assert_eq!(
        rig.clock
            .pending_sleepers_matching(|d| d > start && d <= start + 2000),
        1
    );
    rig.clock.advance(Duration::from_secs(1));
    guard(rig.clock.wait_for_sleeper(|d| d == start + 2000)).await;
    rig.clock.advance(Duration::from_secs(1));
    assert_eq!(guard(first).await.unwrap().unwrap().status(), 401);
    assert_eq!(guard(second).await.unwrap().unwrap().status(), 401);
}
