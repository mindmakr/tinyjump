//! Runtime statistics: per method, how often it ran, failed and was refused, and how many VM
//! steps it took (everything it ran, nested calls included); per class, how many objects are
//! loaded. Counted, not timed, so they are the same on every run and can be tested and
//! budgeted. Servers add wall-clock latency (tj-cli `stats.rs`).

use super::Runtime;
use serde_json::{json, Map, Value as Json};

#[derive(Default, Clone, Copy)]
pub(crate) struct MethodStats {
    calls: u64,
    errors: u64,
    refused: u64,
    steps: u64,
}

/// One entry per method of each class, by class and method index. Requests naming unknown
/// methods are never counted, so they cannot grow it.
pub(crate) type Table = Vec<Vec<MethodStats>>;

pub(crate) fn table(program: &crate::vm::program::Program) -> Table {
    program.objects.iter().map(|o| vec![MethodStats::default(); o.methods.len()]).collect()
}

impl Runtime {
    /// The class and method index of `object`'s class (`Class/id`) and `method`, if both exist.
    pub(crate) fn method_slot(&self, object: &str, method: &str) -> Option<(u16, u16)> {
        let class = object.split('/').next()?;
        let c = self.program.objects.iter().position(|o| &*o.name == class)?;
        let m = self.program.objects[c].methods.get_index_of(method)?;
        Some((c as u16, m as u16))
    }

    /// `Class.method`, the name statistics use, if the method exists. Hosts key their own
    /// measurements (latency) by it, so unknown names from requests are never recorded.
    pub fn method_name(&self, object: &str, method: &str) -> Option<String> {
        self.method_slot(object, method).map(|(c, m)| self.slot_name(c, m))
    }

    fn slot_name(&self, c: u16, m: u16) -> String {
        let def = &self.program.objects[c as usize];
        let (name, _) = def.methods.get_index(m as usize).expect("slots come from the program");
        format!("{}.{name}", def.name)
    }

    pub(crate) fn count_call(&mut self, slot: (u16, u16)) {
        self.stats[slot.0 as usize][slot.1 as usize].calls += 1;
    }

    pub(crate) fn count_error(&mut self, slot: Option<(u16, u16)>) {
        if let Some((c, m)) = slot {
            self.stats[c as usize][m as usize].errors += 1;
        }
    }

    pub(crate) fn count_steps(&mut self, slot: Option<(u16, u16)>, steps: u64) {
        if let Some((c, m)) = slot {
            self.stats[c as usize][m as usize].steps += steps;
        }
    }

    pub(crate) fn count_refused(&mut self, object: &str, method: &str) {
        if let Some((c, m)) = self.method_slot(object, method) {
            self.stats[c as usize][m as usize].refused += 1;
        }
    }

    /// `{ methods: { "Cart.add": { calls, errors, refused, steps } }, objects: { "Cart": 2 } }`,
    /// leaving out methods that never ran and classes with nothing loaded.
    pub fn stats(&self) -> Json {
        let mut methods = Map::new();
        for (c, row) in self.stats.iter().enumerate() {
            for (m, s) in row.iter().enumerate() {
                if s.calls + s.refused > 0 {
                    methods.insert(self.slot_name(c as u16, m as u16), json!({ "calls": s.calls, "errors": s.errors, "refused": s.refused, "steps": s.steps }));
                }
            }
        }
        let mut loaded = vec![0u64; self.program.objects.len()];
        for (class, _) in self.instances.keys() {
            loaded[*class as usize] += 1;
        }
        let objects: Map<String, Json> = self.program.objects.iter().zip(loaded).filter(|(_, n)| *n > 0).map(|(o, n)| (o.name.to_string(), json!(n))).collect();
        json!({ "methods": methods, "objects": objects })
    }
}
