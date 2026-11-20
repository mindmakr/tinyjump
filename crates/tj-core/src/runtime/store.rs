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
    /// Deletes a record.
    fn remove(&mut self, _key: &str) -> Result<(), String> {
        Err("this store cannot delete records".into())
    }
    /// Makes what was stored for `key` unreadable, copies in backups included, where the
    /// store can: an encrypted store destroys the record's own key. Others do nothing, and
    /// erasing then blanks the live record only.
    fn forget(&mut self, _key: &str) -> Result<(), String> {
        Ok(())
    }
    /// Whether the record of `key` cannot be read because its key was destroyed (`forget`):
    /// an erasure that could not finish, or a copy restored from a backup. Any other failure
    /// to read is an error, never this.
    fn is_erased(&mut self, _key: &str) -> Result<bool, String> {
        Ok(false)
    }
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

    fn remove(&mut self, key: &str) -> Result<(), String> {
        self.data.remove(key);
        Ok(())
    }
}
