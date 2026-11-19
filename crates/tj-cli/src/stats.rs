//! The server's own work counters, for `/_tj/inspect` (and so `tj review` and agents):
//! HTTP requests and their latency, and realtime clients, events and fan-out lag (from the
//! change to the event being written to a client).

use serde_json::{json, Value as Json};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

pub struct Stats {
    started: Instant,
    requests: AtomicU64,
    errors: AtomicU64,
    request_us: AtomicU64,
    max_request_us: AtomicU64,
    clients: AtomicI64,
    events: AtomicU64,
    lag_us: AtomicU64,
    max_lag_us: AtomicU64,
    /// States a slow client skipped because a newer one replaced them.
    coalesced: AtomicU64,
    /// Other events dropped for a slow client (see `mailbox.rs`).
    dropped: AtomicU64,
}

impl Default for Stats {
    fn default() -> Self {
        Stats {
            started: Instant::now(),
            requests: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            request_us: AtomicU64::new(0),
            max_request_us: AtomicU64::new(0),
            clients: AtomicI64::new(0),
            events: AtomicU64::new(0),
            lag_us: AtomicU64::new(0),
            max_lag_us: AtomicU64::new(0),
            coalesced: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
        }
    }
}

fn us(d: Duration) -> u64 {
    d.as_micros().min(u64::MAX as u128) as u64
}

/// Milliseconds per item, to the microsecond.
fn ms(total_us: u64, count: u64) -> f64 {
    if count == 0 {
        0.0
    } else {
        (total_us as f64 / count as f64).round() / 1000.0
    }
}

impl Stats {
    pub fn request(&self, took: Duration, ok: bool) {
        self.requests.fetch_add(1, Relaxed);
        if !ok {
            self.errors.fetch_add(1, Relaxed);
        }
        self.request_us.fetch_add(us(took), Relaxed);
        self.max_request_us.fetch_max(us(took), Relaxed);
    }

    pub fn client(&self, connected: bool) {
        self.clients.fetch_add(if connected { 1 } else { -1 }, Relaxed);
    }

    pub fn event_sent(&self, lag: Duration) {
        self.events.fetch_add(1, Relaxed);
        self.lag_us.fetch_add(us(lag), Relaxed);
        self.max_lag_us.fetch_max(us(lag), Relaxed);
    }

    pub fn coalesced(&self) {
        self.coalesced.fetch_add(1, Relaxed);
    }

    pub fn dropped(&self) {
        self.dropped.fetch_add(1, Relaxed);
    }

    pub fn json(&self) -> Json {
        let (requests, events) = (self.requests.load(Relaxed), self.events.load(Relaxed));
        json!({
            "uptimeS": self.started.elapsed().as_secs(),
            "requests": requests,
            "errors": self.errors.load(Relaxed),
            "avgMs": ms(self.request_us.load(Relaxed), requests),
            "maxMs": ms(self.max_request_us.load(Relaxed), 1),
            "clients": self.clients.load(Relaxed),
            "events": events,
            "avgLagMs": ms(self.lag_us.load(Relaxed), events),
            "maxLagMs": ms(self.max_lag_us.load(Relaxed), 1),
            "coalesced": self.coalesced.load(Relaxed),
            "dropped": self.dropped.load(Relaxed),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn averages_and_maxima() {
        let s = Stats::default();
        s.request(Duration::from_millis(2), true);
        s.request(Duration::from_millis(4), false);
        s.event_sent(Duration::from_micros(500));
        s.client(true);
        let j = s.json();
        assert_eq!((j["requests"].clone(), j["errors"].clone(), j["avgMs"].clone(), j["maxMs"].clone()), (json!(2), json!(1), json!(3.0), json!(4.0)));
        assert_eq!((j["events"].clone(), j["avgLagMs"].clone(), j["clients"].clone()), (json!(1), json!(0.5), json!(1)));
    }
}
