//! Access control (docs/access-control.md): who is calling, and the `@read` / `@allow` rules
//! objects declare. Rules are checked before anything is loaded, and every refusal looks
//! the same, so a caller cannot learn which objects or methods exist.

use super::protocol::Caller;
use super::Runtime;
use crate::vm::value::Value;
use std::rc::Rc;

/// The words a rule may use besides role names.
pub const RULE_WORDS: &[&str] = &["anyone", "signedIn", "owner"];

/// Who a running call belongs to; it lives on the call's fiber (see `vm::interp::Fiber`).
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Who {
    /// Timers, boot and trusted hosts on a server: `{ id: "system", roles: ["system"] }`.
    #[default]
    System,
    /// A device's own user, whom the device cannot verify: `{ id: "", roles: [] }`.
    Local,
    /// A verified (or anonymous, with an empty id) remote caller; the rules apply.
    Remote { id: Rc<str>, roles: Rc<[Rc<str>]> },
}

impl Who {
    pub fn owns(&self, object_id: &str) -> bool {
        matches!(self, Who::Remote { id, .. } if owns(id, object_id))
    }

    /// A protocol caller, as a server (`trusted` is the server itself) or a device sees it.
    pub fn from_caller(c: &Caller, server: bool) -> Who {
        match c {
            Caller::Trusted(_) if server => Who::System,
            Caller::Trusted(_) => Who::Local,
            Caller::Remote { id, roles } => Who::Remote { id: Rc::from(id.as_str()), roles: roles.iter().map(|r| Rc::from(r.as_str())).collect() },
        }
    }

    /// The `caller` record app code sees. A fresh record each time, so code cannot change it.
    pub fn record(&self) -> Value {
        let (id, roles): (&str, Vec<Value>) = match self {
            Who::System => ("system", vec![Value::str("system")]),
            Who::Local => ("", Vec::new()),
            Who::Remote { id, roles } => (id, roles.iter().map(|r| Value::Str(r.clone())).collect()),
        };
        Value::rec([(Rc::from("id"), Value::str(id)), (Rc::from("roles"), Value::arr(roles))].into_iter().collect())
    }

    /// How an alarm remembers who scheduled it: `None` for the system.
    pub fn to_caller(&self) -> Option<Caller> {
        match self {
            Who::Remote { id, roles } => Some(Caller::Remote { id: id.to_string(), roles: roles.iter().map(|r| r.to_string()).collect() }),
            Who::System | Who::Local => None,
        }
    }
}

/// The words of an `@read` / `@allow` rule.
pub type Rule = Rc<[Rc<str>]>;

/// Whether the caller with id `caller_id` owns the object with id `object_id`: the object
/// is theirs. Anonymous callers own nothing, even an object with an empty id.
pub fn owns(caller_id: &str, object_id: &str) -> bool {
    !caller_id.is_empty() && caller_id == object_id
}

/// Whether `rule` lets `who` act on the object with id `object_id`.
fn allows(rule: &[Rc<str>], who: &Who, object_id: &str) -> bool {
    let Who::Remote { id, roles } = who else { return true };
    rule.iter().any(|word| match &**word {
        "anyone" => true,
        "signedIn" => !id.is_empty(),
        "owner" => owns(id, object_id),
        role => roles.iter().any(|r| **r == *role),
    })
}

fn describe(rule: &[Rc<str>]) -> String {
    rule.iter().map(|w| if RULE_WORDS.contains(&&**w) { w.to_string() } else { format!("role {w}") }).collect::<Vec<_>>().join(" or ")
}

impl Runtime {
    /// Checks a remote request on `Class/id` (reading it, or calling `method`) before anything
    /// is loaded. `None` if allowed; otherwise the refusal, the same for anything that does not
    /// exist, is private or has no rule. Explaining runtimes (`tj dev`) add the reason.
    pub(crate) fn refusal(&self, key: &str, method: Option<&str>, who: &Who) -> Option<String> {
        if !matches!(who, Who::Remote { .. }) {
            return None;
        }
        let reason = match self.rule_for(key, method) {
            Ok((rule, id)) if allows(&rule, who, id) => return None,
            Ok((rule, _)) => format!("{key}{} needs {}", method.map(|m| format!(".{m}")).unwrap_or_default(), describe(&rule)),
            Err(why) => why,
        };
        Some(if self.explain { format!("not allowed: {reason}") } else { "not allowed".to_string() })
    }

    /// The rule that guards reading `key` (no method) or calling its method, and the object id.
    fn rule_for<'k>(&self, key: &'k str, method: Option<&str>) -> Result<(Rule, &'k str), String> {
        let (class, id) = key.split_once('/').ok_or_else(|| format!("{key:?} is not \"Class/id\""))?;
        let def = self.program.object(class).map(|c| &self.program.objects[c as usize]).ok_or_else(|| format!("no object type {class}"))?;
        let rule = match method {
            None => def.read.clone().ok_or_else(|| format!("{class} has no @read rule"))?,
            Some(m) => match def.methods.get(m) {
                Some(md) if md.private => return Err(format!("{class}.{m} is private")),
                Some(md) => md.allow.clone().ok_or_else(|| format!("{class}.{m} has no @allow rule"))?,
                None => return Err(format!("{class} has no method {m}")),
            },
        };
        Ok((rule, id))
    }

    /// Whether a remote caller may read `key` (for the server's change streams).
    pub fn may_read(&self, key: &str, caller: &Caller) -> Result<(), String> {
        match self.refusal(key, None, &Who::from_caller(caller, self.server)) {
            None => Ok(()),
            Some(why) => Err(why),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(words: &[&str]) -> Vec<Rc<str>> {
        words.iter().map(|w| Rc::from(*w)).collect()
    }

    fn remote(id: &str, roles: &[&str]) -> Who {
        Who::Remote { id: Rc::from(id), roles: roles.iter().map(|r| Rc::from(*r)).collect() }
    }

    #[test]
    fn rule_words_and_roles() {
        let anon = remote("", &[]);
        let ana = remote("user:ana", &[]);
        let staff = remote("key:sync", &["staff"]);
        assert!(allows(&rule(&["anyone"]), &anon, "x"));
        assert!(!allows(&rule(&["signedIn"]), &anon, "x") && allows(&rule(&["signedIn"]), &ana, "x"));
        assert!(allows(&rule(&["owner"]), &ana, "user:ana") && !allows(&rule(&["owner"]), &ana, "user:bo"));
        assert!(!allows(&rule(&["owner"]), &anon, ""), "anonymous callers own nothing, even an empty id");
        assert!(allows(&rule(&["owner", "staff"]), &staff, "user:ana") && !allows(&rule(&["staff"]), &ana, "x"));
        assert!(allows(&rule(&[]), &Who::Local, "x") && allows(&rule(&[]), &Who::System, "x"), "local and system code is not checked");
    }
}
