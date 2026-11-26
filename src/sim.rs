// SPDX-License-Identifier: AGPL-3.0-or-later
//! A simulated companion radio speaking the WiFi (TCP) companion protocol, as
//! implemented by the firmware's `MyMesh::handleCmdFrame`. Used by the
//! integration tests; it is not a mesh, only the app-facing side.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{Notify, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::protocol::{
    self as p, CONTACT_FRAME_LEN, FRAME_FROM_RADIO, FRAME_TO_RADIO, FrameDecoder, MAX_FRAME_SIZE,
    MAX_PATH_SIZE, cmd, encode_frame, err, push, resp,
};

/// `OFFLINE_QUEUE_SIZE` in the firmware.
pub const OFFLINE_QUEUE_SIZE: usize = 16;
pub const MAX_CHANNELS: u8 = 8;

#[derive(Clone)]
pub struct SimContact {
    pub pubkey: [u8; 32],
    pub name: String,
    pub kind: u8,
    pub lastmod: u32,
}

impl SimContact {
    pub fn new(seed: u8, name: &str) -> SimContact {
        let mut pubkey = [0u8; 32];
        for (i, b) in pubkey.iter_mut().enumerate() {
            *b = seed.wrapping_add(i as u8).wrapping_mul(7).wrapping_add(1);
        }
        SimContact {
            pubkey,
            name: name.to_string(),
            kind: 1,
            lastmod: 1000 + seed as u32,
        }
    }

    pub fn prefix(&self) -> [u8; 6] {
        let mut p = [0u8; 6];
        p.copy_from_slice(&self.pubkey[..6]);
        p
    }

    fn frame(&self, code: u8) -> Vec<u8> {
        let mut f = Vec::with_capacity(CONTACT_FRAME_LEN);
        f.push(code);
        f.extend_from_slice(&self.pubkey);
        f.push(self.kind);
        f.push(0); // flags
        f.push(0xff); // out_path_len: unknown
        f.extend_from_slice(&[0u8; MAX_PATH_SIZE]);
        let mut name = [0u8; 32];
        let n = self.name.len().min(31);
        name[..n].copy_from_slice(&self.name.as_bytes()[..n]);
        f.extend_from_slice(&name);
        f.extend_from_slice(&self.lastmod.to_le_bytes()); // last_advert
        f.extend_from_slice(&0i32.to_le_bytes());
        f.extend_from_slice(&0i32.to_le_bytes());
        f.extend_from_slice(&self.lastmod.to_le_bytes());
        f
    }
}

struct SimState {
    name: String,
    pubkey: [u8; 32],
    contacts: Vec<SimContact>,
    channels: Vec<(String, [u8; 16])>,
    queue: VecDeque<Vec<u8>>,
    app_ver: u8,
    rtc: u32,
    next_ack: u32,
    acks: Vec<u32>,
    commands: Vec<Vec<u8>>,
    hold: bool,
    held: VecDeque<Vec<u8>>,
    max_held: usize,
    conn_out: Option<mpsc::UnboundedSender<Vec<u8>>>,
    conn_kill: Option<oneshot::Sender<()>>,
    connections: u32,
    sent: Vec<(Vec<u8>, String)>,
}

#[derive(Clone)]
pub struct SimRadio {
    pub addr: SocketAddr,
    st: Arc<Mutex<SimState>>,
    changed: Arc<Notify>,
    task: Arc<JoinHandle<()>>,
}

impl SimRadio {
    pub async fn start() -> SimRadio {
        Self::start_on("127.0.0.1:0").await
    }

    pub async fn start_on(bind: &str) -> SimRadio {
        let listener = TcpListener::bind(bind).await.expect("bind sim radio");
        let addr = listener.local_addr().unwrap();
        let mut channels = vec![(String::new(), [0u8; 16]); MAX_CHANNELS as usize];
        channels[0] = ("Public".to_string(), [0x11; 16]);
        let mut pubkey = [0u8; 32];
        pubkey[0] = 0x5a;
        pubkey[1] = 0xa5;
        let st = Arc::new(Mutex::new(SimState {
            name: "SimNode".into(),
            pubkey,
            contacts: Vec::new(),
            channels,
            queue: VecDeque::new(),
            app_ver: 0,
            rtc: 1_600_000_000,
            next_ack: 0x1000,
            acks: Vec::new(),
            commands: Vec::new(),
            hold: false,
            held: VecDeque::new(),
            max_held: 0,
            conn_out: None,
            conn_kill: None,
            connections: 0,
            sent: Vec::new(),
        }));
        let changed = Arc::new(Notify::new());
        let task = tokio::spawn(accept_loop(listener, st.clone(), changed.clone()));
        SimRadio {
            addr,
            st,
            changed,
            task: Arc::new(task),
        }
    }

    /// Stop listening and drop any connection.
    pub fn shutdown(&self) {
        self.task.abort();
        self.drop_connection();
    }

    pub fn add_contact(&self, c: SimContact) {
        self.st.lock().unwrap().contacts.push(c);
    }

    pub fn set_channel(&self, idx: u8, name: &str) {
        self.st.lock().unwrap().channels[idx as usize] = (name.to_string(), [idx; 16]);
    }

    /// A DM arrives over the mesh (MyMesh::queueMessage).
    pub fn receive_dm(&self, from: &SimContact, sender_ts: u32, text: &str) {
        let mut st = self.st.lock().unwrap();
        let mut f = Vec::new();
        if st.app_ver >= 3 {
            f.extend_from_slice(&[resp::CONTACT_MSG_RECV_V3, 20, 0, 0]);
        } else {
            f.push(resp::CONTACT_MSG_RECV);
        }
        f.extend_from_slice(&from.pubkey[..6]);
        f.push(2); // path_len (flood, 2 hops)
        f.push(p::txt::PLAIN);
        f.extend_from_slice(&sender_ts.to_le_bytes());
        f.extend_from_slice(text.as_bytes());
        f.truncate(MAX_FRAME_SIZE);
        queue_msg(&mut st, f);
        drop(st);
        self.changed.notify_waiters();
    }

    /// A channel message arrives (MyMesh::onChannelMessageRecv).
    pub fn receive_channel(&self, idx: u8, sender_ts: u32, text: &str) {
        let mut st = self.st.lock().unwrap();
        let mut f = Vec::new();
        if st.app_ver >= 3 {
            f.extend_from_slice(&[resp::CHANNEL_MSG_RECV_V3, 0xf4, 0, 0]);
        } else {
            f.push(resp::CHANNEL_MSG_RECV);
        }
        f.push(idx);
        f.push(0xff); // direct
        f.push(p::txt::PLAIN);
        f.extend_from_slice(&sender_ts.to_le_bytes());
        f.extend_from_slice(text.as_bytes());
        f.truncate(MAX_FRAME_SIZE);
        queue_msg(&mut st, f);
        drop(st);
        self.changed.notify_waiters();
    }

    /// The recipient acked a DM we sent (MyMesh::processAck).
    pub fn confirm(&self, ack: u32) {
        let mut st = self.st.lock().unwrap();
        if let Some(i) = st.acks.iter().position(|a| *a == ack) {
            st.acks.remove(i);
            let mut f = vec![push::SEND_CONFIRMED];
            f.extend_from_slice(&ack.to_le_bytes());
            f.extend_from_slice(&1234u32.to_le_bytes());
            send_out(&st, f);
        }
    }

    /// Send an arbitrary push frame to the connected app.
    pub fn push(&self, f: Vec<u8>) {
        send_out(&self.st.lock().unwrap(), f);
    }

    /// Drop the current app connection, as a WiFi or USB blip would.
    pub fn drop_connection(&self) {
        let mut st = self.st.lock().unwrap();
        st.conn_out = None;
        if let Some(k) = st.conn_kill.take() {
            let _ = k.send(());
        }
    }

    /// Stop answering; commands are held until [`SimRadio::release`].
    pub fn set_hold(&self, hold: bool) {
        self.st.lock().unwrap().hold = hold;
    }

    /// Answer the oldest held command.
    pub fn release(&self) {
        let mut st = self.st.lock().unwrap();
        if let Some(c) = st.held.pop_front() {
            for f in handle(&mut st, &c) {
                send_out(&st, f);
            }
        }
        drop(st);
        self.changed.notify_waiters();
    }

    pub fn held(&self) -> usize {
        self.st.lock().unwrap().held.len()
    }

    /// Most commands ever outstanding at once while holding.
    pub fn max_held(&self) -> usize {
        self.st.lock().unwrap().max_held
    }

    pub fn commands(&self) -> Vec<Vec<u8>> {
        self.st.lock().unwrap().commands.clone()
    }

    pub fn count(&self, code: u8) -> usize {
        self.st
            .lock()
            .unwrap()
            .commands
            .iter()
            .filter(|c| c[0] == code)
            .count()
    }

    pub fn queue_len(&self) -> usize {
        self.st.lock().unwrap().queue.len()
    }

    pub fn connections(&self) -> u32 {
        self.st.lock().unwrap().connections
    }

    pub fn app_ver(&self) -> u8 {
        self.st.lock().unwrap().app_ver
    }

    pub fn rtc(&self) -> u32 {
        self.st.lock().unwrap().rtc
    }

    /// DMs sent through the radio: (6 byte prefix, text).
    pub fn sent(&self) -> Vec<(Vec<u8>, String)> {
        self.st.lock().unwrap().sent.clone()
    }

    pub fn last_ack(&self) -> Option<u32> {
        self.st.lock().unwrap().acks.last().copied()
    }

    /// Wait until `pred` holds over the simulator state.
    pub async fn wait_until(&self, mut pred: impl FnMut(&SimRadio) -> bool) {
        loop {
            let n = self.changed.notified();
            if pred(self) {
                return;
            }
            n.await;
        }
    }

    /// Wait until `n` commands with this code have been received.
    pub async fn wait_for_count(&self, code: u8, n: usize) {
        self.wait_until(|s| s.count(code) >= n).await
    }
}

fn queue_msg(st: &mut SimState, f: Vec<u8>) {
    // MyMesh::addToOfflineQueue: when full, drop the oldest channel message.
    if st.queue.len() >= OFFLINE_QUEUE_SIZE {
        let is_ch = |q: &Vec<u8>| {
            matches!(
                q[0],
                resp::CHANNEL_MSG_RECV | resp::CHANNEL_MSG_RECV_V3 | resp::CHANNEL_DATA_RECV
            )
        };
        if let Some(i) = st.queue.iter().position(is_ch) {
            st.queue.remove(i);
            st.queue.push_back(f);
        }
    } else {
        st.queue.push_back(f);
    }
    send_out(st, vec![push::MSG_WAITING]);
}

fn send_out(st: &SimState, f: Vec<u8>) {
    if let Some(o) = &st.conn_out {
        let _ = o.send(f);
    }
}

async fn accept_loop(listener: TcpListener, st: Arc<Mutex<SimState>>, changed: Arc<Notify>) {
    loop {
        let Ok((sock, _)) = listener.accept().await else {
            continue;
        };
        let _ = sock.set_nodelay(true);
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (kill_tx, mut kill_rx) = oneshot::channel();
        {
            let mut s = st.lock().unwrap();
            // Like SerialWifiInterface: a new client replaces the old one.
            if let Some(k) = s.conn_kill.take() {
                let _ = k.send(());
            }
            s.conn_out = Some(out_tx);
            s.conn_kill = Some(kill_tx);
            s.connections += 1;
            s.held.clear();
        }
        changed.notify_waiters();
        let st2 = st.clone();
        let changed2 = changed.clone();
        tokio::spawn(async move {
            let (mut rd, mut wr) = sock.into_split();
            let mut dec = FrameDecoder::new(FRAME_TO_RADIO, MAX_FRAME_SIZE);
            let mut buf = vec![0u8; 2048];
            loop {
                tokio::select! {
                    _ = &mut kill_rx => break,
                    f = out_rx.recv() => {
                        let Some(f) = f else { break };
                        if wr.write_all(&encode_frame(FRAME_FROM_RADIO, &f)).await.is_err() {
                            break;
                        }
                    }
                    r = rd.read(&mut buf) => {
                        let n = match r { Ok(0) | Err(_) => break, Ok(n) => n };
                        dec.push(&buf[..n]);
                        while let Some(c) = dec.next_frame() {
                            let mut s = st2.lock().unwrap();
                            s.commands.push(c.clone());
                            if s.hold {
                                s.held.push_back(c);
                                s.max_held = s.max_held.max(s.held.len());
                            } else {
                                for f in handle(&mut s, &c) {
                                    send_out(&s, f);
                                }
                            }
                            drop(s);
                            changed2.notify_waiters();
                        }
                    }
                }
            }
            let _ = wr.shutdown().await;
            changed2.notify_waiters();
        });
    }
}

fn u32le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// The firmware's command handler, for the commands meshkeep and the tests use.
fn handle(st: &mut SimState, c: &[u8]) -> Vec<Vec<u8>> {
    let ok = || vec![vec![resp::OK]];
    let e = |code: u8| vec![vec![resp::ERR, code]];
    let len = c.len();
    match c[0] {
        cmd::DEVICE_QUERY if len >= 2 => {
            st.app_ver = c[1];
            let mut f = vec![resp::DEVICE_INFO, 14, 50, MAX_CHANNELS];
            f.extend_from_slice(&0u32.to_le_bytes());
            let mut date = [0u8; 12];
            date[..11].copy_from_slice(b"15 Oct 2026");
            f.extend_from_slice(&date);
            let mut model = [0u8; 40];
            model[..9].copy_from_slice(b"Sim Board");
            f.extend_from_slice(&model);
            let mut ver = [0u8; 20];
            ver[..7].copy_from_slice(b"v1.18.0");
            f.extend_from_slice(&ver);
            f.push(0);
            f.push(0);
            vec![f]
        }
        cmd::APP_START if len >= 8 => {
            let mut f = vec![resp::SELF_INFO, 1, 20, 22];
            f.extend_from_slice(&st.pubkey);
            f.extend_from_slice(&0i32.to_le_bytes());
            f.extend_from_slice(&0i32.to_le_bytes());
            f.extend_from_slice(&[0, 0, 0, 0]);
            f.extend_from_slice(&869_525u32.to_le_bytes());
            f.extend_from_slice(&250_000u32.to_le_bytes());
            f.push(11);
            f.push(5);
            f.extend_from_slice(st.name.as_bytes());
            vec![f]
        }
        cmd::GET_DEVICE_TIME => {
            let mut f = vec![resp::CURR_TIME];
            f.extend_from_slice(&st.rtc.to_le_bytes());
            vec![f]
        }
        cmd::SET_DEVICE_TIME if len >= 5 => {
            let secs = u32le(c, 1);
            if secs >= st.rtc {
                st.rtc = secs;
                ok()
            } else {
                e(err::ILLEGAL_ARG)
            }
        }
        cmd::GET_CONTACTS => {
            let since = if len >= 5 { u32le(c, 1) } else { 0 };
            let mut out = Vec::new();
            let mut start = vec![resp::CONTACTS_START];
            start.extend_from_slice(&(st.contacts.len() as u32).to_le_bytes());
            out.push(start);
            let mut most = 0u32;
            for ct in &st.contacts {
                if ct.lastmod > since {
                    out.push(ct.frame(resp::CONTACT));
                    most = most.max(ct.lastmod);
                }
            }
            let mut end = vec![resp::END_OF_CONTACTS];
            end.extend_from_slice(&most.to_le_bytes());
            out.push(end);
            out
        }
        cmd::GET_CONTACT_BY_KEY => {
            match st.contacts.iter().find(|ct| len > 32 && ct.pubkey[..] == c[1..33]) {
                Some(ct) => vec![ct.frame(resp::CONTACT)],
                None => e(err::NOT_FOUND),
            }
        }
        cmd::GET_CHANNEL if len >= 2 => match st.channels.get(c[1] as usize) {
            Some((name, secret)) => {
                let mut f = vec![resp::CHANNEL_INFO, c[1]];
                let mut n = [0u8; 32];
                let l = name.len().min(31);
                n[..l].copy_from_slice(&name.as_bytes()[..l]);
                f.extend_from_slice(&n);
                f.extend_from_slice(secret);
                vec![f]
            }
            None => e(err::NOT_FOUND),
        },
        cmd::SYNC_NEXT_MESSAGE => match st.queue.pop_front() {
            Some(f) => vec![f],
            None => vec![vec![resp::NO_MORE_MESSAGES]],
        },
        cmd::SEND_TXT_MSG if len >= 14 => {
            let prefix = &c[7..13];
            if !st.contacts.iter().any(|ct| ct.pubkey[..6] == *prefix) {
                return e(err::NOT_FOUND);
            }
            if c[1] != p::txt::PLAIN {
                return e(err::UNSUPPORTED_CMD);
            }
            let ack = st.next_ack;
            st.next_ack += 1;
            st.acks.push(ack);
            st.sent.push((prefix.to_vec(), String::from_utf8_lossy(&c[13..]).into_owned()));
            let mut f = vec![resp::SENT, 1];
            f.extend_from_slice(&ack.to_le_bytes());
            f.extend_from_slice(&8000u32.to_le_bytes());
            vec![f]
        }
        cmd::SEND_CHANNEL_TXT_MSG => {
            if len < 7 || c[1] != p::txt::PLAIN {
                return e(err::UNSUPPORTED_CMD);
            }
            if (c[2] as usize) < st.channels.len() {
                ok()
            } else {
                e(err::NOT_FOUND)
            }
        }
        cmd::SET_ADVERT_NAME if len >= 2 => {
            st.name = String::from_utf8_lossy(&c[1..]).into_owned();
            ok()
        }
        cmd::SEND_SELF_ADVERT => ok(),
        cmd::REMOVE_CONTACT => {
            let before = st.contacts.len();
            st.contacts.retain(|ct| !(len > 32 && ct.pubkey[..] == c[1..33]));
            if st.contacts.len() < before {
                ok()
            } else {
                e(err::NOT_FOUND)
            }
        }
        cmd::GET_BATT_AND_STORAGE => {
            let mut f = vec![resp::BATT_AND_STORAGE];
            f.extend_from_slice(&4100u16.to_le_bytes());
            f.extend_from_slice(&10u32.to_le_bytes());
            f.extend_from_slice(&1000u32.to_le_bytes());
            vec![f]
        }
        cmd::REBOOT if len >= 7 && &c[1..7] == b"reboot" => {
            if let Some(k) = st.conn_kill.take() {
                let _ = k.send(());
            }
            st.conn_out = None;
            vec![]
        }
        _ => e(err::UNSUPPORTED_CMD),
    }
}
