//! The server's own measurements, for `/_tj/inspect` (and so `tj review`, `tj stats` and
//! agents) and `/_tj/metrics`: HTTP requests, the wall-clock latency of each method and each
//! kind of effect, and realtime clients, events and fan-out lag (from the change to the event
//! being written to a client). The core counts the work itself (`Runtime::stats`).

use serde_json::{json, Map, Value as Json};
use std::collections::BTreeMap;
use std::fmt::Write;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering::Relaxed};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Upper bounds of the latency buckets, in milliseconds; one more bucket holds the rest.
const BOUNDS_MS: [f64; 14] = [1.0, 2.5, 5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 2500.0, 5000.0, 10000.0, 30000.0];

/// A latency histogram with fixed buckets: constant memory, and quantiles accurate to a
/// bucket, which is what dashboards and alerts need.
#[derive(Default, Clone)]
pub struct Latency {
    buckets: [u64; BOUNDS_MS.len() + 1],
    count: u64,
    failed: u64,
    sum_us: u64,
    max_us: u64,
}

impl Latency {
    fn add(&mut self, took: Duration, ok: bool) {
        let ms = took.as_secs_f64() * 1000.0;
        let i = BOUNDS_MS.iter().position(|&b| ms <= b).unwrap_or(BOUNDS_MS.len());
        self.buckets[i] += 1;
        self.count += 1;
        self.failed += u64::from(!ok);
        self.sum_us += us(took);
        self.max_us = self.max_us.max(us(took));
    }

    /// The upper bound of the bucket holding quantile `q`, never above the slowest seen.
    fn quantile_ms(&self, q: f64) -> f64 {
        let max = self.max_us as f64 / 1000.0;
        let rank = (q * self.count as f64).ceil().max(1.0) as u64;
        let mut seen = 0;
        for (i, n) in self.buckets.iter().enumerate() {
            seen += n;
            if seen >= rank {
                return BOUNDS_MS.get(i).map_or(max, |&b| b.min(max));
            }
        }
        max
    }

    fn json(&self) -> Json {
        json!({
            "count": self.count,
            "failed": self.failed,
            "avgMs": ms(self.sum_us, self.count),
            "p50Ms": self.quantile_ms(0.5),
            "p95Ms": self.quantile_ms(0.95),
            "maxMs": ms(self.max_us, 1),
        })
    }
}

/// Latencies by name. Names are bounded: methods that exist (`Runtime::method_name`) and
/// effect kinds (`http`, one per configured plugin).
#[derive(Default)]
struct Timings(Mutex<BTreeMap<String, Latency>>);

impl Timings {
    fn add(&self, name: &str, took: Duration, ok: bool) {
        if let Ok(mut m) = self.0.lock() {
            match m.get_mut(name) {
                Some(l) => l.add(took, ok),
                None => {
                    let mut l = Latency::default();
                    l.add(took, ok);
                    m.insert(name.to_string(), l);
                }
            }
        }
    }

    fn snapshot(&self) -> BTreeMap<String, Latency> {
        self.0.lock().map(|m| m.clone()).unwrap_or_default()
    }

    fn json(&self) -> Json {
        Json::Object(self.snapshot().into_iter().map(|(k, l)| (k, l.json())).collect())
    }
}

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
    /// Calls by `Class.method`, from the request reaching the runtime to its answer.
    methods: Timings,
    /// Effects by kind: `http`, `plugin.<name>`.
    effects: Timings,
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
            methods: Timings::default(),
            effects: Timings::default(),
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

    pub fn method(&self, name: &str, took: Duration, ok: bool) {
        self.methods.add(name, took, ok);
    }

    pub fn effect(&self, kind: &str, took: Duration, ok: bool) {
        self.effects.add(kind, took, ok);
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
            "methods": self.methods.json(),
            "effects": self.effects.json(),
        })
    }

    /// Everything, in the OpenMetrics text format, for Prometheus and Grafana: the core's
    /// counters (`core`, from `Runtime::stats`) and this server's measurements. Labels are
    /// class, method and effect names only, never object ids, which can identify people.
    pub fn openmetrics(&self, core: &Json) -> String {
        let mut out = String::new();
        let empty = Map::new();
        let methods = core["methods"].as_object().unwrap_or(&empty);
        for (field, help) in [("calls", "Calls run"), ("errors", "Calls that failed"), ("refused", "Calls refused by the access rules"), ("steps", "VM steps run")] {
            let _ = writeln!(out, "# TYPE tj_method_{field} counter\n# HELP tj_method_{field} {help}, by method.");
            for (name, s) in methods {
                let _ = writeln!(out, "tj_method_{field}_total{{{}}} {}", method_labels(name), s[field].as_u64().unwrap_or(0));
            }
        }
        let _ = writeln!(out, "# TYPE tj_objects_loaded gauge\n# HELP tj_objects_loaded Objects in memory, by class.");
        for (class, n) in core["objects"].as_object().unwrap_or(&empty) {
            let _ = writeln!(out, "tj_objects_loaded{{class=\"{}\"}} {}", escape(class), n.as_u64().unwrap_or(0));
        }
        histogram(&mut out, "tj_method_duration_seconds", "Time from a call reaching the runtime to its answer", &self.methods.snapshot(), method_labels);
        histogram(&mut out, "tj_effect_duration_seconds", "Time to perform an effect", &self.effects.snapshot(), |kind| format!("kind=\"{}\"", escape(kind)));
        let counters = [
            ("tj_http_requests", "HTTP requests", self.requests.load(Relaxed)),
            ("tj_http_errors", "HTTP requests answered with an error", self.errors.load(Relaxed)),
            ("tj_realtime_events", "Realtime events sent", self.events.load(Relaxed)),
            ("tj_realtime_coalesced", "States a slow client skipped for a newer one", self.coalesced.load(Relaxed)),
            ("tj_realtime_dropped", "Events dropped for a slow client", self.dropped.load(Relaxed)),
        ];
        for (name, help, v) in counters {
            let _ = writeln!(out, "# TYPE {name} counter\n# HELP {name} {help}.\n{name}_total {v}");
        }
        let _ = writeln!(out, "# TYPE tj_realtime_clients gauge\n# HELP tj_realtime_clients Connected realtime clients.\ntj_realtime_clients {}", self.clients.load(Relaxed));
        let _ = writeln!(out, "# TYPE tj_uptime_seconds gauge\n# HELP tj_uptime_seconds Seconds since the server started.\ntj_uptime_seconds {}", self.started.elapsed().as_secs());
        out.push_str("# EOF\n");
        out
    }
}

/// `tj stats`: a snapshot's methods, slowest first (then the busiest), with objects loaded
/// and effects. Latency comes from servers; devices count work only.
pub fn report(snapshot: &Json) -> String {
    let empty = Map::new();
    let counted = snapshot["stats"]["methods"].as_object().unwrap_or(&empty);
    let timed = snapshot["server"]["methods"].as_object().unwrap_or(&empty);
    let ms = |name: &str, field: &str| timed.get(name).and_then(|t| t[field].as_f64());
    let mut names: Vec<&String> = counted.keys().chain(timed.keys()).collect();
    names.sort();
    names.dedup();
    let steps = |name: &str| counted.get(name).and_then(|c| c["steps"].as_u64()).unwrap_or(0);
    names.sort_by(|a, b| ms(b, "p95Ms").unwrap_or(-1.0).total_cmp(&ms(a, "p95Ms").unwrap_or(-1.0)).then(steps(b).cmp(&steps(a))));
    let mut out = String::new();
    if names.is_empty() {
        out.push_str("no calls yet\n");
    } else {
        let width = names.iter().map(|n| n.len()).max().unwrap_or(0).max(6);
        let _ = writeln!(out, "{:width$}  {:>7} {:>7} {:>8} {:>11} {:>8} {:>8} {:>8}", "method", "calls", "errors", "refused", "steps/call", "p50 ms", "p95 ms", "max ms");
        for name in names {
            let c = counted.get(name.as_str()).cloned().unwrap_or(Json::Null);
            let n = |f: &str| c[f].as_u64().unwrap_or(0);
            let per_call = n("steps").checked_div(n("calls")).map_or("-".to_string(), |s| s.to_string());
            let time = |f: &str| ms(name, f).map_or("-".to_string(), |v| v.to_string());
            let _ = writeln!(out, "{name:width$}  {:>7} {:>7} {:>8} {per_call:>11} {:>8} {:>8} {:>8}", n("calls"), n("errors"), n("refused"), time("p50Ms"), time("p95Ms"), time("maxMs"));
        }
    }
    let loaded: Vec<String> = snapshot["stats"]["objects"].as_object().unwrap_or(&empty).iter().map(|(class, n)| format!("{class} {n}")).collect();
    let _ = writeln!(out, "\nobjects loaded: {}", if loaded.is_empty() { "none".to_string() } else { loaded.join(", ") });
    for (kind, e) in snapshot["server"]["effects"].as_object().unwrap_or(&empty) {
        let _ = writeln!(out, "effect {kind}: {} done, {} failed, p50 {} ms, p95 {} ms, max {} ms", e["count"], e["failed"], e["p50Ms"], e["p95Ms"], e["maxMs"]);
    }
    out
}

/// `class="Cart",method="add"` for `Cart.add`.
fn method_labels(name: &str) -> String {
    let (class, method) = name.split_once('.').unwrap_or((name, ""));
    format!("class=\"{}\",method=\"{}\"", escape(class), escape(method))
}

fn escape(v: &str) -> String {
    v.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

fn histogram(out: &mut String, name: &str, help: &str, series: &BTreeMap<String, Latency>, labels: impl Fn(&str) -> String) {
    let _ = writeln!(out, "# TYPE {name} histogram\n# HELP {name} {help}.");
    for (key, l) in series {
        let labels = labels(key);
        let mut cumulative = 0;
        for (i, n) in l.buckets.iter().enumerate() {
            cumulative += n;
            let le = BOUNDS_MS.get(i).map_or("+Inf".to_string(), |b| (b / 1000.0).to_string());
            let _ = writeln!(out, "{name}_bucket{{{labels},le=\"{le}\"}} {cumulative}");
        }
        let _ = writeln!(out, "{name}_sum{{{labels}}} {}\n{name}_count{{{labels}}} {}", l.sum_us as f64 / 1e6, l.count);
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

    #[test]
    fn latency_quantiles_come_from_buckets() {
        let mut l = Latency::default();
        for ms in [1, 2, 3, 4, 40, 40, 40, 40, 40, 900] {
            l.add(Duration::from_millis(ms), ms != 900);
        }
        let j = l.json();
        assert_eq!((j["count"].clone(), j["failed"].clone()), (json!(10), json!(1)));
        // Half the calls took 4 ms or less: the bucket up to 5 ms. The slowest bucket's bound
        // is capped by the slowest call actually seen.
        assert_eq!((j["p50Ms"].clone(), j["p95Ms"].clone(), j["maxMs"].clone()), (json!(50.0), json!(900.0), json!(900.0)));
        assert_eq!(Latency::default().json()["p95Ms"], json!(0.0));
    }

    #[test]
    fn report_lists_the_slowest_methods_first() {
        let snapshot = json!({
            "stats": { "methods": { "Cart.add": { "calls": 4, "errors": 1, "refused": 2, "steps": 400 }, "Cart.view": { "calls": 1, "errors": 0, "refused": 0, "steps": 10 } }, "objects": { "Cart": 2 } },
            "server": { "methods": { "Cart.view": { "p50Ms": 25.0, "p95Ms": 50.0, "maxMs": 31.2 } }, "effects": { "http": { "count": 3, "failed": 0, "p50Ms": 100.0, "p95Ms": 250.0, "maxMs": 180.5 } } },
        });
        let r = report(&snapshot);
        let lines: Vec<&str> = r.lines().collect();
        assert!(lines[1].starts_with("Cart.view ") && lines[1].ends_with("25       50     31.2"), "{r}");
        assert!(lines[2].starts_with("Cart.add ") && lines[2].contains("  100 ") && lines[2].ends_with("-        -        -"), "{r}");
        assert!(r.contains("objects loaded: Cart 2") && r.contains("effect http: 3 done, 0 failed, p50 100.0 ms"), "{r}");
        assert_eq!(report(&json!({})), "no calls yet\n\nobjects loaded: none\n");
    }

    #[test]
    fn openmetrics_has_every_series_and_no_object_ids() {
        let s = Stats::default();
        s.method("Cart.add", Duration::from_millis(3), true);
        s.method("Cart.add", Duration::from_millis(30), false);
        s.effect("plugin.ai", Duration::from_secs(2), true);
        let core = json!({ "methods": { "Cart.add": { "calls": 2, "errors": 1, "refused": 4, "steps": 900 } }, "objects": { "Cart": 3 } });
        let text = s.openmetrics(&core);
        for line in [
            "tj_method_calls_total{class=\"Cart\",method=\"add\"} 2",
            "tj_method_refused_total{class=\"Cart\",method=\"add\"} 4",
            "tj_objects_loaded{class=\"Cart\"} 3",
            "tj_method_duration_seconds_bucket{class=\"Cart\",method=\"add\",le=\"0.005\"} 1",
            "tj_method_duration_seconds_bucket{class=\"Cart\",method=\"add\",le=\"+Inf\"} 2",
            "tj_method_duration_seconds_count{class=\"Cart\",method=\"add\"} 2",
            "tj_effect_duration_seconds_bucket{kind=\"plugin.ai\",le=\"2.5\"} 1",
            "tj_http_requests_total 0",
        ] {
            assert!(text.lines().any(|l| l == line), "missing {line}\n{text}");
        }
        assert!(text.ends_with("# EOF\n"));
        assert_eq!(escape("a\"b\\c\nd"), "a\\\"b\\\\c\\nd");
    }
}
