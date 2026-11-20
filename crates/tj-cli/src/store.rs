//! One JSON file per key, written atomically and durably: a temporary file is synced to disk,
//! renamed over the old one, and the folder is synced, so a crash leaves the old record or
//! the new one, never a torn or lost write. Only the server's user can read the files.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use tj_core::Store;

pub struct FileStore {
    dir: PathBuf,
}

impl FileStore {
    pub fn open(dir: &Path) -> io::Result<FileStore> {
        private_dir(dir)?;
        Ok(FileStore { dir: dir.to_path_buf() })
    }

    fn path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{}.json", encode(key)))
    }

    /// Deletes a record, durably (`tj store rekey` removes records it has re-saved sealed).
    pub fn remove(&mut self, key: &str) -> Result<(), String> {
        let path = self.path(key);
        fs::remove_file(&path).and_then(|()| sync_dir(&self.dir)).map_err(|e| format!("{}: {e}", path.display()))
    }
}

pub fn private_dir(dir: &Path) -> io::Result<()> {
    let mut b = fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
    b.create(dir)
}

/// Options for a file only the server's user can read.
pub fn private_file() -> OpenOptions {
    #[allow(unused_mut)] // only Unix sets a mode
    let mut o = OpenOptions::new();
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    o
}

/// A new or removed directory entry must reach the disk too. Windows cannot sync a folder;
/// its renames and deletes complete in one step.
pub fn sync_dir(_dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    fs::File::open(_dir)?.sync_all()?;
    Ok(())
}

/// Percent-encodes everything but `[A-Za-z0-9._-]`, so keys can never escape the directory.
fn encode(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    for b in key.bytes() {
        // Upper-case letters are encoded too: keys differing only in case must stay apart on
        // case-insensitive filesystems (Windows, macOS), or the `owner` rule could be bypassed.
        if b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.') {
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
    fn load(&mut self, key: &str) -> Result<Option<String>, String> {
        let path = self.path(key);
        match fs::read_to_string(&path) {
            Ok(text) => Ok(Some(text)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    fn save(&mut self, key: &str, value: &str) -> Result<(), String> {
        let path = self.path(key);
        let tmp = path.with_extension("json.tmp");
        let write = || -> io::Result<()> {
            let mut f = private_file().write(true).create(true).truncate(true).open(&tmp)?;
            f.write_all(value.as_bytes())?;
            f.sync_all()?;
            fs::rename(&tmp, &path)?;
            sync_dir(&self.dir)
        };
        write().map_err(|e| format!("{}: {e}", path.display()))
    }

    fn keys(&mut self, prefix: &str) -> Result<Vec<String>, String> {
        let entries = fs::read_dir(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))?;
        let mut keys: Vec<String> = entries
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter_map(|n| decode(n.strip_suffix(".json")?))
            .filter(|k| k.starts_with(prefix))
            .collect();
        keys.sort();
        Ok(keys)
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
        assert_eq!(s.load("Counter/main").unwrap().as_deref(), Some("{\"a\":1}"));
        assert_eq!(s.load("Counter/none"), Ok(None));
        assert_eq!(s.keys("Counter/").unwrap(), ["Counter/../x", "Counter/main"]);
        assert!(dir.join("%43ounter%2F..%2Fx.json").exists(), "keys never become paths");
        // Keys that differ only in case stay apart, even where the filesystem ignores case.
        s.save("Cart/Alice", "\"A\"").unwrap();
        s.save("Cart/alice", "\"a\"").unwrap();
        assert_eq!((s.load("Cart/Alice").unwrap(), s.load("Cart/alice").unwrap()), (Some("\"A\"".into()), Some("\"a\"".into())));
        fs::remove_dir_all(dir).unwrap();
    }
}
