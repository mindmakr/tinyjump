//! A realtime client's mailbox: the events the runtime thread has for one client that it
//! has not been sent yet. A slow client gets the latest state of each object instead of
//! every step in between, and at most `MAX_EVENTS` other events, so no client can make the
//! server hold an unbounded backlog. Skipping states in between is safe: a `changed` event
//! carries the whole state and its change number, and the stream stays in change order.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};
use std::time::Duration;

const MAX_EVENTS: usize = 1000;

#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub object: String,
    /// A `changed` event: a newer one for the same object replaces it.
    pub state: bool,
    /// The server-sent-event text.
    pub text: String,
}

pub struct Mailbox {
    /// Only events about these objects are kept; empty means all.
    only: Vec<String>,
    queue: Mutex<Queue>,
    ready: Condvar,
}

#[derive(Default)]
struct Queue {
    events: VecDeque<Event>,
    closed: bool,
}

impl Mailbox {
    pub fn new(only: Vec<String>) -> Mailbox {
        Mailbox { only, queue: Mutex::new(Queue::default()), ready: Condvar::new() }
    }

    /// Adds an event; false once the client has gone, so the sender can forget it.
    pub fn push(&self, e: Event) -> bool {
        let Ok(mut q) = self.queue.lock() else { return false };
        if q.closed {
            return false;
        }
        if !self.only.is_empty() && !self.only.contains(&e.object) {
            return true;
        }
        if e.state {
            q.events.retain(|old| !(old.state && old.object == e.object));
        }
        q.events.push_back(e);
        if q.events.len() > MAX_EVENTS {
            // States are bounded by the number of objects; drop the oldest other event.
            if let Some(i) = q.events.iter().position(|old| !old.state) {
                q.events.remove(i);
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

    /// The client has gone: drop what is queued and refuse more.
    pub fn close(&self) {
        if let Ok(mut q) = self.queue.lock() {
            q.closed = true;
            q.events.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(object: &str, seq: u64) -> Event {
        Event { object: object.into(), state: true, text: format!("{object}@{seq}") }
    }

    fn emit(n: usize) -> Event {
        Event { object: "A/a".into(), state: false, text: format!("emit {n}") }
    }

    #[test]
    fn a_slow_client_gets_the_latest_states_in_order_and_a_bounded_backlog() {
        let m = Mailbox::new(Vec::new());
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

        m.close();
        assert!(!m.push(state("A/a", 5)), "a closed mailbox tells the sender to forget it");
    }

    #[test]
    fn keeps_only_the_objects_asked_for() {
        let m = Mailbox::new(vec!["A/a".into()]);
        m.push(state("B/b", 1));
        m.push(state("A/a", 2));
        let texts: Vec<String> = m.take(Duration::ZERO).into_iter().map(|e| e.text).collect();
        assert_eq!(texts, ["A/a@2"]);
    }
}
