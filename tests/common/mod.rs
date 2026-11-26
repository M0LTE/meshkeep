// SPDX-License-Identifier: AGPL-3.0-or-later
//! Test harness: simulated radio + meshkeep + fake app clients over real
//! local TCP sockets, all on a fake clock.
#![allow(dead_code)]

use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use meshkeep::clock::FakeClock;
use meshkeep::config::{Config, Listener, RadioAddr};
use meshkeep::hub::Event;
use meshkeep::protocol::{self as p, FRAME_FROM_RADIO, FRAME_TO_RADIO, FrameDecoder, encode_frame};
use meshkeep::sim::{SimContact, SimRadio};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::broadcast;

pub const PASSWORD: &str = "test-password";
pub const UNIX_BASE: u64 = 1_700_000_000;

/// Fails a hung test instead of blocking forever. Never part of pass logic.
pub async fn guard<T>(f: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(20), f)
        .await
        .expect("test hung waiting for an event")
}

static N: AtomicU32 = AtomicU32::new(0);

pub fn temp_dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "meshkeep-test-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

pub struct Rig {
    pub sim: SimRadio,
    pub clock: Arc<FakeClock>,
    pub mk: meshkeep::Running,
    pub events: broadcast::Receiver<Event>,
    pub dir: PathBuf,
    pub alice: SimContact,
    pub bob: SimContact,
}

pub fn listeners(names: &[(&str, bool)]) -> Vec<Listener> {
    names
        .iter()
        .map(|(n, per)| Listener {
            name: n.to_string(),
            bind: "127.0.0.1:0".parse().unwrap(),
            per_address: *per,
        })
        .collect()
}

pub type BeforeStart = Box<dyn FnOnce(&SimRadio, &SimContact, &SimContact)>;

pub struct RigOpts {
    pub listeners: Vec<Listener>,
    pub before_start: BeforeStart,
    pub dir: Option<PathBuf>,
    pub auth: bool,
    pub mirror_sent: bool,
}

impl Default for RigOpts {
    fn default() -> Self {
        RigOpts {
            listeners: listeners(&[("phone", false), ("tablet", false)]),
            before_start: Box::new(|_, _, _| {}),
            dir: None,
            auth: true,
            mirror_sent: true,
        }
    }
}

impl Rig {
    pub async fn start() -> Rig {
        Self::start_with(RigOpts::default()).await
    }

    pub async fn start_with(o: RigOpts) -> Rig {
        let sim = SimRadio::start().await;
        let alice = SimContact::new(1, "Alice");
        let bob = SimContact::new(2, "Bob");
        sim.add_contact(alice.clone());
        sim.add_contact(bob.clone());
        sim.set_channel(1, "Hikers");
        (o.before_start)(&sim, &alice, &bob);
        let clock = FakeClock::new(UNIX_BASE);
        let dir = o.dir.unwrap_or_else(temp_dir);
        let cfg = Config {
            radio: Some(RadioAddr::Tcp(sim.addr.to_string())),
            data_dir: dir.clone(),
            listeners: o.listeners,
            web_bind: Some("127.0.0.1:0".parse().unwrap()),
            password: Some(PASSWORD.into()),
            auth: o.auth,
            mirror_sent: o.mirror_sent,
        };
        let mk = meshkeep::start(cfg, clock.clone())
            .await
            .expect("start meshkeep");
        let events = mk.events.subscribe();
        let mut rig = Rig {
            sim,
            clock,
            mk,
            events,
            dir,
            alice,
            bob,
        };
        // Handshake complete and offline queue drained.
        rig.wait_event(|e| matches!(e, Event::DrainDone)).await;
        rig
    }

    pub async fn wait_event(&mut self, mut pred: impl FnMut(&Event) -> bool) -> Event {
        guard(async {
            loop {
                match self.events.recv().await {
                    Ok(e) if pred(&e) => return e,
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(e) => panic!("event stream ended: {e}"),
                }
            }
        })
        .await
    }

    pub fn listener(&self, i: usize) -> SocketAddr {
        self.mk.listeners[i]
    }

    pub async fn app(&self, i: usize) -> App {
        App::connect(self.listener(i)).await
    }

    pub fn web(&self) -> String {
        format!("http://{}", self.mk.web.unwrap())
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.sim.shutdown();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A fake MeshCore app speaking the TCP companion protocol.
pub struct App {
    sock: TcpStream,
    dec: FrameDecoder,
    /// Push frames seen while waiting for responses.
    pub pushes: Vec<Vec<u8>>,
}

impl App {
    pub async fn connect(addr: SocketAddr) -> App {
        let sock = guard(TcpStream::connect(addr)).await.expect("connect");
        sock.set_nodelay(true).unwrap();
        App {
            sock,
            dec: FrameDecoder::new(FRAME_FROM_RADIO, 4096),
            pushes: Vec::new(),
        }
    }

    pub async fn send(&mut self, f: &[u8]) {
        self.sock
            .write_all(&encode_frame(FRAME_TO_RADIO, f))
            .await
            .unwrap();
    }

    /// Next frame of any kind.
    pub async fn recv(&mut self) -> Vec<u8> {
        guard(async {
            loop {
                if let Some(f) = self.dec.next_frame() {
                    return f;
                }
                let mut buf = [0u8; 2048];
                let n = self.sock.read(&mut buf).await.expect("read");
                assert!(n > 0, "connection closed");
                self.dec.push(&buf[..n]);
            }
        })
        .await
    }

    /// Next non-push frame; pushes are kept in `self.pushes`.
    pub async fn response(&mut self) -> Vec<u8> {
        loop {
            let f = self.recv().await;
            if p::is_push(f[0]) {
                self.pushes.push(f);
            } else {
                return f;
            }
        }
    }

    pub async fn request(&mut self, f: &[u8]) -> Vec<u8> {
        self.send(f).await;
        self.response().await
    }

    /// Wait for a push with this code (checking ones already seen first).
    pub async fn push(&mut self, code: u8) -> Vec<u8> {
        if let Some(i) = self.pushes.iter().position(|f| f[0] == code) {
            return self.pushes.remove(i);
        }
        loop {
            let f = self.recv().await;
            if f[0] == code {
                return f;
            }
            if p::is_push(f[0]) {
                self.pushes.push(f);
            } else {
                panic!("unexpected response 0x{:02x} while waiting for push", f[0]);
            }
        }
    }

    /// The official app's connect sequence.
    pub async fn handshake(&mut self, ver: u8) {
        let di = self.request(&[p::cmd::DEVICE_QUERY, ver]).await;
        assert_eq!(di[0], p::resp::DEVICE_INFO);
        let mut start = vec![p::cmd::APP_START, 1, 0, 0, 0, 0, 0, 0];
        start.extend_from_slice(b"test");
        let si = self.request(&start).await;
        assert_eq!(si[0], p::resp::SELF_INFO);
    }

    /// Sync until NO_MORE_MESSAGES; returns the message frames.
    pub async fn sync_all(&mut self) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        loop {
            let f = self.request(&[p::cmd::SYNC_NEXT_MESSAGE]).await;
            if f[0] == p::resp::NO_MORE_MESSAGES {
                return out;
            }
            out.push(f);
        }
    }
}

/// Text of a v3 or legacy message frame.
pub fn msg_text(f: &[u8]) -> String {
    p::parse_inbound(f).unwrap().text
}
