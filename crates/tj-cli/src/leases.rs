//! Work leases (`/_tj/leases` and `tj lease`): who is currently observing, testing or
//! maintaining a running app, so autonomous agents and human operators do not collide.
//!
//! - Leases are short-lived (default 5 minutes, capped at 1 hour) and expire automatically,
//!   so a crashed agent never leaves an app locked.
//! - Non-exclusive leases (`observe`, `test`) coexist freely.
//! - Exclusive leases (`maintain`, or any lease with `exclusive: true`) conflict (`409`)
//!   with another holder's exclusive lease on the same `scope` (or `"app"`, which covers
//!   the whole app). Re-acquiring with the same `(holder, role, scope)` renews the lease.
//! - Stored as plain JSON in `<data>/leases.json` (no personal data: only operational
//!   identities, scopes and expiry times) so active maintenance leases survive restarts.

use crate::auth::OpsScope;
use crate::bytes::{hex, random};
use crate::engine::{now_ms, Failure};
use crate::store::{private_dir, private_file, sync_dir};
use serde_json::{json, Value as Json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const DEFAULT_TTL_SECS: u64 = 300;
const MIN_TTL_SECS: u64 = 10;
const MAX_TTL_SECS: u64 = 3600;
const MAX_LEASES: usize = 256;
const MAX_SCOPE_LEN: usize = 64;
const MAX_NOTE_LEN: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    pub id: String,
    pub holder: String,
    pub role: OpsScope,
    pub scope: String,
    pub exclusive: bool,
    pub note: String,
    pub acquired_at: u64,
    pub expires_at: u64,
}

impl Lease {
    pub fn json(&self) -> Json {
        let mut o = json!({
            "id": self.id,
            "holder": self.holder,
            "role": self.role.as_str(),
            "scope": self.scope,
            "exclusive": self.exclusive,
            "acquiredAt": self.acquired_at,
            "expiresAt": self.expires_at,
        });
        if !self.note.is_empty() {
            o["note"] = json!(self.note);
        }
        o
    }

    fn from_json(v: &Json) -> Option<Lease> {
        Some(Lease {
            id: v["id"].as_str()?.to_string(),
            holder: v["holder"].as_str()?.to_string(),
            role: OpsScope::parse(v["role"].as_str()?).ok()?,
            scope: v["scope"].as_str()?.to_string(),
            exclusive: v["exclusive"].as_bool()?,
            note: v["note"].as_str().unwrap_or("").to_string(),
            acquired_at: v["acquiredAt"].as_u64()?,
            expires_at: v["expiresAt"].as_u64()?,
        })
    }
}

pub struct Leases {
    file: Option<PathBuf>,
    items: Mutex<Vec<Lease>>,
}

impl Leases {
    /// Opens `<data>/leases.json`, loading any leases that have not expired yet.
    pub fn open(data: &Path) -> Leases {
        let file = data.join("leases.json");
        let now = now_ms() as u64;
        let items = std::fs::read_to_string(&file)
            .ok()
            .and_then(|t| serde_json::from_str::<Json>(&t).ok())
            .and_then(|j| j.as_array().cloned())
            .map(|arr| arr.iter().filter_map(Lease::from_json).filter(|l| l.expires_at > now).collect())
            .unwrap_or_default();
        Leases { file: Some(file), items: Mutex::new(items) }
    }

    #[cfg(test)]
    pub fn in_memory() -> Leases {
        Leases { file: None, items: Mutex::new(Vec::new()) }
    }

    /// Active leases, dropping any that have expired.
    pub fn list(&self) -> Vec<Lease> {
        self.list_at(now_ms() as u64)
    }

    fn list_at(&self, now: u64) -> Vec<Lease> {
        let Ok(mut items) = self.items.lock() else { return Vec::new() };
        let before = items.len();
        items.retain(|l| l.expires_at > now);
        if items.len() != before {
            self.save(&items);
        }
        items.clone()
    }

    /// Acquires a lease, or renews the holder's existing lease on `(role, scope)`.
    pub fn acquire(&self, holder: String, role: OpsScope, scope: String, exclusive: Option<bool>, ttl_secs: Option<u64>, note: String) -> Result<Lease, Failure> {
        self.acquire_at(now_ms() as u64, holder, role, scope, exclusive, ttl_secs, note)
    }

    #[allow(clippy::too_many_arguments)]
    fn acquire_at(&self, now: u64, holder: String, role: OpsScope, scope: String, exclusive: Option<bool>, ttl_secs: Option<u64>, note: String) -> Result<Lease, Failure> {
        let scope = scope.trim().to_string();
        if scope.is_empty() || scope.len() > MAX_SCOPE_LEN || !scope.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '/')) {
            return Err((400, format!("lease scope must be 1..={MAX_SCOPE_LEN} letters, digits or -_.:/, got {scope:?}")));
        }
        let note = note.trim().to_string();
        if note.chars().count() > MAX_NOTE_LEN {
            return Err((400, format!("lease note must be at most {MAX_NOTE_LEN} characters")));
        }
        let ttl = ttl_secs.unwrap_or(DEFAULT_TTL_SECS);
        if !(MIN_TTL_SECS..=MAX_TTL_SECS).contains(&ttl) {
            return Err((400, format!("lease ttlSecs must be between {MIN_TTL_SECS} and {MAX_TTL_SECS}")));
        }
        let exclusive = exclusive.unwrap_or(role == OpsScope::Maintain);
        let expires_at = now.saturating_add(ttl * 1000);
        let mut items = self.items.lock().map_err(|_| (500, "leases unavailable".to_string()))?;
        items.retain(|l| l.expires_at > now);

        // Exclusive leases conflict with any other holder's exclusive lease on the same scope
        // or covering the whole app ("app").
        if exclusive {
            if let Some(other) = items.iter().find(|l| l.exclusive && l.holder != holder && scopes_overlap(&l.scope, &scope)) {
                let left = other.expires_at.saturating_sub(now) / 1000;
                return Err((409, format!("scope {:?} is already leased exclusively by {} (role {}, id {}, {}s left)", other.scope, other.holder, other.role.as_str(), other.id, left)));
            }
        }

        // Renew an existing lease held by the same caller on the same (role, scope).
        if let Some(existing) = items.iter_mut().find(|l| l.holder == holder && l.role == role && l.scope == scope) {
            existing.exclusive = exclusive;
            existing.note = note;
            existing.expires_at = expires_at;
            let renewed = existing.clone();
            self.save(&items);
            return Ok(renewed);
        }

        if items.len() >= MAX_LEASES {
            return Err((503, format!("too many active leases (limit {MAX_LEASES}); release finished leases first")));
        }
        let id = hex(&random::<8>().map_err(|e| (500, e))?);
        let lease = Lease { id, holder, role, scope, exclusive, note, acquired_at: now, expires_at };
        items.push(lease.clone());
        self.save(&items);
        Ok(lease)
    }

    /// Releases a lease by id. Allowed for its holder, or for callers with `can_force`
    /// (`maintain` / `admin`).
    pub fn release(&self, id: &str, holder: &str, can_force: bool) -> Result<Lease, Failure> {
        self.release_at(now_ms() as u64, id, holder, can_force)
    }

    fn release_at(&self, now: u64, id: &str, holder: &str, can_force: bool) -> Result<Lease, Failure> {
        let mut items = self.items.lock().map_err(|_| (500, "leases unavailable".to_string()))?;
        items.retain(|l| l.expires_at > now);
        let Some(pos) = items.iter().position(|l| l.id == id) else {
            return Err((404, format!("lease {id:?} not found or already expired")));
        };
        if items[pos].holder != holder && !can_force {
            return Err((403, format!("lease {id} is held by {}; only its holder or a maintain key may release it", items[pos].holder)));
        }
        let removed = items.remove(pos);
        self.save(&items);
        Ok(removed)
    }

    /// Fails with `409` if another holder currently holds an active exclusive lease
    /// overlapping `scope`. Unleased callers and the lease holder itself proceed.
    pub fn check_exclusive(&self, holder: &str, scope: &str) -> Result<(), Failure> {
        self.check_exclusive_at(now_ms() as u64, holder, scope)
    }

    fn check_exclusive_at(&self, now: u64, holder: &str, scope: &str) -> Result<(), Failure> {
        let mut items = self.items.lock().map_err(|_| (500, "leases unavailable".to_string()))?;
        items.retain(|l| l.expires_at > now);
        if let Some(other) = items.iter().find(|l| l.exclusive && l.holder != holder && scopes_overlap(&l.scope, scope)) {
            let left = other.expires_at.saturating_sub(now) / 1000;
            return Err((409, format!("scope {:?} is leased exclusively by {} (role {}, id {}, {}s left); wait or release that lease first", other.scope, other.holder, other.role.as_str(), other.id, left)));
        }
        Ok(())
    }

    /// Best-effort persistence to `<data>/leases.json`, atomic via a private temp file + rename.
    fn save(&self, items: &[Lease]) {
        let Some(file) = &self.file else { return };
        if let Some(parent) = file.parent() {
            let _ = private_dir(parent);
        }
        let body = serde_json::to_string_pretty(&items.iter().map(Lease::json).collect::<Vec<_>>()).unwrap_or_default() + "\n";
        let tmp = file.with_extension("json.tmp");
        let write = || -> std::io::Result<()> {
            let mut f = private_file().write(true).create(true).truncate(true).open(&tmp)?;
            f.write_all(body.as_bytes())?;
            f.sync_all()?;
            std::fs::rename(&tmp, file)?;
            if let Some(parent) = file.parent() {
                sync_dir(parent)?;
            }
            Ok(())
        };
        let _ = write();
    }
}

fn scopes_overlap(a: &str, b: &str) -> bool {
    a == b
        || a == "app"
        || b == "app"
        || a.split_once('/').is_some_and(|(c, _)| c == b)
        || b.split_once('/').is_some_and(|(c, _)| c == a)
}

/// `tj lease list | acquire [...] | release <id> [--target <url>]`.
pub fn run(args: &[String]) -> Result<(), String> {
    let usage = "usage:\n  tj lease list [--target <url>]\n  tj lease acquire [--role observe|test|maintain] [--scope <name>] [--ttl <secs>] [--note <text>] [--exclusive | --shared] [--target <url>]\n  tj lease release <id> [--target <url>]";
    let Some(sub) = args.first().map(String::as_str) else {
        return Err(usage.into());
    };
    let mut target = "http://127.0.0.1:4700".to_string();
    let mut role: Option<String> = None;
    let mut scope: Option<String> = None;
    let mut ttl: Option<u64> = None;
    let mut note: Option<String> = None;
    let mut exclusive: Option<bool> = None;
    let mut id: Option<String> = None;
    let mut it = args.iter().skip(1).peekable();
    if sub == "release" {
        id = it.next_if(|a| !a.starts_with("--")).cloned();
    }
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--target" => target = value()?.clone(),
            "--role" => role = Some(value()?.clone()),
            "--scope" => scope = Some(value()?.clone()),
            "--ttl" => ttl = Some(value()?.parse().map_err(|_| "--ttl must be a whole number of seconds")?),
            "--note" => note = Some(value()?.clone()),
            "--exclusive" => exclusive = Some(true),
            "--shared" => exclusive = Some(false),
            other => return Err(format!("unknown option {other}\n\n{usage}")),
        }
    }
    let base = target.trim_end_matches('/');
    let auth = crate::auth::client_header();
    let send = |mut req: ureq::Request, body: Option<Json>| -> Result<Json, String> {
        if let Some(h) = &auth {
            req = req.set("Authorization", h);
        }
        let resp = match body {
            Some(j) => req.set("Content-Type", "application/json").send_string(&j.to_string()),
            None => req.call(),
        };
        let text = match resp {
            Ok(r) | Err(ureq::Error::Status(_, r)) => r.into_string().map_err(|e| e.to_string())?,
            Err(e) => return Err(format!("cannot reach {base}: {e}")),
        };
        let reply: Json = serde_json::from_str(&text).map_err(|e| format!("bad reply from {base}: {e}"))?;
        if let Some(e) = reply["error"].as_str() {
            return Err(e.to_string());
        }
        Ok(reply["ok"].clone())
    };
    match sub {
        "list" => {
            let ok = send(ureq::get(&format!("{base}/_tj/leases")), None)?;
            println!("{}", serde_json::to_string_pretty(&ok).expect("JSON serializes"));
            Ok(())
        }
        "acquire" => {
            let mut body = serde_json::Map::new();
            if let Some(r) = role {
                body.insert("role".into(), json!(r));
            }
            if let Some(s) = scope {
                body.insert("scope".into(), json!(s));
            }
            if let Some(t) = ttl {
                body.insert("ttlSecs".into(), json!(t));
            }
            if let Some(n) = note {
                body.insert("note".into(), json!(n));
            }
            if let Some(e) = exclusive {
                body.insert("exclusive".into(), json!(e));
            }
            let ok = send(ureq::post(&format!("{base}/_tj/leases")), Some(Json::Object(body)))?;
            println!("{}", serde_json::to_string_pretty(&ok).expect("JSON serializes"));
            Ok(())
        }
        "release" => {
            let id = id.ok_or_else(|| format!("`tj lease release` needs a lease id\n\n{usage}"))?;
            let ok = send(ureq::delete(&format!("{base}/_tj/leases/{id}")), None)?;
            println!("{}", serde_json::to_string_pretty(&ok).expect("JSON serializes"));
            Ok(())
        }
        _ => Err(usage.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leases_prevent_exclusive_collisions_renew_for_same_holder_and_expire() {
        let leases = Leases::in_memory();
        let t0 = 1_000_000;
        // Two observers coexist freely on the same scope.
        let l1 = leases.acquire_at(t0, "ops:critic-1".into(), OpsScope::Observe, "app".into(), None, Some(60), "review".into()).unwrap();
        let l2 = leases.acquire_at(t0, "ops:critic-2".into(), OpsScope::Observe, "app".into(), None, Some(60), String::new()).unwrap();
        assert!(!l1.exclusive && !l2.exclusive);
        assert_eq!(leases.list_at(t0).len(), 2);

        // An exclusive maintain lease succeeds alongside non-exclusive observers.
        let m1 = leases.acquire_at(t0, "ops:maint-1".into(), OpsScope::Maintain, "Slot".into(), None, Some(120), "upgrade".into()).unwrap();
        assert!(m1.exclusive);

        // Another holder cannot take an exclusive lease on the same scope OR on "app".
        let conflict_same = leases.acquire_at(t0, "ops:maint-2".into(), OpsScope::Maintain, "Slot".into(), None, Some(60), String::new()).unwrap_err();
        assert_eq!(conflict_same.0, 409);
        let conflict_app = leases.acquire_at(t0, "ops:maint-2".into(), OpsScope::Maintain, "app".into(), None, Some(60), String::new()).unwrap_err();
        assert_eq!(conflict_app.0, 409);

        // `check_exclusive_at` blocks competing holders on overlapping scopes (including Class vs Class/id), allows self & non-overlapping scopes.
        assert_eq!(leases.check_exclusive_at(t0, "ops:maint-2", "Slot").unwrap_err().0, 409);
        assert_eq!(leases.check_exclusive_at(t0, "ops:maint-2", "Slot/s1").unwrap_err().0, 409);
        assert_eq!(leases.check_exclusive_at(t0, "ops:maint-2", "app").unwrap_err().0, 409);
        assert!(leases.check_exclusive_at(t0, "ops:maint-2", "Settings").is_ok());
        assert!(leases.check_exclusive_at(t0, "ops:maint-1", "Slot").is_ok());

        // Object-scoped exclusive lease (`Settings/s1`) blocks `Settings`, `Settings/s1`, and `app`, but not `Settings/s2`.
        let s1 = leases.acquire_at(t0, "ops:maint-2".into(), OpsScope::Maintain, "Settings/s1".into(), None, Some(60), String::new()).unwrap();
        assert_eq!(leases.check_exclusive_at(t0, "ops:maint-1", "Settings/s1").unwrap_err().0, 409);
        assert_eq!(leases.check_exclusive_at(t0, "ops:maint-1", "Settings").unwrap_err().0, 409);
        assert!(leases.check_exclusive_at(t0, "ops:maint-1", "Settings/s2").is_ok());
        assert!(leases.release_at(t0, &s1.id, "ops:maint-2", false).is_ok());

        // Same holder renewing extends the same lease id instead of conflicting.
        let renewed = leases.acquire_at(t0 + 10_000, "ops:maint-1".into(), OpsScope::Maintain, "Slot".into(), None, Some(120), "still upgrading".into()).unwrap();
        assert_eq!(renewed.id, m1.id);
        assert_eq!(renewed.expires_at, t0 + 130_000);

        // Non-owner without force cannot release someone else's lease; owner or maintain can.
        assert_eq!(leases.release_at(t0 + 10_000, &m1.id, "ops:critic-1", false).unwrap_err().0, 403);
        assert!(leases.release_at(t0 + 10_000, &l1.id, "ops:critic-1", false).is_ok());
        assert!(leases.release_at(t0 + 10_000, &m1.id, "ops:maint-2", true).is_ok());

        // Remaining lease expires cleanly once its TTL passes; an expired exclusive lease never blocks.
        assert_eq!(leases.list_at(t0 + 61_000).len(), 0);
        let _ = leases.acquire_at(t0 + 70_000, "ops:maint-1".into(), OpsScope::Maintain, "app".into(), None, Some(10), String::new()).unwrap();
        assert_eq!(leases.check_exclusive_at(t0 + 75_000, "ops:maint-2", "app").unwrap_err().0, 409);
        assert!(leases.check_exclusive_at(t0 + 81_000, "ops:maint-2", "app").is_ok());
    }

    #[test]
    fn leases_persist_across_reopen() {
        let dir = std::env::temp_dir().join(format!("tj-leases-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let a = Leases::open(&dir);
        let l = a.acquire("ops:maint".into(), OpsScope::Maintain, "app".into(), None, Some(300), "deploy".into()).unwrap();
        let b = Leases::open(&dir);
        let active = b.list();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, l.id);
        assert_eq!(active[0].holder, "ops:maint");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
