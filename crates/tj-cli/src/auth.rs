//! Who may use the server.
//!
//! The automation endpoints (`/_tj/*`) can run any input, so on `tj serve` they need the
//! admin key: `Authorization: Bearer <key>`, with the key from the `TJ_ADMIN_KEY`
//! environment variable (not a flag: flags show up in process lists). Without a key they
//! are off. `tj dev` keeps them open: it is for your own machine.

pub const ADMIN_KEY_ENV: &str = "TJ_ADMIN_KEY";
const MIN_KEY_LEN: usize = 24;

pub enum Automation {
    /// `tj dev`: open.
    Open,
    /// `tj serve` with `TJ_ADMIN_KEY`: only with the key.
    Key(String),
    /// `tj serve` without a key: off.
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

    /// Whether a request with this `Authorization` header may use automation; else the status
    /// and a message saying what to do.
    pub fn check(&self, authorization: Option<&str>) -> Result<(), (u16, String)> {
        match self {
            Automation::Open => Ok(()),
            Automation::Off => Err((403, format!("automation is off on this server; start it with {ADMIN_KEY_ENV} set, and send the key as `Authorization: Bearer <key>`"))),
            Automation::Key(key) => match authorization.and_then(|h| h.strip_prefix("Bearer ")) {
                Some(given) if same(given.trim().as_bytes(), key.as_bytes()) => Ok(()),
                _ => Err((401, format!("automation needs the admin key: `Authorization: Bearer <key>` (tj review and tj mcp send {ADMIN_KEY_ENV} when it is set)"))),
            },
        }
    }

    pub fn describe(&self) -> &'static str {
        match self {
            Automation::Open => "open (dev)",
            Automation::Key(_) => "needs the admin key",
            Automation::Off => "off (set TJ_ADMIN_KEY to use tj review and tj mcp here)",
        }
    }
}

/// The header automation clients send, from `TJ_ADMIN_KEY` if it is set.
pub fn client_header() -> Option<String> {
    std::env::var(ADMIN_KEY_ENV).ok().filter(|k| !k.is_empty()).map(|k| format!("Bearer {k}"))
}

/// Compares in constant time, so the time taken does not reveal how much of a key matched.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automation_needs_the_key_on_a_server() {
        let key = Automation::Key("k".repeat(MIN_KEY_LEN));
        assert!(key.check(Some(&format!("Bearer {}", "k".repeat(MIN_KEY_LEN)))).is_ok());
        assert_eq!(key.check(Some("Bearer wrong")).unwrap_err().0, 401);
        assert_eq!(key.check(None).unwrap_err().0, 401);
        assert_eq!(Automation::Off.check(Some("Bearer anything")).unwrap_err().0, 403);
        assert!(Automation::Open.check(None).is_ok());
        assert!(!same(b"abc", b"abd") && !same(b"abc", b"ab") && same(b"abc", b"abc"));
    }
}
