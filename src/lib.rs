// SPDX-License-Identifier: AGPL-3.0-or-later
//! meshkeep: an always-on storing proxy and web client for MeshCore companion
//! radios.

pub mod clients;
pub mod clock;
pub mod config;
pub mod hub;
pub mod protocol;
pub mod radio;
pub mod sim;
pub mod store;
pub mod web;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use clock::SharedClock;
use config::Config;
use hub::{Event, Hub, SharedStatus, Status};
use store::Store;

/// A started meshkeep instance.
pub struct Running {
    /// Bound app listener addresses, in config order.
    pub listeners: Vec<SocketAddr>,
    pub web: Option<SocketAddr>,
    pub events: broadcast::Sender<Event>,
    pub store: Store,
    pub status: SharedStatus,
    tasks: Vec<JoinHandle<()>>,
}

impl Running {
    /// Wait until any task ends (they normally run forever).
    pub async fn wait(&mut self) {
        let tasks = std::mem::take(&mut self.tasks);
        let _ = futures_util::future::select_all(tasks).await;
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

/// Open the store, bind every socket and start all tasks.
pub async fn start(cfg: Config, clock: SharedClock) -> Result<Running, String> {
    let radio = cfg.radio.clone().ok_or("no radio configured")?;
    std::fs::create_dir_all(&cfg.data_dir).map_err(|e| {
        format!(
            "cannot create data directory {}: {e}",
            cfg.data_dir.display()
        )
    })?;
    let db = cfg.data_dir.join("meshkeep.db");
    let store =
        Store::open(&db).map_err(|e| format!("cannot open database {}: {e}", db.display()))?;

    // Every configured listener gets its cursor now, so messages that arrive
    // before its device first connects are kept for it.
    let max = store.max_message_id();
    for l in &cfg.listeners {
        store.ensure_cursor(&l.name, max);
    }

    let (events, _) = broadcast::channel(1024);
    let status: SharedStatus = Arc::new(Mutex::new(Status {
        radio_addr: radio.to_string(),
        ..Default::default()
    }));
    if let Some(si) = store
        .get_kv("self_info")
        .and_then(|f| protocol::parse_self_info(&f))
    {
        let mut st = status.lock().unwrap();
        st.name = Some(si.name);
        st.pubkey = Some(protocol::hex(&si.pubkey));
    }

    let (hub_tx, hub_rx) = mpsc::unbounded_channel();
    let (kick_tx, kick_rx) = mpsc::unbounded_channel();
    let mut tasks = Vec::new();

    let mut bound = Vec::new();
    for l in &cfg.listeners {
        let sock = TcpListener::bind(l.bind)
            .await
            .map_err(|e| format!("cannot listen on {} for {}: {e}", l.bind, l.name))?;
        bound.push(sock.local_addr().map_err(|e| e.to_string())?);
        tasks.push(tokio::spawn(clients::serve(
            sock,
            Arc::new(l.clone()),
            hub_tx.clone(),
        )));
    }

    let mut web_addr = None;
    if let Some(bind) = cfg.web_bind {
        let sock = TcpListener::bind(bind)
            .await
            .map_err(|e| format!("cannot listen on {bind} for the web interface: {e}"))?;
        web_addr = Some(sock.local_addr().map_err(|e| e.to_string())?);
        let app = web::router(web::WebState {
            store: store.clone(),
            hub: hub_tx.clone(),
            events: events.clone(),
            status: status.clone(),
            clock: clock.clone(),
            password: if cfg.auth { cfg.password.clone() } else { None },
            login_gate: Default::default(),
        });
        tasks.push(tokio::spawn(async move {
            let _ = axum::serve(sock, app).await;
        }));
    }

    let hub = Hub::new(
        clock.clone(),
        store.clone(),
        events.clone(),
        status.clone(),
        kick_tx,
        cfg.mirror_sent,
    );
    tasks.push(tokio::spawn(hub.run(hub_rx)));
    tasks.push(tokio::spawn(radio::run(radio, clock, hub_tx, kick_rx)));

    Ok(Running {
        listeners: bound,
        web: web_addr,
        events,
        store,
        status,
        tasks,
    })
}
