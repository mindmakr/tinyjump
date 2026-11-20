//! Who is calling (docs/access-control.md): API keys, `tj dev --as`, and anonymous callers.
//! Sign-in tokens from OpenID Connect providers come next. A credential that is present but
//! invalid gets 401; it never falls back to anonymous.

use ring::digest::{digest, SHA256};
use serde_json::{json, Value as Json};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};
use tj_core::runtime::protocol::Caller;

pub const KEYS_ENV: &str = "TJ_KEYS";
const KEY_PREFIX: &str = "tjk_";
const RELOAD_EVERY: Duration = Duration::from_secs(30);
/// Words that rules use, which no credential may claim as a role.
const NOT_ROLES: &[&str] = &["system", "anyone", "signedIn", "owner"];

#[derive(Clone)]
struct Key {
    name: String,
    roles: Vec<String>,
    sha256: String,
}

struct KeyFile {
    path: PathBuf,
    keys: Vec<Key>,
    loaded: Option<Instant>,
}

pub struct Identity {
    keys: Option<Mutex<KeyFile>>,
    /// `tj dev --as name:role,role`: the identity of requests without credentials.
    dev_as: Option<Caller>,
}

/// A verified caller, and when its credential stops being valid (re-checked on streams).
pub struct Verified {
    pub caller: Caller,
    pub expires: Option<SystemTime>,
}

pub fn sha256_hex(text: &str) -> String {
    digest(&SHA256, text.as_bytes()).as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

fn clean_roles(roles: Vec<String>) -> Vec<String> {
    roles.into_iter().filter(|r| !NOT_ROLES.contains(&r.as_str())).collect()
}

fn read_keys(path: &Path) -> Result<Vec<Key>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read keys file {}: {e}", path.display()))?;
    let list: Json = serde_json::from_str(&text).map_err(|e| format!("keys file {} is not JSON: {e}", path.display()))?;
    let mut keys = Vec::new();
    for k in list.as_array().ok_or(format!("keys file {} must be a JSON array", path.display()))? {
        let field = |f: &str| k[f].as_str().map(str::to_string).ok_or(format!("a key in {} has no \"{f}\"", path.display()));
        let roles = k["roles"].as_array().map(|a| a.iter().filter_map(|r| r.as_str().map(str::to_string)).collect()).unwrap_or_default();
        keys.push(Key { name: field("name")?, roles: clean_roles(roles), sha256: field("sha256")?.to_lowercase() });
    }
    Ok(keys)
}

impl Identity {
    /// From the environment (`TJ_KEYS`) and `tj dev --as`.
    pub fn new(dev_as: Option<&str>) -> Result<Identity, String> {
        let keys = match std::env::var(KEYS_ENV) {
            Ok(p) if !p.is_empty() => {
                let path = PathBuf::from(p);
                let keys = read_keys(&path)?;
                Some(Mutex::new(KeyFile { path, keys, loaded: Some(Instant::now()) }))
            }
            _ => None,
        };
        let dev_as = dev_as.map(parse_as).transpose()?;
        Ok(Identity { keys, dev_as })
    }

    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if self.keys.is_some() {
            parts.push("API keys".to_string());
        }
        if let Some(Caller::Remote { id, .. }) = &self.dev_as {
            parts.push(format!("requests without credentials act as {id}"));
        }
        if parts.is_empty() {
            "anonymous only (set TJ_KEYS for API keys)".into()
        } else {
            parts.join("; ")
        }
    }

    /// Who sent a request with this `Authorization` header.
    pub fn verify(&self, authorization: Option<&str>) -> Result<Verified, (u16, String)> {
        let Some(header) = authorization else {
            let caller = self.dev_as.clone().unwrap_or(Caller::Remote { id: String::new(), roles: Vec::new() });
            return Ok(Verified { caller, expires: None });
        };
        let Some(credential) = header.strip_prefix("Bearer ").map(str::trim) else {
            return Err((401, "send credentials as `Authorization: Bearer <credential>`".into()));
        };
        if credential.starts_with(KEY_PREFIX) {
            return self.api_key(credential).ok_or((401, "unknown or revoked API key".into()));
        }
        Err((401, "unknown credential (this server accepts API keys; sign-in tokens are not configured)".into()))
    }

    fn api_key(&self, key: &str) -> Option<Verified> {
        let mut file = self.keys.as_ref()?.lock().ok()?;
        if file.loaded.map_or(true, |t| t.elapsed() >= RELOAD_EVERY) {
            match read_keys(&file.path) {
                Ok(keys) => file.keys = keys,
                Err(e) => eprintln!("[tj] {e}; keeping the keys loaded before"),
            }
            file.loaded = Some(Instant::now());
        }
        let hash = sha256_hex(key);
        let k = file.keys.iter().find(|k| k.sha256 == hash)?;
        Some(Verified { caller: Caller::Remote { id: format!("key:{}", k.name), roles: k.roles.clone() }, expires: None })
    }
}

/// `name:role,role` for `tj dev --as`.
fn parse_as(spec: &str) -> Result<Caller, String> {
    let (name, roles) = spec.split_once(':').unwrap_or((spec, ""));
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(format!("--as needs a name like ana or ana:staff, got {spec:?}"));
    }
    let roles = clean_roles(roles.split(',').filter(|r| !r.is_empty()).map(str::to_string).collect());
    Ok(Caller::Remote { id: format!("dev:{name}"), roles })
}

/// `tj key new <name> --role r --keys file`: makes a key, prints it once, stores its hash.
pub fn new_key(name: &str, roles: &[String], file: &Path) -> Result<String, String> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(format!("a key name is letters, digits, - and _, got {name:?}"));
    }
    let mut list: Vec<Json> = match std::fs::read_to_string(file) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| format!("{} is not a JSON array: {e}", file.display()))?,
        Err(_) => Vec::new(),
    };
    if list.iter().any(|k| k["name"] == name) {
        return Err(format!("{} already has a key named {name}; remove its line to revoke it first", file.display()));
    }
    let mut bytes = [0u8; 32];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut bytes).map_err(|_| "no secure random source".to_string())?;
    let key = format!("{KEY_PREFIX}{}", bytes.iter().map(|b| format!("{b:02x}")).collect::<String>());
    list.push(json!({ "name": name, "roles": clean_roles(roles.to_vec()), "sha256": sha256_hex(&key) }));
    let text = serde_json::to_string_pretty(&list).expect("JSON serializes");
    std::fs::write(file, text + "\n").map_err(|e| format!("cannot write {}: {e}", file.display()))?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote(v: Verified) -> (String, Vec<String>) {
        match v.caller {
            Caller::Remote { id, roles } => (id, roles),
            Caller::Trusted(_) => panic!("never trusted"),
        }
    }

    #[test]
    fn api_keys_anonymous_and_dev_identities() {
        let dir = std::env::temp_dir().join(format!("tj-keys-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let file = dir.join("keys.json");
        let key = new_key("sync", &["staff".into(), "system".into()], &file).expect("made");
        assert!(key.starts_with("tjk_") && key.len() == 68);
        assert!(!std::fs::read_to_string(&file).unwrap().contains(&key), "only the hash is stored");
        assert!(new_key("sync", &[], &file).is_err(), "names are unique");

        let id = Identity { keys: Some(Mutex::new(KeyFile { path: file.clone(), keys: read_keys(&file).unwrap(), loaded: Some(Instant::now()) })), dev_as: None };
        assert_eq!(remote(id.verify(Some(&format!("Bearer {key}"))).ok().unwrap()), ("key:sync".into(), vec!["staff".into()]), "system cannot be granted");
        assert_eq!(id.verify(Some("Bearer tjk_nope")).err().unwrap().0, 401);
        assert_eq!(id.verify(Some("Bearer eyJhbGciOi")).err().unwrap().0, 401, "a bad credential never becomes anonymous");
        assert_eq!(remote(id.verify(None).ok().unwrap()), (String::new(), vec![]));

        let dev = Identity { keys: None, dev_as: Some(parse_as("ana:staff,owner").unwrap()) };
        assert_eq!(remote(dev.verify(None).ok().unwrap()), ("dev:ana".into(), vec!["staff".into()]), "rule words are not roles");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
