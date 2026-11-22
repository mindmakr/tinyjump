//! Upgrade compatibility checker (`tj check <new.tj> --against <old.tj>` and non-blocking
//! reload notices in `tj dev`): compares two compiled apps' schemas, outbox method signatures
//! (structural `ArgTy`), access rules and personal-data declarations so agentic refactorings
//! cannot silently drop stored fields, break queued device calls or accidentally widen
//! security rules.

use crate::manifest::{arg_ty_from_json, canonical_field_types, Manifest};
use serde_json::{json, Value as Json};
use std::collections::HashMap;
use tj_core::vm::program::{ArgTy, Program};

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

type ParamSpec = (String, bool, ArgTy);

/// Precomputed snapshot of one app version so `tj dev` can cache the previous version without
/// recompiling on every save.
pub struct Snapshot {
    pub manifest: Json,
    /// `(Object, method)` -> compiled `(param_name, optional, ArgTy)` for structural outbox checks.
    method_params: HashMap<(String, String), Vec<ParamSpec>>,
    /// `(Object, field)` -> alias-resolved, canonicalized type string.
    field_types: HashMap<(String, String), String>,
}

impl Snapshot {
    pub fn build(source: &str, program: &Program) -> Snapshot {
        let manifest = Manifest::build(source, program, &[]).json;
        let mut method_params = HashMap::new();
        for o in &program.objects {
            for (mname, m) in &o.methods {
                let params = m.params.iter().map(|(n, opt, ty)| (n.to_string(), *opt, ty.clone())).collect();
                method_params.insert((o.name.to_string(), mname.to_string()), params);
            }
        }
        let field_types = canonical_field_types(source);
        Snapshot { manifest, method_params, field_types }
    }

    /// Builds a snapshot directly from a saved or live `/_tj/manifest` JSON document
    /// (unwrapping an outer `{"ok": ...}` envelope if present).
    pub fn from_manifest_json(raw: Json) -> Result<Snapshot, String> {
        let manifest = match raw.get("ok") {
            Some(inner) if inner.is_object() => inner.clone(),
            _ => raw,
        };
        let objs = manifest
            .get("objects")
            .and_then(Json::as_array)
            .ok_or("manifest JSON must have an \"objects\" array")?;
        let mut method_params = HashMap::new();
        let mut field_types = HashMap::new();
        for o in objs {
            let Some(oname) = o["name"].as_str() else { continue };
            for f in o["fields"].as_array().into_iter().flatten() {
                if let (Some(fname), Some(ty)) = (f["name"].as_str(), f["type"].as_str()) {
                    field_types.insert((oname.to_string(), fname.to_string()), ty.to_string());
                }
            }
            for m in o["methods"].as_array().into_iter().flatten() {
                let Some(mname) = m["name"].as_str() else { continue };
                let params: Vec<ParamSpec> = m["params"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|p| {
                        let pname = p["name"].as_str().unwrap_or("argument").to_string();
                        let opt = p["optional"].as_bool().unwrap_or(false);
                        let ty = p.get("schema").and_then(arg_ty_from_json).unwrap_or(ArgTy::Any);
                        (pname, opt, ty)
                    })
                    .collect();
                method_params.insert((oname.to_string(), mname.to_string()), params);
            }
        }
        Ok(Snapshot { manifest, method_params, field_types })
    }
}

/// Compares `new` against `old` and returns all breaking changes and warnings.
pub fn check(old_src: &str, old_prog: &Program, new_src: &str, new_prog: &Program) -> Vec<Finding> {
    check_snapshots(&Snapshot::build(old_src, old_prog), &Snapshot::build(new_src, new_prog))
}

pub fn check_snapshots(old: &Snapshot, new: &Snapshot) -> Vec<Finding> {
    let mut out = Vec::new();
    let old_objs = by_name(&old.manifest["objects"]);
    let new_objs = by_name(&new.manifest["objects"]);

    for (name, o) in &old_objs {
        let Some(n) = new_objs.get(name) else {
            let has_fields = o["fields"].as_array().is_some_and(|f| !f.is_empty());
            let has_remote_methods = o["methods"].as_array().is_some_and(|ms| ms.iter().any(|m| m["allow"].is_array()));
            if has_fields || has_remote_methods {
                out.push(Finding {
                    severity: Severity::Breaking,
                    code: "object-removed",
                    subject: (*name).to_string(),
                    message: format!("object {name} was removed; existing stored {name}/* records or queued device calls will break"),
                    hint: format!("keep `object {name}` until its stored data and device callers are migrated, or pass --allow-breaking"),
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

        // 2. Object-level `@read` widening and `belongsToPerson` loss.
        if rule_widened(&o["read"], &n["read"]) {
            out.push(Finding {
                severity: Severity::Breaking,
                code: "read-widened",
                subject: (*name).to_string(),
                message: format!("@read on {name} widened from {} to {}", fmt_rule(&o["read"]), fmt_rule(&n["read"])),
                hint: "keep @read the same or narrower, or pass --allow-breaking if exposing more readers is intentional".into(),
            });
        }
        if o["belongsToPerson"].as_bool() == Some(true) && n["belongsToPerson"].as_bool() != Some(true) {
            out.push(Finding {
                severity: Severity::Breaking,
                code: "person-ownership-lost",
                subject: (*name).to_string(),
                message: format!("object {name} no longer belongs to a person (`belongsToPerson` became false), dropping its records from `tj privacy` export and erase"),
                hint: "keep an \"owner\" rule or @pii field on this object, or pass --allow-breaking if migrating records intentionally".into(),
            });
        }

        // 3. Retention / legalHold weakening.
        check_retention(name, o, n, &mut out);

        // 4. Fields: removals (deterministic rename hint), type changes, field `@read` widening, `@pii` / `@by` regressions.
        let old_fields = by_name(&o["fields"]);
        let new_fields = by_name(&n["fields"]);
        let mut added_fields: Vec<(&str, &Json)> = new_fields.iter().filter(|(k, _)| !old_fields.contains_key(*k)).map(|(k, v)| (*k, *v)).collect();
        added_fields.sort_by_key(|(k, _)| *k);

        for (fname, of) in &old_fields {
            let subj = format!("{name}.{fname}");
            let old_ty = old.field_types.get(&((*name).to_string(), (*fname).to_string()));
            let Some(nf) = new_fields.get(fname) else {
                let candidate = added_fields
                    .iter()
                    .find(|(ak, _)| old_ty.is_some() && new.field_types.get(&((*name).to_string(), (*ak).to_string())) == old_ty)
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
            let new_ty = new.field_types.get(&((*name).to_string(), (*fname).to_string()));
            if let (Some(ot), Some(nt)) = (old_ty, new_ty) {
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
            // Effective field `@read` is the intersection of object `@read` and field `@read`.
            let eff_old = effective_field_read(&o["read"], &of["read"]);
            let eff_new = effective_field_read(&n["read"], &nf["read"]);
            if rule_words_widened(&eff_old, &eff_new) {
                out.push(Finding {
                    severity: Severity::Breaking,
                    code: "field-read-widened",
                    subject: subj.clone(),
                    message: format!("@read on {name}.{fname} widened from {} to {}", fmt_words(&eff_old), fmt_words(&eff_new)),
                    hint: "keep the field's @read rule the same or narrower, or pass --allow-breaking if intentional".into(),
                });
            }
            // Removing `"self"` (`namesPerson`) silently drops the field from GDPR export & erasure.
            if of["namesPerson"].as_bool() == Some(true) && nf["namesPerson"].as_bool() != Some(true) {
                out.push(Finding {
                    severity: Severity::Breaking,
                    code: "names-person-removed",
                    subject: subj.clone(),
                    message: format!("\"self\" was removed from @read on {name}.{fname}; records naming a person would no longer be exported or erased for them"),
                    hint: "keep \"self\" in the field's @read rule, or pass --allow-breaking after migrating existing records".into(),
                });
            }
            // `@pii`, `@by`, and special-category regressions.
            match (of["pii"].as_object(), nf["pii"].as_object()) {
                (Some(_), None) => out.push(Finding {
                    severity: Severity::Breaking,
                    code: "pii-removed",
                    subject: subj,
                    message: format!("@pii tag was removed from {name}.{fname}; values would no longer be masked, exported or erased"),
                    hint: "keep @pii on personal data fields, or pass --allow-breaking if the field no longer holds personal data".into(),
                }),
                (Some(op), Some(np)) => {
                    if op.get("by") != np.get("by") {
                        out.push(Finding {
                            severity: Severity::Breaking,
                            code: "pii-by-changed",
                            subject: subj.clone(),
                            message: format!("@by metadata changed on {name}.{fname}; existing list entries will not match export, erasure or retention"),
                            hint: "keep @by(\"person\", \"at\") unchanged for existing stored lists or migrate records first".into(),
                        });
                    }
                    if op.get("special").and_then(Json::as_bool) == Some(true) && np.get("special").and_then(Json::as_bool) != Some(true) {
                        out.push(Finding {
                            severity: Severity::Breaking,
                            code: "pii-special-downgraded",
                            subject: subj.clone(),
                            message: format!("{name}.{fname} changed from a special personal-data category to a non-special category"),
                            hint: "verify the category classification; special categories carry stricter compliance controls".into(),
                        });
                    }
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

        // 5. Methods: removed remote methods, structural `ArgTy` parameter compatibility, `@allow` widening, `@needs` removal.
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
            let old_needs = str_set(om.get("needs"));
            let new_needs = str_set(nm.get("needs"));
            let dropped_needs: Vec<&str> = old_needs.iter().filter(|x| !new_needs.contains(*x)).copied().collect();
            if !dropped_needs.is_empty() {
                out.push(Finding {
                    severity: Severity::Breaking,
                    code: "needs-removed",
                    subject: subj.clone(),
                    message: format!("@needs consent gate(s) removed from {name}.{mname}: {}", dropped_needs.join(", ")),
                    hint: "keep @needs(\"consent:...\") unless consent is no longer required for this method".into(),
                });
            }
            if was_remote && nm["allow"].is_array() {
                let key = ((*name).to_string(), (*mname).to_string());
                if let (Some(op), Some(np)) = (old.method_params.get(&key), new.method_params.get(&key)) {
                    check_params(&subj, op, np, &mut out);
                }
            }
        }
    }

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
    if o["retain"]["from"].as_str() == Some("created") && n["retain"]["from"].as_str() == Some("changed") {
        out.push(Finding {
            severity: Severity::Breaking,
            code: "retain-from-changed",
            subject: name.into(),
            message: format!("@retain on {name} changed from \"created\" to \"changed\" (extending retention on every edit)"),
            hint: "keep @retain(..., \"created\") or pass --allow-breaking if changing retention policy intentionally".into(),
        });
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

fn check_params(subj: &str, old: &[ParamSpec], new: &[ParamSpec], out: &mut Vec<Finding>) {
    if new.len() < old.len() {
        out.push(Finding {
            severity: Severity::Breaking,
            code: "params-removed",
            subject: subj.into(),
            message: format!("{subj} shrank from {} parameter(s) to {}; queued device calls sending {} args will fail with 400", old.len(), new.len(), old.len()),
            hint: "keep trailing parameters as optional (`unused?: any`) during rollout or pass --allow-breaking".into(),
        });
    }
    for (i, (pname, n_opt, n_ty)) in new.iter().enumerate() {
        match old.get(i) {
            None if !n_opt => out.push(Finding {
                severity: Severity::Breaking,
                code: "param-required-added",
                subject: subj.into(),
                message: format!("{subj} added required parameter `{pname}` ({}); queued calls from older devices omit it", n_ty.describe()),
                hint: format!("make `{pname}` optional (`{pname}?: ...` or give it a default value) so older callers keep working"),
            }),
            Some((_, o_opt, o_ty)) => {
                if *o_opt && !n_opt {
                    out.push(Finding {
                        severity: Severity::Breaking,
                        code: "param-made-required",
                        subject: subj.into(),
                        message: format!("{subj} made optional parameter `{pname}` required"),
                        hint: format!("keep `{pname}` optional during rollout or pass --allow-breaking"),
                    });
                }
                if !arg_ty_compatible(o_ty, n_ty) {
                    out.push(Finding {
                        severity: Severity::Breaking,
                        code: "param-type-changed",
                        subject: subj.into(),
                        message: format!("{subj} parameter `{pname}` changed type incompatibly (old callers send {}, new expects {})", o_ty.describe(), n_ty.describe()),
                        hint: "accept a union of old and new types (or add a new method) so queued calls do not fail".into(),
                    });
                }
            }
            None => {}
        }
    }
}

/// True if every JSON value accepted by `old` is also accepted by `new` (contravariant parameter safety).
fn arg_ty_compatible(old: &ArgTy, new: &ArgTy) -> bool {
    if matches!(new, ArgTy::Any) {
        return true;
    }
    if let ArgTy::Union(os) = old {
        return os.iter().all(|o| arg_ty_compatible(o, new));
    }
    if let ArgTy::Union(ns) = new {
        return ns.iter().any(|n| arg_ty_compatible(old, n));
    }
    match (old, new) {
        (ArgTy::Num, ArgTy::Num)
        | (ArgTy::Str, ArgTy::Str)
        | (ArgTy::Bool, ArgTy::Bool)
        | (ArgTy::Null, ArgTy::Null)
        | (ArgTy::StrLit(_), ArgTy::Str)
        | (ArgTy::NumLit(_), ArgTy::Num)
        | (ArgTy::BoolLit(_), ArgTy::Bool) => true,
        (ArgTy::StrLit(a), ArgTy::StrLit(b)) => a == b,
        (ArgTy::NumLit(a), ArgTy::NumLit(b)) => a == b,
        (ArgTy::BoolLit(a), ArgTy::BoolLit(b)) => a == b,
        (ArgTy::Arr(o), ArgTy::Arr(n)) => arg_ty_compatible(o, n),
        (ArgTy::Dict(o), ArgTy::Dict(n)) => arg_ty_compatible(o, n),
        (ArgTy::Rec(of), ArgTy::Rec(nf)) => {
            // Every field an old caller might send must be allowed by `new` with a compatible type,
            // and `new` may not require any field that `old` did not require.
            let old_fits = of.iter().all(|(k, o_opt, ot)| {
                nf.iter().any(|(nk, n_opt, nt)| nk == k && (!*o_opt || *n_opt) && arg_ty_compatible(ot, nt))
            });
            let req_met = nf.iter().all(|(nk, n_opt, _)| *n_opt || of.iter().any(|(k, o_opt, _)| k == nk && !*o_opt));
            old_fits && req_met
        }
        _ => false,
    }
}

/// Effective remote readers of a field: empty whenever the object itself is not remotely readable.
fn effective_field_read<'a>(obj_read: &'a Json, field_read: &'a Json) -> Vec<&'a str> {
    let obj = str_set(Some(obj_read));
    if obj.is_empty() || obj.iter().all(|w| *w == "none") {
        return Vec::new();
    }
    let fld = str_set(Some(field_read));
    if fld.is_empty() {
        obj
    } else if fld.iter().all(|w| *w == "none") {
        Vec::new()
    } else {
        fld
    }
}

fn rule_widened(old_rule: &Json, new_rule: &Json) -> bool {
    rule_words_widened(&str_set(Some(old_rule)), &str_set(Some(new_rule)))
}

/// True if `new` admits any caller that `old` refused.
fn rule_words_widened(old: &[&str], new: &[&str]) -> bool {
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
        return false;
    }
    if new.contains(&"signedIn") {
        return true;
    }
    new.iter().any(|w| !old.contains(w))
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
    fmt_words(&str_set(Some(v)))
}

fn fmt_words(words: &[&str]) -> String {
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
    fn additive_tightening_union_widening_and_alias_refactors_are_compatible() {
        let old = r#"
type Cents = number
@server @read("anyone") object Cart {
  stock: Cents = 10
  meta: { a: number, b: string } = { a: 0, b: "" }
  @allow("anyone") add(qty: number) { this.stock -= qty }
}
@server object Vault {
  email: string = ""
}
"#;
        let new = r#"
@server @read("signedIn") object Cart {
  stock: number = 10
  meta: { b: string, a: number } = { a: 0, b: "" }
  note: string = ""
  @invariant nonNeg(): boolean { return this.stock >= 0 }
  @allow("signedIn") add(qty: number | string, note?: string) { if (note !== null) { this.note = note } }
}
@server object Vault {
  @pii("contact", "account") email: string = ""
}
"#;
        let f = diff(old, new);
        assert!(f.is_empty(), "expected compatible upgrade, got:\n{}", report(&f));
    }

    #[test]
    fn catches_record_param_breaks_privacy_regressions_and_stateless_object_removal() {
        let old = r#"
@server @read("owner") @retain("30d", "created") @legalHold("365d") object Account {
  @pii("health", "care") email: string = ""
  qty: number = 0
  @read("owner") secret: string = ""
  @allow("owner") update(item: { price: number }) { this.qty = item.price }
}
@server @read("anyone") object Ping {
  @allow("anyone") ok(): boolean { return true }
}
"#;
        let new = r#"
@server @read("anyone") @retain("30d", "changed") object Account {
  @pii("contact", "care") email: string = ""
  quantity: number = 0
  secret: string = ""
  @allow("anyone") update(item: { price: string }, extra: number) { this.quantity = extra }
}
"#;
        let f = diff(old, new);
        let codes: Vec<&str> = f.iter().map(|x| x.code).collect();
        for want in ["read-widened", "retain-from-changed", "legal-hold-removed", "pii-special-downgraded", "field-removed", "field-read-widened", "allow-widened", "param-type-changed", "param-required-added", "object-removed"] {
            assert!(codes.contains(&want), "missing {want} in {codes:?}\n{}", report(&f));
        }
        let fld_read = f.iter().find(|x| x.code == "field-read-widened").unwrap();
        assert!(fld_read.message.contains("from owner to anyone"), "effective rule shown cleanly: {}", fld_read.message);
        let rename = f.iter().find(|x| x.code == "field-removed").unwrap();
        assert!(rename.hint.contains("Account.quantity"), "should suggest rename candidate: {}", rename.hint);
    }

    #[test]
    fn infers_literal_field_types_catches_person_ownership_loss_and_compares_manifest_json() {
        let old = r#"
@server @read("owner", "staff") object Order {
  qty = 0
  @allow("owner") update(item: { qty: number }) { this.qty = item.qty }
}
"#;
        let new = r#"
@server @read("staff") object Order {
  qty = "none"
  @allow("staff") update(item: { qty: string }) {}
}
"#;
        let f = diff(old, new);
        let codes: Vec<&str> = f.iter().map(|x| x.code).collect();
        assert!(codes.contains(&"field-type-changed"), "untyped literal default change must be caught: {codes:?}");
        assert!(codes.contains(&"person-ownership-lost"), "dropping owner rule loses belongsToPerson: {codes:?}");
        assert!(codes.contains(&"param-type-changed"), "{codes:?}");

        // Comparing against a live `/_tj/manifest` JSON envelope matches `.tj` comparison losslessly.
        let old_prog = tj_core::compile(old).unwrap();
        let new_prog = tj_core::compile(new).unwrap();
        let live_envelope = json!({ "ok": Manifest::build(old, &old_prog, &[]).json });
        let old_snap = Snapshot::from_manifest_json(live_envelope).unwrap();
        let new_snap = Snapshot::build(new, &new_prog);
        let f_json = check_snapshots(&old_snap, &new_snap);
        let codes_json: Vec<&str> = f_json.iter().map(|x| x.code).collect();
        assert_eq!(codes, codes_json);

        // C-037.5: comparing an unchanged app containing type aliases, unsorted unions
        // (`string | null`), and unsorted record fields against its own `Manifest::build` JSON
        // must produce zero findings.
        let tricky = r#"
type Cents = number
@server @read("anyone") object Item {
  price: Cents = 0
  note: string | null = null
  meta: { b: string, a: number } = { a: 0, b: "" }
  @allow("anyone") ping() {}
}
"#;
        let tp = tj_core::compile(tricky).unwrap();
        let m_snap = Snapshot::from_manifest_json(Manifest::build(tricky, &tp, &[]).json).unwrap();
        let s_snap = Snapshot::build(tricky, &tp);
        let self_diff = check_snapshots(&m_snap, &s_snap);
        assert!(self_diff.is_empty(), "unchanged app vs own manifest must have 0 findings: {}", report(&self_diff));

        // T1-6: removing `"self"` (`namesPerson`) from `@read("staff", "self")` is a Breaking change.
        let with_self = r#"
@server @read("staff", "signedIn") object Slot {
  @read("staff", "self") customer: string = ""
  @allow("staff") assign(c: string) { this.customer = c }
}
"#;
        let without_self = r#"
@server @read("staff", "signedIn") object Slot {
  @read("staff") customer: string = ""
  @allow("staff") assign(c: string) { this.customer = c }
}
"#;
        let self_rm = diff(with_self, without_self);
        assert!(self_rm.iter().any(|x| x.code == "names-person-removed" && x.severity == Severity::Breaking), "{}", report(&self_rm));
    }
}
