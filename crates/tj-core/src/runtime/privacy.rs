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
use {super::objects::Stored, super::TimerKind, crate::runtime::protocol::Output, crate::vm::value::{Exc, Value}, serde_json::Map, std::rc::Rc};

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

/// A list of several people's entries (`@by`): class, field, whose field, time field.
#[cfg(feature = "server")]
type SharedList = (u16, Rc<str>, Rc<str>, Option<Rc<str>>);
/// Entries found in one object's list: the object, the field, the entries.
#[cfg(feature = "server")]
type Entries = (Rc<crate::vm::value::ObjRef>, Rc<str>, Vec<Json>);
/// A field naming a person (`"self"`): the object, the field.
#[cfg(feature = "server")]
type Named = (Rc<crate::vm::value::ObjRef>, Rc<str>);

impl Runtime {
    /// Hides the fields of `state`, an object of class `class`, that [`Runtime::hidden`]
    /// found: personal data shows `•••`, a field naming a person (`"self"`) shows `""` when
    /// empty and `•••` when set (so others see it is taken without seeing whom), and any other
    /// hidden field shows its default value.
    pub(crate) fn hide(&mut self, class: u16, state: &mut Json, hidden: &[usize]) {
        let guards = &self.program.objects[class as usize].guards;
        let defaults = if hidden.iter().any(|&i| !guards[i].pii && !guards[i].names_a_person()) { self.defaults(class).ok() } else { None };
        for &i in hidden {
            let g = &self.program.objects[class as usize].guards[i];
            if let Some(v) = state.get_mut(&*g.field) {
                *v = if g.pii {
                    Json::from(MASK)
                } else if g.names_a_person() {
                    if v.as_str().is_some_and(|s| !s.is_empty()) { Json::from(MASK) } else { Json::from("") }
                } else {
                    defaults.as_ref().and_then(|d| d.borrow().get(&*g.field).and_then(|d| d.to_json().ok())).unwrap_or(Json::Null)
                };
            }
        }
    }

    /// `state` of `object` (`Class/id`) without the fields `hidden` names (from
    /// [`Runtime::hidden_from`]). For a server's change streams.
    pub fn without(&mut self, object: &str, state: &Json, hidden: &[usize]) -> Json {
        let mut state = state.clone();
        if let Some(class) = object.split_once('/').and_then(|(c, _)| self.program.object(c)) {
            self.hide(class, &mut state, hidden);
        }
        state
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
            if !def.belongs_to_a_person() {
                continue;
            }
            let key = format!("{}/{subject}", def.name);
            let present = self.is_loaded(&key)
                || match self.store.load(&key) {
                    Ok(found) => found.is_some(),
                    // A record whose key was already destroyed (an erasure that could not
                    // finish, a copy from a backup) still counts, and erasing overwrites it.
                    // Any other failure to read fails the erasure.
                    Err(e) if !self.store.is_erased(&key)? => return Err(e),
                    Err(_) => true,
                };
            if present {
                held.push((class, key));
            }
        }
        Ok(held)
    }

    /// Everything held about `subject`, unmasked: the state of every object whose id is
    /// theirs, and their entries in lists of several people's (`@by`), as
    /// `"Class/id#field": [entries]`. Servers only (`tj privacy export`).
    pub fn export_subject(&mut self, subject: &str) -> Result<Json, String> {
        let mut out = Map::new();
        for (class, key) in self.subject_objects(subject)? {
            let o = self.instance(class, subject).map_err(|e| e.message())?;
            out.insert(key, self.object_state(&o)?);
        }
        for (o, field, entries) in self.entries_of(subject, None)? {
            out.insert(format!("{}#{field}", o.key()), Json::Array(entries));
        }
        for (o, field) in self.take_named(subject, false)? {
            out.insert(format!("{}#{field}", o.key()), Json::from(subject));
        }
        Ok(Json::Object(out))
    }

    /// Lists holding several people's entries (`@by`): class, field, whose field, time field.
    fn shared_lists(&self) -> Vec<SharedList> {
        let mut out = Vec::new();
        for (class, def) in self.program.objects.iter().enumerate() {
            for p in &def.pii {
                if let Some((by, at)) = &p.by {
                    out.push((class as u16, p.field.clone(), by.clone(), at.clone()));
                }
            }
        }
        out
    }

    /// Fields that name `subject` (`@read(..., "self")`), with their objects. `clear` puts
    /// each back to its default.
    fn take_named(&mut self, subject: &str, clear: bool) -> Result<Vec<Named>, String> {
        let mut found = Vec::new();
        for class in 0..self.program.objects.len() as u16 {
            let fields: Vec<Rc<str>> = self.program.objects[class as usize].guards.iter().filter(|g| g.names_a_person()).map(|g| g.field.clone()).collect();
            if fields.is_empty() {
                continue;
            }
            for id in self.class_ids(class) {
                let o = self.instance(class, &id).map_err(|e| e.message())?;
                for field in &fields {
                    let state = self.inst(&o).state.clone();
                    if !matches!(state.borrow().get(&**field), Some(Value::Str(s)) if &**s == subject) {
                        continue;
                    }
                    if clear {
                        let blank = self.defaults(class).map_err(|e| e.message())?.borrow().get(&**field).cloned().unwrap_or(Value::Null);
                        state.borrow_mut().insert(field.clone(), blank);
                        self.touch(&o);
                    }
                    found.push((o.clone(), field.clone()));
                }
            }
        }
        Ok(found)
    }

    /// Entries of `@by` lists that belong to `subject` (or, with `older`, any person's entry
    /// whose time is before it), with where they are: for export, erasure and retention.
    /// `remove` takes them out of the lists as well.
    fn entries_of(&mut self, subject: &str, older: Option<(u16, f64)>) -> Result<Vec<Entries>, String> {
        self.take_entries(subject, older, false)
    }

    fn take_entries(&mut self, subject: &str, older: Option<(u16, f64)>, remove: bool) -> Result<Vec<Entries>, String> {
        let mut found = Vec::new();
        for (class, field, by, at) in self.shared_lists() {
            if older.is_some_and(|(c, _)| c != class) {
                continue;
            }
            for id in self.class_ids(class) {
                let o = self.instance(class, &id).map_err(|e| e.message())?;
                let list = self.inst(&o).state.borrow().get(&*field).cloned();
                let Some(Value::Arr(list)) = list else { continue };
                let matches = |e: &Value| {
                    let Value::Rec(r) = e else { return false };
                    let r = r.borrow();
                    match older {
                        // A missing or broken time fails closed: the entry is erased.
                        Some((_, before)) => at.as_ref().is_some_and(|at| match r.get(&**at) {
                            Some(Value::Num(t)) if t.is_finite() => *t < before,
                            _ => true,
                        }),
                        None => matches!(r.get(&*by), Some(Value::Str(s)) if &**s == subject),
                    }
                };
                let mine: Vec<Json> = list.borrow().iter().filter(|e| matches(e)).filter_map(|e| e.to_json().ok()).collect();
                if mine.is_empty() {
                    continue;
                }
                if remove {
                    list.borrow_mut().retain(|e| !matches(e));
                    self.touch(&o);
                }
                found.push((o, field.clone(), mine));
            }
        }
        Ok(found)
    }

    /// Erases `subject`: every object whose id is theirs goes back to its defaults, and loses
    /// its alarms and the stored results of calls. The blank state is saved with a new change
    /// number, so replicas and devices receive the erasure like any other change, including
    /// those that reconnect later. Alarms the person scheduled on other objects are dropped.
    /// An object under `@legalHold` is kept until its hold ends, then erased by the sweep.
    /// Returns what was erased, what is held and until when (epoch ms), or why not (409
    /// while a call is part way through one of the objects; 503 when the store fails).
    /// Servers only (`tj privacy erase`).
    /// `awaiting`: the promise of the call doing the erasure (`tj:privacy` from a method of
    /// the person's own object), which is not counted as a call part way through.
    pub fn erase_subject(&mut self, subject: &str, awaiting: Option<u32>) -> Result<Erasure, (u16, String)> {
        let objects = self.subject_objects(subject).map_err(|e| (503, e))?;
        if let Some((_, key)) = objects.iter().find(|(class, _)| self.in_use(*class, subject, awaiting)) {
            return Err((409, format!("{key} is part way through a call; try again in a moment")));
        }
        let (mut erased, mut held, mut unreadable) = (Vec::new(), Vec::new(), Vec::new());
        for (class, key) in objects {
            // Its key is gone, so it cannot be kept, held or not: it is overwritten.
            if !self.is_loaded(&key) && self.store.is_erased(&key).map_err(|e| (503, e))? {
                unreadable.push((class, key));
                continue;
            }
            let o = self.instance(class, subject).map_err(|e| (503, e.message()))?;
            if let Some(until) = self.held_until(class, &o) {
                held.push((key, until));
                continue;
            }
            // A call still running on it (the one erasing, from the person's own method)
            // keeps no record there: its result would come back after the erasure.
            for (on, _, record) in self.call_replies.values_mut() {
                if on.class == o.class && on.id == o.id {
                    *record = false;
                }
            }
            let fresh = self.defaults(class).map_err(|e| (503, e.message()))?;
            self.reset(&o, fresh);
            erased.push(key);
        }
        // Their entries in lists of several people's go too. These lists live under the
        // store's own key, not the person's, so backups keep them until those backups expire.
        for (o, field, _) in self.take_entries(subject, None, true).map_err(|e| (503, e))? {
            erased.push(format!("{}#{field}", o.key()));
        }
        // So do the fields that name them: who booked a slot, say.
        for (o, field) in self.take_named(subject, true).map_err(|e| (503, e))? {
            erased.push(format!("{}#{field}", o.key()));
        }
        self.drop_alarms_of(subject, &held);
        // First the stored copies' keys, so copies in backups cannot be read either; then the
        // blank, under a new key.
        // Entries taken out of shared lists and fields naming them (`Class/id#field`) have no
        // key of their own.
        for key in erased.iter().filter(|k| !k.contains('#')) {
            self.store.forget(key).map_err(|e| (503, format!("could not destroy the key of {key}: {e}; nothing was erased; try again")))?;
        }
        if !self.flush() {
            return Err((503, "the old data can no longer be read, but the blank could not be saved; try again".into()));
        }
        for (class, key) in unreadable {
            self.store.forget(&key).map_err(|e| (503, format!("could not destroy the key of {key}: {e}; try again")))?;
            self.write_blank(class, &key).map_err(|e| (503, format!("could not overwrite {key}: {e}; try again")))?;
            erased.push(key);
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
        // Entries of lists of several people's whose own time ran out, checked once a round.
        if self.retain_queue.is_empty() {
            for (class, _, _, at) in self.shared_lists() {
                let Some(r) = self.program.objects[class as usize].retain.filter(|_| at.is_some()) else { continue };
                match self.take_entries("", Some((class, self.now - r.ms)), true) {
                    Ok(gone) => {
                        for (o, field, entries) in gone {
                            self.outputs.push(Output::Erased { object: format!("{}#{field}", o.key()), reason: format!("retention: {} entries", entries.len()), forget: false });
                        }
                    }
                    Err(e) => self.outputs.push(Output::Error { message: format!("retention could not check {}: {e}", self.program.objects[class as usize].name), trace: Vec::new() }),
                }
            }
            self.flush();
        }
        if self.retain_queue.is_empty() {
            // Whole objects that are one person's; lists of several people's are swept above.
            let classes: Vec<String> = self.program.objects.iter().filter(|d| d.retain.is_some() && d.belongs_to_a_person()).map(|d| format!("{}/", d.name)).collect();
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
            if self.in_use(class, id, None) {
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
            // A deleted record may keep the records of its calls, and is blank all the same.
            let blank = stored.alarms.is_empty() && Json::Object(stored.state.clone()) == defaults;
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
        self.store.forget(key)?;
        if loaded {
            let o = self.instance(class, id).map_err(|e| e.message())?;
            let fresh = self.defaults(class).map_err(|e| e.message())?;
            self.reset(&o, fresh);
        } else {
            self.write_blank(class, key)?;
        }
        let reason = if waiting.is_some() { "erase after hold" } else { "retain" };
        if waiting.is_some() {
            self.erasures.remove(key);
            self.save_erasures()?;
        }
        // The person was erased earlier; when nothing of theirs is held any more, the audit
        // log forgets them too. Retention alone does not: the person may still be active.
        let forget = waiting.is_some() && !self.erasures.keys().any(|k| k.split_once('/').is_some_and(|(_, i)| i == id));
        self.outputs.push(Output::Erased { object: key.to_string(), reason: reason.into(), forget });
        Ok(())
    }

    /// Writes a blank record for an object that is not loaded, with a new change number so
    /// devices that reconnect get it too, and no clock: it holds nothing until someone writes
    /// to it again.
    fn write_blank(&mut self, class: u16, key: &str) -> Result<(), String> {
        let Json::Object(state) = Value::Rec(self.defaults(class).map_err(|e| e.message())?.into()).to_json()? else { unreachable!("state is a record") };
        self.change_seq += 1;
        let seq = self.change_seq;
        let record = serde_json::json!({ "state": state, "seq": seq });
        self.store.save(key, &record.to_string())?;
        self.store.save("tj:seq", &seq.to_string())?;
        // Replicas hear of it as a deletion, like any erasure.
        self.outputs.push(Output::Changed { object: key.to_string(), state: Json::Null, seq });
        Ok(())
    }

    /// Whether a call is part way through the object: waiting on an effect, or holding or
    /// waiting for it as `@exclusive`.
    fn in_use(&self, class: u16, id: &str, awaiting: Option<u32>) -> bool {
        let here = |o: &Option<std::rc::Rc<crate::vm::value::ObjRef>>| o.as_ref().is_some_and(|o| o.class == class && &*o.id == id);
        let waiting = self.waiting.iter().filter(|(p, _)| Some(**p) != awaiting).any(|(_, (f, _))| f.frames.iter().any(|fr| here(&fr.this)));
        let own_chain = awaiting.and_then(|p| self.waiting.get(&p)).map(|(f, _)| f.chain);
        waiting || self.exclusive_busy(class, id, own_chain)
    }

    /// `@needs("consent:<purpose>")`: whether the person `o` belongs to has consented to
    /// `purpose` (in `Consent/<their id>`, tj:consent) and not withdrawn it.
    pub(crate) fn needs_consent(&mut self, o: &crate::vm::value::ObjRef, purpose: &str) -> Result<(), Exc> {
        let class = self.program.object("Consent").ok_or_else(|| Exc::msg("@needs(\"consent:...\") needs tj:consent"))?;
        let consent = self.instance(class, &o.id)?;
        let given = self.obj_field(None, &consent, "given")?;
        let standing = match &given {
            Value::Rec(r) => match r.borrow().get(purpose) {
                Some(Value::Rec(g)) => matches!(g.borrow().get("withdrawn"), Some(Value::Num(n)) if *n == 0.0),
                _ => false,
            },
            _ => false,
        };
        if standing {
            Ok(())
        } else {
            Err(Exc::refused(format!("this person has not consented to {purpose}, or withdrew it (tj:consent)")))
        }
    }

    /// Whether a call made by the person `id` is still running (waiting on something), so
    /// what it does next would be written to the audit log under their name.
    pub fn runs_for(&self, id: &str) -> bool {
        self.waiting.values().any(|(f, _)| matches!(&f.caller, super::access::Who::Remote { id: i, .. } if &**i == id))
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
