//! Crash-safe, owner-only atomic file store implementing `tj_core::Store`.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use tj_core::Store;

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

pub struct FileStore {
    dir: PathBuf,
}

impl FileStore {
    pub fn open(dir: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(&dir).map_err(|e| format!("cannot create data dir {}: {e}", dir.display()))?;
        #[cfg(unix)]
        {
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
                .map_err(|e| format!("cannot chmod 0700 {}: {e}", dir.display()))?;
        }
        Ok(Self { dir })
    }

    /// Percent-encodes everything except `[a-z0-9._-]` so keys never escape `dir`.
    fn encode(key: &str) -> String {
        let mut out = String::with_capacity(key.len());
        for b in key.bytes() {
            let safe = matches!(b, b'0'..=b'9' | b'a'..=b'z' | b'-' | b'_' | b'.');
            if safe {
                out.push(b as char);
            } else {
                out.push_str(&format!("%{b:02X}"));
            }
        }
        out
    }

    fn decode(encoded: &str) -> Option<String> {
        let bytes = encoded.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 2 < bytes.len() {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
                let val = u8::from_str_radix(hex, 16).ok()?;
                out.push(val);
                i += 3;
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        String::from_utf8(out).ok()
    }

    fn path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{}.json", Self::encode(key)))
    }

    fn sync_dir(&self) -> Result<(), String> {
        if let Ok(d) = File::open(&self.dir) {
            d.sync_all().map_err(|e| format!("sync dir {}: {e}", self.dir.display()))?;
        }
        Ok(())
    }
}

impl Store for FileStore {
    fn load(&mut self, key: &str) -> Result<Option<String>, String> {
        let p = self.path(key);
        match fs::read_to_string(&p) {
            Ok(s) => Ok(Some(s)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("read {}: {e}", p.display())),
        }
    }

    fn save(&mut self, key: &str, value: &str) -> Result<(), String> {
        let p = self.path(key);
        let tmp = self.dir.join(format!(".{}.tmp.{}", Self::encode(key), std::process::id()));
        let mut opts = OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        opts.mode(0o600);
        let write_res = (|| -> Result<(), String> {
            let mut f = opts.open(&tmp).map_err(|e| format!("create {}: {e}", tmp.display()))?;
            f.write_all(value.as_bytes()).map_err(|e| format!("write {}: {e}", tmp.display()))?;
            f.sync_all().map_err(|e| format!("sync {}: {e}", tmp.display()))?;
            Ok(())
        })();
        if let Err(e) = write_res {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
        if let Err(e) = fs::rename(&tmp, &p) {
            let _ = fs::remove_file(&tmp);
            return Err(format!("rename {}: {e}", p.display()));
        }
        self.sync_dir()
    }

    fn keys(&mut self, prefix: &str) -> Result<Vec<String>, String> {
        let mut out = Vec::new();
        let entries = fs::read_dir(&self.dir).map_err(|e| format!("readdir {}: {e}", self.dir.display()))?;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(s) = name.to_str() else { continue };
            if let Some(stem) = s.strip_suffix(".json") {
                if let Some(decoded) = Self::decode(stem) {
                    if decoded.starts_with(prefix) {
                        out.push(decoded);
                    }
                }
            }
        }
        out.sort();
        Ok(out)
    }

    fn remove(&mut self, key: &str) -> Result<(), String> {
        let p = self.path(key);
        match fs::remove_file(&p) {
            Ok(()) => self.sync_dir(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("remove {}: {e}", p.display())),
        }
    }
}
