// SPDX-License-Identifier: AGPL-3.0-or-later
//! Run the simulated companion radio on its own, for trying meshkeep without
//! hardware: `cargo run --example sim_radio -- 127.0.0.1:5050`.
//! It receives a message every 20 seconds, and hears the last channel
//! message sent repeated.

use std::time::Duration;

use meshkeep::sim::{SimContact, SimRadio};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let bind = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:5050".into());
    let sim = SimRadio::start_on(&bind).await;
    let alice = SimContact::new(1, "Alice");
    let bob = SimContact::new(2, "Bob");
    sim.add_contact(alice.clone());
    sim.add_contact(bob.clone());
    let mut room = SimContact::new(3, "Hilltop Room");
    room.kind = 3;
    sim.add_contact(room.clone());
    let mut rpt = SimContact::new(4, "Ridge Repeater");
    rpt.kind = 2;
    sim.add_contact(rpt);
    let author: [u8; 4] = alice.pubkey[..4].try_into().unwrap();
    sim.receive_room_post(
        &room,
        &author,
        1_700_000_050,
        "Room is open for the weekend",
    );
    sim.set_channel(1, "Hikers");
    sim.receive_dm(&alice, 1_700_000_000, "Morning. Are you heading up today?");
    sim.receive_channel(0, 1_700_000_100, "Carol: Repeater on the ridge is back up");
    sim.receive_channel(1, 1_700_000_200, "Dave: Car park is full by nine, go early");
    println!("simulated radio on {}", sim.addr);
    let mut n = 0u32;
    loop {
        tokio::time::sleep(Duration::from_secs(20)).await;
        // Repeaters pass on whatever was last sent to a channel.
        sim.echo_last_channel_send(&[0x42]);
        n += 1;
        if n.is_multiple_of(2) {
            sim.receive_dm(&bob, 1_700_000_300 + n, &format!("Test message {n}"));
        } else {
            sim.receive_channel(1, 1_700_000_300 + n, &format!("Erin: Checking in, {n}"));
        }
    }
}
