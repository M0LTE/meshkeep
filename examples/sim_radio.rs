// SPDX-License-Identifier: AGPL-3.0-or-later
//! Run the simulated companion radio on its own, for trying meshkeep without
//! hardware: `cargo run --example sim_radio -- 127.0.0.1:5050`.
//! It receives a message every 20 seconds.

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
    sim.add_contact(SimContact::new(3, "Hilltop Room"));
    sim.set_channel(1, "Hikers");
    sim.receive_dm(&alice, 1_700_000_000, "Morning. Are you heading up today?");
    sim.receive_channel(0, 1_700_000_100, "Carol: Repeater on the ridge is back up");
    sim.receive_channel(1, 1_700_000_200, "Dave: Car park is full by nine, go early");
    println!("simulated radio on {}", sim.addr);
    let mut n = 0u32;
    loop {
        tokio::time::sleep(Duration::from_secs(20)).await;
        n += 1;
        if n % 2 == 0 {
            sim.receive_dm(&bob, 1_700_000_300 + n, &format!("Test message {n}"));
        } else {
            sim.receive_channel(1, 1_700_000_300 + n, &format!("Erin: Checking in, {n}"));
        }
    }
}
