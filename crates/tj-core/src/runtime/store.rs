//! Durable storage interface. Hosts back it with files, localStorage, SQLite, etc.

use std::collections::BTreeMap;

/// A synchronous key-value store of JSON strings. Keys look like `Class/id`.
pub trait Store {
    fn load(&mut self, key: &str) -> Option<String>;
    fn save(&mut self, key: &str, value: &str) -> Result<(), String>;
    /// All keys starting with `prefix`.
    fn keys(&mut self, prefix: &str) -> Vec<String>;
}

/// In-memory store, for tests and ephemeral runs.
#[derive(Default, Debug, Clone)]
pub struct MemStore {
    pub data: BTreeMap<String, String>,
}

impl Store for MemStore {
    fn load(&mut self, key: &str) -> Option<String> {
        self.data.get(key).cloned()
    }

    fn save(&mut self, key: &str, value: &str) -> Result<(), String> {
        self.data.insert(key.to_string(), value.to_string());
        Ok(())
    }

    fn keys(&mut self, prefix: &str) -> Vec<String> {
        self.data.range(prefix.to_string()..).take_while(|(k, _)| k.starts_with(prefix)).map(|(k, _)| k.clone()).collect()
    }
}
