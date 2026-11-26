// SPDX-License-Identifier: AGPL-3.0-or-later
//! The permanent link to the companion radio, over TCP or USB serial.
//! Reconnects with exponential backoff; the hub does the handshake.

use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::clock::SharedClock;
use crate::config::RadioAddr;
use crate::hub::HubMsg;
use crate::protocol::{FRAME_FROM_RADIO, FRAME_TO_RADIO, FrameDecoder, encode_frame};

const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Radio frames are at most 177 bytes; allow slack for future firmware.
const MAX_RADIO_FRAME: usize = 1024;
/// The companion firmware's serial speed (USB CDC ignores it).
const SERIAL_BAUD: u32 = 115_200;

trait Link: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Link for T {}

async fn connect(addr: &RadioAddr, clock: &SharedClock) -> Result<Box<dyn Link>, String> {
    match addr {
        RadioAddr::Tcp(a) => {
            let fut = tokio::net::TcpStream::connect(a.as_str());
            tokio::select! {
                r = fut => {
                    let s = r.map_err(|e| e.to_string())?;
                    let _ = s.set_nodelay(true);
                    Ok(Box::new(s))
                }
                _ = clock.sleep(CONNECT_TIMEOUT) => Err("timed out".into()),
            }
        }
        RadioAddr::Serial(path) => {
            use tokio_serial::SerialPortBuilderExt;
            let port = tokio_serial::new(path, SERIAL_BAUD)
                .open_native_async()
                .map_err(|e| e.to_string())?;
            Ok(Box::new(port))
        }
    }
}

/// Runs until the hub goes away.
pub async fn run(
    addr: RadioAddr,
    clock: SharedClock,
    hub: mpsc::UnboundedSender<HubMsg>,
    mut kick: mpsc::UnboundedReceiver<()>,
) {
    let mut backoff = BACKOFF_MIN;
    let mut epoch = 0u64;
    let mut failing = false;
    loop {
        match connect(&addr, &clock).await {
            Ok(link) => {
                epoch += 1;
                info!("radio connected at {addr}");
                failing = false;
                let (out_tx, out_rx) = mpsc::unbounded_channel();
                if hub.send(HubMsg::RadioUp { epoch, tx: out_tx }).is_err() {
                    return;
                }
                let heard = session(link, epoch, &hub, out_rx, &mut kick).await;
                if hub.send(HubMsg::RadioDown { epoch }).is_err() {
                    return;
                }
                if heard {
                    backoff = BACKOFF_MIN;
                }
                warn!(
                    "radio at {addr} disconnected, reconnecting in {}s",
                    backoff.as_secs()
                );
            }
            Err(e) => {
                if !failing {
                    warn!("cannot reach radio at {addr}: {e}; retrying until it answers");
                    failing = true;
                }
            }
        }
        // Drain stale kicks so they do not cut the next session short.
        while kick.try_recv().is_ok() {}
        clock.sleep(backoff).await;
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

/// One connected session. Returns whether the radio sent anything.
async fn session(
    link: Box<dyn Link>,
    epoch: u64,
    hub: &mpsc::UnboundedSender<HubMsg>,
    mut out_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    kick: &mut mpsc::UnboundedReceiver<()>,
) -> bool {
    let (mut rd, mut wr) = tokio::io::split(link);
    let mut dec = FrameDecoder::new(FRAME_FROM_RADIO, MAX_RADIO_FRAME);
    let mut buf = vec![0u8; 4096];
    let mut heard = false;
    loop {
        tokio::select! {
            r = rd.read(&mut buf) => {
                let n = match r {
                    Ok(0) | Err(_) => return heard,
                    Ok(n) => n,
                };
                dec.push(&buf[..n]);
                while let Some(frame) = dec.next_frame() {
                    heard = true;
                    if hub.send(HubMsg::RadioFrame { epoch, frame }).is_err() {
                        return heard;
                    }
                }
            }
            f = out_rx.recv() => {
                let Some(f) = f else { return heard };
                // An empty frame from the hub means: close the link now.
                if f.is_empty() {
                    let _ = wr.flush().await;
                    return heard;
                }
                if wr.write_all(&encode_frame(FRAME_TO_RADIO, &f)).await.is_err() {
                    return heard;
                }
                let _ = wr.flush().await;
            }
            Some(()) = kick.recv() => return heard,
        }
    }
}
