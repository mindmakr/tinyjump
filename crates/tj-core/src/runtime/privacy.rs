//! Personal data (docs/privacy.md): `@pii` fields are masked wherever tools and other
//! readers look, and a person's data can be exported and erased.
//!
//! An object with personal data belongs to one person: the one whose id is the object id.
//! Everything held about a person is therefore every object whose id is theirs.

use super::Runtime;
use crate::runtime::protocol::Output;
use serde_json::{Map, Value as Json};

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

    /// Every object held about `subject`: loaded, or in the store.
    fn subject_objects(&mut self, subject: &str) -> Result<Vec<(u16, String)>, String> {
        let mut held = Vec::new();
        for class in 0..self.program.objects.len() as u16 {
            let key = format!("{}/{subject}", self.program.objects[class as usize].name);
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
    /// those that reconnect later. The objects erased, or why not (409 while a call is part
    /// way through one of them; 503 when the store fails). Servers only (`tj privacy erase`).
    pub fn erase_subject(&mut self, subject: &str) -> Result<(Vec<String>, Vec<Output>), (u16, String)> {
        let held = self.subject_objects(subject).map_err(|e| (503, e))?;
        if let Some((_, key)) = held.iter().find(|(class, _)| self.in_use(*class, subject)) {
            return Err((409, format!("{key} is part way through a call; try again in a moment")));
        }
        let mut alarms = Vec::new();
        for (class, _) in &held {
            let o = self.instance(*class, subject).map_err(|e| (503, e.message()))?;
            let fresh = self.defaults(*class).map_err(|e| (503, e.message()))?;
            alarms.push((o.clone(), self.reset(&o, fresh)));
        }
        if !self.flush() {
            // The failed save put state and call records back; the alarms go back too.
            for (o, a) in alarms {
                self.put_alarms(&o, a);
            }
            return Err((503, "the erasure could not be saved; nothing was erased; try again".into()));
        }
        Ok((held.into_iter().map(|(_, key)| key).collect(), std::mem::take(&mut self.outputs)))
    }

    /// Whether a call is part way through the object: waiting on an effect, or holding or
    /// waiting for it as `@exclusive`.
    fn in_use(&self, class: u16, id: &str) -> bool {
        let here = |o: &Option<std::rc::Rc<crate::vm::value::ObjRef>>| o.as_ref().is_some_and(|o| o.class == class && &*o.id == id);
        self.waiting.values().any(|(f, _)| f.frames.iter().any(|fr| here(&fr.this))) || self.exclusive_busy(class, id)
    }
}
