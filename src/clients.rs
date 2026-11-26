// SPDX-License-Identifier: AGPL-3.0-or-later
//! App listeners: each looks like a WiFi companion radio's TCP port.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tracing::warn;

use crate::config::Listener;
use crate::hub::{CLIENT_QUEUE, HubMsg};
use crate::protocol::{
    FRAME_FROM_RADIO, FRAME_TO_RADIO, FrameDecoder, MAX_FRAME_SIZE, encode_frame,
};

static NEXT_CONN: AtomicU64 = AtomicU64::new(1);

pub async fn serve(listener: TcpListener, cfg: Arc<Listener>, hub: mpsc::UnboundedSender<HubMsg>) {
    loop {
        let (sock, peer) = match listener.accept().await {
            Ok(x) => x,
            Err(e) => {
                warn!("listener {} accept failed: {e}", cfg.name);
                continue;
            }
        };
        let _ = sock.set_nodelay(true);
        let conn = NEXT_CONN.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel(CLIENT_QUEUE);
        if hub
            .send(HubMsg::ClientUp {
                conn,
                listener: cfg.name.clone(),
                per_address: cfg.per_address,
                peer,
                tx,
            })
            .is_err()
        {
            return;
        }
        tokio::spawn(connection(sock, conn, rx, hub.clone()));
    }
}

async fn connection(
    sock: TcpStream,
    conn: u64,
    mut rx: mpsc::Receiver<Vec<u8>>,
    hub: mpsc::UnboundedSender<HubMsg>,
) {
    let (mut rd, mut wr) = sock.into_split();
    let writer = tokio::spawn(async move {
        while let Some(f) = rx.recv().await {
            if wr
                .write_all(&encode_frame(FRAME_FROM_RADIO, &f))
                .await
                .is_err()
            {
                break;
            }
        }
        // The hub dropped us (or the socket broke): close our side.
        let _ = wr.shutdown().await;
    });
    // Same rules as the firmware's WiFi interface: '<' frames only, empty and
    // oversize frames skipped.
    let mut dec = FrameDecoder::new(FRAME_TO_RADIO, MAX_FRAME_SIZE);
    let mut buf = vec![0u8; 4096];
    loop {
        let n = match rd.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        dec.push(&buf[..n]);
        while let Some(frame) = dec.next_frame() {
            let _ = hub.send(HubMsg::ClientFrame { conn, frame });
        }
        if writer.is_finished() {
            break;
        }
    }
    let _ = hub.send(HubMsg::ClientDown { conn });
    writer.abort();
}
