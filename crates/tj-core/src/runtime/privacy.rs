//! Personal data (docs/privacy.md): `@pii` fields are masked wherever tools and other
//! readers look, and a person's data can be exported and erased.
//!
//! An object with personal data belongs to one person: the one whose id is the object id.
//! Everything held about a person is therefore every object whose id is theirs.
//!
//! Retention (`@retain`) and legal holds (`@legalHold`) run as one sweep: it reads the
//! records of each class that needs it, at most `SWEEP_BATCH` per step so the server keeps
//! answering, and never loads a record it does not erase. A round starts every hour.

use super::access::Who;
use super::Runtime;
use serde_json::Value as Json;
#[cfg(feature = "server")]
use {super::objects::Stored, super::TimerKind, crate::runtime::protocol::Output, crate::vm::value::Value, serde_json::Map};

#[cfg(feature = "server")]
const SWEEP_BATCH: usize = 500;
#[cfg(feature = "server")]
const SWEEP_EVERY: f64 = 3_600_000.0;
/// Where erasures waiting for a legal hold are kept.
#[cfg(feature = "server")]
const ERASURES_KEY: &str = "tj:erasures";
/// Where the secret behind `caller_tag` is kept.
const SECRET_KEY: &str = "tj:secret";

/// What a personal-data field shows to anyone but its owner, and to every tool.
pub const MASK: &str = "•••";

impl Runtime {
    /// Masks the personal-data fields of `state`, an object of class `class`.
    pub(crate) fn mask(&self, class: u16, state: &mut Json) {
        for p in &self.program.objects[class as usize].pii {
            if let Some(v) = state.get_mut(&*p.field) {
                *v = Json::from(MASK);
            }
        }
    }

    /// The state of `object` as someone who does not own it sees it, or `None` when its class
    /// holds no personal data. For a server's change streams.
    pub fn masked(&self, object: &str, state: &Json) -> Option<Json> {
        let class = self.personal_class(object)?;
        let mut masked = state.clone();
        self.mask(class, &mut masked);
        Some(masked)
    }

    /// Whether objects like `object` (`Class/id`) hold personal data.
    pub fn holds_personal_data(&self, object: &str) -> bool {
        self.personal_class(object).is_some()
    }

    fn personal_class(&self, object: &str) -> Option<u16> {
        let class = self.program.object(object.split_once('/')?.0)?;
        (!self.program.objects[class as usize].pii.is_empty()).then_some(class)
    }

    /// The tag a call record keeps for `who`: a keyed hash of a remote caller's id, so shared
    /// objects hold no readable ids, while repeats by the same caller still match.
    pub(crate) fn caller_tag(&mut self, who: &Who) -> Result<String, String> {
        let Who::Remote { id, .. } = who else { return Ok(if matches!(who, Who::System) { "system".into() } else { String::new() }) };
        let secret = match &self.secret {
            Some(s) => s.clone(),
            None => {
                let s = match self.store.load(SECRET_KEY)? {
                    Some(s) => s,
                    None => {
                        let mut bytes = [0u8; 32];
                        self.drbg.fill(&mut bytes);
                        let s = crate::crypto::hex(&bytes);
                        self.store.save(SECRET_KEY, &s)?;
                        s
                    }
                };
                let s: std::rc::Rc<str> = s.into();
                self.secret = Some(s.clone());
                s
            }
        };
        Ok(crate::crypto::hex(&crate::crypto::hmac(secret.as_bytes(), &[id.as_bytes()])[..16]))
    }
}

/// What only servers do: export, erasure and the retention sweep (the `server` feature;
/// devices leave it out).
#[cfg(feature = "server")]
impl Runtime {
    /// Every object held about `subject`: loaded, or in the store.
    /// Only classes that can belong to a person count: those with `@pii`, or an `"owner"`
    /// rule. A mistyped id (`lobby` for `user:lobby`) cannot blank a shared `Room/lobby`.
    fn subject_objects(&mut self, subject: &str) -> Result<Vec<(u16, String)>, String> {
        let mut held = Vec::new();
        for class in 0..self.program.objects.len() as u16 {
            let def = &self.program.objects[class as usize];
            let owner = |r: &Option<std::rc::Rc<[std::rc::Rc<str>]>>| r.as_ref().is_some_and(|r| r.iter().any(|w| &**w == "owner"));
            if def.pii.is_empty() && !owner(&def.read) && !def.methods.values().any(|m| owner(&m.allow)) {
                continue;
            }
            let key = format!("{}/{subject}", def.name);
            if self.is_loaded(&key) || self.store.load(&key)?.is_some() {
                held.push((class, key));
            }
        }
        Ok(held)
    }

    /// Everything held about `subject`, unmasked: the state of every object whose id is
    /// theirs. Servers only (`tj privacy export`).
    pub fn export_subject(&mut self, subject: &str) -> Result<Json, String> {
        let mut out = Map::new();
        for (class, key) in self.subject_objects(subject)? {
            let o = self.instance(class, subject).map_err(|e| e.message())?;
            out.insert(key, self.object_state(&o)?);
        }
        Ok(Json::Object(out))
    }

    /// Erases `subject`: every object whose id is theirs goes back to its defaults, and loses
    /// its alarms and the stored results of calls. The blank state is saved with a new change
    /// number, so replicas and devices receive the erasure like any other change, including
    /// those that reconnect later. Alarms the person scheduled on other objects are dropped.
    /// An object under `@legalHold` is kept until its hold ends, then erased by the sweep.
    /// Returns what was erased, what is held and until when (epoch ms), or why not (409
    /// while a call is part way through one of the objects; 503 when the store fails).
    /// Servers only (`tj privacy erase`).
    pub fn erase_subject(&mut self, subject: &str) -> Result<Erasure, (u16, String)> {
        let objects = self.subject_objects(subject).map_err(|e| (503, e))?;
        if let Some((_, key)) = objects.iter().find(|(class, _)| self.in_use(*class, subject)) {
            return Err((409, format!("{key} is part way through a call; try again in a moment")));
        }
        let (mut erased, mut held) = (Vec::new(), Vec::new());
        for (class, key) in objects {
            let o = self.instance(class, subject).map_err(|e| (503, e.message()))?;
            if let Some(until) = self.held_until(class, &o) {
                held.push((key, until));
                continue;
            }
            let fresh = self.defaults(class).map_err(|e| (503, e.message()))?;
            self.reset(&o, fresh);
            erased.push(key);
        }
        self.drop_alarms_of(subject, &held);
        if !self.flush() {
            return Err((503, "the erasure could not be saved; nothing was erased; try again".into()));
        }
        if !held.is_empty() {
            for (key, until) in &held {
                self.erasures.insert(key.clone(), *until);
            }
            self.save_erasures().map_err(|e| (503, format!("the erasure was done, but the objects under a legal hold could not be put on the list to erase later: {e}; try again")))?;
        }
        Ok(Erasure { erased, held, outputs: std::mem::take(&mut self.outputs) })
    }

    /// When the legal hold on `o` ends, if it holds now.
    fn held_until(&self, class: u16, o: &crate::vm::value::ObjRef) -> Option<f64> {
        let hold = self.program.objects[class as usize].hold?;
        let until = self.inst(o).changed + hold;
        (self.now < until).then_some(until)
    }

    /// Drops the alarms `subject` scheduled, wherever they are. Objects with alarms are always
    /// loaded (the alarm index brings them in at boot), so memory is the whole picture.
    fn drop_alarms_of(&mut self, subject: &str, held: &[(String, f64)]) {
        let mine = |a: &super::objects::Alarm| matches!(&a.caller, Some(crate::runtime::protocol::Caller::Remote { id, .. }) if id == subject);
        let keys: Vec<(u16, std::rc::Rc<str>)> = self
            .instances
            .iter()
            .filter(|(_, i)| i.alarms.iter().any(mine) && !held.iter().any(|(k, _)| *k == i.handle.key()))
            .map(|(k, _)| k.clone())
            .collect();
        for key in keys {
            if let Some(inst) = self.instances.get_mut(&key) {
                inst.alarms.retain(|a| !mine(a));
            }
            self.touched.insert(key);
        }
    }

    fn save_erasures(&mut self) -> Result<(), String> {
        let text = serde_json::to_string(&self.erasures).expect("a map of numbers serializes");
        self.store.save(ERASURES_KEY, &text)
    }

    /// At boot: reads the erasures waiting for holds, and starts the sweep if anything needs it.
    pub(crate) fn start_retention(&mut self) -> Result<(), String> {
        if !self.server {
            return Ok(());
        }
        if let Some(text) = self.store.load(ERASURES_KEY)? {
            self.erasures = serde_json::from_str(&text).map_err(|e| format!("{ERASURES_KEY} is damaged ({e}); restore it from a backup"))?;
        }
        let needed = !self.erasures.is_empty() || self.program.objects.iter().any(|d| d.retain.is_some());
        if needed {
            self.push_timer(self.now, TimerKind::Retain);
        }
        Ok(())
    }

    /// One step of the sweep: up to `SWEEP_BATCH` objects; a new round every hour.
    pub(crate) fn retain_step(&mut self) {
        if self.retain_queue.is_empty() {
            let classes: Vec<String> = self.program.objects.iter().filter(|d| d.retain.is_some()).map(|d| format!("{}/", d.name)).collect();
            let mut keys: Vec<String> = self.erasures.keys().cloned().collect();
            for prefix in classes {
                match self.store.keys(&prefix) {
                    Ok(k) => keys.extend(k),
                    Err(e) => self.outputs.push(Output::Error { message: format!("retention could not list {prefix}: {e}"), trace: Vec::new() }),
                }
            }
            keys.sort();
            keys.dedup();
            keys.reverse();
            self.retain_queue = keys;
        }
        for _ in 0..SWEEP_BATCH {
            let Some(key) = self.retain_queue.pop() else { break };
            if let Err(e) = self.retain_check(&key) {
                self.outputs.push(Output::Error { message: format!("retention could not erase {key}: {e}"), trace: Vec::new() });
            }
        }
        // The next batch waits for the next tick, so requests are answered in between.
        let next = if self.retain_queue.is_empty() { self.now + SWEEP_EVERY } else { self.now + 1.0 };
        self.push_timer(next, TimerKind::Retain);
    }

    /// Erases `key` if its retention has run out, or a person's erasure was waiting for its
    /// hold to end; never while a hold lasts. Objects that are not loaded are read and
    /// written as records, without loading them.
    fn retain_check(&mut self, key: &str) -> Result<(), String> {
        let Some((class, id)) = key.split_once('/').and_then(|(c, id)| Some((self.program.object(c)?, id))) else { return Ok(()) };
        let def = &self.program.objects[class as usize];
        let (retain, hold) = (def.retain, def.hold);
        let defaults = Value::Rec(self.defaults(class).map_err(|e| e.message())?.into()).to_json()?;
        let loaded = self.is_loaded(key);
        let (created, changed, blank) = if loaded {
            let o = self.instance(class, id).map_err(|e| e.message())?;
            if self.in_use(class, id) {
                // A call is part way through it: the next round looks again.
                return Ok(());
            }
            let inst = self.inst(&o);
            if inst.created <= 0.0 || inst.changed <= 0.0 {
                // Its clock starts when what it now holds is saved, at the end of this turn.
                return Ok(());
            }
            (inst.created, inst.changed, self.is_blank(&o, &defaults)?)
        } else {
            let Some(text) = self.store.load(key)? else { return Ok(()) };
            let mut stored: Stored = serde_json::from_str(&text).map_err(|e| format!("unreadable stored state: {e}"))?;
            let blank = stored.alarms.is_empty() && stored.calls.is_empty() && Json::Object(stored.state.clone()) == defaults;
            if !blank && (stored.created <= 0.0 || stored.changed <= 0.0) {
                // A record from before times were kept: its clock starts now.
                stored.created = self.now;
                stored.changed = self.now;
                return self.store.save(key, &serde_json::to_string(&stored).expect("records serialize"));
            }
            (stored.created, stored.changed, blank)
        };
        let waiting = self.erasures.get(key).copied();
        let retained = retain.is_some_and(|r| self.now >= (if r.from_created { created } else { changed }) + r.ms);
        if blank || !(retained || waiting.is_some_and(|until| self.now >= until)) {
            if blank && waiting.is_some() {
                self.erasures.remove(key);
                self.save_erasures()?;
            }
            return Ok(());
        }
        if hold.is_some_and(|h| self.now < changed + h) {
            return Ok(());
        }
        if loaded {
            let o = self.instance(class, id).map_err(|e| e.message())?;
            let fresh = self.defaults(class).map_err(|e| e.message())?;
            self.reset(&o, fresh);
        } else {
            // A blank record with a new change number, so devices that reconnect get it too.
            self.change_seq += 1;
            let seq = self.change_seq;
            let Json::Object(state) = defaults else { unreachable!("state is a record") };
            // No clock: it holds nothing until someone writes to it again.
            let record = serde_json::json!({ "state": state, "seq": seq });
            self.store.save(key, &record.to_string())?;
            self.store.save("tj:seq", &seq.to_string())?;
            self.outputs.push(Output::Changed { object: key.to_string(), state: Json::Object(state), seq });
        }
        let reason = if waiting.is_some() { "erase after hold" } else { "retain" };
        self.outputs.push(Output::Erased { object: key.to_string(), reason: reason.into() });
        if waiting.is_some() {
            self.erasures.remove(key);
            self.save_erasures()?;
        }
        Ok(())
    }

    /// Whether a call is part way through the object: waiting on an effect, or holding or
    /// waiting for it as `@exclusive`.
    fn in_use(&self, class: u16, id: &str) -> bool {
        let here = |o: &Option<std::rc::Rc<crate::vm::value::ObjRef>>| o.as_ref().is_some_and(|o| o.class == class && &*o.id == id);
        self.waiting.values().any(|(f, _)| f.frames.iter().any(|fr| here(&fr.this))) || self.exclusive_busy(class, id)
    }
}

/// What `erase_subject` did: objects erased, objects kept under a legal hold and until when,
/// and the outputs to carry out (the blank states for subscribers).
#[cfg(feature = "server")]
pub struct Erasure {
    pub erased: Vec<String>,
    pub held: Vec<(String, f64)>,
    pub outputs: Vec<Output>,
}
