// SPDX-License-Identifier: AGPL-3.0-or-later
//! Proxy behaviour: handshake, draining, per-app sync, serialisation,
//! disconnects. Simulated radio and apps over real local TCP, fake clock.

mod common;

use std::time::Duration;

use common::*;
use meshkeep::clock::Clock;
use meshkeep::hub::{Event, RADIO_WAIT, RESPONSE_TIMEOUT};
use meshkeep::protocol::{self as p, cmd, err, push, resp};

#[tokio::test]
async fn handshake_matches_app_sequence() {
    let rig = Rig::start().await;
    let cmds = rig.sim.commands();
    let codes: Vec<u8> = cmds
        .iter()
        .map(|c| c[0])
        .filter(|c| *c != cmd::GET_CHANNEL)
        .collect();
    assert_eq!(
        &codes[..5],
        &[
            cmd::DEVICE_QUERY,
            cmd::APP_START,
            cmd::SET_DEVICE_TIME,
            cmd::GET_CONTACTS,
            cmd::SYNC_NEXT_MESSAGE
        ]
    );
    // Asks for v3 message frames so SNR is kept.
    assert_eq!(cmds[0][1], 3);
    // Radio clock set from the (fake) host clock.
    assert_eq!(rig.sim.rtc() as u64, UNIX_BASE);
    assert_eq!(rig.sim.count(cmd::GET_CHANNEL), 8);
    assert!(rig.sim.count(cmd::SYNC_NEXT_MESSAGE) >= 1);
    assert_eq!(rig.mk.store.contacts().len(), 2);
    assert_eq!(rig.mk.store.channels().len(), 2);
    let st = rig.mk.status.lock().unwrap().clone();
    assert!(st.radio_connected);
    assert_eq!(st.name.as_deref(), Some("SimNode"));
}

#[tokio::test]
async fn drains_queue_present_at_startup() {
    let rig = Rig::start_with(RigOpts {
        before_start: Box::new(|sim, alice, _| {
            sim.receive_dm(alice, 1, "queued before meshkeep");
            sim.receive_channel(1, 2, "Carol: on the hill");
        }),
        ..Default::default()
    })
    .await;
    assert_eq!(rig.sim.queue_len(), 0);
    let dm = rig
        .mk
        .store
        .messages(&format!("dm:{}", p::hex(&rig.alice.prefix())), None, 10);
    assert_eq!(dm.len(), 1);
    assert_eq!(dm[0].text, "queued before meshkeep");
    let ch = rig.mk.store.messages("ch:1", None, 10);
    assert_eq!(ch[0].sender.as_deref(), Some("Carol"));
    assert_eq!(ch[0].text, "on the hill");
    assert_eq!(ch[0].channel_name.as_deref(), Some("Hikers"));
}

#[tokio::test]
async fn drains_immediately_on_msg_waiting() {
    let mut rig = Rig::start().await;
    rig.sim.receive_dm(&rig.alice.clone(), 10, "hello");
    let e = rig.wait_event(|e| matches!(e, Event::Message { .. })).await;
    let Event::Message { message } = e else {
        unreachable!()
    };
    assert_eq!(message.text, "hello");
    assert_eq!(message.snr, Some(5.0));
    rig.wait_event(|e| matches!(e, Event::DrainDone)).await;
    assert_eq!(rig.sim.queue_len(), 0);
}

#[tokio::test]
async fn periodic_drain_is_a_safety_net() {
    let mut rig = Rig::start().await;
    let before = rig.sim.count(cmd::SYNC_NEXT_MESSAGE);
    rig.clock.advance(Duration::from_secs(61));
    rig.wait_event(|e| matches!(e, Event::DrainDone)).await;
    assert!(rig.sim.count(cmd::SYNC_NEXT_MESSAGE) > before);
}

#[tokio::test]
async fn each_app_gets_every_message_once() {
    let mut rig = Rig::start().await;
    let alice = rig.alice.clone();

    let mut phone = rig.app(0).await;
    phone.handshake(3).await;

    for i in 0..3 {
        rig.sim.receive_dm(&alice, i, &format!("m{i}"));
        rig.wait_event(|e| matches!(e, Event::DrainDone)).await;
    }
    // The phone is tickled for new messages.
    phone.push(push::MSG_WAITING).await;
    let got: Vec<String> = phone.sync_all().await.iter().map(|f| msg_text(f)).collect();
    assert_eq!(got, ["m0", "m1", "m2"]);

    // The tablet was never connected, but still gets everything.
    let mut tablet = rig.app(1).await;
    tablet.handshake(3).await;
    assert_eq!(tablet.sync_all().await.len(), 3);

    // Reconnecting gives no duplicates.
    drop(phone);
    let mut phone = rig.app(0).await;
    phone.handshake(3).await;
    assert!(phone.sync_all().await.is_empty());

    // New message reaches both.
    rig.sim.receive_dm(&alice, 9, "m9");
    rig.wait_event(|e| matches!(e, Event::DrainDone)).await;
    assert_eq!(msg_text(&phone.sync_all().await[0]), "m9");
    assert_eq!(msg_text(&tablet.sync_all().await[0]), "m9");
}

#[tokio::test]
async fn message_cut_off_by_disconnect_is_redelivered() {
    let mut rig = Rig::start().await;
    let alice = rig.alice.clone();
    rig.sim.receive_dm(&alice, 1, "first");
    rig.sim.receive_dm(&alice, 2, "second");
    rig.wait_event(|e| matches!(e, Event::DrainDone)).await;

    let mut phone = rig.app(0).await;
    let f = phone.request(&[cmd::SYNC_NEXT_MESSAGE]).await;
    assert_eq!(msg_text(&f), "first");
    // Gone before asking for the next one: "first" is not known to have arrived.
    drop(phone);
    rig.wait_event(|e| matches!(e, Event::Apps)).await;

    let mut phone = rig.app(0).await;
    let got: Vec<String> = phone.sync_all().await.iter().map(|f| msg_text(f)).collect();
    assert_eq!(got, ["first", "second"]);
}

#[tokio::test]
async fn legacy_apps_get_legacy_frames() {
    let mut rig = Rig::start().await;
    rig.sim.receive_dm(&rig.alice.clone(), 1, "old app");
    rig.wait_event(|e| matches!(e, Event::DrainDone)).await;
    let mut phone = rig.app(0).await;
    phone.handshake(1).await;
    let f = &phone.sync_all().await[0];
    assert_eq!(f[0], resp::CONTACT_MSG_RECV);
    assert_eq!(&f[1..7], &rig.alice.prefix());
    // The radio was not downgraded by the old app.
    assert_eq!(rig.sim.app_ver(), 3);
}

#[tokio::test]
async fn per_address_listener_tracks_each_device() {
    let mut rig = Rig::start_with(RigOpts {
        listeners: listeners(&[("lan", true)]),
        ..Default::default()
    })
    .await;
    rig.sim.receive_dm(&rig.alice.clone(), 1, "x");
    rig.wait_event(|e| matches!(e, Event::DrainDone)).await;
    let mut a = rig.app(0).await;
    assert_eq!(a.sync_all().await.len(), 1);
    drop(a);
    let mut a = rig.app(0).await;
    assert!(a.sync_all().await.is_empty());
    assert!(rig.mk.store.cursor("lan@127.0.0.1").is_some());
}

#[tokio::test]
async fn commands_are_serialised_and_routed() {
    let rig = Rig::start().await;
    let mut a = rig.app(0).await;
    let mut b = rig.app(1).await;
    rig.sim.set_hold(true);
    let base = rig.sim.commands().len();
    a.send(&[cmd::GET_BATT_AND_STORAGE]).await;
    b.send(&[cmd::GET_DEVICE_TIME]).await;
    guard(rig.sim.wait_until(|s| s.held() == 1)).await;
    // The second command is not sent while the first is outstanding.
    assert_eq!(rig.sim.commands().len(), base + 1);
    rig.sim.release();
    guard(
        rig.sim
            .wait_until(|s| s.held() == 1 && s.commands().len() == base + 2),
    )
    .await;
    rig.sim.release();
    rig.sim.set_hold(false);
    assert_eq!(a.response().await[0], resp::BATT_AND_STORAGE);
    assert_eq!(b.response().await[0], resp::CURR_TIME);
    assert_eq!(rig.sim.max_held(), 1);
}

#[tokio::test]
async fn contact_listing_reaches_only_the_asker() {
    let rig = Rig::start().await;
    let mut a = rig.app(0).await;
    let mut b = rig.app(1).await;
    a.send(&[cmd::GET_CONTACTS]).await;
    assert_eq!(a.response().await[0], resp::CONTACTS_START);
    assert_eq!(a.response().await[0], resp::CONTACT);
    assert_eq!(a.response().await[0], resp::CONTACT);
    assert_eq!(a.response().await[0], resp::END_OF_CONTACTS);
    // b sees only its own answer.
    assert_eq!(b.request(&[cmd::GET_DEVICE_TIME]).await[0], resp::CURR_TIME);
}

#[tokio::test]
async fn pushes_go_to_every_app_but_msg_waiting_is_ours() {
    let rig = Rig::start().await;
    let mut a = rig.app(0).await;
    let mut b = rig.app(1).await;
    a.handshake(3).await;
    b.handshake(3).await;
    let mut advert = vec![push::ADVERT];
    advert.extend_from_slice(&rig.alice.pubkey);
    rig.sim.push(advert.clone());
    assert_eq!(a.push(push::ADVERT).await, advert);
    assert_eq!(b.push(push::ADVERT).await, advert);
}

#[tokio::test]
async fn unknown_commands_pass_through() {
    let rig = Rig::start().await;
    let mut a = rig.app(0).await;
    assert_eq!(
        a.request(&[200, 1, 2, 3]).await,
        vec![resp::ERR, err::UNSUPPORTED_CMD]
    );
    assert_eq!(rig.sim.commands().last().unwrap(), &vec![200, 1, 2, 3]);
}

#[tokio::test]
async fn radio_loss_and_return() {
    let mut rig = Rig::start().await;
    let mut a = rig.app(0).await;
    a.handshake(3).await;

    rig.sim.shutdown();
    rig.wait_event(|e| matches!(e, Event::Radio { connected: false }))
        .await;

    // The app stays connected and can still run its connect sequence.
    a.handshake(3).await;
    let listing = {
        a.send(&[cmd::GET_CONTACTS]).await;
        let mut n = 0;
        loop {
            let f = a.response().await;
            if f[0] == resp::CONTACT {
                n += 1;
            }
            if f[0] == resp::END_OF_CONTACTS {
                break n;
            }
        }
    };
    assert_eq!(listing, 2);
    assert_eq!(
        a.request(&[cmd::GET_CHANNEL, 1]).await[0],
        resp::CHANNEL_INFO
    );
    assert_eq!(
        a.request(&[cmd::SYNC_NEXT_MESSAGE]).await,
        vec![resp::NO_MORE_MESSAGES]
    );

    // Anything that needs the radio is refused once RADIO_WAIT has passed.
    a.send(&[cmd::GET_BATT_AND_STORAGE]).await;
    let now = rig.clock.now_ms();
    guard(
        rig.clock
            .wait_for_sleeper(|d| d == now + RADIO_WAIT.as_millis() as u64),
    )
    .await;
    rig.clock.advance(RADIO_WAIT);
    assert_eq!(a.response().await, vec![resp::ERR, err::BAD_STATE]);
}

#[tokio::test]
async fn reconnects_after_backoff() {
    let mut rig = Rig::start().await;
    let mut a = rig.app(0).await;
    a.handshake(3).await;
    assert_eq!(rig.sim.connections(), 1);
    rig.sim.drop_connection();
    rig.wait_event(|e| matches!(e, Event::Radio { connected: false }))
        .await;
    let now = rig.clock.now_ms();
    guard(rig.clock.wait_for_sleeper(|d| d == now + 1000)).await;
    rig.clock.advance(Duration::from_secs(1));
    rig.wait_event(|e| matches!(e, Event::Radio { connected: true }))
        .await;
    rig.wait_event(|e| matches!(e, Event::DrainDone)).await;
    assert_eq!(rig.sim.connections(), 2);
    // The app's connection survived and works through the new radio link.
    assert_eq!(
        a.request(&[cmd::GET_BATT_AND_STORAGE]).await[0],
        resp::BATT_AND_STORAGE
    );
}

#[tokio::test]
async fn message_arriving_during_radio_outage_is_not_lost() {
    let mut rig = Rig::start().await;
    rig.sim.drop_connection();
    rig.wait_event(|e| matches!(e, Event::Radio { connected: false }))
        .await;
    // Queued on the radio while meshkeep is away.
    rig.sim.receive_dm(&rig.alice.clone(), 5, "while away");
    let now = rig.clock.now_ms();
    guard(rig.clock.wait_for_sleeper(|d| d == now + 1000)).await;
    rig.clock.advance(Duration::from_secs(1));
    let e = rig.wait_event(|e| matches!(e, Event::Message { .. })).await;
    let Event::Message { message } = e else {
        unreachable!()
    };
    assert_eq!(message.text, "while away");
}

#[tokio::test]
async fn app_disconnect_mid_command_does_not_wedge() {
    let rig = Rig::start().await;
    let mut a = rig.app(0).await;
    rig.sim.set_hold(true);
    a.send(&[cmd::GET_BATT_AND_STORAGE]).await;
    guard(rig.sim.wait_until(|s| s.held() == 1)).await;
    drop(a);
    rig.sim.release();
    rig.sim.set_hold(false);
    let mut b = rig.app(1).await;
    assert_eq!(b.request(&[cmd::GET_DEVICE_TIME]).await[0], resp::CURR_TIME);
}

async fn time_out(rig: &Rig) {
    let now = rig.clock.now_ms();
    guard(
        rig.clock
            .wait_for_sleeper(|d| d == now + RESPONSE_TIMEOUT.as_millis() as u64),
    )
    .await;
    rig.clock.advance(RESPONSE_TIMEOUT);
}

#[tokio::test]
async fn late_answer_is_not_misrouted() {
    let rig = Rig::start().await;
    let mut a = rig.app(0).await;
    rig.sim.set_hold(true);
    a.send(&[cmd::GET_BATT_AND_STORAGE]).await;
    guard(rig.sim.wait_until(|s| s.held() == 1)).await;
    time_out(&rig).await;
    a.send(&[cmd::GET_DEVICE_TIME]).await;
    guard(rig.sim.wait_until(|s| s.held() == 2)).await;
    rig.sim.release(); // the late battery answer: dropped
    rig.sim.release();
    assert_eq!(a.response().await[0], resp::CURR_TIME);
}

#[tokio::test]
async fn silent_radio_times_out_then_link_restarts() {
    let mut rig = Rig::start().await;
    let mut a = rig.app(0).await;
    rig.sim.set_hold(true);
    a.send(&[cmd::GET_BATT_AND_STORAGE]).await;
    guard(rig.sim.wait_until(|s| s.held() == 1)).await;
    time_out(&rig).await;
    a.send(&[cmd::GET_DEVICE_TIME]).await;
    guard(rig.sim.wait_until(|s| s.held() == 2)).await;
    time_out(&rig).await;
    rig.wait_event(|e| matches!(e, Event::Radio { connected: false }))
        .await;
}

#[tokio::test]
async fn reboot_expects_no_answer() {
    let mut rig = Rig::start().await;
    let mut a = rig.app(0).await;
    let mut f = vec![cmd::REBOOT];
    f.extend_from_slice(b"reboot");
    a.send(&f).await;
    rig.wait_event(|e| matches!(e, Event::Radio { connected: false }))
        .await;
    // The queue is not stuck behind the reboot.
    assert_eq!(
        a.request(&[cmd::SYNC_NEXT_MESSAGE]).await,
        vec![resp::NO_MORE_MESSAGES]
    );
}

#[tokio::test]
async fn app_sent_dm_is_recorded_and_confirmed() {
    let mut rig = Rig::start().await;
    let mut a = rig.app(0).await;
    a.handshake(3).await;
    let f = p::build_send_txt(&rig.alice.prefix(), 777, 0, "from the app");
    let sent = a.request(&f).await;
    let (_, ack, _) = p::parse_sent(&sent).unwrap();
    let e = rig
        .wait_event(|e| matches!(e, Event::MessageStatus { .. }))
        .await;
    let Event::MessageStatus { id, status } = e else {
        unreachable!()
    };
    assert_eq!(status, "sent");
    rig.sim.confirm(ack);
    assert_eq!(a.push(push::SEND_CONFIRMED).await[1..5], ack.to_le_bytes());
    rig.wait_event(|e| matches!(e, Event::MessageStatus { status, .. } if status == "delivered"))
        .await;
    let m = rig.mk.store.message(id).unwrap();
    assert_eq!(m.text, "from the app");
    assert_eq!(m.direction, "out");
    // An app retry of the same message updates the same row.
    let retry = p::build_send_txt(&rig.alice.prefix(), 777, 1, "from the app");
    a.request(&retry).await;
    let conv = format!("dm:{}", p::hex(&rig.alice.prefix()));
    assert_eq!(rig.mk.store.messages(&conv, None, 10).len(), 1);
}

#[tokio::test]
async fn store_survives_restart() {
    let dir = temp_dir();
    let keep = dir.clone();
    {
        let mut rig = Rig::start_with(RigOpts {
            dir: Some(dir.clone()),
            ..Default::default()
        })
        .await;
        rig.sim.receive_dm(&rig.alice.clone(), 1, "persisted");
        rig.wait_event(|e| matches!(e, Event::DrainDone)).await;
        // Keep the directory for the second instance.
        rig.dir = temp_dir();
    }
    let rig = Rig::start_with(RigOpts {
        dir: Some(keep),
        ..Default::default()
    })
    .await;
    let mut phone = rig.app(0).await;
    assert_eq!(msg_text(&phone.sync_all().await[0]), "persisted");
}

/// After the link has dropped, let the backoff run and wait for the handshake.
async fn back_after_restart(rig: &mut Rig) {
    rig.wait_event(|e| matches!(e, Event::Radio { connected: false }))
        .await;
    let now = rig.clock.now_ms();
    guard(rig.clock.wait_for_sleeper(|d| d == now + 1000)).await;
    rig.clock.advance(Duration::from_secs(1));
    rig.wait_event(|e| matches!(e, Event::Radio { connected: true }))
        .await;
    rig.wait_event(|e| matches!(e, Event::DrainDone)).await;
}

#[tokio::test]
async fn reboot_restarts_link_even_when_the_port_survives() {
    let mut rig = Rig::start().await;
    rig.sim.set_reboot_keeps_connection(true);
    let mut a = rig.app(0).await;
    let mut f = vec![cmd::REBOOT];
    f.extend_from_slice(b"reboot");
    a.send(&f).await;
    back_after_restart(&mut rig).await;
    assert_eq!(rig.sim.reboots(), 1);
    assert_eq!(rig.sim.connections(), 2);
    assert_eq!(rig.sim.app_ver(), 3);
}

#[tokio::test]
async fn cli_reboot_expects_no_reply_and_restarts_link() {
    let mut rig = Rig::start().await;
    rig.sim.set_reboot_keeps_connection(true);
    let mut a = rig.app(0).await;
    let mut f = vec![cmd::RUN_CLI_COMMAND];
    f.extend_from_slice(b"reboot");
    a.send(&f).await;
    back_after_restart(&mut rig).await;
    assert_eq!(rig.sim.app_ver(), 3);
    // Not counted as a timeout, and the queue moved on.
    assert_eq!(
        a.request(&[cmd::GET_BATT_AND_STORAGE]).await[0],
        resp::BATT_AND_STORAGE
    );
}

#[tokio::test]
async fn factory_reset_waits_for_ok_then_restarts_link() {
    let mut rig = Rig::start().await;
    rig.sim.set_reboot_keeps_connection(true);
    let mut a = rig.app(0).await;
    let mut f = vec![cmd::FACTORY_RESET];
    f.extend_from_slice(b"reset");
    assert_eq!(a.request(&f).await, vec![resp::OK]);
    back_after_restart(&mut rig).await;
    assert_eq!(rig.sim.connections(), 2);
    // The radio's contacts are gone, and so is the cache.
    assert!(rig.mk.store.contacts().is_empty());
}

#[tokio::test]
async fn legacy_frame_means_radio_reset_so_handshake_again() {
    let mut rig = Rig::start().await;
    rig.sim.set_reboot_keeps_connection(true);
    let alice = rig.alice.clone();
    rig.sim.reboot();
    rig.sim.receive_dm(&alice, 1, "after reset");
    let e = rig.wait_event(|e| matches!(e, Event::Message { .. })).await;
    let Event::Message { message } = e else {
        unreachable!()
    };
    assert_eq!(message.text, "after reset");
    assert_eq!(message.snr, None);
    guard(rig.sim.wait_until(|s| s.app_ver() == 3)).await;
    assert_eq!(rig.sim.count(cmd::DEVICE_QUERY), 2);
    rig.wait_event(|e| matches!(e, Event::DrainDone)).await;
    rig.sim.receive_dm(&alice, 2, "v3 again");
    let e = rig.wait_event(|e| matches!(e, Event::Message { .. })).await;
    let Event::Message { message } = e else {
        unreachable!()
    };
    assert_eq!(message.snr, Some(5.0));
}

#[tokio::test]
async fn store_failure_stops_draining_until_stored() {
    let mut rig = Rig::start().await;
    let alice = rig.alice.clone();
    rig.mk
        .store
        .execute_batch_for_tests(
            "CREATE TRIGGER nostore BEFORE INSERT ON messages BEGIN SELECT RAISE(ABORT, 'disk full'); END;",
        )
        .unwrap();
    let now = rig.clock.now_ms();
    rig.sim.receive_dm(&alice, 1, "one");
    rig.sim.receive_dm(&alice, 2, "two");
    guard(
        rig.clock
            .wait_for_sleeper(|d| d == now + meshkeep::hub::STORE_RETRY.as_millis() as u64),
    )
    .await;
    // The first is held in memory; the second is still safe on the radio.
    assert_eq!(rig.sim.queue_len(), 1);
    assert_eq!(rig.mk.store.max_message_id(), 0);

    rig.mk
        .store
        .execute_batch_for_tests("DROP TRIGGER nostore;")
        .unwrap();
    rig.clock.advance(meshkeep::hub::STORE_RETRY);
    rig.wait_event(|e| matches!(e, Event::DrainDone)).await;
    assert_eq!(rig.sim.queue_len(), 0);
    let conv = format!("dm:{}", p::hex(&alice.prefix()));
    let texts: Vec<String> = rig
        .mk
        .store
        .messages(&conv, None, 10)
        .into_iter()
        .map(|m| m.text)
        .collect();
    assert_eq!(texts, ["one", "two"]);
}

#[tokio::test]
async fn stale_ok_cannot_end_a_drain() {
    let mut rig = Rig::start().await;
    let mut a = rig.app(0).await;
    rig.sim.set_hold(true);
    a.send(&[cmd::SEND_SELF_ADVERT]).await;
    guard(rig.sim.wait_until(|s| s.held() == 1)).await;
    time_out(&rig).await;
    rig.sim.receive_dm(&rig.alice.clone(), 1, "kept");
    guard(rig.sim.wait_until(|s| s.held() == 2)).await;
    rig.sim.release(); // late OK for the advert, arriving while the sync is in flight
    rig.sim.set_hold(false);
    rig.sim.release(); // the message
    let e = rig.wait_event(|e| matches!(e, Event::Message { .. })).await;
    let Event::Message { message } = e else {
        unreachable!()
    };
    assert_eq!(message.text, "kept");
    rig.wait_event(|e| matches!(e, Event::DrainDone)).await;
    assert_eq!(rig.sim.queue_len(), 0);
}

#[tokio::test]
async fn late_sync_answer_is_stored_whatever_is_in_flight() {
    let mut rig = Rig::start().await;
    let mut a = rig.app(0).await;
    rig.sim.set_hold(true);
    rig.sim.receive_dm(&rig.alice.clone(), 1, "late");
    guard(rig.sim.wait_until(|s| s.held() == 1)).await;
    time_out(&rig).await;
    a.send(&[cmd::GET_BATT_AND_STORAGE]).await;
    guard(rig.sim.wait_until(|s| s.held() == 2)).await;
    rig.sim.release(); // the sync's message, while the battery query is in flight
    let e = rig.wait_event(|e| matches!(e, Event::Message { .. })).await;
    let Event::Message { message } = e else {
        unreachable!()
    };
    assert_eq!(message.text, "late");
    rig.sim.set_hold(false);
    rig.sim.release();
    assert_eq!(a.response().await[0], resp::BATT_AND_STORAGE);
    rig.wait_event(|e| matches!(e, Event::DrainDone)).await;
}
