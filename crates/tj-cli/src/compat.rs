//! Upgrade compatibility checker (`tj check <new.tj> --against <old.tj>` and non-blocking
//! reload notices in `tj dev`): compares two compiled apps' schemas, outbox method signatures,
//! access rules and personal-data declarations so agentic refactorings cannot silently drop
//! stored fields, break queued device calls or accidentally widen security rules.

use crate::manifest::Manifest;
use serde_json::{json, Value as Json};
use std::collections::HashMap;
use tj_core::vm::program::Program;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Breaking,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub severity: Severity,
    pub code: &'static str,
    pub subject: String,
    pub message: String,
    pub hint: String,
}

impl Finding {
    pub fn json(&self) -> Json {
        json!({
            "severity": match self.severity { Severity::Breaking => "breaking", Severity::Warning => "warning" },
            "code": self.code,
            "subject": self.subject,
            "message": self.message,
            "hint": self.hint,
        })
    }
}

/// Compares `new` against `old` and returns all breaking changes and warnings.
pub fn check(old_src: &str, old_prog: &Program, new_src: &str, new_prog: &Program) -> Vec<Finding> {
    let old = Manifest::build(old_src, old_prog, &[]).json;
    let new = Manifest::build(new_src, new_prog, &[]).json;
    check_manifests(&old, &new)
}

pub fn check_manifests(old: &Json, new: &Json) -> Vec<Finding> {
    let mut out = Vec::new();
    let old_objs = by_name(&old["objects"]);
    let new_objs = by_name(&new["objects"]);

    for (name, o) in &old_objs {
        let Some(n) = new_objs.get(name) else {
            // Only persistent objects with fields risk losing stored data.
            if o["fields"].as_array().is_some_and(|f| !f.is_empty()) {
                out.push(Finding {
                    severity: Severity::Breaking,
                    code: "object-removed",
                    subject: (*name).to_string(),
                    message: format!("object {name} was removed; existing stored {name}/* records will become unreachable"),
                    hint: format!("keep `object {name}` until its stored data is migrated or pass --allow-breaking if intentional"),
                });
            }
            continue;
        };

        // 1. `@server` vs local change.
        if o["server"] != n["server"] {
            out.push(Finding {
                severity: Severity::Breaking,
                code: "server-changed",
                subject: (*name).to_string(),
                message: format!("object {name} changed between @server and local"),
                hint: "moving an object between @server and local breaks existing replicas and stored data".into(),
            });
        }

        // 2. Object-level `@read` widening.
        if rule_widened(&o["read"], &n["read"]) {
            out.push(Finding {
                severity: Severity::Breaking,
                code: "read-widened",
                subject: (*name).to_string(),
                message: format!("@read on {name} widened from {} to {}", fmt_rule(&o["read"]), fmt_rule(&n["read"])),
                hint: "keep @read the same or narrower, or pass --allow-breaking if exposing more readers is intentional".into(),
            });
        }

        // 3. Retention / legalHold weakening.
        check_retention(name, o, n, &mut out);

        // 4. Fields: removals (with rename hint), type changes, field `@read` widening, `@pii` removal/widening.
        let old_fields = by_name(&o["fields"]);
        let new_fields = by_name(&n["fields"]);
        let added_fields: Vec<(&str, &Json)> = new_fields.iter().filter(|(k, _)| !old_fields.contains_key(*k)).map(|(k, v)| (*k, *v)).collect();

        for (fname, of) in &old_fields {
            let subj = format!("{name}.{fname}");
            let Some(nf) = new_fields.get(fname) else {
                let candidate = added_fields
                    .iter()
                    .find(|(_, af)| of.get("type").is_some() && af.get("type") == of.get("type"))
                    .map(|(k, _)| *k);
                let hint = match candidate {
                    Some(c) => format!("did you rename {name}.{fname} to {name}.{c}? Keep `{fname}` (or migrate stored records) so existing values are not dropped"),
                    None => format!("keep field `{fname}` on {name} until stored records are migrated, or pass --allow-breaking"),
                };
                out.push(Finding {
                    severity: Severity::Breaking,
                    code: "field-removed",
                    subject: subj,
                    message: format!("field {name}.{fname} was removed; existing stored values will be dropped on the next save"),
                    hint,
                });
                continue;
            };
            if let (Some(ot), Some(nt)) = (of["type"].as_str(), nf["type"].as_str()) {
                if ot != nt {
                    out.push(Finding {
                        severity: Severity::Breaking,
                        code: "field-type-changed",
                        subject: subj.clone(),
                        message: format!("field {name}.{fname} changed type from `{ot}` to `{nt}`"),
                        hint: "add a new field with the new type instead of changing an existing stored field's type".into(),
                    });
                }
            }
            // Field-level `@read` widening (removing a field `@read` exposes it to all object readers!).
            if field_read_widened(&o["read"], &of["read"], &n["read"], &nf["read"]) {
                out.push(Finding {
                    severity: Severity::Breaking,
                    code: "field-read-widened",
                    subject: subj.clone(),
                    message: format!("@read on {name}.{fname} widened from {} to {}", fmt_rule(&of["read"]), fmt_rule(&nf["read"])),
                    hint: "keep the field's @read rule the same or narrower, or pass --allow-breaking if intentional".into(),
                });
            }
            // `@pii` removal or purpose widening.
            match (of["pii"].as_object(), nf["pii"].as_object()) {
                (Some(_), None) => out.push(Finding {
                    severity: Severity::Breaking,
                    code: "pii-removed",
                    subject: subj,
                    message: format!("@pii tag was removed from {name}.{fname}; values would no longer be masked, exported or erased"),
                    hint: "keep @pii on personal data fields, or pass --allow-breaking if the field no longer holds personal data".into(),
                }),
                (Some(op), Some(np)) => {
                    let old_p = str_set(op.get("purposes"));
                    let new_p = str_set(np.get("purposes"));
                    let added: Vec<&str> = new_p.iter().filter(|p| !old_p.contains(*p)).copied().collect();
                    if !added.is_empty() {
                        out.push(Finding {
                            severity: Severity::Warning,
                            code: "pii-purpose-added",
                            subject: subj,
                            message: format!("new @pii purpose(s) added to {name}.{fname}: {}", added.join(", ")),
                            hint: "verify lawfulBasis and user notice cover the added purpose(s)".into(),
                        });
                    }
                }
                _ => {}
            }
        }

        // 5. Methods: removed remote methods, parameter breaks (outbox compatibility), `@allow` widening.
        let old_methods = by_name(&o["methods"]);
        let new_methods = by_name(&n["methods"]);
        for (mname, om) in &old_methods {
            let subj = format!("{name}.{mname}");
            let was_remote = om["allow"].is_array();
            let Some(nm) = new_methods.get(mname) else {
                if was_remote {
                    out.push(Finding {
                        severity: Severity::Breaking,
                        code: "method-removed",
                        subject: subj,
                        message: format!("remote method {name}.{mname} was removed; queued offline calls from older devices will fail"),
                        hint: "keep the method during rollout (even as a forwarding shim) or pass --allow-breaking".into(),
                    });
                }
                continue;
            };
            if was_remote && !nm["allow"].is_array() {
                out.push(Finding {
                    severity: Severity::Breaking,
                    code: "method-unexposed",
                    subject: subj.clone(),
                    message: format!("remote method {name}.{mname} lost its @allow rule; queued device calls will get 403"),
                    hint: "keep @allow during client rollout or pass --allow-breaking".into(),
                });
            }
            if rule_widened(&om["allow"], &nm["allow"]) {
                out.push(Finding {
                    severity: Severity::Breaking,
                    code: "allow-widened",
                    subject: subj.clone(),
                    message: format!("@allow on {name}.{mname} widened from {} to {}", fmt_rule(&om["allow"]), fmt_rule(&nm["allow"])),
                    hint: "keep @allow the same or narrower, or pass --allow-breaking if intentional".into(),
                });
            }
            if was_remote && nm["allow"].is_array() {
                check_params(&subj, &om["params"], &nm["params"], &mut out);
            }
        }
    }

    // Also flag newly added objects that have no issues unless they have access warnings.
    out.sort_by(|a, b| a.severity.cmp(&b.severity).then(a.subject.cmp(&b.subject)).then(a.code.cmp(b.code)));
    out
}

fn check_retention(name: &str, o: &Json, n: &Json, out: &mut Vec<Finding>) {
    match (o["retain"]["ms"].as_f64(), n["retain"]["ms"].as_f64()) {
        (Some(_), None) => out.push(Finding {
            severity: Severity::Breaking,
            code: "retain-removed",
            subject: name.into(),
            message: format!("@retain was removed from {name}; data would now be kept indefinitely"),
            hint: "keep @retain or pass --allow-breaking if changing retention policy intentionally".into(),
        }),
        (Some(old_ms), Some(new_ms)) if new_ms > old_ms => out.push(Finding {
            severity: Severity::Breaking,
            code: "retain-lengthened",
            subject: name.into(),
            message: format!("@retain on {name} lengthened from {old_ms}ms to {new_ms}ms"),
            hint: "keep retention the same or shorter, or pass --allow-breaking if policy changed".into(),
        }),
        _ => {}
    }
    match (o["legalHoldMs"].as_f64(), n["legalHoldMs"].as_f64()) {
        (Some(_), None) => out.push(Finding {
            severity: Severity::Breaking,
            code: "legal-hold-removed",
            subject: name.into(),
            message: format!("@legalHold was removed from {name}"),
            hint: "keep @legalHold or pass --allow-breaking if legal requirements changed".into(),
        }),
        (Some(old_ms), Some(new_ms)) if new_ms < old_ms => out.push(Finding {
            severity: Severity::Breaking,
            code: "legal-hold-shortened",
            subject: name.into(),
            message: format!("@legalHold on {name} shortened from {old_ms}ms to {new_ms}ms"),
            hint: "keep legal hold the same or longer, or pass --allow-breaking if legal requirements changed".into(),
        }),
        _ => {}
    }
}

fn check_params(subj: &str, old_params: &Json, new_params: &Json, out: &mut Vec<Finding>) {
    let old = old_params.as_array().map(Vec::as_slice).unwrap_or(&[]);
    let new = new_params.as_array().map(Vec::as_slice).unwrap_or(&[]);
    if new.len() < old.len() {
        out.push(Finding {
            severity: Severity::Breaking,
            code: "params-removed",
            subject: subj.into(),
            message: format!("{subj} shrank from {} parameter(s) to {}; queued device calls sending {} args will fail with 400", old.len(), new.len(), old.len()),
            hint: "keep trailing parameters as optional (`unused?: any`) during rollout or pass --allow-breaking".into(),
        });
    }
    for (i, np) in new.iter().enumerate() {
        let pname = np["name"].as_str().unwrap_or("arg");
        let n_opt = np["optional"].as_bool().unwrap_or(false);
        let n_ty = np["type"].as_str().unwrap_or("anything");
        match old.get(i) {
            None if !n_opt => out.push(Finding {
                severity: Severity::Breaking,
                code: "param-required-added",
                subject: subj.into(),
                message: format!("{subj} added required parameter `{pname}` ({n_ty}); queued calls from older devices omit it"),
                hint: format!("make `{pname}` optional (`{pname}?: ...` or give it a default value) so older callers keep working"),
            }),
            Some(op) => {
                let o_opt = op["optional"].as_bool().unwrap_or(false);
                let o_ty = op["type"].as_str().unwrap_or("anything");
                if o_opt && !n_opt {
                    out.push(Finding {
                        severity: Severity::Breaking,
                        code: "param-made-required",
                        subject: subj.into(),
                        message: format!("{subj} made optional parameter `{pname}` required"),
                        hint: format!("keep `{pname}` optional during rollout or pass --allow-breaking"),
                    });
                }
                if o_ty != n_ty && n_ty != "anything" {
                    out.push(Finding {
                        severity: Severity::Breaking,
                        code: "param-type-changed",
                        subject: subj.into(),
                        message: format!("{subj} parameter `{pname}` changed type from {o_ty} to {n_ty}"),
                        hint: "accept a union of old and new types (or add a new method) so queued calls do not fail".into(),
                    });
                }
            }
            None => {}
        }
    }
}

/// True if `new_rule` admits any caller that `old_rule` refused.
fn rule_widened(old_rule: &Json, new_rule: &Json) -> bool {
    let old = str_set(Some(old_rule));
    let new = str_set(Some(new_rule));
    if new.is_empty() || new.iter().all(|w| *w == "none") {
        return false;
    }
    if old.is_empty() || old.iter().all(|w| *w == "none") {
        return true;
    }
    if old.contains(&"anyone") {
        return false;
    }
    if new.contains(&"anyone") {
        return true;
    }
    if old.contains(&"signedIn") {
        // Only "anyone" is wider than "signedIn", already handled above.
        return false;
    }
    if new.contains(&"signedIn") {
        return true;
    }
    // Otherwise every word in `new` must have been in `old`.
    new.iter().any(|w| !old.contains(w))
}

fn field_read_widened(old_obj: &Json, old_field: &Json, new_obj: &Json, new_field: &Json) -> bool {
    let eff_old = if old_field.is_array() { old_field } else { old_obj };
    let eff_new = if new_field.is_array() { new_field } else { new_obj };
    rule_widened(eff_old, eff_new)
}

fn by_name(list: &Json) -> HashMap<&str, &Json> {
    list.as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v["name"].as_str().map(|n| (n, v)))
        .collect()
}

fn str_set(v: Option<&Json>) -> Vec<&str> {
    v.and_then(Json::as_array).into_iter().flatten().filter_map(Json::as_str).collect()
}

fn fmt_rule(v: &Json) -> String {
    let words = str_set(Some(v));
    if words.is_empty() { "not remote".into() } else { words.join(", ") }
}

pub fn has_breaking(findings: &[Finding]) -> bool {
    findings.iter().any(|f| f.severity == Severity::Breaking)
}

pub fn report(findings: &[Finding]) -> String {
    if findings.is_empty() {
        return "upgrade check: compatible (0 breaking changes, 0 warnings)\n".into();
    }
    let mut out = String::new();
    for f in findings {
        let tag = match f.severity {
            Severity::Breaking => "BREAKING",
            Severity::Warning => "warning ",
        };
        out.push_str(&format!("{tag} [{}] {}: {}\n  hint: {}\n", f.code, f.subject, f.message, f.hint));
    }
    let breaking = findings.iter().filter(|f| f.severity == Severity::Breaking).count();
    let warnings = findings.len() - breaking;
    out.push_str(&format!("upgrade check: {breaking} breaking, {warnings} warning(s)\n"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diff(old: &str, new: &str) -> Vec<Finding> {
        let op = tj_core::compile(old).expect("old compiles");
        let np = tj_core::compile(new).expect("new compiles");
        check(old, &op, new, &np)
    }

    #[test]
    fn additive_and_tightening_changes_are_compatible() {
        let old = r#"
@server @read("anyone") object Cart {
  stock: number = 10
  @allow("anyone") add(qty: number) { this.stock -= qty }
}
"#;
        let new = r#"
@server @read("signedIn") object Cart {
  stock: number = 10
  note: string = ""
  @invariant nonNeg(): boolean { return this.stock >= 0 }
  @allow("signedIn") add(qty: number, note?: string) { this.stock -= qty; if (note !== null) { this.note = note } }
}
"#;
        let f = diff(old, new);
        assert!(f.is_empty(), "expected compatible upgrade, got:\n{}", report(&f));
    }

    #[test]
    fn catches_field_renames_type_changes_outbox_breaks_and_rule_widenings() {
        let old = r#"
@server @read("owner") @retain("30d") @legalHold("365d") object Account {
  @pii("contact", "account") email: string = ""
  qty: number = 0
  @read("owner") secret: string = ""
  @allow("owner") update(n: number) { this.qty = n }
}
"#;
        let new = r#"
@server @read("anyone") object Account {
  email: string = ""
  quantity: number = 0
  secret: string = ""
  @allow("anyone") update(n: string, extra: number) { this.quantity = extra }
}
"#;
        let f = diff(old, new);
        let codes: Vec<&str> = f.iter().map(|x| x.code).collect();
        for want in ["read-widened", "retain-removed", "legal-hold-removed", "pii-removed", "field-removed", "field-read-widened", "allow-widened", "param-type-changed", "param-required-added"] {
            assert!(codes.contains(&want), "missing {want} in {codes:?}\n{}", report(&f));
        }
        let rename = f.iter().find(|x| x.code == "field-removed").unwrap();
        assert!(rename.hint.contains("Account.quantity"), "should suggest rename candidate: {}", rename.hint);
    }
}
