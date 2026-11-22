//! Who may use the server's automation and operations surface (`/_tj/*`).
//!
//! `tj dev` keeps `/_tj/*` open on this machine unless a scoped API key (`tjk_...`) is sent
//! explicitly. `tj serve` accepts either:
//! - the legacy full-access admin key (`TJ_ADMIN_KEY`, 24+ characters), or
//! - named, scoped operational keys in `TJ_KEYS` (`tj key <name> --keys <file> --ops observe|test|maintain`):
//!   - `observe`: read-only (`/_tj/manifest`, `/_tj/metrics`, `/_tj/inspect` with `@pii` masked, `/_tj/leases` + own observe leases).
//!   - `test`: everything in `observe`, plus `POST /_tj/input` on `tj dev` only (never mutating a live `tj serve` store), plus own observe/test leases.
//!   - `maintain`: everything, including `POST /_tj/input`, `POST /_tj/privacy`, unfiltered `/api/events` and releasing any lease.
//!
//! Each scoped key is audited under its own name (`ops:<name>`). Without `TJ_ADMIN_KEY` or
//! `TJ_KEYS`, automation fails closed (`403`).

use crate::audit::Actor;
use crate::identity::{Identity, KEY_PREFIX};

pub const ADMIN_KEY_ENV: &str = "TJ_ADMIN_KEY";
pub const OPS_KEY_ENV: &str = "TJ_OPS_KEY";
const MIN_KEY_LEN: usize = 24;

/// Operational scope granted to a named key in `TJ_KEYS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum OpsScope {
    Observe = 1,
    Test = 2,
    Maintain = 3,
}

impl OpsScope {
    pub fn parse(s: &str) -> Result<OpsScope, String> {
        match s {
            "observe" => Ok(OpsScope::Observe),
            "test" => Ok(OpsScope::Test),
            "maintain" => Ok(OpsScope::Maintain),
            other => Err(format!("--ops scope must be observe, test or maintain, got {other:?}")),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            OpsScope::Observe => "observe",
            OpsScope::Test => "test",
            OpsScope::Maintain => "maintain",
        }
    }
}

/// Authenticated operational caller on `/_tj/*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpsAccess {
    /// `TJ_ADMIN_KEY`, or unauthenticated `tj dev`.
    Admin,
    /// Named operational key from `TJ_KEYS`, audited as `ops:<name>`.
    Scoped { name: String, scope: OpsScope },
}

impl OpsAccess {
    pub fn actor(&self) -> Actor {
        match self {
            OpsAccess::Admin => Actor::Admin,
            OpsAccess::Scoped { name, .. } => Actor::Ops(name.clone()),
        }
    }

    pub fn holder(&self) -> String {
        match self {
            OpsAccess::Admin => "admin".into(),
            OpsAccess::Scoped { name, .. } => format!("ops:{name}"),
        }
    }

    pub fn scope(&self) -> OpsScope {
        match self {
            OpsAccess::Admin => OpsScope::Maintain,
            OpsAccess::Scoped { scope, .. } => *scope,
        }
    }

    pub fn can_force_release(&self) -> bool {
        self.scope() == OpsScope::Maintain
    }

    /// Checks whether this caller may perform an action requiring `need` (`dev` is true on
    /// `tj dev`, where `ops:test` may drive `/_tj/input`).
    pub fn require(&self, need: OpsScope, dev: bool, what: &str) -> Result<(), (u16, String)> {
        let OpsAccess::Scoped { name, scope } = self else {
            return Ok(());
        };
        match need {
            OpsScope::Observe => Ok(()),
            OpsScope::Test => match scope {
                OpsScope::Maintain => Ok(()),
                OpsScope::Test if dev => Ok(()),
                OpsScope::Test => Err((403, format!("ops:{name} has scope \"test\"; {what} is only allowed on `tj dev`, not on a live `tj serve` store"))),
                OpsScope::Observe => Err((403, format!("ops:{name} has scope \"observe\"; {what} needs scope \"test\" (on tj dev), \"maintain\", or {ADMIN_KEY_ENV}"))),
            },
            OpsScope::Maintain => match scope {
                OpsScope::Maintain => Ok(()),
                other => Err((403, format!("ops:{name} has scope \"{}\"; {what} needs scope \"maintain\" or {ADMIN_KEY_ENV}", other.as_str()))),
            },
        }
    }

    /// Checks whether this caller may acquire a lease with `role`.
    pub fn require_lease_role(&self, role: OpsScope) -> Result<(), (u16, String)> {
        if role <= self.scope() {
            return Ok(());
        }
        Err((403, format!("{} has scope \"{}\"; it cannot acquire a \"{}\" lease", self.holder(), self.scope().as_str(), role.as_str())))
    }
}

pub enum Automation {
    /// `tj dev`: open by default; honors scoped `tjk_` keys when sent.
    Open,
    /// `tj serve` with `TJ_ADMIN_KEY`: accepts the admin key or a scoped `--ops` key in `TJ_KEYS`.
    Key(String),
    /// `tj serve` without `TJ_ADMIN_KEY`: accepts only scoped `--ops` keys in `TJ_KEYS` (off if none).
    Off,
}

impl Automation {
    /// The setting for `tj serve` from the environment; a key that is too short is refused.
    pub fn for_serve() -> Result<Automation, String> {
        match std::env::var(ADMIN_KEY_ENV) {
            Ok(k) if k.len() >= MIN_KEY_LEN => Ok(Automation::Key(k)),
            Ok(k) if !k.is_empty() => Err(format!("{ADMIN_KEY_ENV} must be at least {MIN_KEY_LEN} characters; generate one, e.g. `openssl rand -hex 24`")),
            _ => Ok(Automation::Off),
        }
    }

    /// Authenticates a caller for `/_tj/*`. Does not check the route's scope yet; call
    /// [`OpsAccess::require`] on the returned access.
    pub fn authenticate(&self, authorization: Option<&str>, identity: &Identity) -> Result<OpsAccess, (u16, String)> {
        match self {
            Automation::Open => {
                // On `tj dev`, unauthenticated requests are open; a `tjk_` key is still verified
                // so scoped keys can be tested locally and audited under their own name.
                if let Some(token) = bearer(authorization).filter(|t| t.starts_with(KEY_PREFIX)) {
                    return ops_from_keys(token, identity);
                }
                Ok(OpsAccess::Admin)
            }
            Automation::Key(key) => {
                let Some(token) = bearer(authorization) else {
                    return Err((401, format!("automation needs {ADMIN_KEY_ENV} or a scoped --ops key in TJ_KEYS: `Authorization: Bearer <key>`")));
                };
                if same(token.as_bytes(), key.as_bytes()) {
                    return Ok(OpsAccess::Admin);
                }
                if token.starts_with(KEY_PREFIX) {
                    return ops_from_keys(token, identity);
                }
                Err((401, format!("automation needs {ADMIN_KEY_ENV} or a scoped --ops key in TJ_KEYS: `Authorization: Bearer <key>`")))
            }
            Automation::Off => {
                if !identity.has_keys() {
                    return Err((403, format!("automation is off on this server; set {ADMIN_KEY_ENV} or add a scoped --ops key to TJ_KEYS")));
                }
                let Some(token) = bearer(authorization) else {
                    return Err((401, "automation needs a scoped --ops key from TJ_KEYS: `Authorization: Bearer <key>`".into()));
                };
                if token.starts_with(KEY_PREFIX) {
                    return ops_from_keys(token, identity);
                }
                Err((401, "automation needs a scoped --ops key from TJ_KEYS: `Authorization: Bearer <key>`".into()))
            }
        }
    }

    pub fn describe(&self, has_keys: bool) -> &'static str {
        match self {
            Automation::Open => "open (dev)",
            Automation::Key(_) if has_keys => "admin key or scoped --ops keys (TJ_KEYS)",
            Automation::Key(_) => "needs the admin key",
            Automation::Off if has_keys => "scoped --ops keys (TJ_KEYS)",
            Automation::Off => "off (set TJ_ADMIN_KEY or add --ops keys to TJ_KEYS)",
        }
    }
}

fn bearer(authorization: Option<&str>) -> Option<&str> {
    authorization.and_then(|h| h.strip_prefix("Bearer ")).map(str::trim)
}

fn ops_from_keys(token: &str, identity: &Identity) -> Result<OpsAccess, (u16, String)> {
    match identity.verify_ops_key(token)? {
        Some((name, scope)) => Ok(OpsAccess::Scoped { name, scope }),
        None => Err((403, "this API key has no operational scope; make one with `tj key <name> --keys <file> --ops observe|test|maintain`".into())),
    }
}

/// The header automation clients send: `TJ_OPS_KEY` if set, otherwise `TJ_ADMIN_KEY`.
pub fn client_header() -> Option<String> {
    [OPS_KEY_ENV, ADMIN_KEY_ENV]
        .into_iter()
        .find_map(|env| std::env::var(env).ok().filter(|k| !k.is_empty()))
        .map(|k| format!("Bearer {k}"))
}

/// Compares in constant time, so the time taken does not reveal how much of a key matched.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automation_enforces_admin_and_scoped_ops_keys() {
        let dir = std::env::temp_dir().join(format!("tj-auth-ops-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("keys.json");
        let obs = crate::identity::new_key("critic", &[], Some(OpsScope::Observe), &file).unwrap();
        let tst = crate::identity::new_key("tester", &[], Some(OpsScope::Test), &file).unwrap();
        let mnt = crate::identity::new_key("maint", &[], Some(OpsScope::Maintain), &file).unwrap();
        let user = crate::identity::new_key("ana", &["staff".into()], None, &file).unwrap();
        let id = Identity::from_key_file(&file).unwrap();

        let off_no_keys = Automation::Off;
        let empty = Identity::anonymous();
        assert_eq!(off_no_keys.authenticate(Some("Bearer anything"), &empty).unwrap_err().0, 403);

        // Even without TJ_ADMIN_KEY, scoped ops keys work on `tj serve`.
        let obs_acc = off_no_keys.authenticate(Some(&format!("Bearer {obs}")), &id).unwrap();
        assert_eq!(obs_acc, OpsAccess::Scoped { name: "critic".into(), scope: OpsScope::Observe });
        assert!(obs_acc.require(OpsScope::Observe, false, "inspect").is_ok());
        assert_eq!(obs_acc.require(OpsScope::Test, true, "input").unwrap_err().0, 403);
        assert_eq!(obs_acc.require(OpsScope::Maintain, false, "privacy").unwrap_err().0, 403);
        assert!(obs_acc.require_lease_role(OpsScope::Observe).is_ok());
        assert_eq!(obs_acc.require_lease_role(OpsScope::Maintain).unwrap_err().0, 403);

        let tst_acc = off_no_keys.authenticate(Some(&format!("Bearer {tst}")), &id).unwrap();
        assert!(tst_acc.require(OpsScope::Test, true, "input").is_ok(), "test scope works on tj dev");
        assert_eq!(tst_acc.require(OpsScope::Test, false, "input").unwrap_err().0, 403, "test scope refused on live tj serve");
        assert_eq!(tst_acc.require(OpsScope::Maintain, true, "privacy").unwrap_err().0, 403);

        let mnt_acc = off_no_keys.authenticate(Some(&format!("Bearer {mnt}")), &id).unwrap();
        assert!(mnt_acc.require(OpsScope::Maintain, false, "privacy").is_ok());
        assert!(mnt_acc.require(OpsScope::Test, false, "input").is_ok());

        // Plain user API keys (no --ops) are refused on /_tj/*.
        assert_eq!(off_no_keys.authenticate(Some(&format!("Bearer {user}")), &id).unwrap_err().0, 403);

        // Legacy TJ_ADMIN_KEY still grants Admin.
        let key = Automation::Key("k".repeat(MIN_KEY_LEN));
        assert_eq!(key.authenticate(Some(&format!("Bearer {}", "k".repeat(MIN_KEY_LEN))), &empty).unwrap(), OpsAccess::Admin);
        assert_eq!(key.authenticate(Some("Bearer wrong"), &empty).unwrap_err().0, 401);
        assert_eq!(key.authenticate(None, &empty).unwrap_err().0, 401);

        // `tj dev` is open when no key is sent, and enforces scoped `tjk_` keys when sent.
        assert_eq!(Automation::Open.authenticate(None, &id).unwrap(), OpsAccess::Admin);
        assert_eq!(Automation::Open.authenticate(Some(&format!("Bearer {obs}")), &id).unwrap(), obs_acc);
        assert!(!same(b"abc", b"abd") && !same(b"abc", b"ab") && same(b"abc", b"abc"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
