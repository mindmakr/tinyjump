//! Encryption at rest for servers (docs/trust.md, section 5). It wraps the store on the host,
//! so the core stays free of cryptography.
//!
//! - Every save seals the record with AES-256-GCM under a new random data key. The data key
//!   is wrapped, also with AES-256-GCM, under a one-time key derived from the key-encryption
//!   key and a random salt. No key ever sees a second nonce, however many saves there are.
//!   Moving to a new key-encryption key re-wraps data keys and leaves records as they are
//!   (`tj store rekey`).
//! - Stored names are keyed hashes, `<class>.<key>`, so they reveal nothing, and the objects
//!   of one class can still be listed. The real key is inside the sealed record.
//! - The authenticated data binds the format and the stored name, so a record cannot be
//!   moved to another object. Nothing here can stop an older copy of the same record being
//!   put back; that needs a counter kept off this disk.
//! - The key file (`TJ_KEY_FILE`, readable by the server only) holds a `names` secret and
//!   one or more key-encryption keys, `<id> <64 hex>`, newest last.
//! - **Objects that belong to a person** (their class has `@pii` or an `"owner"` rule) get a
//!   key of their own: the record's data key is wrapped under it, and it is wrapped under the
//!   key-encryption key and kept **apart from the data**, in `<TJ_KEY_FILE>.own/`. Erasing
//!   the person destroys it (`Store::forget`), so copies of the record in backups of the data
//!   can no longer be opened. That holds as long as backups of the own-keys folder are kept
//!   no longer than the retention promised; losing that folder loses those objects.
//!   Rotation re-wraps these keys, never the records under them.

use crate::bytes::{base64, hex, random, unbase64, unhex};
use crate::store::{private_dir, private_file, FileStore};
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use ring::hmac;
use serde_json::{json, Value as Json};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use tj_core::Store;

pub const KEY_FILE_ENV: &str = "TJ_KEY_FILE";
const FORMAT: &str = "tj-sealed-1";

/// The key file's secrets.
pub struct Keyring {
    names: hmac::Key,
    /// Key-encryption keys by id, newest last.
    keks: Vec<(String, [u8; 32])>,
}

impl Keyring {
    /// Reads the key file; a missing one is made with new random keys. Whether it was made.
    pub fn load(path: &Path) -> Result<(Keyring, bool), String> {
        let (text, created) = match fs::read_to_string(path) {
            Ok(text) => (text, false),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let text = format!("names {}\nk1 {}\n", hex(&random::<32>()?), hex(&random::<32>()?));
                let write = || -> io::Result<()> {
                    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
                        private_dir(dir)?;
                    }
                    let mut f = private_file().write(true).create_new(true).open(path)?;
                    f.write_all(text.as_bytes())?;
                    f.sync_all()
                };
                write().map_err(|e| format!("cannot create the key file {}: {e}", path.display()))?;
                (text, true)
            }
            Err(e) => return Err(format!("cannot read the key file {}: {e}", path.display())),
        };
        Keyring::parse(&text).map(|k| (k, created)).map_err(|e| format!("{}: {e}", path.display()))
    }

    fn parse(text: &str) -> Result<Keyring, String> {
        let (mut names, mut keks) = (None, Vec::new());
        for (i, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty() && !l.trim_start().starts_with('#')) {
            let bad = || format!("line {}: write `<id> <64 hex characters>`, e.g. `k2 $(openssl rand -hex 32)`", i + 1);
            let (id, secret) = line.trim().split_once(' ').ok_or_else(bad)?;
            let secret: [u8; 32] = unhex(secret.trim()).and_then(|s| s.try_into().ok()).ok_or_else(bad)?;
            if id == "names" {
                names = Some(hmac::Key::new(hmac::HMAC_SHA256, &secret));
            } else if !id.is_empty() && id.len() <= 32 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') && !keks.iter().any(|(k, _)| k == id) {
                keks.push((id.to_string(), secret));
            } else {
                return Err(format!("line {}: key ids are letters, digits and `-`, each used once", i + 1));
            }
        }
        let names = names.ok_or("no `names` line: it keys the stored names, and must never change")?;
        if keks.is_empty() {
            return Err("no key-encryption key: add a line `k1 <64 hex characters>`".into());
        }
        Ok(Keyring { names, keks })
    }

    fn current(&self) -> &(String, [u8; 32]) {
        self.keks.last().expect("parse requires one")
    }

    /// The stored name of `key`: hashes of its class and of the whole key.
    fn name(&self, key: &str) -> String {
        let class = key.split_once('/').map_or(key, |(c, _)| c);
        format!("{}.{}", self.class_name(class), hex(&hmac::sign(&self.names, format!("key:{key}").as_bytes()).as_ref()[..16]))
    }

    fn class_name(&self, class: &str) -> String {
        hex(&hmac::sign(&self.names, format!("class:{class}").as_bytes()).as_ref()[..8])
    }

    /// A one-time key for wrapping one data key.
    fn wrapping_key(kek: &[u8; 32], salt: &[u8]) -> hmac::Tag {
        hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, kek), salt)
    }

    fn wrap(&self, name: &str, data_key: &[u8]) -> Result<Json, String> {
        let (id, kek) = self.current();
        let salt = random::<32>()?;
        let wrapped = seal(Self::wrapping_key(kek, &salt).as_ref(), &aad(&[id, name]), data_key);
        Ok(json!({ "kid": id, "salt": base64(&salt), "key": base64(&wrapped) }))
    }

    fn unwrap(&self, name: &str, env: &Json) -> Option<Vec<u8>> {
        let id = env["kid"].as_str()?;
        let (_, kek) = self.keks.iter().find(|(k, _)| k == id)?;
        let salt = unbase64(env["salt"].as_str()?)?;
        open(Self::wrapping_key(kek, &salt).as_ref(), &aad(&[id, name]), unbase64(env["key"].as_str()?)?)
    }

    /// The sealed record for `value`, stored under `key`: its data key wrapped under the
    /// key-encryption key, or under `own`, the object's own key.
    fn seal_record(&self, key: &str, value: &str, own: Option<(&str, &[u8; 32])>) -> Result<String, String> {
        let name = self.name(key);
        let data_key = random::<32>()?;
        let mut plain = Vec::with_capacity(4 + key.len() + value.len());
        plain.extend_from_slice(&(key.len() as u32).to_le_bytes());
        plain.extend_from_slice(key.as_bytes());
        plain.extend_from_slice(value.as_bytes());
        let mut env = match own {
            Some((own_name, own)) => {
                let salt = random::<32>()?;
                let wrapped = seal(Self::wrapping_key(own, &salt).as_ref(), &aad(&["own", &name]), &data_key);
                json!({ "own": own_name, "salt": base64(&salt), "key": base64(&wrapped) })
            }
            None => self.wrap(&name, &data_key)?,
        };
        env["f"] = FORMAT.into();
        env["data"] = base64(&seal(&data_key, &aad(&[&name]), &plain)).into();
        Ok(env.to_string())
    }

    /// The key and value inside the sealed record stored as `name`; `own` is the object's own
    /// key when the record is sealed under one.
    fn open_record(&self, name: &str, text: &str, own: Option<&[u8; 32]>) -> Result<(String, String), String> {
        let fail = || format!("cannot decrypt the record stored as {name}: it was changed, moved from another name, or sealed with a key not in the key file");
        let env: Json = serde_json::from_str(text).map_err(|_| fail())?;
        if env["f"] != FORMAT {
            return Err(fail());
        }
        let data_key = if env["own"].is_string() {
            let own = own.ok_or_else(|| format!("the record stored as {name} belonged to a person who was erased: its key was destroyed, so it cannot be read (a copy from a backup?); delete it"))?;
            let salt = unbase64(env["salt"].as_str().ok_or_else(fail)?).ok_or_else(fail)?;
            open(Self::wrapping_key(own, &salt).as_ref(), &aad(&["own", name]), unbase64(env["key"].as_str().ok_or_else(fail)?).ok_or_else(fail)?).ok_or_else(fail)?
        } else {
            self.unwrap(name, &env).ok_or_else(fail)?
        };
        let plain = open(&data_key, &aad(&[name]), unbase64(env["data"].as_str().ok_or_else(fail)?).ok_or_else(fail)?).ok_or_else(fail)?;
        let len = u32::from_le_bytes(plain.get(..4).and_then(|b| b.try_into().ok()).ok_or_else(fail)?) as usize;
        let key = plain.get(4..4 + len).and_then(|k| std::str::from_utf8(k).ok()).ok_or_else(fail)?;
        let value = std::str::from_utf8(&plain[4 + len..]).map_err(|_| fail())?;
        Ok((key.to_string(), value.to_string()))
    }

    /// The same record (or an object's own key) wrapped under the newest key-encryption key;
    /// `None` if it already is, or if it is under an object's own key, which is re-wrapped
    /// instead.
    fn rewrap(&self, name: &str, text: &str) -> Result<Option<String>, String> {
        let mut env: Json = serde_json::from_str(text).map_err(|e| e.to_string())?;
        if env["own"].is_string() {
            return Ok(None);
        }
        if env.get("data").is_some() {
            self.open_record(name, text, None)?;
        } else if self.unwrap(name, &env).is_none() {
            return Err(format!("cannot open the key stored as {name}: it is not sealed with a key in the key file"));
        }
        if env["kid"] == self.current().0.as_str() {
            return Ok(None);
        }
        let inner = self.unwrap(name, &env).ok_or("cannot unwrap")?;
        let fresh = self.wrap(name, &inner)?;
        for k in ["kid", "salt", "key"] {
            env[k] = fresh[k].clone();
        }
        Ok(Some(env.to_string()))
    }
}

/// Authenticated data: the format and the given parts, each ended by a zero byte.
fn aad(parts: &[&str]) -> Vec<u8> {
    let mut out = Vec::new();
    for p in std::iter::once(&FORMAT).chain(parts) {
        out.extend_from_slice(p.as_bytes());
        out.push(0);
    }
    out
}

/// AES-256-GCM under a key used for this one message, so a fixed nonce is safe.
fn seal(key: &[u8], aad: &[u8], plain: &[u8]) -> Vec<u8> {
    let key = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, key).expect("a 32-byte key"));
    let mut buf = plain.to_vec();
    key.seal_in_place_append_tag(Nonce::assume_unique_for_key([0; 12]), Aad::from(aad), &mut buf).expect("the record fits");
    buf
}

fn open(key: &[u8], aad: &[u8], mut sealed: Vec<u8>) -> Option<Vec<u8>> {
    let key = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, key).ok()?);
    key.open_in_place(Nonce::assume_unique_for_key([0; 12]), Aad::from(aad), &mut sealed).ok().map(|p| p.to_vec())
}

/// A store whose records are sealed on the way in and opened on the way out.
pub struct Sealed<S: Store> {
    inner: S,
    /// Objects' own keys, kept apart from the data so data backups do not hold them.
    own_store: Box<dyn Store>,
    keys: Keyring,
    /// Classes whose objects belong to a person, and so get keys of their own.
    personal: HashSet<String>,
    /// Objects' own keys already read or made, by their stored names: generation and key.
    own: HashMap<String, (String, [u8; 32])>,
}

impl<S: Store> Sealed<S> {
    pub fn new(inner: S, own_store: Box<dyn Store>, keys: Keyring, personal: &[String]) -> Sealed<S> {
        Sealed { inner, own_store, keys, personal: personal.iter().cloned().collect(), own: HashMap::new() }
    }

    fn is_personal(&self, key: &str) -> bool {
        key.split_once('/').is_some_and(|(class, _)| self.personal.contains(class))
    }

    /// Where an object's own key is stored (a name like any record's).
    fn own_name(&self, key: &str) -> String {
        self.keys.name(&format!("tj:own/{key}"))
    }

    /// The own key kept at `slot`, with its generation; made when `create` and there is none.
    /// Each key has a random generation: a record names the generation it was sealed under,
    /// so a copy from before an erasure is recognised as erased even after a new key has been
    /// made for the same object.
    fn own_key(&mut self, slot: &str, create: bool) -> Result<Option<(String, [u8; 32])>, String> {
        if let Some(k) = self.own.get(slot) {
            return Ok(Some(k.clone()));
        }
        let k = match self.own_store.load(slot)? {
            Some(text) => {
                let env: Json = serde_json::from_str(&text).map_err(|e| e.to_string())?;
                let key = self.keys.unwrap(slot, &env).and_then(|k| k.try_into().ok()).ok_or_else(|| format!("cannot open the key stored as {slot}: it is not sealed with a key in the key file"))?;
                Some((env["gen"].as_str().unwrap_or_default().to_string(), key))
            }
            None if create => {
                let (key, gen) = (random::<32>()?, hex(&random::<8>()?));
                let mut env = self.keys.wrap(slot, &key)?;
                env["f"] = FORMAT.into();
                env["gen"] = gen.clone().into();
                self.own_store.save(slot, &env.to_string())?;
                Some((gen, key))
            }
            None => None,
        };
        if let Some(k) = &k {
            self.own.insert(slot.to_string(), k.clone());
        }
        Ok(k)
    }

    /// The key a record sealed under `own` (`slot#generation`) needs, if it still exists.
    fn own_for(&mut self, own: &str) -> Result<Option<[u8; 32]>, String> {
        let (slot, gen) = own.split_once('#').unwrap_or((own, ""));
        Ok(self.own_key(slot, false)?.filter(|(g, _)| g == gen).map(|(_, k)| k))
    }

    /// Opens the record stored as `name`, with its own key when it has one.
    fn open(&mut self, name: &str, text: &str) -> Result<(String, String), String> {
        let own_ref = serde_json::from_str::<Json>(text).ok().and_then(|e| e["own"].as_str().map(str::to_string));
        let own = match own_ref {
            Some(o) => self.own_for(&o)?,
            None => None,
        };
        self.keys.open_record(name, text, own.as_ref())
    }
}

impl<S: Store> Store for Sealed<S> {
    fn load(&mut self, key: &str) -> Result<Option<String>, String> {
        let name = self.keys.name(key);
        let Some(text) = self.inner.load(&name)? else { return Ok(None) };
        let (inside, value) = self.open(&name, &text)?;
        if inside != key {
            return Err(format!("the record stored as {name} belongs to another key"));
        }
        Ok(Some(value))
    }

    fn save(&mut self, key: &str, value: &str) -> Result<(), String> {
        let slot = self.own_name(key);
        let own = if self.is_personal(key) { self.own_key(&slot, true)?.map(|(gen, k)| (format!("{slot}#{gen}"), k)) } else { None };
        let text = self.keys.seal_record(key, value, own.as_ref().map(|(r, k)| (r.as_str(), k)))?;
        self.inner.save(&self.keys.name(key), &text)
    }

    fn remove(&mut self, key: &str) -> Result<(), String> {
        self.inner.remove(&self.keys.name(key))
    }

    /// Destroys the object's own key: what was stored for it, copies included, can no longer
    /// be opened. The next save makes a new key.
    fn forget(&mut self, key: &str) -> Result<(), String> {
        if !self.is_personal(key) {
            return Ok(());
        }
        let name = self.own_name(key);
        self.own.remove(&name);
        match self.own_store.load(&name)? {
            Some(_) => self.own_store.remove(&name),
            None => Ok(()),
        }
    }

    fn is_erased(&mut self, key: &str) -> Result<bool, String> {
        let Some(text) = self.inner.load(&self.keys.name(key))? else { return Ok(false) };
        match serde_json::from_str::<Json>(&text).ok().and_then(|e| e["own"].as_str().map(str::to_string)) {
            Some(own) => Ok(self.own_for(&own)?.is_none()),
            None => Ok(false),
        }
    }

    /// Lists by class: `prefix` must start with `Class/`, as the runtime's always do.
    fn keys(&mut self, prefix: &str) -> Result<Vec<String>, String> {
        let (class, _) = prefix.split_once('/').ok_or("a sealed store lists keys by class: the prefix must be `Class/...`")?;
        let mut keys = Vec::new();
        for name in self.inner.keys(&format!("{}.", self.keys.class_name(class)))? {
            let Some(text) = self.inner.load(&name)? else { continue };
            // The real key is inside. A record whose own key was destroyed (an erased person's,
            // copied back from a backup) cannot be opened, so it is not listed.
            if let Some(own) = serde_json::from_str::<Json>(&text).ok().and_then(|e| e["own"].as_str().map(str::to_string)) {
                if self.own_for(&own)?.is_none() {
                    continue;
                }
            }
            let (key, _) = self.open(&name, &text)?;
            if key.starts_with(prefix) {
                keys.push(key);
            }
        }
        keys.sort();
        Ok(keys)
    }
}

/// Whether a stored name is a sealed one (`<16 hex>.<32 hex>`). Plain keys always hold a `/`
/// or a `:`.
fn is_sealed_name(name: &str) -> bool {
    name.len() == 49 && name.as_bytes()[16] == b'.' && name.bytes().enumerate().all(|(i, b)| i == 16 || b.is_ascii_hexdigit())
}

/// The store for the data in `data`: sealed when `TJ_KEY_FILE` is set, with keys of their
/// own for objects of the `personal` classes. Refuses a folder that mixes the two, so a
/// missing setting never writes plain records next to sealed ones. Also returns a line for
/// the startup banner.
pub fn open_store(data: &Path, personal: &[String]) -> Result<(Box<dyn Store>, String), String> {
    let mut files = FileStore::open(data).map_err(|e| format!("cannot open data dir {}: {e}", data.display()))?;
    let names = files.keys("")?;
    let key_file = std::env::var_os(KEY_FILE_ENV).filter(|v| !v.is_empty());
    match key_file {
        Some(path) => {
            if names.iter().any(|n| !is_sealed_name(n)) {
                return Err(format!("{} holds records that are not encrypted; with the server stopped, run `tj store rekey --data {}` to encrypt them", data.display(), data.display()));
            }
            let path = Path::new(&path);
            let (keys, created) = Keyring::load(path)?;
            let own = own_keys_dir(path);
            let own_store = FileStore::open(&own).map_err(|e| format!("cannot open {}: {e}", own.display()))?;
            let mut about = format!("encrypted (key file {}; objects' own keys in {}", path.display(), own.display());
            if created {
                about += "; new: back the key file up, as without it the data cannot be read";
            }
            Ok((Box::new(Sealed::new(files, Box::new(own_store), keys, personal)), about + ")"))
        }
        None => {
            if names.iter().any(|n| is_sealed_name(n)) {
                return Err(format!("{} is encrypted; set {KEY_FILE_ENV} to its key file", data.display()));
            }
            Ok((Box::new(files), format!("not encrypted (set {KEY_FILE_ENV} to encrypt)")))
        }
    }
}

/// Where objects' own keys are kept: beside the key file, apart from the data.
fn own_keys_dir(key_file: &Path) -> std::path::PathBuf {
    let mut dir = key_file.as_os_str().to_owned();
    dir.push(".own");
    dir.into()
}

/// `tj store rekey`: encrypts plain records and re-wraps sealed ones, and objects' own keys,
/// under the newest key, so older keys can then be removed from the key file. Run it with
/// the server stopped.
pub fn rekey(data: &Path) -> Result<String, String> {
    let path = std::env::var_os(KEY_FILE_ENV).filter(|v| !v.is_empty()).ok_or(format!("set {KEY_FILE_ENV} to the key file (it is made if missing)"))?;
    let (keys, created) = Keyring::load(Path::new(&path))?;
    let mut files = FileStore::open(data).map_err(|e| format!("cannot open data dir {}: {e}", data.display()))?;
    let (sealed, mut rewrapped) = rekey_with(&keys, &mut files)?;
    let own = own_keys_dir(Path::new(&path));
    let mut own_files = FileStore::open(&own).map_err(|e| format!("cannot open {}: {e}", own.display()))?;
    rewrapped += rekey_with(&keys, &mut own_files)?.1;
    let mut report = format!("{sealed} record(s) encrypted, {rewrapped} re-wrapped under key {}\n", keys.current().0);
    if sealed > 0 {
        report += "records of objects that belong to a person get a key of their own when the server next saves them; until then, erasing them does not reach backups\n";
    }
    if created {
        report += &format!("new key file {}: back it up, as without it the data cannot be read\n", Path::new(&path).display());
    }
    if sealed > 0 {
        report += "the plain files were deleted, but their old contents may remain on the disk until it is overwritten\n";
    }
    Ok(report)
}

/// How many records were encrypted, and how many re-wrapped.
fn rekey_with(keys: &Keyring, files: &mut FileStore) -> Result<(usize, usize), String> {
    let (mut sealed, mut rewrapped) = (0, 0);
    for name in files.keys("")? {
        let text = files.load(&name)?.unwrap_or_default();
        if is_sealed_name(&name) {
            if let Some(fresh) = keys.rewrap(&name, &text)? {
                files.save(&name, &fresh)?;
                rewrapped += 1;
            }
        } else {
            // Sealed first, then the plain file removed: a crash in between leaves both, and
            // running this again finishes the job.
            files.save(&keys.name(&name), &keys.seal_record(&name, &text, None)?)?;
            files.remove(&name)?;
            sealed += 1;
        }
    }
    Ok((sealed, rewrapped))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tj_core::MemStore;

    fn keys(extra: &str) -> Keyring {
        Keyring::parse(&format!("names {}\nk1 {}\n{extra}", "11".repeat(32), "22".repeat(32))).unwrap()
    }

    #[test]
    fn records_round_trip_and_nothing_readable_is_stored() {
        let mut s = Sealed::new(MemStore::default(), Box::new(MemStore::default()), keys(""), &[]);
        s.save("Cart/ana@example.com", r#"{"state":{"card":"4111"}}"#).unwrap();
        s.save("Cart/bo", "{}").unwrap();
        s.save("tj:seq", "42").unwrap();
        assert_eq!(s.load("Cart/ana@example.com").unwrap().as_deref(), Some(r#"{"state":{"card":"4111"}}"#));
        assert_eq!((s.load("tj:seq").unwrap().as_deref(), s.load("Cart/nobody").unwrap()), (Some("42"), None));
        assert_eq!(s.keys("Cart/").unwrap(), ["Cart/ana@example.com", "Cart/bo"]);
        let stored = format!("{:?}", s.inner.data);
        assert!(!stored.contains("ana") && !stored.contains("4111") && !stored.contains("Cart"), "{stored}");
        assert!(s.inner.data.keys().all(|n| is_sealed_name(n)));
        // The same value sealed twice looks different.
        let before = s.inner.data.clone();
        s.save("Cart/bo", "{}").unwrap();
        assert_ne!(before, s.inner.data);
    }

    #[test]
    fn changed_moved_or_foreign_records_are_errors_not_empty_objects() {
        let mut s = Sealed::new(MemStore::default(), Box::new(MemStore::default()), keys(""), &[]);
        s.save("Cart/ana", r#"{"n":1}"#).unwrap();
        s.save("Cart/bo", r#"{"n":2}"#).unwrap();
        let (ana, bo) = (s.keys.name("Cart/ana"), s.keys.name("Cart/bo"));

        // A record copied over another object's name.
        let copy = s.inner.data[&ana].clone();
        s.inner.data.insert(bo.clone(), copy);
        assert!(s.load("Cart/bo").unwrap_err().contains("cannot decrypt"));

        // One changed byte.
        let mut env: Json = serde_json::from_str(&s.inner.data[&ana]).unwrap();
        let mut data = unbase64(env["data"].as_str().unwrap()).unwrap();
        data[5] ^= 1;
        env["data"] = base64(&data).into();
        s.inner.data.insert(ana.clone(), env.to_string());
        assert!(s.load("Cart/ana").is_err());

        // Another key file.
        let mut other = Sealed::new(MemStore::default(), Box::new(MemStore::default()), keys(""), &[]);
        other.save("Cart/ana", "{}").unwrap();
        other.keys = Keyring::parse(&format!("names {}\nk1 {}\n", "11".repeat(32), "33".repeat(32))).unwrap();
        assert!(other.load("Cart/ana").is_err());
        assert!(other.keys("Cart/").is_err(), "listing fails rather than skipping");
    }

    #[test]
    fn a_new_key_re_wraps_without_re_encrypting_and_the_old_key_can_go() {
        let mut s = Sealed::new(MemStore::default(), Box::new(MemStore::default()), keys(""), &[]);
        s.save("Cart/ana", r#"{"n":1}"#).unwrap();
        let name = s.keys.name("Cart/ana");
        let old: Json = serde_json::from_str(&s.inner.data[&name]).unwrap();

        let both = keys(&format!("k2 {}\n", "44".repeat(32)));
        assert_eq!(s.keys.rewrap(&name, &s.inner.data[&name]).unwrap(), None, "already under the newest key");
        let fresh: Json = serde_json::from_str(&both.rewrap(&name, &s.inner.data[&name]).unwrap().unwrap()).unwrap();
        assert_eq!((fresh["kid"].as_str(), &fresh["data"]), (Some("k2"), &old["data"]), "the record itself is unchanged");

        let only_new = Keyring::parse(&format!("names {}\nk2 {}\n", "11".repeat(32), "44".repeat(32))).unwrap();
        let mut s = Sealed::new(MemStore::default(), Box::new(MemStore::default()), only_new, &[]);
        s.inner.data.insert(name, fresh.to_string());
        assert_eq!(s.load("Cart/ana").unwrap().as_deref(), Some(r#"{"n":1}"#));
    }

    #[test]
    fn a_persons_objects_have_keys_of_their_own_that_erasing_destroys() {
        let mut s = Sealed::new(MemStore::default(), Box::new(MemStore::default()), keys(""), &["Account".to_string()]);
        s.save("Account/user:ana", r#"{"email":"ana@example.com"}"#).unwrap();
        s.save("Room/lobby", "{}").unwrap();
        let name = s.keys.name("Account/user:ana");
        let backup = s.inner.data[&name].clone();
        assert!(backup.contains("\"own\""), "under its own key: {backup}");
        assert_eq!(s.load("Account/user:ana").unwrap().as_deref(), Some(r#"{"email":"ana@example.com"}"#));

        // Erasing destroys the key: the record, and any copy of it, can no longer be opened.
        s.forget("Account/user:ana").unwrap();
        s.forget("Room/lobby").unwrap();
        assert!(s.load("Account/user:ana").unwrap_err().contains("was erased"));
        assert!(s.keys("Account/").unwrap().is_empty(), "not listed");
        assert_eq!(s.load("Room/lobby").unwrap().as_deref(), Some("{}"), "objects that belong to nobody are not affected");
        // The blank saved after it gets a new key; a copy from a backup still cannot be read.
        s.save("Account/user:ana", r#"{"email":""}"#).unwrap();
        assert_eq!(s.load("Account/user:ana").unwrap().as_deref(), Some(r#"{"email":""}"#));
        s.inner.data.insert(name.clone(), backup);
        assert!(s.load("Account/user:ana").unwrap_err().contains("was erased"), "the old copy asks for an older key");
        assert_eq!(s.keys("Account/").unwrap(), Vec::<String>::new(), "and is skipped, so listing the class still works");

        // A new key-encryption key re-wraps the object's key, never the record under it.
        let both = keys(&format!("k2 {}\n", "44".repeat(32)));
        let own_name = s.own_name("Account/user:ana");
        let own_text = s.own_store.load(&own_name).unwrap().unwrap();
        assert!(both.rewrap(&own_name, &own_text).unwrap().is_some());
        assert!(s.inner.data.keys().all(|n| *n != own_name), "kept apart from the data");
        assert_eq!(both.rewrap(&name, &s.inner.data[&name]).unwrap(), None);
    }

    #[test]
    fn key_files_say_what_is_wrong() {
        assert!(Keyring::parse("k1 abcd").err().unwrap().contains("64 hex"));
        assert!(Keyring::parse(&format!("k1 {}", "22".repeat(32))).err().unwrap().contains("names"));
        assert!(Keyring::parse(&format!("names {}", "11".repeat(32))).err().unwrap().contains("key-encryption key"));
        assert!(Keyring::parse(&format!("names {0}\nk1 {0}\nk1 {0}", "11".repeat(32))).err().unwrap().contains("each used once"));
        assert!(Keyring::parse(&format!("# made by ops\nnames {0}\n\nk1 {0}", "11".repeat(32))).is_ok());
    }

    #[test]
    fn rekey_encrypts_a_plain_folder_and_the_server_refuses_mixed_ones() {
        let data = std::env::temp_dir().join(format!("tj-sealed-rekey-{}", std::process::id()));
        let _ = fs::remove_dir_all(&data);
        let mut plain = FileStore::open(&data).unwrap();
        plain.save("Cart/ana", r#"{"n":1}"#).unwrap();
        plain.save("tj:seq", "7").unwrap();
        let keys = keys("");
        assert_eq!(rekey_with(&keys, &mut plain), Ok((2, 0)));
        assert_eq!(rekey_with(&keys, &mut plain), Ok((0, 0)), "running it again changes nothing");
        let names = plain.keys("").unwrap();
        assert!(names.len() == 2 && names.iter().all(|n| is_sealed_name(n)), "{names:?}");
        let mut s = Sealed::new(FileStore::open(&data).unwrap(), Box::new(MemStore::default()), keys, &[]);
        assert_eq!(s.load("Cart/ana").unwrap().as_deref(), Some(r#"{"n":1}"#));
        // Without the key file, the server will not start on it.
        assert!(open_store(&data, &[]).err().unwrap().contains("is encrypted"));
        fs::remove_dir_all(data).unwrap();
    }
}
