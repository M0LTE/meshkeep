// SPDX-License-Identifier: AGPL-3.0-or-later
//! Injectable clock. Everything in meshkeep that needs the time or a timer
//! goes through [`Clock`], so tests can run on [`FakeClock`] without real
//! sleeps.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::{Notify, oneshot};

pub type SleepFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

pub trait Clock: Send + Sync + 'static {
    /// Wall clock, seconds since the Unix epoch.
    fn now_unix(&self) -> u64;
    /// Monotonic milliseconds from an arbitrary origin.
    fn now_ms(&self) -> u64;
    /// Completes once `now_ms() >= deadline_ms`.
    fn sleep_until(&self, deadline_ms: u64) -> SleepFuture;

    fn sleep(&self, d: Duration) -> SleepFuture {
        let deadline = self.now_ms().saturating_add(d.as_millis() as u64);
        self.sleep_until(deadline)
    }
}

pub type SharedClock = Arc<dyn Clock>;

/// The real clock.
pub struct SystemClock {
    origin: tokio::time::Instant,
}

impl SystemClock {
    pub fn new() -> Self {
        SystemClock {
            origin: tokio::time::Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now_unix(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    fn now_ms(&self) -> u64 {
        self.origin.elapsed().as_millis() as u64
    }

    fn sleep_until(&self, deadline_ms: u64) -> SleepFuture {
        let at = self.origin + Duration::from_millis(deadline_ms);
        Box::pin(tokio::time::sleep_until(at))
    }
}

/// A manually driven clock for tests. Time only moves when [`FakeClock::advance`]
/// is called.
pub struct FakeClock {
    inner: Mutex<FakeInner>,
    sleepers_changed: Notify,
}

struct FakeInner {
    now_ms: u64,
    unix_base: u64,
    sleepers: Vec<(u64, oneshot::Sender<()>)>,
}

impl FakeClock {
    pub fn new(unix_base: u64) -> Arc<Self> {
        Arc::new(FakeClock {
            inner: Mutex::new(FakeInner {
                now_ms: 0,
                unix_base,
                sleepers: Vec::new(),
            }),
            sleepers_changed: Notify::new(),
        })
    }

    pub fn advance(&self, d: Duration) {
        let mut g = self.inner.lock().unwrap();
        g.now_ms += d.as_millis() as u64;
        let now = g.now_ms;
        let mut keep = Vec::new();
        for (deadline, tx) in g.sleepers.drain(..) {
            if deadline <= now {
                let _ = tx.send(());
            } else if !tx.is_closed() {
                keep.push((deadline, tx));
            }
        }
        g.sleepers = keep;
    }

    /// Number of live timers with a deadline in the future.
    pub fn pending_sleepers(&self) -> usize {
        let mut g = self.inner.lock().unwrap();
        g.sleepers.retain(|(_, tx)| !tx.is_closed());
        g.sleepers.len()
    }

    /// Earliest pending deadline, if any.
    pub fn next_deadline(&self) -> Option<u64> {
        let mut g = self.inner.lock().unwrap();
        g.sleepers.retain(|(_, tx)| !tx.is_closed());
        g.sleepers.iter().map(|(d, _)| *d).min()
    }

    /// Wait until some task registers a timer with a deadline strictly after
    /// `after_ms`. Lets a test advance time only once the code under test is
    /// actually waiting on it.
    pub async fn wait_for_sleeper_after(&self, after_ms: u64) {
        self.wait_for_sleeper(|d| d > after_ms).await
    }

    /// Wait until some task registers a timer whose deadline satisfies `pred`.
    pub async fn wait_for_sleeper(&self, pred: impl Fn(u64) -> bool) {
        loop {
            let notified = self.sleepers_changed.notified();
            {
                let mut g = self.inner.lock().unwrap();
                g.sleepers.retain(|(_, tx)| !tx.is_closed());
                if g.sleepers.iter().any(|(d, _)| pred(*d)) {
                    return;
                }
            }
            notified.await;
        }
    }
}

impl Clock for FakeClock {
    fn now_unix(&self) -> u64 {
        let g = self.inner.lock().unwrap();
        g.unix_base + g.now_ms / 1000
    }

    fn now_ms(&self) -> u64 {
        self.inner.lock().unwrap().now_ms
    }

    fn sleep_until(&self, deadline_ms: u64) -> SleepFuture {
        let mut g = self.inner.lock().unwrap();
        if deadline_ms <= g.now_ms {
            return Box::pin(async {});
        }
        let (tx, rx) = oneshot::channel();
        g.sleepers.push((deadline_ms, tx));
        drop(g);
        self.sleepers_changed.notify_waiters();
        Box::pin(async move {
            // A dropped sender means the clock itself went away; never wake.
            if rx.await.is_err() {
                std::future::pending::<()>().await;
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fake_clock_fires_only_when_advanced() {
        let c = FakeClock::new(1_700_000_000);
        let mut s = c.sleep(Duration::from_secs(5));
        assert!(futures_util::poll!(&mut s).is_pending());
        c.advance(Duration::from_secs(4));
        assert!(futures_util::poll!(&mut s).is_pending());
        c.advance(Duration::from_secs(1));
        assert!(futures_util::poll!(&mut s).is_ready());
        assert_eq!(c.now_unix(), 1_700_000_005);
    }

    #[tokio::test]
    async fn past_deadline_is_immediate() {
        let c = FakeClock::new(0);
        c.advance(Duration::from_secs(10));
        let mut s = c.sleep_until(5_000);
        assert!(futures_util::poll!(&mut s).is_ready());
    }
}
