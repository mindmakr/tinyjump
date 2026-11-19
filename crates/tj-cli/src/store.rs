//! One JSON file per key, written atomically (temp file + rename).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use tj_core::Store;

pub struct FileStore {
    dir: PathBuf,
}

impl FileStore {
    pub fn open(dir: &Path) -> io::Result<FileStore> {
        fs::create_dir_all(dir)?;
        Ok(FileStore { dir: dir.to_path_buf() })
    }

    fn path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{}.json", encode(key)))
    }
}

/// Percent-encodes everything but `[A-Za-z0-9._-]`, so keys can never escape the directory.
fn encode(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    for b in key.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn decode(name: &str) -> Option<String> {
    let b = name.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

impl Store for FileStore {
    fn load(&mut self, key: &str) -> Option<String> {
        fs::read_to_string(self.path(key)).ok()
    }

    fn save(&mut self, key: &str, value: &str) -> Result<(), String> {
        let path = self.path(key);
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, value).and_then(|()| fs::rename(&tmp, &path)).map_err(|e| format!("{}: {e}", path.display()))
    }

    fn keys(&mut self, prefix: &str) -> Vec<String> {
        let Ok(entries) = fs::read_dir(&self.dir) else { return Vec::new() };
        let mut keys: Vec<String> = entries
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter_map(|n| decode(n.strip_suffix(".json")?))
            .filter(|k| k.starts_with(prefix))
            .collect();
        keys.sort();
        keys
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_lists_keys() {
        let dir = std::env::temp_dir().join(format!("tj-store-test-{}", std::process::id()));
        let mut s = FileStore::open(&dir).unwrap();
        s.save("Counter/main", "{\"a\":1}").unwrap();
        s.save("Counter/../x", "2").unwrap();
        s.save("Other/z", "3").unwrap();
        assert_eq!(s.load("Counter/main").as_deref(), Some("{\"a\":1}"));
        assert_eq!(s.keys("Counter/"), vec!["Counter/../x".to_string(), "Counter/main".to_string()]);
        assert!(dir.join("Counter%2F..%2Fx.json").exists(), "keys never become paths");
        fs::remove_dir_all(dir).unwrap();
    }
}
