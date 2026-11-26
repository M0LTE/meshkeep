// SPDX-License-Identifier: AGPL-3.0-or-later
//! The hub: one task that owns the radio connection state, the command queue
//! and every app connection. All protocol decisions live here.
//!
//! The radio is strictly request/response, so exactly one command is in flight
//! at a time. Responses (codes below 0x80) go back to whoever sent the
//! command; pushes (0x80 and up) go to every app.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tokio::sync::{broadcast, mpsc, oneshot};
use tracing::{debug, info, warn};

use crate::clock::SharedClock;
use crate::protocol::{self as p, Expect, cmd, err, push, resp};
use crate::store::{self, Message, Store};

/// How long the radio gets to answer a command.
pub const RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a command waits for a missing radio before it is refused.
pub const RADIO_WAIT: Duration = Duration::from_secs(3);
/// Safety-net poll of the radio's offline queue.
pub const DRAIN_INTERVAL: Duration = Duration::from_secs(60);
/// Grace on top of the radio's own ack timeout estimate.
pub const ACK_GRACE: Duration = Duration::from_secs(5);
/// Back-to-back response timeouts before the radio link is restarted.
const TIMEOUTS_BEFORE_RECONNECT: u32 = 2;
/// The protocol version meshkeep itself speaks (the firmware switches message
/// layouts at 3).
const OWN_APP_VER: u8 = 3;
/// Per-app outgoing frame buffer; an app that falls this far behind is dropped.
pub const CLIENT_QUEUE: usize = 1024;

pub type ConnId = u64;

pub enum HubMsg {
    RadioUp {
        epoch: u64,
        tx: mpsc::UnboundedSender<Vec<u8>>,
    },
    RadioFrame {
        epoch: u64,
        frame: Vec<u8>,
    },
    RadioDown {
        epoch: u64,
    },
    ClientUp {
        conn: ConnId,
        listener: String,
        per_address: bool,
        peer: SocketAddr,
        tx: mpsc::Sender<Vec<u8>>,
    },
    ClientFrame {
        conn: ConnId,
        frame: Vec<u8>,
    },
    ClientDown {
        conn: ConnId,
    },
    SendDm {
        prefix: [u8; 6],
        text: String,
        reply: oneshot::Sender<Result<i64, String>>,
    },
    SendChannel {
        idx: u8,
        text: String,
        reply: oneshot::Sender<Result<i64, String>>,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Radio {
        connected: bool,
    },
    Message {
        message: Message,
    },
    MessageStatus {
        id: i64,
        status: String,
    },
    Contacts,
    Channels,
    SelfInfo,
    Apps,
    /// The radio's offline queue has been emptied.
    DrainDone,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct AppConn {
    pub client: String,
    pub peer: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Status {
    pub radio_connected: bool,
    pub radio_addr: String,
    pub name: Option<String>,
    pub pubkey: Option<String>,
    pub model: Option<String>,
    pub firmware: Option<String>,
    pub apps: Vec<AppConn>,
}

pub type SharedStatus = Arc<Mutex<Status>>;

#[derive(Debug)]
enum Origin {
    Client(ConnId),
    Web(i64),
    Internal,
}

struct Cmd {
    origin: Origin,
    frame: Vec<u8>,
    enqueued_ms: u64,
}

struct InFlight {
    cmd: Cmd,
    expect: Expect,
    deadline: u64,
    /// Full contact listing being collected (GET_CONTACTS without `since`).
    listing: Option<Vec<Vec<u8>>>,
}

struct Client {
    key: String,
    tx: mpsc::Sender<Vec<u8>>,
    peer: SocketAddr,
    app_ver: u8,
    /// Message handed out by SYNC_NEXT but not yet known to be received.
    unacked: Option<i64>,
}

pub struct Hub {
    clock: SharedClock,
    store: Store,
    events: broadcast::Sender<Event>,
    status: SharedStatus,
    kick: mpsc::UnboundedSender<()>,

    radio: Option<mpsc::UnboundedSender<Vec<u8>>>,
    epoch: u64,
    internal_q: VecDeque<Cmd>,
    client_q: VecDeque<Cmd>,
    last_internal: bool,
    in_flight: Option<InFlight>,
    timeouts: u32,
    draining: bool,
    drain_again: bool,
    next_drain_at: u64,
    ack_deadlines: Vec<(i64, u64)>,
    clients: HashMap<ConnId, Client>,
}

impl Hub {
    pub fn new(
        clock: SharedClock,
        store: Store,
        events: broadcast::Sender<Event>,
        status: SharedStatus,
        kick: mpsc::UnboundedSender<()>,
    ) -> Hub {
        Hub {
            clock,
            store,
            events,
            status,
            kick,
            radio: None,
            epoch: 0,
            internal_q: VecDeque::new(),
            client_q: VecDeque::new(),
            last_internal: false,
            in_flight: None,
            timeouts: 0,
            draining: false,
            drain_again: false,
            next_drain_at: u64::MAX,
            ack_deadlines: Vec::new(),
            clients: HashMap::new(),
        }
    }

    pub async fn run(mut self, mut rx: mpsc::UnboundedReceiver<HubMsg>) {
        loop {
            let deadline = self.next_deadline();
            let sleep: crate::clock::SleepFuture = if deadline == u64::MAX {
                Box::pin(std::future::pending())
            } else {
                self.clock.sleep_until(deadline)
            };
            tokio::select! {
                msg = rx.recv() => match msg {
                    Some(m) => self.handle(m),
                    None => return,
                },
                _ = sleep => self.on_tick(),
            }
            self.pump();
        }
    }

    fn now(&self) -> u64 {
        self.clock.now_ms()
    }

    fn emit(&self, e: Event) {
        let _ = self.events.send(e);
    }

    fn next_deadline(&self) -> u64 {
        let mut d = u64::MAX;
        if let Some(f) = &self.in_flight {
            d = d.min(f.deadline);
        }
        if self.radio.is_some() {
            d = d.min(self.next_drain_at);
        } else if let Some(c) = self.client_q.front() {
            d = d.min(c.enqueued_ms + RADIO_WAIT.as_millis() as u64);
        }
        for (_, a) in &self.ack_deadlines {
            d = d.min(*a);
        }
        d
    }

    fn handle(&mut self, m: HubMsg) {
        match m {
            HubMsg::RadioUp { epoch, tx } => self.radio_up(epoch, tx),
            HubMsg::RadioDown { epoch } => {
                if epoch == self.epoch {
                    self.radio_down();
                }
            }
            HubMsg::RadioFrame { epoch, frame } => {
                if epoch == self.epoch && !frame.is_empty() {
                    self.radio_frame(frame);
                }
            }
            HubMsg::ClientUp {
                conn,
                listener,
                per_address,
                peer,
                tx,
            } => self.client_up(conn, listener, per_address, peer, tx),
            HubMsg::ClientFrame { conn, frame } => self.client_frame(conn, frame),
            HubMsg::ClientDown { conn } => self.client_down(conn),
            HubMsg::SendDm {
                prefix,
                text,
                reply,
            } => {
                let _ = reply.send(self.web_send_dm(prefix, text));
            }
            HubMsg::SendChannel { idx, text, reply } => {
                let _ = reply.send(self.web_send_channel(idx, text));
            }
        }
    }

    // ------------------------------------------------------------- radio

    fn radio_up(&mut self, epoch: u64, tx: mpsc::UnboundedSender<Vec<u8>>) {
        self.epoch = epoch;
        self.radio = Some(tx);
        self.timeouts = 0;
        self.internal_q.clear();
        self.draining = false;
        self.next_drain_at = self.now() + DRAIN_INTERVAL.as_millis() as u64;
        self.status.lock().unwrap().radio_connected = true;

        let ver = self
            .store
            .get_kv("app_ver")
            .and_then(|v| v.first().copied())
            .unwrap_or(0)
            .max(OWN_APP_VER);
        self.internal(vec![cmd::DEVICE_QUERY, ver]);
        let mut start = vec![cmd::APP_START, 1, 0, 0, 0, 0, 0, 0];
        start.extend_from_slice(b"meshkeep");
        self.internal(start);
        let mut t = vec![cmd::SET_DEVICE_TIME];
        t.extend_from_slice(&(self.clock.now_unix() as u32).to_le_bytes());
        self.internal(t);
        self.internal(vec![cmd::GET_CONTACTS]);
        self.schedule_drain();
        self.emit(Event::Radio { connected: true });
    }

    fn radio_down(&mut self) {
        self.radio = None;
        self.internal_q.clear();
        self.draining = false;
        self.drain_again = false;
        if let Some(f) = self.in_flight.take() {
            self.fail_command(f.cmd);
        }
        self.status.lock().unwrap().radio_connected = false;
        self.emit(Event::Radio { connected: false });
    }

    /// Tell the origin of an unanswered command that it failed.
    fn fail_command(&mut self, c: Cmd) {
        match c.origin {
            Origin::Client(conn) => self.send_to(conn, p::err_frame(err::BAD_STATE)),
            Origin::Web(id) => self.set_status(id, "failed"),
            Origin::Internal => {
                if c.frame.first() == Some(&cmd::SYNC_NEXT_MESSAGE) {
                    self.draining = false;
                }
            }
        }
    }

    fn internal(&mut self, frame: Vec<u8>) {
        if self.radio.is_none() {
            return;
        }
        let enqueued_ms = self.now();
        self.internal_q.push_back(Cmd {
            origin: Origin::Internal,
            frame,
            enqueued_ms,
        });
    }

    fn internal_queued(&self, frame: &[u8]) -> bool {
        self.internal_q.iter().any(|c| c.frame == frame)
    }

    fn schedule_drain(&mut self) {
        if self.radio.is_none() {
            return;
        }
        if self.draining {
            self.drain_again = true;
            return;
        }
        self.draining = true;
        self.drain_again = false;
        self.internal(vec![cmd::SYNC_NEXT_MESSAGE]);
    }

    fn radio_frame(&mut self, f: Vec<u8>) {
        let code = f[0];
        let self_telemetry = matches!(
            self.in_flight.as_ref().map(|x| x.expect),
            Some(Expect::SelfTelemetry)
        );
        if p::is_push(code) && !(self_telemetry && code == push::TELEMETRY_RESPONSE) {
            self.radio_push(f);
            return;
        }
        let Some(inf) = self.in_flight.as_mut() else {
            debug!("radio sent 0x{code:02x} with no command outstanding, dropped");
            return;
        };
        let done = match inf.expect {
            Expect::One(codes) => {
                if code == resp::ERR || codes.contains(&code) {
                    true
                } else {
                    debug!(
                        "radio sent 0x{code:02x} for command 0x{:02x}, dropped",
                        inf.cmd.frame[0]
                    );
                    return;
                }
            }
            Expect::AnyOne | Expect::SyncNext | Expect::SelfTelemetry => true,
            Expect::Contacts => match code {
                resp::CONTACTS_START | resp::CONTACT => {
                    inf.deadline = self.clock.now_ms() + RESPONSE_TIMEOUT.as_millis() as u64;
                    if code == resp::CONTACT
                        && let Some(l) = inf.listing.as_mut()
                    {
                        l.push(f.clone());
                    }
                    false
                }
                resp::END_OF_CONTACTS | resp::ERR => true,
                _ => {
                    debug!("radio sent 0x{code:02x} during contact listing, dropped");
                    return;
                }
            },
            Expect::Nothing => true,
        };
        self.timeouts = 0;
        if done {
            let inf = self.in_flight.take().unwrap();
            self.observe(&inf.cmd, &f, inf.listing);
            self.deliver(&inf.cmd.origin, &inf.cmd.frame, f);
        } else {
            let origin_conn = match inf.cmd.origin {
                Origin::Client(c) => Some(c),
                _ => None,
            };
            if code == resp::CONTACT {
                self.store.upsert_contact(&f);
            }
            if let Some(c) = origin_conn {
                self.send_to(c, f);
            }
        }
    }

    /// Route a final response to whoever asked.
    fn deliver(&mut self, origin: &Origin, cmd_frame: &[u8], f: Vec<u8>) {
        match *origin {
            Origin::Client(conn) => {
                self.client_result(conn, cmd_frame, &f);
                self.send_to(conn, f);
            }
            Origin::Web(id) => self.web_result(id, cmd_frame, &f),
            Origin::Internal => self.internal_result(cmd_frame, &f),
        }
    }

    /// Side effects of any completed command, whoever sent it: keep the
    /// cached radio state current.
    fn observe(&mut self, c: &Cmd, f: &[u8], listing: Option<Vec<Vec<u8>>>) {
        let code = f[0];
        let sent = c.frame[0];
        match code {
            resp::SELF_INFO => {
                self.store.put_kv("self_info", f);
                if let Some(si) = p::parse_self_info(f) {
                    let mut st = self.status.lock().unwrap();
                    st.name = Some(si.name);
                    st.pubkey = Some(p::hex(&si.pubkey));
                }
                self.emit(Event::SelfInfo);
            }
            resp::DEVICE_INFO => {
                self.store.put_kv("device_info", f);
                if let Some(di) = p::parse_device_info(f) {
                    {
                        let mut st = self.status.lock().unwrap();
                        st.model = Some(di.model);
                        st.firmware = Some(di.version);
                    }
                    // Channel names are needed before the first drain stores
                    // channel messages, so these jump the internal queue.
                    if matches!(c.origin, Origin::Internal) && self.radio.is_some() {
                        let now = self.now();
                        for i in (0..di.max_channels).rev() {
                            self.internal_q.push_front(Cmd {
                                origin: Origin::Internal,
                                frame: vec![cmd::GET_CHANNEL, i],
                                enqueued_ms: now,
                            });
                        }
                    }
                }
            }
            resp::CONTACT => {
                self.store.upsert_contact(f);
                self.emit(Event::Contacts);
            }
            resp::END_OF_CONTACTS => {
                if let Some(l) = listing {
                    self.store.replace_contacts(&l);
                }
                self.emit(Event::Contacts);
            }
            resp::CHANNEL_INFO => {
                self.store.upsert_channel(f);
                self.emit(Event::Channels);
            }
            resp::OK => match sent {
                cmd::REMOVE_CONTACT if c.frame.len() > 32 => {
                    self.store.delete_contact(&c.frame[1..33]);
                    self.emit(Event::Contacts);
                }
                cmd::ADD_UPDATE_CONTACT | cmd::RESET_PATH if c.frame.len() > 32 => {
                    let mut q = vec![cmd::GET_CONTACT_BY_KEY];
                    q.extend_from_slice(&c.frame[1..33]);
                    self.internal(q);
                }
                cmd::IMPORT_CONTACT => self.internal(vec![cmd::GET_CONTACTS]),
                cmd::SET_CHANNEL if c.frame.len() > 1 => {
                    self.internal(vec![cmd::GET_CHANNEL, c.frame[1]]);
                }
                cmd::SET_ADVERT_NAME
                | cmd::SET_ADVERT_LATLON
                | cmd::SET_RADIO_PARAMS
                | cmd::SET_RADIO_TX_POWER
                | cmd::SET_OTHER_PARAMS => {
                    let mut s = vec![cmd::APP_START, 1, 0, 0, 0, 0, 0, 0];
                    s.extend_from_slice(b"meshkeep");
                    self.internal(s);
                }
                _ => {}
            },
            _ => {}
        }
    }

    fn internal_result(&mut self, cmd_frame: &[u8], f: &[u8]) {
        if cmd_frame[0] != cmd::SYNC_NEXT_MESSAGE {
            return;
        }
        let code = f[0];
        if p::is_queued_message(code) {
            if let Some(m) = p::parse_inbound(f) {
                self.store_inbound(m);
            }
            // Keep going, behind anything apps have queued meanwhile.
            self.internal(vec![cmd::SYNC_NEXT_MESSAGE]);
        } else {
            self.draining = false;
            if self.drain_again {
                self.schedule_drain();
            } else {
                self.emit(Event::DrainDone);
            }
        }
    }

    fn store_inbound(&mut self, m: p::InboundMessage) {
        let ch_name = m.channel.and_then(|i| self.store.channel_name(i));
        match self
            .store
            .insert_inbound(&m, self.clock.now_unix(), ch_name.as_deref())
        {
            Ok(id) => {
                debug!("stored {} message {id}", m.kind.as_str());
                if let Some(msg) = self.store.message(id) {
                    self.emit(Event::Message { message: msg });
                }
                // Tickle every app, as the radio does when it queues a message.
                let conns: Vec<ConnId> = self.clients.keys().copied().collect();
                for c in conns {
                    self.send_to(c, vec![push::MSG_WAITING]);
                }
            }
            Err(e) => warn!("cannot store message, check the data directory: {e}"),
        }
    }

    fn radio_push(&mut self, f: Vec<u8>) {
        match f[0] {
            push::MSG_WAITING => {
                self.schedule_drain();
                return;
            }
            push::SEND_CONFIRMED => {
                if let Some((ack, _)) = p::parse_send_confirmed(&f)
                    && let Some(id) = self.store.confirm_ack(ack)
                {
                    self.ack_deadlines.retain(|(m, _)| *m != id);
                    self.emit(Event::MessageStatus {
                        id,
                        status: "delivered".into(),
                    });
                }
            }
            push::NEW_ADVERT => {
                self.store.upsert_contact(&f);
                self.emit(Event::Contacts);
            }
            push::ADVERT | push::PATH_UPDATED if f.len() > 32 => {
                let mut q = vec![cmd::GET_CONTACT_BY_KEY];
                q.extend_from_slice(&f[1..33]);
                if !self.internal_queued(&q) {
                    self.internal(q);
                }
            }
            push::CONTACT_DELETED if f.len() > 32 => {
                self.store.delete_contact(&f[1..33]);
                self.emit(Event::Contacts);
            }
            _ => {}
        }
        let conns: Vec<ConnId> = self.clients.keys().copied().collect();
        for c in conns {
            self.send_to(c, f.clone());
        }
    }

    // ------------------------------------------------------------- queue

    fn pump(&mut self) {
        while self.in_flight.is_none() {
            let use_internal =
                !self.internal_q.is_empty() && (self.client_q.is_empty() || !self.last_internal);
            let c = if use_internal {
                self.internal_q.pop_front()
            } else {
                self.client_q.pop_front()
            };
            let Some(c) = c else { return };
            self.last_internal = use_internal;

            if let Origin::Client(conn) = c.origin
                && let Some(frames) = self.answer_locally(conn, &c.frame)
            {
                for f in frames {
                    self.send_to(conn, f);
                }
                continue;
            }

            let Some(radio) = self.radio.clone() else {
                if self.now() >= c.enqueued_ms + RADIO_WAIT.as_millis() as u64 {
                    self.fail_command(c);
                    continue;
                }
                // Still waiting for the radio to come back.
                self.client_q.push_front(c);
                return;
            };

            let mut frame = c.frame.clone();
            if frame[0] == cmd::DEVICE_QUERY && frame.len() >= 2 {
                frame[1] = frame[1].max(OWN_APP_VER);
            }
            if radio.send(frame).is_err() {
                // Radio task is gone; RadioDown will follow.
                self.client_q.push_front(c);
                return;
            }
            let expect = p::expect_for(&c.frame);
            if expect == Expect::Nothing {
                continue;
            }
            let listing = (c.frame[0] == cmd::GET_CONTACTS && c.frame.len() < 5).then(Vec::new);
            self.in_flight = Some(InFlight {
                deadline: self.now() + RESPONSE_TIMEOUT.as_millis() as u64,
                cmd: c,
                expect,
                listing,
            });
        }
    }

    fn on_tick(&mut self) {
        let now = self.now();
        if let Some(f) = &self.in_flight
            && now >= f.deadline
        {
            let f = self.in_flight.take().unwrap();
            warn!(
                "radio did not answer command 0x{:02x} within {}s",
                f.cmd.frame[0],
                RESPONSE_TIMEOUT.as_secs()
            );
            // An app gets no reply, exactly as if the radio had dropped it;
            // a late error would be mistaken for the answer to its next command.
            match f.cmd.origin {
                Origin::Client(_) => {}
                _ => self.fail_command(f.cmd),
            }
            self.timeouts += 1;
            if self.timeouts >= TIMEOUTS_BEFORE_RECONNECT {
                warn!("radio is not answering, reconnecting");
                self.timeouts = 0;
                let _ = self.kick.send(());
            }
        }
        if self.radio.is_some() && now >= self.next_drain_at {
            self.next_drain_at = now + DRAIN_INTERVAL.as_millis() as u64;
            self.schedule_drain();
        }
        let expired: Vec<i64> = self
            .ack_deadlines
            .iter()
            .filter(|(_, d)| *d <= now)
            .map(|(id, _)| *id)
            .collect();
        self.ack_deadlines.retain(|(_, d)| *d > now);
        for id in expired {
            if self.store.message(id).and_then(|m| m.status).as_deref() == Some("sent") {
                self.set_status(id, "unconfirmed");
            }
        }
    }

    // ------------------------------------------------------------- apps

    fn client_up(
        &mut self,
        conn: ConnId,
        listener: String,
        per_address: bool,
        peer: SocketAddr,
        tx: mpsc::Sender<Vec<u8>>,
    ) {
        let key = if per_address {
            let key = format!("{listener}@{}", peer.ip());
            if self.store.cursor(&key).is_none() {
                let base = self.store.cursor(&listener).unwrap_or(0);
                let backlog = self.store.nth_newest_inbound_id(store::NEW_DEVICE_BACKLOG) - 1;
                self.store.ensure_cursor(&key, base.max(backlog));
            }
            key
        } else {
            let max = self.store.max_message_id();
            self.store.ensure_cursor(&listener, max);
            listener
        };
        info!("app connected from {peer} as {key}");
        self.clients.insert(
            conn,
            Client {
                key,
                tx,
                peer,
                app_ver: 0,
                unacked: None,
            },
        );
        self.update_apps();
    }

    fn client_down(&mut self, conn: ConnId) {
        if let Some(c) = self.clients.remove(&conn) {
            info!("app {} at {} disconnected", c.key, c.peer);
            self.client_q
                .retain(|q| !matches!(q.origin, Origin::Client(x) if x == conn));
            self.update_apps();
        }
    }

    fn update_apps(&mut self) {
        let mut apps: Vec<AppConn> = self
            .clients
            .values()
            .map(|c| AppConn {
                client: c.key.clone(),
                peer: c.peer.to_string(),
            })
            .collect();
        apps.sort_by(|a, b| a.client.cmp(&b.client).then(a.peer.cmp(&b.peer)));
        self.status.lock().unwrap().apps = apps;
        self.emit(Event::Apps);
    }

    fn send_to(&mut self, conn: ConnId, f: Vec<u8>) {
        let Some(c) = self.clients.get(&conn) else {
            return;
        };
        if c.tx.try_send(f).is_err() {
            warn!(
                "app {} at {} is not reading, disconnecting it",
                c.key, c.peer
            );
            self.client_down(conn);
        }
    }

    fn client_busy(&self, conn: ConnId) -> bool {
        let mine = |o: &Origin| matches!(o, Origin::Client(x) if *x == conn);
        self.in_flight.as_ref().is_some_and(|f| mine(&f.cmd.origin))
            || self.client_q.iter().any(|c| mine(&c.origin))
    }

    fn client_frame(&mut self, conn: ConnId, frame: Vec<u8>) {
        let Some(c) = self.clients.get_mut(&conn) else {
            return;
        };
        if frame.is_empty() {
            return;
        }
        // The app only sends its next command after reading our last reply,
        // so anything it sends confirms the message we last handed it.
        if let Some(id) = c.unacked.take() {
            self.store.advance_cursor(&c.key, id);
        }
        if frame[0] == cmd::DEVICE_QUERY && frame.len() >= 2 {
            c.app_ver = frame[1];
            let seen = self
                .store
                .get_kv("app_ver")
                .and_then(|v| v.first().copied())
                .unwrap_or(0);
            if frame[1] > seen {
                self.store.put_kv("app_ver", &[frame[1]]);
            }
        }
        if !self.client_busy(conn)
            && let Some(frames) = self.answer_locally(conn, &frame)
        {
            for f in frames {
                self.send_to(conn, f);
            }
            return;
        }
        let enqueued_ms = self.now();
        self.client_q.push_back(Cmd {
            origin: Origin::Client(conn),
            frame,
            enqueued_ms,
        });
    }

    /// Commands meshkeep answers itself: message sync always, and a few
    /// read-only queries from the cache while the radio is away.
    fn answer_locally(&mut self, conn: ConnId, f: &[u8]) -> Option<Vec<Vec<u8>>> {
        if f[0] == cmd::SYNC_NEXT_MESSAGE {
            let c = self.clients.get_mut(&conn)?;
            let pos = self.store.cursor(&c.key).unwrap_or(0);
            return Some(vec![match self.store.next_for_app(pos) {
                Some((id, frame)) => {
                    c.unacked = Some(id);
                    p::frame_for_app(&frame, c.app_ver)
                }
                None => vec![resp::NO_MORE_MESSAGES],
            }]);
        }
        if self.radio.is_some() {
            return None;
        }
        match f[0] {
            cmd::DEVICE_QUERY => self.store.get_kv("device_info").map(|v| vec![v]),
            cmd::APP_START => self.store.get_kv("self_info").map(|v| vec![v]),
            cmd::GET_DEVICE_TIME => {
                let mut v = vec![resp::CURR_TIME];
                v.extend_from_slice(&(self.clock.now_unix() as u32).to_le_bytes());
                Some(vec![v])
            }
            // meshkeep sets the radio clock itself when the radio returns.
            cmd::SET_DEVICE_TIME => Some(vec![p::ok_frame()]),
            cmd::GET_CONTACTS => {
                let since = if f.len() >= 5 {
                    u32::from_le_bytes([f[1], f[2], f[3], f[4]])
                } else {
                    0
                };
                Some(p::contacts_listing(&self.store.contact_frames(), since))
            }
            cmd::GET_CONTACT_BY_KEY if f.len() > 32 => Some(vec![
                self.store
                    .contact_frame(&f[1..33])
                    .unwrap_or_else(|| p::err_frame(err::NOT_FOUND)),
            ]),
            cmd::GET_CHANNEL if f.len() >= 2 => Some(vec![
                self.store
                    .channel_frame(f[1])
                    .unwrap_or_else(|| p::err_frame(err::NOT_FOUND)),
            ]),
            _ => None,
        }
    }

    /// Record messages apps send through us, so the web UI shows them.
    fn client_result(&mut self, _conn: ConnId, cmd_frame: &[u8], f: &[u8]) {
        match (cmd_frame[0], f[0]) {
            (cmd::SEND_TXT_MSG, resp::SENT) => {
                let (Some((txt_type, _attempt, ts, prefix, text)), Some((_, ack, timeout))) =
                    (p::parse_send_txt_cmd(cmd_frame), p::parse_sent(f))
                else {
                    return;
                };
                if txt_type != p::txt::PLAIN {
                    return;
                }
                let conv = store::conv_dm(&prefix);
                let id = match self.store.find_outbound_dm(&conv, ts, &text) {
                    Some(id) => id,
                    None => {
                        let name = self.status.lock().unwrap().name.clone();
                        match self.store.insert_outbound(
                            &conv,
                            p::MsgKind::Direct,
                            self.clock.now_unix(),
                            ts,
                            name.as_deref(),
                            &text,
                            "sent",
                            None,
                        ) {
                            Ok(id) => {
                                if let Some(m) = self.store.message(id) {
                                    self.emit(Event::Message { message: m });
                                }
                                id
                            }
                            Err(_) => return,
                        }
                    }
                };
                self.mark_sent(id, ack, timeout);
            }
            (cmd::SEND_CHANNEL_TXT_MSG, resp::OK) => {
                let Some((_, idx, ts, text)) = p::parse_send_channel_cmd(cmd_frame) else {
                    return;
                };
                let name = self.status.lock().unwrap().name.clone();
                let ch = self.store.channel_name(idx);
                if let Ok(id) = self.store.insert_outbound(
                    &store::conv_channel(idx),
                    p::MsgKind::Channel,
                    self.clock.now_unix(),
                    ts,
                    name.as_deref(),
                    &text,
                    "sent",
                    ch.as_deref(),
                ) && let Some(m) = self.store.message(id)
                {
                    self.emit(Event::Message { message: m });
                }
            }
            _ => {}
        }
    }

    fn mark_sent(&mut self, id: i64, ack: u32, timeout_ms: u32) {
        self.store.set_status(id, "sent", Some(ack));
        self.ack_deadlines.retain(|(m, _)| *m != id);
        self.ack_deadlines.push((
            id,
            self.now() + timeout_ms as u64 + ACK_GRACE.as_millis() as u64,
        ));
        self.emit(Event::MessageStatus {
            id,
            status: "sent".into(),
        });
    }

    fn set_status(&mut self, id: i64, status: &str) {
        self.store.set_status(id, status, None);
        self.emit(Event::MessageStatus {
            id,
            status: status.into(),
        });
    }

    // ------------------------------------------------------------- web

    fn web_send_dm(&mut self, prefix: [u8; 6], text: String) -> Result<i64, String> {
        let text = text.trim().to_string();
        if text.is_empty() {
            return Err("Message is empty".into());
        }
        if text.len() > p::MAX_TEXT_LEN {
            return Err(format!(
                "Message is too long, {} bytes at most",
                p::MAX_TEXT_LEN
            ));
        }
        let ts = self.clock.now_unix() as u32;
        let name = self.status.lock().unwrap().name.clone();
        let id = self
            .store
            .insert_outbound(
                &store::conv_dm(&prefix),
                p::MsgKind::Direct,
                self.clock.now_unix(),
                ts,
                name.as_deref(),
                &text,
                "sending",
                None,
            )
            .map_err(|e| format!("Cannot store message: {e}"))?;
        if let Some(m) = self.store.message(id) {
            self.emit(Event::Message { message: m });
        }
        let enqueued_ms = self.now();
        self.client_q.push_back(Cmd {
            origin: Origin::Web(id),
            frame: p::build_send_txt(&prefix, ts, 0, &text),
            enqueued_ms,
        });
        Ok(id)
    }

    fn web_send_channel(&mut self, idx: u8, text: String) -> Result<i64, String> {
        let text = text.trim().to_string();
        if text.is_empty() {
            return Err("Message is empty".into());
        }
        let name = self.status.lock().unwrap().name.clone();
        // The radio sends "<name>: <text>" in one packet.
        let max = p::MAX_TEXT_LEN.saturating_sub(name.as_deref().map_or(0, str::len) + 2);
        if text.len() > max {
            return Err(format!("Message is too long, {max} bytes at most"));
        }
        let ts = self.clock.now_unix() as u32;
        let ch = self.store.channel_name(idx);
        let id = self
            .store
            .insert_outbound(
                &store::conv_channel(idx),
                p::MsgKind::Channel,
                self.clock.now_unix(),
                ts,
                name.as_deref(),
                &text,
                "sending",
                ch.as_deref(),
            )
            .map_err(|e| format!("Cannot store message: {e}"))?;
        if let Some(m) = self.store.message(id) {
            self.emit(Event::Message { message: m });
        }
        let enqueued_ms = self.now();
        self.client_q.push_back(Cmd {
            origin: Origin::Web(id),
            frame: p::build_send_channel(idx, ts, &text),
            enqueued_ms,
        });
        Ok(id)
    }

    fn web_result(&mut self, id: i64, cmd_frame: &[u8], f: &[u8]) {
        match (cmd_frame[0], f[0]) {
            (cmd::SEND_TXT_MSG, resp::SENT) => {
                if let Some((_, ack, timeout)) = p::parse_sent(f) {
                    self.mark_sent(id, ack, timeout);
                }
            }
            (cmd::SEND_CHANNEL_TXT_MSG, resp::OK) => self.set_status(id, "sent"),
            _ => self.set_status(id, "failed"),
        }
    }
}
