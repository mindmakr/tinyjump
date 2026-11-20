//! Durable storage interface. Hosts back it with files, localStorage, SQLite, etc.

use std::collections::BTreeMap;

/// A synchronous key-value store of JSON strings. Keys look like `Class/id`.
///
/// A record that exists but cannot be read is an error, never `None`: an object that
/// started empty would overwrite it on its next save.
pub trait Store {
    fn load(&mut self, key: &str) -> Result<Option<String>, String>;
    fn save(&mut self, key: &str, value: &str) -> Result<(), String>;
    /// All keys starting with `prefix`.
    fn keys(&mut self, prefix: &str) -> Result<Vec<String>, String>;
}

/// In-memory store, for tests and ephemeral runs.
#[derive(Default, Debug, Clone)]
pub struct MemStore {
    pub data: BTreeMap<String, String>,
}

impl Store for MemStore {
    fn load(&mut self, key: &str) -> Result<Option<String>, String> {
        Ok(self.data.get(key).cloned())
    }

    fn save(&mut self, key: &str, value: &str) -> Result<(), String> {
        self.data.insert(key.to_string(), value.to_string());
        Ok(())
    }

    fn keys(&mut self, prefix: &str) -> Result<Vec<String>, String> {
        Ok(self.data.range(prefix.to_string()..).take_while(|(k, _)| k.starts_with(prefix)).map(|(k, _)| k.clone()).collect())
    }
}
