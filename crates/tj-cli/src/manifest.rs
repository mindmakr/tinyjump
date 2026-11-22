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
use std::collections::{BTreeMap, BTreeSet, HashMap};
use tj_core::modules;
use tj_core::syntax::ast::{Decorator, ExprKind, Item, TypeExpr};
use tj_core::syntax::parse_range;
use tj_core::vm::program::{ObjectDef, Program, SPECIAL_CATEGORIES};

#[derive(Clone)]
pub struct Manifest {
    pub json: Json,
}

/// AST metadata for one user-declared object, complementing `Program`'s compiled `ObjectDef`.
struct AstObject {
    field_types: HashMap<String, String>,
    method_needs: HashMap<String, Vec<String>>,
}

impl Manifest {
    /// Builds the static manifest for a compiled app bundle.
    pub fn build(source: &str, program: &Program, configured_plugins: &[&str]) -> Manifest {
        let app_sha256 = sha256_hex(source);
        let files: Vec<String> = modules::files(source).into_iter().map(|f| f.name).collect();
        let mut ast_objects: HashMap<String, AstObject> = HashMap::new();
        let mut imported_plugins: BTreeSet<String> = BTreeSet::new();

        for f in modules::files(source) {
            let Ok(parsed) = parse_range(source, f.start, f.end) else { continue };
            for imp in &parsed.imports {
                if imp.from.starts_with("tj:") {
                    imported_plugins.insert(imp.from.clone());
                }
            }
            for item in &parsed.items {
                let Item::Object(o) = item else { continue };
                let field_types = o.fields.iter().filter_map(|fl| fl.ty.as_ref().map(|t| (fl.name.clone(), format_type(t)))).collect();
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

        // Include only the app's own objects (built-in plugin objects like `tj:ui`'s Form/Actions
        // are already represented under `plugins.imported`).
        let objects: Vec<Json> = program
            .objects
            .iter()
            .filter_map(|o| ast_objects.get(&*o.name).map(|ast| object_json(o, ast)))
            .collect();

        let mut configured: Vec<String> = configured_plugins.iter().map(|s| (*s).to_string()).collect();
        configured.sort();
        configured.dedup();

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

        let warnings = access::warnings(source);
        let json = json!({
            "appSha256": app_sha256,
            "files": files,
            "hasUi": program.app.is_some(),
            "objects": objects,
            "plugins": {
                "imported": imported_plugins.into_iter().collect::<Vec<_>>(),
                "configured": configured,
            },
            "privacy": {
                "lawfulBasis": lawful_basis,
                "reveals": reveals,
                "transfers": transfers,
            },
            "warnings": warnings,
        });
        Manifest { json }
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
            if let Some(ms) = m.every {
                obj.insert("everyMs".into(), json!(ms));
            }
            if let Some(needs) = ast.method_needs.get(&**name) {
                obj.insert("needs".into(), json!(needs));
            }
            let params: Vec<Json> = m
                .params
                .iter()
                .map(|(p, opt, ty)| json!({ "name": &**p, "optional": opt, "type": ty.describe() }))
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

fn decorator_strings(decorators: &[Decorator], name: &str) -> Vec<String> {
    decorators
        .iter()
        .filter(|d| d.name == name)
        .flat_map(|d| d.args.iter().filter_map(|a| if let ExprKind::Str(s) = &a.kind { Some(s.clone()) } else { None }))
        .collect()
}

fn format_type(ty: &TypeExpr) -> String {
    match ty {
        TypeExpr::Named(n, args) if args.is_empty() => n.clone(),
        TypeExpr::Named(n, args) => format!("{n}<{}>", args.iter().map(format_type).collect::<Vec<_>>().join(", ")),
        TypeExpr::Array(inner) => format!("{}[]", format_type(inner)),
        TypeExpr::Union(ts) => ts.iter().map(format_type).collect::<Vec<_>>().join(" | "),
        TypeExpr::Record(fs) => {
            let parts: Vec<String> = fs.iter().map(|(k, opt, t)| format!("{k}{}: {}", if *opt { "?" } else { "" }, format_type(t))).collect();
            format!("{{ {} }}", parts.join(", "))
        }
        TypeExpr::Func(ps, r) => format!("({}) => {}", ps.iter().map(format_type).collect::<Vec<_>>().join(", "), format_type(r)),
        TypeExpr::StrLit(s) => format!("{s:?}"),
        TypeExpr::NumLit(n) => n.to_string(),
    }
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
