//! Canonical App Manifest (`tj check --manifest [--json]` and `GET /_tj/manifest`):
//! one typed contract of an app's files, code hash, objects, fields, access rules,
//! personal data, retention, methods, plugins, outbound transfers and warnings.
//!
//! Static fields come from the compiled `Program`, the AST, `tj_core::facts` and
//! `plugins.json`. Live servers attach `health` and active `leases` without exposing any
//! user data or object ids.

use crate::access;
use crate::identity::sha256_hex;
use crate::privacy;
use serde_json::{json, Map, Value as Json};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use tj_core::modules;
use tj_core::syntax::ast::{Decorator, ExprKind, Item, TypeExpr};
use tj_core::syntax::parse_range;
use tj_core::vm::program::{ArgTy, ObjectDef, Program, SPECIAL_CATEGORIES};

#[derive(Clone)]
pub struct Manifest {
    pub json: Json,
}

/// Object names declared by built-in plugin modules (`tj:ui`, `tj:consent`, ...),
/// excluded from `.tjb` manifests so they match `.tj` source manifests across reloads.
fn builtin_plugin_objects() -> HashSet<String> {
    let mut names = HashSet::new();
    for &(_, src) in modules::BUILT_IN {
        if let Ok(parsed) = tj_core::syntax::parse(src) {
            for item in parsed.items {
                if let Item::Object(o) = item {
                    names.insert(o.name);
                }
            }
        }
    }
    names
}

/// AST metadata for one user-declared object, complementing `Program`'s compiled `ObjectDef`.
#[derive(Default)]
struct AstObject {
    field_types: HashMap<String, String>,
    method_needs: HashMap<String, Vec<String>>,
}

impl Manifest {
    /// Builds the static manifest for a compiled app bundle (or precompiled AOT `.tjb` bundle).
    pub fn build(source: &str, program: &Program, configured_plugins: &[&str]) -> Manifest {
        let app_sha256 = sha256_hex(source);
        let is_aot = source.starts_with(tj_core::vm::bytecode::BUNDLE_PREFIX);
        let files: Vec<String> = if is_aot { Vec::new() } else { modules::files(source).into_iter().map(|f| f.name).collect() };
        let mut ast_objects: HashMap<String, AstObject> = HashMap::new();
        let mut imported_plugins: BTreeSet<String> = BTreeSet::new();

        if !is_aot {
            let canon_types = canonical_field_types(source);
            for f in modules::files(source) {
                if tj_core::i18n::is_locale_file(&f.name) {
                    continue;
                }
                let Ok(parsed) = parse_range(source, f.start, f.end) else { continue };
                for imp in &parsed.imports {
                    if imp.from.starts_with("tj:") {
                        imported_plugins.insert(imp.from.clone());
                    }
                }
                for item in &parsed.items {
                    let Item::Object(o) = item else { continue };
                    let field_types = o
                        .fields
                        .iter()
                        .filter_map(|fl| canon_types.get(&(o.name.clone(), fl.name.clone())).map(|t| (fl.name.clone(), t.clone())))
                        .collect();
                    let mut method_needs = HashMap::new();
                    for m in &o.methods {
                        let needs = decorator_strings(&m.decorators, "needs");
                        if !needs.is_empty() {
                            method_needs.insert(m.func.name.clone(), needs);
                        }
                    }
                    ast_objects.insert(o.name.clone(), AstObject { field_types, method_needs });
                }
            }
        }

        // For `.tj` source, include only the app's own objects (built-in plugin objects like
        // `tj:ui`'s Form/Actions are represented under `plugins.imported`). For precompiled AOT
        // `.tjb` bundles (no AST), emit every compiled `ObjectDef` excluding built-in plugin
        // objects so `/_tj/manifest` and upgrade compatibility checks stay consistent across
        // `.tj` and `.tjb` reloads.
        let empty_ast = AstObject::default();
        let objects: Vec<Json> = if is_aot {
            let builtin_objs = builtin_plugin_objects();
            program
                .objects
                .iter()
                .filter(|o| !builtin_objs.contains(&*o.name))
                .map(|o| object_json(o, &empty_ast))
                .collect()
        } else {
            program.objects.iter().filter_map(|o| ast_objects.get(&*o.name).map(|ast| object_json(o, ast))).collect()
        };

        let mut configured: Vec<String> = configured_plugins.iter().map(|s| (*s).to_string()).collect();
        configured.sort();
        configured.dedup();

        let (lawful_basis, reveals, transfers, warnings) = if is_aot {
            (Map::new(), Vec::new(), Vec::new(), Vec::new())
        } else {
            let facts = tj_core::facts(source);
            let lawful_basis: Map<String, Json> = facts
                .lawful_basis
                .unwrap_or_default()
                .into_iter()
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .map(|(k, v)| (k, json!(v)))
                .collect();
            let reveals: Vec<Json> = facts
                .reveals
                .iter()
                .map(|r| json!({ "at": privacy::place(source, r.at), "fields": r.fields, "purpose": r.purpose }))
                .collect();
            let transfers: Vec<Json> = facts
                .transfers
                .iter()
                .map(|t| json!({ "at": privacy::place(source, t.at), "fields": t.fields, "to": privacy::destination(&t.to) }))
                .collect();
            (lawful_basis, reveals, transfers, access::warnings(source))
        };
        let mut json = json!({
            "appSha256": app_sha256,
            "files": files,
            "hasUi": program.app.is_some(),
            "objects": objects,
            "plugins": {
                "imported": imported_plugins.into_iter().collect::<Vec<_>>(),
                "configured": configured,
            },
            "devicePermissions": [],
            "privacy": {
                "lawfulBasis": lawful_basis,
                "reveals": reveals,
                "transfers": transfers,
            },
            "warnings": warnings,
        });
        if is_aot {
            json["aot"] = json!(true);
        }
        Manifest { json }
    }

    /// Attaches validated device capability permissions (from `tj.lock` and optional `permissions.json`).
    pub fn with_device_permissions(&mut self, perms: &BTreeMap<String, crate::device_perms::DevicePermission>) {
        self.json["devicePermissions"] = json!(crate::device_perms::manifest_json(perms));
    }

    /// The static manifest plus live server health and active work leases (for `GET /_tj/manifest`).
    pub fn with_live(&self, health: Json, leases: Json) -> Json {
        let mut live = self.json.clone();
        live["health"] = health;
        live["leases"] = leases;
        live
    }

    /// Compact plain-text summary for `tj check <app.tj> --manifest`.
    pub fn text(&self) -> String {
        let j = &self.json;
        let mut out = String::new();
        let sha = j["appSha256"].as_str().unwrap_or("");
        let short = &sha[..sha.len().min(12)];
        let files: Vec<&str> = j["files"].as_array().map(|a| a.iter().filter_map(Json::as_str).collect()).unwrap_or_default();
        let ui = if j["hasUi"].as_bool() == Some(true) { "yes" } else { "no" };
        out.push_str(&format!("app: {short}  files: {}  ui: {ui}\n", files.join(", ")));

        let imported: Vec<&str> = j["plugins"]["imported"].as_array().map(|a| a.iter().filter_map(Json::as_str).collect()).unwrap_or_default();
        let configured: Vec<&str> = j["plugins"]["configured"].as_array().map(|a| a.iter().filter_map(Json::as_str).collect()).unwrap_or_default();
        if !imported.is_empty() || !configured.is_empty() {
            let mut parts = Vec::new();
            if !imported.is_empty() {
                parts.push(format!("imported: {}", imported.join(", ")));
            }
            if !configured.is_empty() {
                parts.push(format!("configured: {}", configured.join(", ")));
            }
            out.push_str(&format!("plugins: {}\n", parts.join("; ")));
        }
        if let Some(dev_perms) = j["devicePermissions"].as_array() {
            if !dev_perms.is_empty() {
                let items: Vec<String> = dev_perms
                    .iter()
                    .filter_map(|p| {
                        let name = p["name"].as_str()?;
                        let srcs: Vec<&str> = p["sources"].as_array().map(|a| a.iter().filter_map(Json::as_str).collect()).unwrap_or_default();
                        if srcs.is_empty() {
                            Some(name.to_string())
                        } else {
                            Some(format!("{name} ({})", srcs.join(", ")))
                        }
                    })
                    .collect();
                if !items.is_empty() {
                    out.push_str(&format!("device permissions: {}\n", items.join(", ")));
                }
            }
        }

        let objs = j["objects"].as_array().map(Vec::as_slice).unwrap_or(&[]);
        if objs.is_empty() {
            out.push_str("no objects\n");
        }
        for o in objs {
            let where_ = if o["server"].as_bool() == Some(true) { "@server object" } else { "object" };
            let name = o["name"].as_str().unwrap_or("");
            let read = str_list(&o["read"]).unwrap_or_else(|| "not remote".into());
            let mut meta = vec![format!("read: {read}")];
            if o["belongsToPerson"].as_bool() == Some(true) {
                meta.push("personal".into());
            }
            if let Some(r) = o["retain"].as_object() {
                let ms = r.get("ms").and_then(Json::as_f64).unwrap_or(0.0);
                let from = r.get("from").and_then(Json::as_str).unwrap_or("changed");
                meta.push(format!("retain: {} from {from}", format_ms(ms)));
            }
            if let Some(ms) = o["legalHoldMs"].as_f64() {
                meta.push(format!("legalHold: {}", format_ms(ms)));
            }
            out.push_str(&format!("{where_} {name}  ({})\n", meta.join("; ")));
            for f in o["fields"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
                let fname = f["name"].as_str().unwrap_or("");
                let ty = f["type"].as_str().map(|t| format!(": {t}")).unwrap_or_default();
                let mut tags = Vec::new();
                if let Some(r) = str_list(&f["read"]) {
                    tags.push(format!("read: {r}"));
                }
                if let Some(p) = f["pii"].as_object() {
                    let cat = p.get("category").and_then(Json::as_str).unwrap_or("");
                    let purp = p.get("purposes").and_then(str_list).unwrap_or_default();
                    let special = if p.get("special").and_then(Json::as_bool) == Some(true) { " special" } else { "" };
                    let by = p.get("by").and_then(Json::as_object).map(|b| format!(" by {}", b.get("person").and_then(Json::as_str).unwrap_or(""))).unwrap_or_default();
                    tags.push(format!("pii: {cat}{special} ({purp}){by}"));
                } else if f["namesPerson"].as_bool() == Some(true) {
                    tags.push("names person".into());
                }
                let suffix = if tags.is_empty() { String::new() } else { format!("  [{}]", tags.join("; ")) };
                out.push_str(&format!("  .{fname}{ty}{suffix}\n"));
            }
            for m in o["methods"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
                let mname = m["name"].as_str().unwrap_or("");
                let call = if m["private"].as_bool() == Some(true) {
                    "private".into()
                } else {
                    str_list(&m["allow"]).unwrap_or_else(|| "not remote".into())
                };
                let mut tags = vec![format!("call: {call}")];
                if m["exclusive"].as_bool() == Some(true) {
                    tags.push("exclusive".into());
                }
                if m["invariant"].as_bool() == Some(true) {
                    tags.push("invariant".into());
                }
                if let Some(ms) = m["everyMs"].as_f64() {
                    tags.push(format!("every {}", format_ms(ms)));
                }
                if let Some(needs) = str_list(&m["needs"]) {
                    tags.push(format!("needs: {needs}"));
                }
                out.push_str(&format!("  {mname}()  {}\n", tags.join("; ")));
            }
        }

        if let Some(basis) = j["privacy"]["lawfulBasis"].as_object().filter(|m| !m.is_empty()) {
            let items: Vec<String> = basis.iter().map(|(k, v)| format!("{k}: {}", v.as_str().unwrap_or(""))).collect();
            out.push_str(&format!("lawfulBasis: {}\n", items.join(", ")));
        }
        for r in j["privacy"]["reveals"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
            let fields = str_list(&r["fields"]).unwrap_or_default();
            out.push_str(&format!("reveal: {}  {} for {}\n", r["at"].as_str().unwrap_or(""), fields, r["purpose"].as_str().unwrap_or("")));
        }
        for t in j["privacy"]["transfers"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
            let fields = str_list(&t["fields"]).unwrap_or_default();
            out.push_str(&format!("transfer: {}  {} -> {}\n", t["at"].as_str().unwrap_or(""), fields, t["to"].as_str().unwrap_or("")));
        }
        for w in j["warnings"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
            if let Some(s) = w.as_str() {
                out.push_str(&format!("warning: {s}\n"));
            }
        }
        out
    }
}

fn object_json(o: &ObjectDef, ast: &AstObject) -> Json {
    let fields: Vec<Json> = o
        .fields
        .iter()
        .map(|name| {
            let mut f = Map::new();
            f.insert("name".into(), json!(&**name));
            if let Some(ty) = ast.field_types.get(&**name) {
                f.insert("type".into(), json!(ty));
            }
            if let Some(guard) = o.guards.iter().find(|g| g.field == *name) {
                let rule: Vec<&str> = guard.rule.iter().map(|r| &**r).collect();
                f.insert("read".into(), json!(rule));
                if guard.names_a_person() {
                    f.insert("namesPerson".into(), json!(true));
                }
            }
            if let Some(pii) = o.pii(name) {
                let mut p = json!({
                    "category": &*pii.category,
                    "purposes": pii.purposes.iter().map(|s| &**s).collect::<Vec<_>>(),
                    "special": SPECIAL_CATEGORIES.contains(&&*pii.category),
                });
                if let Some((who, at)) = &pii.by {
                    p["by"] = match at {
                        Some(at) => json!({ "person": &**who, "at": &**at }),
                        None => json!({ "person": &**who }),
                    };
                }
                f.insert("pii".into(), p);
            }
            Json::Object(f)
        })
        .collect();

    let methods: Vec<Json> = o
        .methods
        .iter()
        .map(|(name, m)| {
            let mut obj = Map::new();
            obj.insert("name".into(), json!(&**name));
            obj.insert("private".into(), json!(m.private));
            obj.insert("allow".into(), match &m.allow {
                Some(r) => json!(r.iter().map(|s| &**s).collect::<Vec<_>>()),
                None => Json::Null,
            });
            obj.insert("exclusive".into(), json!(m.exclusive));
            if m.invariant {
                obj.insert("invariant".into(), json!(true));
            }
            if let Some(ms) = m.every {
                obj.insert("everyMs".into(), json!(ms));
            }
            if let Some(needs) = ast.method_needs.get(&**name) {
                obj.insert("needs".into(), json!(needs));
            }
            let params: Vec<Json> = m
                .params
                .iter()
                .map(|(p, opt, ty)| json!({ "name": &**p, "optional": opt, "type": ty.describe(), "schema": arg_ty_to_json(ty) }))
                .collect();
            obj.insert("params".into(), json!(params));
            Json::Object(obj)
        })
        .collect();

    let mut obj = Map::new();
    obj.insert("name".into(), json!(&*o.name));
    obj.insert("server".into(), json!(o.server));
    obj.insert("belongsToPerson".into(), json!(o.belongs_to_a_person()));
    obj.insert("read".into(), match &o.read {
        Some(r) => json!(r.iter().map(|s| &**s).collect::<Vec<_>>()),
        None => Json::Null,
    });
    if let Some(r) = o.retain {
        obj.insert("retain".into(), json!({ "ms": r.ms, "from": if r.from_created { "created" } else { "changed" } }));
    }
    if let Some(ms) = o.hold {
        obj.insert("legalHoldMs".into(), json!(ms));
    }
    obj.insert("fields".into(), json!(fields));
    obj.insert("methods".into(), json!(methods));
    Json::Object(obj)
}

/// Resolves local `type Foo = ...` aliases, infers literal default field types (`qty = 0`),
/// and canonicalizes unions and record field order so `Manifest` and `compat::Snapshot`
/// always emit identical field type strings.
pub fn canonical_field_types(source: &str) -> HashMap<(String, String), String> {
    let mut aliases: HashMap<String, TypeExpr> = HashMap::new();
    let mut raw_fields: Vec<(String, String, TypeExpr)> = Vec::new();
    for f in modules::files(source) {
        if tj_core::i18n::is_locale_file(&f.name) {
            continue;
        }
        let Ok(parsed) = parse_range(source, f.start, f.end) else { continue };
        for item in &parsed.items {
            match item {
                Item::TypeAlias { name, generics, ty, .. } if generics.is_empty() => {
                    aliases.insert(name.clone(), ty.clone());
                }
                Item::Object(o) => {
                    for fl in &o.fields {
                        if let Some(ty) = &fl.ty {
                            raw_fields.push((o.name.clone(), fl.name.clone(), ty.clone()));
                        } else if let Some(init) = &fl.init {
                            let inferred = match &init.kind {
                                ExprKind::Num(_) => Some("number"),
                                ExprKind::Str(_) | ExprKind::Template(_) => Some("string"),
                                ExprKind::Bool(_) => Some("boolean"),
                                _ => None,
                            };
                            if let Some(t) = inferred {
                                raw_fields.push((o.name.clone(), fl.name.clone(), TypeExpr::Named(t.into(), Vec::new())));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    raw_fields
        .into_iter()
        .map(|(o, f, ty)| ((o, f), canon_type(&ty, &aliases, &mut HashSet::new())))
        .collect()
}

fn canon_type(ty: &TypeExpr, aliases: &HashMap<String, TypeExpr>, seen: &mut HashSet<String>) -> String {
    match ty {
        TypeExpr::Named(n, args) if args.is_empty() => {
            if let Some(target) = aliases.get(n) {
                if seen.insert(n.clone()) {
                    let s = canon_type(target, aliases, seen);
                    seen.remove(n);
                    return s;
                }
            }
            n.clone()
        }
        TypeExpr::Named(n, args) => format!("{n}<{}>", args.iter().map(|a| canon_type(a, aliases, seen)).collect::<Vec<_>>().join(", ")),
        TypeExpr::Array(inner) => format!("{}[]", canon_type(inner, aliases, seen)),
        TypeExpr::Union(ts) => {
            let mut parts: Vec<String> = ts.iter().map(|t| canon_type(t, aliases, seen)).collect();
            parts.sort();
            parts.dedup();
            parts.join(" | ")
        }
        TypeExpr::Record(fs) => {
            let mut sorted: BTreeMap<&str, String> = BTreeMap::new();
            for (k, opt, t) in fs {
                sorted.insert(k.as_str(), format!("{k}{}: {}", if *opt { "?" } else { "" }, canon_type(t, aliases, seen)));
            }
            format!("{{ {} }}", sorted.into_values().collect::<Vec<_>>().join(", "))
        }
        TypeExpr::Func(ps, r) => format!("({}) => {}", ps.iter().map(|p| canon_type(p, aliases, seen)).collect::<Vec<_>>().join(", "), canon_type(r, aliases, seen)),
        TypeExpr::StrLit(s) => format!("{s:?}"),
        TypeExpr::NumLit(n) => n.to_string(),
    }
}

pub fn arg_ty_to_json(ty: &ArgTy) -> Json {
    match ty {
        ArgTy::Any => json!("any"),
        ArgTy::Num => json!("number"),
        ArgTy::Str => json!("string"),
        ArgTy::Bool => json!("boolean"),
        ArgTy::Null => json!("null"),
        ArgTy::StrLit(s) => json!({ "lit": &**s }),
        ArgTy::NumLit(n) => json!({ "num": n }),
        ArgTy::BoolLit(b) => json!({ "bool": b }),
        ArgTy::Arr(inner) => json!({ "arr": arg_ty_to_json(inner) }),
        ArgTy::Dict(inner) => json!({ "dict": arg_ty_to_json(inner) }),
        ArgTy::Rec(fields) => json!({
            "rec": fields.iter().map(|(k, opt, t)| json!({ "name": &**k, "optional": opt, "ty": arg_ty_to_json(t) })).collect::<Vec<_>>()
        }),
        ArgTy::Union(ts) => json!({ "union": ts.iter().map(arg_ty_to_json).collect::<Vec<_>>() }),
    }
}

pub fn arg_ty_from_json(v: &Json) -> Option<ArgTy> {
    if let Some(s) = v.as_str() {
        return match s {
            "any" => Some(ArgTy::Any),
            "number" => Some(ArgTy::Num),
            "string" => Some(ArgTy::Str),
            "boolean" => Some(ArgTy::Bool),
            "null" => Some(ArgTy::Null),
            _ => None,
        };
    }
    let obj = v.as_object()?;
    if let Some(s) = obj.get("lit").and_then(Json::as_str) {
        return Some(ArgTy::StrLit(s.into()));
    }
    if let Some(n) = obj.get("num").and_then(Json::as_f64) {
        return Some(ArgTy::NumLit(n));
    }
    if let Some(b) = obj.get("bool").and_then(Json::as_bool) {
        return Some(ArgTy::BoolLit(b));
    }
    if let Some(a) = obj.get("arr") {
        return Some(ArgTy::Arr(Box::new(arg_ty_from_json(a)?)));
    }
    if let Some(d) = obj.get("dict") {
        return Some(ArgTy::Dict(Box::new(arg_ty_from_json(d)?)));
    }
    if let Some(fields) = obj.get("rec").and_then(Json::as_array) {
        let mut out = Vec::with_capacity(fields.len());
        for f in fields {
            let name = f["name"].as_str()?;
            let opt = f["optional"].as_bool().unwrap_or(false);
            let ty = arg_ty_from_json(&f["ty"])?;
            out.push((name.into(), opt, ty));
        }
        return Some(ArgTy::Rec(out.into()));
    }
    if let Some(ts) = obj.get("union").and_then(Json::as_array) {
        let mut out = Vec::with_capacity(ts.len());
        for t in ts {
            out.push(arg_ty_from_json(t)?);
        }
        return Some(ArgTy::Union(out));
    }
    None
}

fn decorator_strings(decorators: &[Decorator], name: &str) -> Vec<String> {
    decorators
        .iter()
        .filter(|d| d.name == name)
        .flat_map(|d| d.args.iter().filter_map(|a| if let ExprKind::Str(s) = &a.kind { Some(s.clone()) } else { None }))
        .collect()
}

fn str_list(v: &Json) -> Option<String> {
    let arr = v.as_array()?;
    let items: Vec<&str> = arr.iter().filter_map(Json::as_str).collect();
    (!items.is_empty()).then(|| items.join(", "))
}

fn format_ms(ms: f64) -> String {
    for (unit, label) in [(86_400_000.0, "d"), (3_600_000.0, "h"), (60_000.0, "m"), (1_000.0, "s")] {
        if ms >= unit && (ms % unit).abs() < 1e-6 {
            return format!("{}{label}", (ms / unit) as u64);
        }
    }
    format!("{ms}ms")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_manifest_json_and_text_without_builtin_plugin_noise() {
        let src = r#"
import { Form } from "tj:ui"

const lawfulBasis = { booking: "contract" }

@server @read("owner") @retain("30d") @legalHold("365d") object Customer {
  @pii("contact", "booking") email: string = ""
  @read("none") internal: number = 0
  @allow("owner") @exclusive save(e: string) { this.email = e }
  @every("1h") cleanup() {}
}

@server @read("anyone") object Slot {
  @read("staff", "self") customer: string = ""
  @allow("anyone") bad(author: string) { this.customer = author }
}

function App() {
  const f = Form.get("p")
  return <Text>{f.value("x", "")}</Text>
}
"#;
        let program = tj_core::compile(src).expect("compiles");
        let m = Manifest::build(src, &program, &["ai"]);
        let j = &m.json;
        assert_eq!(j["appSha256"].as_str().unwrap().len(), 64);
        assert_eq!(j["hasUi"], json!(true));
        assert_eq!(j["plugins"]["imported"], json!(["tj:ui"]));
        assert_eq!(j["plugins"]["configured"], json!(["ai"]));
        assert_eq!(j["privacy"]["lawfulBasis"]["booking"], json!("contract"));
        let names: Vec<&str> = j["objects"].as_array().unwrap().iter().map(|o| o["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["Customer", "Slot"], "built-in tj:ui Form is omitted");

        let cust = &j["objects"][0];
        assert_eq!(cust["belongsToPerson"], json!(true));
        assert_eq!(cust["retain"]["from"], json!("changed"));
        assert_eq!(cust["legalHoldMs"], json!(365.0 * 86_400_000.0));
        assert_eq!(cust["fields"][0]["pii"]["category"], json!("contact"));
        assert_eq!(cust["fields"][0]["type"], json!("string"));
        assert_eq!(cust["fields"][1]["read"], json!(["none"]));
        assert_eq!(cust["methods"][0]["exclusive"], json!(true));
        assert_eq!(cust["methods"][1]["everyMs"], json!(3_600_000.0));

        let slot = &j["objects"][1];
        assert_eq!(slot["fields"][0]["namesPerson"], json!(true));
        assert!(j["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("Slot.bad")));

        let text = m.text();
        assert!(text.contains("@server object Customer") && text.contains("pii: contact (booking)") && text.contains("exclusive") && text.contains("every 1h"), "{text}");
        assert!(text.contains("plugins: imported: tj:ui; configured: ai") && text.contains("lawfulBasis: booking: contract"), "{text}");
        assert!(text.contains("warning: Slot.bad"), "{text}");
    }
}
