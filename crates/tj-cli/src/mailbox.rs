//! A realtime client's mailbox: the events the runtime thread has for one client that it
//! has not been sent yet. A slow client gets the latest state of each object instead of
//! every step in between, and at most `MAX_EVENTS` other events, so no client can make the
//! server hold an unbounded backlog. Skipping states in between is safe: a `changed` event
//! carries the whole state and its change number, and the stream stays in change order.

use crate::stats::Stats;
use tj_core::runtime::protocol::Caller;
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const MAX_EVENTS: usize = 1000;

#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub object: String,
    /// Events of one kind (`"state"`, `"read"`) about one object replace each other: a slow
    /// client gets the latest. `None`: every one is kept.
    pub merge: Option<&'static str>,
    /// The server-sent-event text.
    pub text: String,
    /// When it happened, to measure how long it took to reach the client.
    pub at: Instant,
}

impl Event {
    pub fn new(object: String, merge: Option<&'static str>, text: String) -> Event {
        Event { object, merge, text, at: Instant::now() }
    }
}

pub struct Mailbox {
    /// Only events about these objects are kept; `None`: every object (the admin's stream).
    /// Events about no object (`refused`, `reload`) are always kept.
    only: Option<Vec<String>>,
    /// Who reads it, for the fields their rules let them see. `None`: the admin's stream,
    /// which sees what tools see.
    reader: Option<Caller>,
    stats: Option<Arc<Stats>>,
    queue: Mutex<Queue>,
    ready: Condvar,
}

#[derive(Default)]
struct Queue {
    events: VecDeque<Event>,
    closed: bool,
}

impl Mailbox {
    pub fn new(only: Option<Vec<String>>, stats: Option<Arc<Stats>>) -> Mailbox {
        Mailbox { only, reader: None, stats, queue: Mutex::new(Queue::default()), ready: Condvar::new() }
    }

    pub fn read_by(mut self, reader: Option<Caller>) -> Mailbox {
        self.reader = reader;
        self
    }

    pub fn reader(&self) -> Option<&Caller> {
        self.reader.as_ref()
    }

    pub fn objects(&self) -> Option<&[String]> {
        self.only.as_deref()
    }

    pub fn is_closed(&self) -> bool {
        self.queue.lock().map_or(true, |q| q.closed)
    }

    /// Adds an event; false once the client has gone, so the sender can forget it.
    pub fn push(&self, e: Event) -> bool {
        let Ok(mut q) = self.queue.lock() else { return false };
        if q.closed {
            return false;
        }
        if !e.object.is_empty()
            && self
                .only
                .as_ref()
                .is_some_and(|only| !only.iter().any(|pat| matches_pattern(pat, &e.object)))
        {
            return true;
        }
        if e.merge.is_some() {
            let before = q.events.len();
            q.events.retain(|old| !(old.merge == e.merge && old.object == e.object));
            if q.events.len() < before {
                self.stats.iter().for_each(|s| s.coalesced());
            }
        }
        q.events.push_back(e);
        if q.events.len() > MAX_EVENTS {
            // Merged events are bounded by the number of objects; drop the oldest other event.
            if let Some(i) = q.events.iter().position(|old| old.merge.is_none()) {
                q.events.remove(i);
                self.stats.iter().for_each(|s| s.dropped());
            }
        }
        self.ready.notify_one();
        true
    }

    /// Waits up to `timeout` for events and takes them all (none on timeout).
    pub fn take(&self, timeout: Duration) -> Vec<Event> {
        let Ok(q) = self.queue.lock() else { return Vec::new() };
        match self.ready.wait_timeout_while(q, timeout, |q| q.events.is_empty() && !q.closed) {
            Ok((mut q, _)) => q.events.drain(..).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// The client is connected (true) or has gone (false), for the counters.
    pub fn connected(&self, yes: bool) {
        self.stats.iter().for_each(|s| s.client(yes));
    }

    /// These events were written to the client.
    pub fn delivered(&self, events: &[Event]) {
        if let Some(s) = &self.stats {
            events.iter().for_each(|e| s.event_sent(e.at.elapsed()));
        }
    }

    /// The client has gone (or lost read access on reload): drop what is queued and refuse more.
    pub fn close(&self) {
        if let Ok(mut q) = self.queue.lock() {
            q.closed = true;
            q.events.clear();
        }
        self.ready.notify_all();
    }
}

fn matches_pattern(pattern: &str, object: &str) -> bool {
    pattern == object
        || pattern
            .strip_suffix('*')
            .is_some_and(|pref| pref.contains('/') && object.starts_with(pref))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(object: &str, seq: u64) -> Event {
        Event::new(object.into(), Some("state"), format!("{object}@{seq}"))
    }

    fn emit(n: usize) -> Event {
        Event::new("A/a".into(), None, format!("emit {n}"))
    }

    #[test]
    fn a_slow_client_gets_the_latest_states_in_order_and_a_bounded_backlog() {
        let stats = Arc::new(Stats::default());
        let m = Mailbox::new(None, Some(stats.clone()));
        for e in [state("A/a", 1), state("B/b", 2), state("A/a", 3)] {
            assert!(m.push(e));
        }
        let texts: Vec<String> = m.take(Duration::ZERO).into_iter().map(|e| e.text).collect();
        assert_eq!(texts, ["B/b@2", "A/a@3"], "superseded states are skipped; the rest stay in change order");

        for n in 0..MAX_EVENTS + 10 {
            m.push(emit(n));
        }
        m.push(state("A/a", 4));
        let got = m.take(Duration::ZERO);
        assert_eq!(got.len(), MAX_EVENTS);
        assert_eq!(got[0].text, "emit 11", "the oldest other events are dropped first");
        assert_eq!(got.last().map(|e| e.text.as_str()), Some("A/a@4"), "states are never dropped");

        let counted = stats.json();
        assert_eq!((counted["coalesced"].clone(), counted["dropped"].clone()), (serde_json::json!(1), serde_json::json!(11)), "both are counted");

        m.close();
        assert!(!m.push(state("A/a", 5)), "a closed mailbox tells the sender to forget it");
    }

    #[test]
    fn keeps_only_the_objects_asked_for() {
        let m = Mailbox::new(Some(vec!["A/a".into()]), None);
        m.push(state("B/b", 1));
        m.push(state("A/a", 2));
        let texts: Vec<String> = m.take(Duration::ZERO).into_iter().map(|e| e.text).collect();
        assert_eq!(texts, ["A/a@2"]);
    }

    #[test]
    fn prefix_topic_pattern_matches_only_matching_room_and_class() {
        let m = Mailbox::new(Some(vec!["Room/studio:*".into()]), None);
        m.push(state("Room/studio:1", 1));
        m.push(state("Room/other:1", 2));
        m.push(state("Other/studio:1", 3));
        m.push(state("Room/studio:2", 4));
        let texts: Vec<String> = m.take(Duration::ZERO).into_iter().map(|e| e.text).collect();
        assert_eq!(texts, ["Room/studio:1@1", "Room/studio:2@4"]);
    }
}
