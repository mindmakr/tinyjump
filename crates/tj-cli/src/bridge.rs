//! `tj bridge openapi <spec.json>`: generates a typed `.tj` REST client module (and optional
//! `mindmakr/tj-plugins` bridge manifest) from an OpenAPI 3.0 / 3.1 JSON specification.
//!
//! Every generated `fetch(...)` call embeds a literal `https://<host>/...` template prefix so
//! `tj registry check` statically verifies `permissions.network` with zero `"*"` wildcards.

use crate::registry::{self, PackageManifest, Permissions, Trust};
use serde_json::{json, Map, Value as Json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::net::IpAddr;
use std::path::PathBuf;

const MAX_SPEC_BYTES: usize = 4 << 20;
const MAX_BASE_URL_BYTES: usize = 2048;
const MAX_SCHEMA_DEPTH: usize = 16;
const HTTP_METHODS: &[&str] = &["get", "post", "put", "patch", "delete"];
const RESERVED_IDENTS: &[&str] = &[
    "Array", "Date", "Error", "JSON", "Math", "Number", "Object", "Promise", "Record", "String",
    "as", "async", "await", "boolean", "break", "buildHeaders", "caller", "case", "catch", "check",
    "class", "console", "const", "continue", "crypto", "default", "delete", "do", "else",
    "encodeURIComponent", "enum", "export", "extends", "false", "fetch", "finally", "for",
    "function", "if", "import", "in", "instanceof", "interface", "invoke", "let", "new", "null",
    "number", "object", "of", "parseFloat", "parseInt", "plugin", "private", "return", "reveal",
    "static", "string", "super", "switch", "t", "this", "throw", "true", "try", "type", "typeof",
    "undefined", "var", "void", "while", "yield",
];

pub fn run(args: &[String]) -> Result<(), String> {
    let usage = "usage: tj bridge openapi <spec.json> [--out <file.tj>] [--base-url <https://...>]\n  [--manifest <file.json>] [--name <pkg>] [--publisher <handle>] [--license <spdx>]";
    let Some(sub) = args.first().map(String::as_str) else {
        return Err(usage.into());
    };
    if sub != "openapi" {
        return Err(format!("unknown bridge generator {sub:?}; expected `openapi`\n\n{usage}"));
    }
    let mut spec_path: Option<PathBuf> = None;
    let mut out_path: Option<PathBuf> = None;
    let mut base_url: Option<String> = None;
    let mut manifest_path: Option<PathBuf> = None;
    let mut pkg_name: Option<String> = None;
    let mut publisher = "community".to_string();
    let mut license = "MIT".to_string();

    let mut it = args[1..].iter().peekable();
    while let Some(arg) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--out" => out_path = Some(PathBuf::from(value()?)),
            "--base-url" => base_url = Some(value()?.clone()),
            "--manifest" => manifest_path = Some(PathBuf::from(value()?)),
            "--name" => pkg_name = Some(value()?.clone()),
            "--publisher" => publisher = value()?.clone(),
            "--license" => license = value()?.clone(),
            other if !other.starts_with("--") && spec_path.is_none() => spec_path = Some(PathBuf::from(other)),
            other => return Err(format!("unknown option {other}\n\n{usage}")),
        }
    }
    let spec_path = spec_path.ok_or_else(|| format!("`tj bridge openapi` needs an OpenAPI JSON file\n\n{usage}"))?;
    let mut f = fs::File::open(&spec_path).map_err(|e| format!("cannot read {}: {e}", spec_path.display()))?;
    let mut bytes = Vec::new();
    f.by_ref()
        .take(MAX_SPEC_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("cannot read {}: {e}", spec_path.display()))?;
    if bytes.len() > MAX_SPEC_BYTES {
        return Err(format!("{} exceeds {} MiB OpenAPI spec size limit", spec_path.display(), MAX_SPEC_BYTES >> 20));
    }
    let spec: Json = serde_json::from_slice(&bytes).map_err(|e| format!("{} is not valid JSON: {e}", spec_path.display()))?;

    let generated = generate_openapi_bridge(&spec, base_url.as_deref())?;

    let default_stem = spec_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("api")
        .to_string();
    let dest = out_path.unwrap_or_else(|| PathBuf::from(format!("{default_stem}.tj")));

    // Validate and audit draft manifest BEFORE writing any files to disk (fail closed!).
    let verified_manifest: Option<(PathBuf, Json)> = if let Some(mf_path) = manifest_path {
        if publisher.is_empty()
            || publisher == "."
            || publisher == ".."
            || publisher.len() > 64
            || !publisher.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        {
            return Err(format!("invalid --publisher {publisher:?}: use GitHub handle characters [A-Za-z0-9._-]"));
        }
        let name = pkg_name.unwrap_or_else(|| sanitize_pkg_name(&generated.title, &default_stem));
        registry::valid_package_name(&name)?;
        let entry_name = dest
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("plugin.tj")
            .to_string();
        let sha = registry::sha256_bytes(generated.code.as_bytes());
        let mf = PackageManifest {
            name: name.clone(),
            kind: "bridge".into(),
            version: generated.version.clone(),
            trust: Trust::Community,
            source: format!("https://github.com/{publisher}/tj-{name}"),
            entry: entry_name,
            url: None,
            commit: "0".repeat(40),
            sha256: sha,
            license,
            publisher,
            permissions: Permissions {
                network: vec![generated.host.clone()],
                plugins: Vec::new(),
                pii: Vec::new(),
                device: Vec::new(),
            },
            port: None,
            flagged: None,
        };
        let errs = registry::validate_manifest(&mf, None, false);
        if !errs.is_empty() {
            return Err(format!("generated draft manifest validation failed:\n  - {}", errs.join("\n  - ")));
        }
        registry::verify_package_source(&mf, generated.code.as_bytes())
            .map_err(|e| format!("generated bridge failed registry security audit:\n  - {}", e.join("\n  - ")))?;
        let mf_json = json!({
            "name": mf.name,
            "kind": mf.kind,
            "version": mf.version,
            "trust": mf.trust.as_str(),
            "source": mf.source,
            "entry": mf.entry,
            "commit": mf.commit,
            "sha256": mf.sha256,
            "license": mf.license,
            "publisher": mf.publisher,
            "permissions": {
                "network": mf.permissions.network,
                "plugins": mf.permissions.plugins,
                "pii": mf.permissions.pii,
                "device": mf.permissions.device,
            },
        });
        Some((mf_path, mf_json))
    } else {
        None
    };

    if let Some(p) = dest.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(p).map_err(|e| format!("cannot create {}: {e}", p.display()))?;
    }
    fs::write(&dest, &generated.code).map_err(|e| format!("cannot write {}: {e}", dest.display()))?;

    if let Some((mf_path, mf_json)) = verified_manifest {
        if let Some(p) = mf_path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(p).map_err(|e| format!("cannot create {}: {e}", p.display()))?;
        }
        fs::write(&mf_path, serde_json::to_string_pretty(&mf_json).expect("json") + "\n")
            .map_err(|e| format!("cannot write {}: {e}", mf_path.display()))?;
        println!(
            "generated {} ({} types, {} operations, host {}) + draft manifest {}",
            dest.display(),
            generated.type_count,
            generated.op_count,
            generated.host,
            mf_path.display(),
        );
        println!("  note: update `source` and `commit` (40-char SHA) in {} after committing {}", mf_path.display(), dest.display());
    } else {
        println!(
            "generated {} ({} types, {} operations, host {})",
            dest.display(),
            generated.type_count,
            generated.op_count,
            generated.host,
        );
    }
    Ok(())
}

pub struct GeneratedBridge {
    pub code: String,
    pub host: String,
    pub title: String,
    pub version: String,
    pub type_count: usize,
    pub op_count: usize,
}

fn valid_openapi_version(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    parts.len() == 3
        && parts[0] == "3"
        && parts.iter().all(|p| !p.is_empty() && p.len() <= 4 && p.bytes().all(|b| b.is_ascii_digit()))
}

pub fn generate_openapi_bridge(spec: &Json, base_url_override: Option<&str>) -> Result<GeneratedBridge, String> {
    let obj = spec.as_object().ok_or("OpenAPI spec must be a JSON object")?;
    let openapi_ver = obj
        .get("openapi")
        .and_then(Json::as_str)
        .ok_or("OpenAPI spec missing string `openapi` field (expected 3.0.x or 3.1.x)")?;
    if !valid_openapi_version(openapi_ver) {
        return Err(format!("unsupported OpenAPI version {openapi_ver:?}; expected 3.<minor>.<patch> (e.g. 3.0.3 or 3.1.0)"));
    }

    let info = obj.get("info").and_then(Json::as_object);
    let title = info
        .and_then(|i| i.get("title"))
        .and_then(Json::as_str)
        .unwrap_or("API")
        .trim()
        .to_string();
    let version = normalize_semver(info.and_then(|i| i.get("version")).and_then(Json::as_str).unwrap_or("1.0.0"));

    let raw_base = match base_url_override {
        Some(u) => u.to_string(),
        None => resolve_server_url(obj)?,
    };
    let (base_url, host) = validate_base_url(&raw_base)?;

    let schemas = obj
        .get("components")
        .and_then(|c| c.get("schemas"))
        .and_then(Json::as_object);
    let mut type_names: BTreeMap<String, String> = BTreeMap::new();
    let mut used_types: BTreeSet<String> = BTreeSet::new();
    used_types.insert("BridgeOpts".into());
    if let Some(map) = schemas {
        for raw_name in map.keys() {
            let mut candidate = pascal_ident(raw_name);
            let mut n = 2usize;
            while used_types.contains(&candidate) {
                candidate = format!("{}{n}", pascal_ident(raw_name));
                n += 1;
            }
            used_types.insert(candidate.clone());
            type_names.insert(raw_name.clone(), candidate);
        }
    }

    let mut out = String::new();
    out.push_str(&format!(
        "// Generated by `tj bridge openapi` from {} (OpenAPI {}).\n",
        clean_comment(&title),
        clean_comment(openapi_ver),
    ));
    out.push_str(&format!("// Allowed network host: {host}\n\n"));
    out.push_str("export type BridgeOpts = { token?: string, headers?: Record<string, string> }\n\n");
    out.push_str(
        "function buildHeaders(hasBody: boolean, opts: BridgeOpts | null): Record<string, string> {\n\
           const h: Record<string, string> = { accept: \"application/json\" }\n\
           if (hasBody) {\n\
             h[\"content-type\"] = \"application/json\"\n\
           }\n\
           if (opts !== null) {\n\
             if (opts.headers !== null) {\n\
               for (const pair of Object.entries(opts.headers)) {\n\
                 h[pair[0]] = pair[1]\n\
               }\n\
             }\n\
             if (opts.token !== null) {\n\
               h[\"authorization\"] = `Bearer ${opts.token}`\n\
             }\n\
           }\n\
           return h\n\
         }\n\n",
    );

    let mut type_count = 0usize;
    if let Some(map) = schemas {
        for (raw_name, schema) in map {
            let tname = &type_names[raw_name];
            let ts = schema_to_ts(schema, &type_names, 0);
            out.push_str(&format!("export type {tname} = {ts}\n"));
            type_count += 1;
        }
        if type_count > 0 {
            out.push('\n');
        }
    }

    let paths = obj
        .get("paths")
        .and_then(Json::as_object)
        .ok_or("OpenAPI spec missing `paths` object")?;

    let mut used_fns: BTreeSet<String> = BTreeSet::new();
    used_fns.insert("buildHeaders".into());
    let mut op_count = 0usize;

    for (raw_path, item_val) in paths {
        let Some(path_item) = item_val.as_object() else { continue };
        let path_params = collect_params(path_item.get("parameters"), obj);
        for &method in HTTP_METHODS {
            let Some(op_obj) = path_item.get(method).and_then(Json::as_object) else { continue };
            let op_params = merge_params(&path_params, &collect_params(op_obj.get("parameters"), obj));
            let base_fn_name = match op_obj.get("operationId").and_then(Json::as_str) {
                Some(id) if !id.trim().is_empty() => camel_ident(id),
                _ => camel_ident(&format!("{method}_{raw_path}")),
            };
            let mut fn_name = base_fn_name.clone();
            let mut suffix = 2usize;
            while used_fns.contains(&fn_name) {
                fn_name = format!("{base_fn_name}{suffix}");
                suffix += 1;
            }
            used_fns.insert(fn_name.clone());

            emit_operation(&mut out, &fn_name, method, &base_url, raw_path, op_obj, &op_params, obj, &type_names)?;
            op_count += 1;
        }
    }

    if op_count == 0 {
        return Err("OpenAPI spec contains no supported operations (GET/POST/PUT/PATCH/DELETE) under `paths`".into());
    }

    // Verify generated code compiles cleanly under `tj_core::compile` before returning!
    let check_src = format!("//# file: tj:bridge-check\n{out}");
    if let Err(diags) = tj_core::compile(&check_src) {
        let rendered = diags
            .iter()
            .map(|d| d.render("generated.tj", &check_src))
            .collect::<Vec<_>>()
            .join("\n\n");
        return Err(format!("internal error: generated bridge did not compile:\n{rendered}"));
    }

    Ok(GeneratedBridge {
        code: out,
        host,
        title,
        version,
        type_count,
        op_count,
    })
}

#[derive(Clone)]
struct ParamDef {
    name: String,
    location: String,
    required: bool,
    schema: Json,
}

fn resolve_ref<'a>(val: &'a Json, root: &'a Map<String, Json>) -> Option<&'a Json> {
    let Some(ref_str) = val.get("$ref").and_then(Json::as_str) else {
        return Some(val);
    };
    let path = ref_str.strip_prefix("#/")?;
    let mut cur: &Json = root.get(path.split('/').next()?)?;
    for seg in path.split('/').skip(1) {
        let unescaped = seg.replace("~1", "/").replace("~0", "~");
        cur = cur.get(&unescaped)?;
    }
    Some(cur)
}

fn collect_params(val: Option<&Json>, root: &Map<String, Json>) -> Vec<ParamDef> {
    let Some(arr) = val.and_then(Json::as_array) else { return Vec::new() };
    let mut out: Vec<ParamDef> = Vec::new();
    for item in arr {
        let Some(p) = resolve_ref(item, root).and_then(Json::as_object) else { continue };
        let Some(name) = p.get("name").and_then(Json::as_str) else { continue };
        let Some(loc) = p.get("in").and_then(Json::as_str) else { continue };
        if !matches!(loc, "path" | "query") {
            continue;
        }
        let required = loc == "path" || p.get("required").and_then(Json::as_bool).unwrap_or(false);
        let schema = p.get("schema").cloned().unwrap_or(Json::Null);
        let def = ParamDef {
            name: name.to_string(),
            location: loc.to_string(),
            required,
            schema,
        };
        if let Some(slot) = out.iter_mut().find(|m| m.name == def.name && m.location == def.location) {
            *slot = def;
        } else {
            out.push(def);
        }
    }
    out
}

fn merge_params(path_level: &[ParamDef], op_level: &[ParamDef]) -> Vec<ParamDef> {
    let mut merged: Vec<ParamDef> = path_level.to_vec();
    for p in op_level {
        if let Some(slot) = merged.iter_mut().find(|m| m.name == p.name && m.location == p.location) {
            *slot = p.clone();
        } else {
            merged.push(p.clone());
        }
    }
    merged
}

#[allow(clippy::too_many_arguments)]
fn emit_operation(
    out: &mut String,
    fn_name: &str,
    method: &str,
    base_url: &str,
    raw_path: &str,
    op: &Map<String, Json>,
    params: &[ParamDef],
    root: &Map<String, Json>,
    type_names: &BTreeMap<String, String>,
) -> Result<(), String> {
    if !raw_path.starts_with('/') {
        return Err(format!("OpenAPI path {raw_path:?} must start with '/'"));
    }
    if raw_path.chars().any(char::is_control) {
        return Err(format!("OpenAPI path {raw_path:?} contains control characters"));
    }

    let mut used_args: BTreeSet<String> = BTreeSet::new();
    let mut alloc_arg = |raw: &str| -> String {
        let base = camel_ident(raw);
        let mut cand = base.clone();
        let mut n = 2usize;
        while used_args.contains(&cand) || matches!(cand.as_str(), "opts" | "headers" | "res" | "q" | "qs" | "txt") {
            cand = format!("{base}{n}");
            n += 1;
        }
        used_args.insert(cand.clone());
        cand
    };

    let mut sig_params: Vec<String> = Vec::new();
    let mut path_arg_map: BTreeMap<String, String> = BTreeMap::new();

    // 1. Path parameters (in appearance order inside raw_path so positional signatures are intuitive).
    for placeholder in extract_path_placeholders(raw_path) {
        if path_arg_map.contains_key(&placeholder) {
            continue;
        }
        let arg = alloc_arg(&placeholder);
        let ty = params
            .iter()
            .find(|p| p.location == "path" && p.name == placeholder)
            .map(|p| schema_to_ts(&p.schema, type_names, 0))
            .unwrap_or_else(|| "string".into());
        sig_params.push(format!("{arg}: {ty}"));
        path_arg_map.insert(placeholder, arg);
    }

    // 2. Request body (JSON, if present).
    let mut body_arg: Option<String> = None;
    if let Some(rb_val) = op.get("requestBody") {
        if let Some(rb) = resolve_ref(rb_val, root).and_then(Json::as_object) {
            if let Some(schema) = rb
                .get("content")
                .and_then(|c| c.get("application/json"))
                .and_then(|m| m.get("schema"))
            {
                let arg = alloc_arg("body");
                let mut ty = schema_to_ts(schema, type_names, 0);
                let required = rb.get("required").and_then(Json::as_bool).unwrap_or(false);
                if !required && ty != "any" && !ty.ends_with(" | null") {
                    ty.push_str(" | null");
                }
                sig_params.push(format!("{arg}: {ty}"));
                body_arg = Some(arg);
            }
        }
    }

    // 3. Query parameters (grouped into a single typed `query` record when present so call sites stay readable).
    let query_params: Vec<&ParamDef> = params.iter().filter(|p| p.location == "query").collect();
    let mut query_arg: Option<(String, Vec<(String, String)>)> = None;
    if !query_params.is_empty() {
        let arg = alloc_arg("query");
        let mut fields = Vec::new();
        let mut prop_pairs = Vec::new();
        let mut all_optional = true;
        for qp in &query_params {
            if qp.required {
                all_optional = false;
            }
            let ty = schema_to_ts(&qp.schema, type_names, 0);
            let opt = if qp.required { "" } else { "?" };
            fields.push(format!("{}{opt}: {ty}", format_prop_key(&qp.name)));
            prop_pairs.push((qp.name.clone(), format_prop_access(&arg, &qp.name)));
        }
        let rec = format!("{{ {} }}", fields.join(", "));
        let ty = if all_optional { format!("{rec} | null") } else { rec };
        sig_params.push(format!("{arg}: {ty}"));
        query_arg = Some((arg, prop_pairs));
    }

    sig_params.push("opts: BridgeOpts | null".into());

    // 4. Response return type.
    let (ret_ty, is_void, is_text) = infer_response_type(op, root, type_names);

    if let Some(summary) = op.get("summary").and_then(Json::as_str).filter(|s| !s.trim().is_empty()) {
        out.push_str(&format!("/// {}\n", clean_comment(summary)));
    }
    out.push_str(&format!(
        "export async function {fn_name}({}): Promise<{ret_ty}> {{\n",
        sig_params.join(", ")
    ));

    // Build query string if needed (respecting `?` already present in `raw_path`).
    let qs_suffix = if let Some((qarg, pairs)) = &query_arg {
        let sep = if raw_path.contains('?') { "&" } else { "?" };
        out.push_str("  const q: string[] = []\n");
        out.push_str(&format!("  if ({qarg} !== null) {{\n"));
        for (raw_key, access) in pairs {
            let key_enc = escape_template_lit(&url_encode_key(raw_key));
            out.push_str(&format!(
                "    if ({access} !== null) {{ q.push(`{key_enc}=${{encodeURIComponent(String({access}))}}`) }}\n"
            ));
        }
        out.push_str("  }\n");
        out.push_str(&format!("  const qs = q.length > 0 ? `{sep}${{q.join(\"&\")}}` : \"\"\n"));
        "${qs}"
    } else {
        ""
    };

    // Build literal https://<host><basePath><path> template so `tj registry check` verifies the host statically!
    let tpl_url = build_template_url(base_url, raw_path, &path_arg_map, qs_suffix);
    let upper = method.to_ascii_uppercase();
    if let Some(barg) = body_arg {
        out.push_str(&format!("  const headers = buildHeaders({barg} !== null, opts)\n"));
        out.push_str(&format!(
            "  const res = await fetch(`{tpl_url}`, {{ method: \"{upper}\", headers, body: {barg} !== null ? JSON.stringify({barg}) : null }})\n"
        ));
    } else {
        out.push_str("  const headers = buildHeaders(false, opts)\n");
        out.push_str(&format!(
            "  const res = await fetch(`{tpl_url}`, {{ method: \"{upper}\", headers }})\n"
        ));
    }
    out.push_str(&format!(
        "  if (!res.ok) {{ throw new Error(`{fn_name} failed: HTTP ${{res.status}}`) }}\n"
    ));
    if is_void {
        out.push_str("  return\n");
    } else if is_text {
        out.push_str("  return await res.text()\n");
    } else {
        out.push_str("  if (res.status === 204) { return null }\n");
        out.push_str("  const txt = await res.text()\n");
        out.push_str("  return txt.length > 0 ? JSON.parse(txt) : null\n");
    }
    out.push_str("}\n\n");
    Ok(())
}

fn infer_response_type(
    op: &Map<String, Json>,
    root: &Map<String, Json>,
    type_names: &BTreeMap<String, String>,
) -> (String, bool, bool) {
    let Some(resps) = op.get("responses").and_then(Json::as_object) else {
        return ("any".into(), false, false);
    };
    for code in ["200", "201", "202", "204", "default"] {
        let Some(r_val) = resps.get(code) else { continue };
        if code == "204" {
            return ("void".into(), true, false);
        }
        let Some(r_obj) = resolve_ref(r_val, root).and_then(Json::as_object) else { continue };
        let Some(content) = r_obj.get("content").and_then(Json::as_object) else {
            return ("void".into(), true, false);
        };
        if let Some(schema) = content.get("application/json").and_then(|m| m.get("schema")) {
            let ts = schema_to_ts(schema, type_names, 0);
            return (ts, false, false);
        }
        if content.contains_key("text/plain") {
            return ("string".into(), false, true);
        }
        return ("any".into(), false, false);
    }
    ("any".into(), false, false)
}

fn schema_to_ts(schema: &Json, type_names: &BTreeMap<String, String>, depth: usize) -> String {
    if depth > MAX_SCHEMA_DEPTH {
        return "any".into();
    }
    let Some(obj) = schema.as_object() else {
        return "any".into();
    };
    if let Some(ref_str) = obj.get("$ref").and_then(Json::as_str) {
        if let Some(name) = ref_str.strip_prefix("#/components/schemas/") {
            if let Some(ts_name) = type_names.get(name) {
                return ts_name.clone();
            }
        }
        return "any".into();
    }

    let mut nullable = obj.get("nullable").and_then(Json::as_bool).unwrap_or(false);

    if let Some(enums) = obj.get("enum").and_then(Json::as_array) {
        let mut lits = Vec::new();
        let mut has_neg_num = false;
        let mut all_nums = true;
        for v in enums {
            match v {
                Json::String(s) => {
                    all_nums = false;
                    lits.push(serde_json::to_string(s).expect("json str"));
                }
                Json::Number(n) => {
                    if n.as_f64().is_some_and(|x| x < 0.0) {
                        has_neg_num = true;
                    }
                    lits.push(n.to_string());
                }
                Json::Null => nullable = true,
                _ => return "any".into(),
            }
        }
        if has_neg_num {
            let base = if all_nums { "number" } else { "any" };
            return if nullable && base != "any" { format!("{base} | null") } else { base.into() };
        }
        if !lits.is_empty() {
            let mut joined = lits.join(" | ");
            if nullable {
                joined.push_str(" | null");
            }
            return joined;
        }
    }

    for comb in ["oneOf", "anyOf"] {
        if let Some(arr) = obj.get(comb).and_then(Json::as_array) {
            let mut parts = Vec::new();
            for sub in arr {
                let t = schema_to_ts(sub, type_names, depth + 1);
                if !parts.contains(&t) {
                    parts.push(t);
                }
            }
            if !parts.is_empty() {
                let mut joined = parts.join(" | ");
                if nullable && !parts.iter().any(|p| p == "null") {
                    joined.push_str(" | null");
                }
                return joined;
            }
        }
    }

    if let Some(arr) = obj.get("allOf").and_then(Json::as_array) {
        let mut merged: BTreeMap<String, (String, bool)> = BTreeMap::new();
        let mut all_objs = true;
        for sub in arr {
            if let Some(so) = sub.as_object() {
                if let Some(props) = so.get("properties").and_then(Json::as_object) {
                    let req: BTreeSet<&str> = so
                        .get("required")
                        .and_then(Json::as_array)
                        .map(|r| r.iter().filter_map(Json::as_str).collect())
                        .unwrap_or_default();
                    for (k, v) in props {
                        let ft = schema_to_ts(v, type_names, depth + 1);
                        let is_req = req.contains(k.as_str());
                        merged
                            .entry(k.clone())
                            .and_modify(|e| {
                                e.0 = ft.clone();
                                e.1 |= is_req;
                            })
                            .or_insert((ft, is_req));
                    }
                    continue;
                }
            }
            all_objs = false;
        }
        if all_objs && !merged.is_empty() {
            let fields: Vec<String> = merged
                .into_iter()
                .map(|(k, (ft, is_req))| {
                    let opt = if is_req { "" } else { "?" };
                    format!("{}{opt}: {ft}", format_prop_key(&k))
                })
                .collect();
            let rec = format!("{{ {} }}", fields.join(", "));
            return if nullable { format!("{rec} | null") } else { rec };
        }
        return "any".into();
    }

    // OpenAPI 3.0 string `"type"` or OpenAPI 3.1 array `["string", "null"]`.
    let primary_type: Option<&str> = match obj.get("type") {
        Some(Json::String(s)) => Some(s.as_str()),
        Some(Json::Array(arr)) => {
            let mut found = None;
            for v in arr {
                if let Some(s) = v.as_str() {
                    if s == "null" {
                        nullable = true;
                    } else if found.is_none() {
                        found = Some(s);
                    }
                }
            }
            found
        }
        _ => {
            if obj.contains_key("properties") {
                Some("object")
            } else if obj.contains_key("items") {
                Some("array")
            } else {
                None
            }
        }
    };

    let base = match primary_type {
        Some("string") => "string".into(),
        Some("number") | Some("integer") => "number".into(),
        Some("boolean") => "boolean".into(),
        Some("null") => return "null".into(),
        Some("array") => {
            let item = obj
                .get("items")
                .map(|it| schema_to_ts(it, type_names, depth + 1))
                .unwrap_or_else(|| "any".into());
            if item.contains(" | ") {
                format!("Array<{item}>")
            } else {
                format!("{item}[]")
            }
        }
        Some("object") => {
            if let Some(props) = obj.get("properties").and_then(Json::as_object) {
                if props.is_empty() {
                    "Record<string, any>".into()
                } else {
                    let req: BTreeSet<&str> = obj
                        .get("required")
                        .and_then(Json::as_array)
                        .map(|r| r.iter().filter_map(Json::as_str).collect())
                        .unwrap_or_default();
                    let mut fields = Vec::new();
                    for (k, v) in props {
                        let ft = schema_to_ts(v, type_names, depth + 1);
                        let opt = if req.contains(k.as_str()) { "" } else { "?" };
                        fields.push(format!("{}{opt}: {ft}", format_prop_key(k)));
                    }
                    format!("{{ {} }}", fields.join(", "))
                }
            } else if let Some(addl) = obj.get("additionalProperties") {
                if addl.is_object() {
                    let vt = schema_to_ts(addl, type_names, depth + 1);
                    format!("Record<string, {vt}>")
                } else {
                    "Record<string, any>".into()
                }
            } else {
                "Record<string, any>".into()
            }
        }
        _ => "any".into(),
    };

    if nullable && base != "any" && base != "null" {
        format!("{base} | null")
    } else {
        base
    }
}

/// Single-pass `{var}` expansion on `servers[0].url` (never re-expands substituted values, bounded to 2048 bytes).
fn resolve_server_url(spec: &Map<String, Json>) -> Result<String, String> {
    let server = spec
        .get("servers")
        .and_then(Json::as_array)
        .and_then(|arr| arr.first())
        .and_then(Json::as_object)
        .ok_or("OpenAPI spec has no `servers[0].url`; pass --base-url https://<host>")?;
    let template = server
        .get("url")
        .and_then(Json::as_str)
        .ok_or("OpenAPI `servers[0]` missing string `url`")?;
    let vars = server.get("variables").and_then(Json::as_object);

    let mut out = String::with_capacity(template.len().min(MAX_BASE_URL_BYTES));
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            return Err(format!("OpenAPI server URL {template:?} has an unclosed '{{'"));
        };
        let key = &after[..close];
        let val = vars
            .and_then(|m| m.get(key))
            .and_then(|v| v.get("default"))
            .and_then(Json::as_str)
            .ok_or_else(|| format!("OpenAPI server variable {{{key}}} has no default value; pass --base-url"))?;
        out.push_str(val);
        if out.len() > MAX_BASE_URL_BYTES {
            return Err(format!("expanded OpenAPI server URL exceeds {MAX_BASE_URL_BYTES} bytes"));
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    if out.len() > MAX_BASE_URL_BYTES {
        return Err(format!("OpenAPI server URL exceeds {MAX_BASE_URL_BYTES} bytes"));
    }
    Ok(out)
}

fn validate_base_url(raw: &str) -> Result<(String, String), String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.len() > MAX_BASE_URL_BYTES {
        return Err(format!("OpenAPI base URL exceeds {MAX_BASE_URL_BYTES} bytes"));
    }
    if !trimmed.starts_with("https://") {
        return Err(format!("OpenAPI base URL {raw:?} must start with https:// (use --base-url https://<host>)"));
    }
    if trimmed.chars().any(char::is_control)
        || trimmed.contains('`')
        || trimmed.contains('$')
        || trimmed.contains('"')
        || trimmed.contains('\'')
        || trimmed.contains('\\')
        || trimmed.contains('<')
        || trimmed.contains('>')
        || trimmed.contains('?')
        || trimmed.contains('#')
        || trimmed.contains('*')
        || trimmed.contains(',')
        || trimmed.contains('{')
        || trimmed.contains('}')
        || trimmed.contains(char::is_whitespace)
    {
        return Err(format!("OpenAPI base URL {raw:?} contains forbidden characters"));
    }
    let rest = &trimmed[8..];
    let authority = rest.split('/').next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') {
        return Err(format!("OpenAPI base URL {raw:?} has an invalid host"));
    }
    let host_raw = if let Some(v6) = authority.strip_prefix('[') {
        let Some((addr, after)) = v6.split_once(']') else {
            return Err(format!("OpenAPI base URL {raw:?} has malformed IPv6 host"));
        };
        if !after.is_empty() && !after.strip_prefix(':').is_some_and(|p| !p.is_empty() && p.parse::<u16>().is_ok()) {
            return Err(format!("OpenAPI base URL {raw:?} has an invalid port"));
        }
        addr
    } else {
        match authority.split_once(':') {
            Some((h, port)) => {
                if port.is_empty() || port.parse::<u16>().is_err() {
                    return Err(format!("OpenAPI base URL {raw:?} has an invalid port"));
                }
                h
            }
            None => authority,
        }
    };
    let host = host_raw.trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() || host == "localhost" || host.ends_with(".localhost") {
        return Err(format!("OpenAPI base URL host {host:?} is not a public HTTPS host"));
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        if !crate::engine::is_public(ip) {
            return Err(format!("OpenAPI base URL IP {ip} is not a public HTTPS host"));
        }
    } else {
        let valid_domain = host.contains('.')
            && !host.starts_with('.')
            && !host.starts_with('-')
            && !host.ends_with('-')
            && !host.contains("..")
            && host.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-');
        if !valid_domain {
            return Err(format!("OpenAPI base URL host {host:?} is not a valid public domain"));
        }
    }
    Ok((trimmed.to_string(), host))
}

fn extract_path_placeholders(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = path;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else { break };
        let name = after[..end].trim();
        if !name.is_empty() {
            out.push(name.to_string());
        }
        rest = &after[end + 1..];
    }
    out
}

fn build_template_url(base_url: &str, raw_path: &str, path_args: &BTreeMap<String, String>, qs_suffix: &str) -> String {
    let mut tpl = escape_template_lit(base_url);
    let mut rest = raw_path;
    while let Some(start) = rest.find('{') {
        tpl.push_str(&escape_template_lit(&rest[..start]));
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            tpl.push_str(&escape_template_lit(&rest[start..]));
            rest = "";
            break;
        };
        let key = after[..end].trim();
        if let Some(arg) = path_args.get(key) {
            tpl.push_str(&format!("${{encodeURIComponent(String({arg}))}}"));
        } else {
            tpl.push_str(&escape_template_lit(&rest[start..start + 1 + end + 1]));
        }
        rest = &after[end + 1..];
    }
    tpl.push_str(&escape_template_lit(rest));
    tpl.push_str(qs_suffix);
    tpl
}

fn escape_template_lit(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '`' => out.push_str("\\`"),
            '$' => out.push_str("\\$"),
            _ => out.push(c),
        }
    }
    out
}

fn url_encode_key(k: &str) -> String {
    let mut out = String::with_capacity(k.len());
    for b in k.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            use std::fmt::Write;
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

fn is_valid_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !RESERVED_IDENTS.contains(&s)
}

fn format_prop_key(k: &str) -> String {
    if !k.is_empty()
        && k.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        k.to_string()
    } else {
        serde_json::to_string(k).expect("json str")
    }
}

fn format_prop_access(obj: &str, key: &str) -> String {
    if is_valid_ident(key) {
        format!("{obj}.{key}")
    } else {
        format!("{obj}[{}]", serde_json::to_string(key).expect("json str"))
    }
}

fn pascal_ident(raw: &str) -> String {
    let mut out = String::new();
    let mut cap = true;
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() {
            if cap {
                out.push(c.to_ascii_uppercase());
                cap = false;
            } else {
                out.push(c);
            }
        } else {
            cap = true;
        }
    }
    if out.is_empty() || !out.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
        out.insert(0, 'T');
    }
    if RESERVED_IDENTS.contains(&out.as_str()) {
        out.push_str("Type");
    }
    out
}

fn camel_ident(raw: &str) -> String {
    let p = pascal_ident(raw);
    let mut chars = p.chars();
    let Some(first) = chars.next() else { return "op".into() };
    let mut out = String::with_capacity(p.len());
    out.push(first.to_ascii_lowercase());
    out.push_str(chars.as_str());
    if RESERVED_IDENTS.contains(&out.as_str()) {
        out.push_str("Op");
    }
    out
}

fn sanitize_pkg_name(title: &str, fallback: &str) -> String {
    let clean = |s: &str| {
        let mut out = String::new();
        let mut prev_dash = true;
        for c in s.chars() {
            if c.is_ascii_alphanumeric() {
                out.push(c.to_ascii_lowercase());
                prev_dash = false;
            } else if !prev_dash {
                out.push('-');
                prev_dash = true;
            }
        }
        while out.ends_with('-') {
            out.pop();
        }
        out
    };
    let cand = clean(title);
    if registry::valid_package_name(&cand).is_ok() {
        return cand;
    }
    let fb = clean(fallback);
    if registry::valid_package_name(&fb).is_ok() {
        return fb;
    }
    "api-bridge".into()
}

fn normalize_semver(raw: &str) -> String {
    let parts: Vec<&str> = raw.trim().trim_start_matches('v').split('.').collect();
    if parts.len() == 3 && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())) {
        return parts.join(".");
    }
    if parts.len() == 2 && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())) {
        return format!("{}.{}.0", parts[0], parts[1]);
    }
    "1.0.0".into()
}

fn clean_comment(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_compiling_registry_auditable_bridge_from_openapi_spec() {
        let spec = json!({
            "openapi": "3.0.3",
            "info": { "title": "Pet Store", "version": "1.2" },
            "servers": [{
                "url": "https://{env}.petstore.example/v1",
                "variables": { "env": { "default": "api" } }
            }],
            "components": {
                "schemas": {
                    "Pet": {
                        "type": "object",
                        "required": ["id", "name"],
                        "properties": {
                            "id": { "type": "string" },
                            "name": { "type": "string" },
                            "status": { "type": "string", "enum": ["available", "sold"] },
                            "delta": { "type": "integer", "enum": [-1, 0, 1] },
                            "tags": { "type": "array", "items": { "type": "string" } }
                        }
                    },
                    "NewPet": {
                        "type": "object",
                        "required": ["name"],
                        "properties": {
                            "name": { "type": "string" },
                            "owner-note": { "type": "string", "nullable": true }
                        }
                    }
                }
            },
            "paths": {
                "/pets": {
                    "get": {
                        "operationId": "check",
                        "summary": "List all pets",
                        "parameters": [
                            { "name": "limit", "in": "query", "schema": { "type": "integer" } },
                            { "name": "limit", "in": "query", "schema": { "type": "integer" } },
                            { "name": "tag-name", "in": "query", "schema": { "type": "string" } }
                        ],
                        "responses": {
                            "200": {
                                "description": "ok",
                                "content": {
                                    "application/json": {
                                        "schema": { "type": "array", "items": { "$ref": "#/components/schemas/Pet" } }
                                    }
                                }
                            }
                        }
                    },
                    "post": {
                        "operationId": "createPet",
                        "requestBody": {
                            "required": false,
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/NewPet" }
                                }
                            }
                        },
                        "responses": {
                            "201": {
                                "description": "created",
                                "content": {
                                    "application/json": { "schema": { "$ref": "#/components/schemas/Pet" } }
                                }
                            }
                        }
                    }
                },
                "/pets/{as}": {
                    "parameters": [
                        { "name": "as", "in": "path", "required": true, "schema": { "type": "string" } }
                    ],
                    "get": {
                        "operationId": "getPet",
                        "responses": {
                            "200": {
                                "description": "ok",
                                "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Pet" } } }
                            }
                        }
                    },
                    "delete": {
                        "operationId": "deletePet",
                        "responses": { "204": { "description": "deleted" } }
                    }
                }
            }
        });

        let gen = generate_openapi_bridge(&spec, None).unwrap();
        assert_eq!(gen.host, "api.petstore.example");
        assert_eq!(gen.version, "1.2.0");
        assert_eq!(gen.type_count, 2);
        assert_eq!(gen.op_count, 4);
        assert!(gen.code.contains("export type Pet ="));
        assert!(gen.code.contains("\"available\" | \"sold\""));
        assert!(gen.code.contains("delta?: number"));
        // Reserved `check` operationId and `as` path parameter are safely renamed so tj_core compiles cleanly.
        assert!(gen.code.contains("export async function checkOp("));
        assert!(gen.code.contains("export async function getPet(asOp: string, opts: BridgeOpts | null): Promise<Pet>"));
        assert!(gen.code.contains("buildHeaders(body !== null, opts)"));
        assert!(gen.code.contains("body !== null ? JSON.stringify(body) : null"));
        assert!(gen.code.contains("if (res.status === 204) { return null }"));
        assert!(gen.code.contains("https://api.petstore.example/v1/pets/${encodeURIComponent(String(asOp))}"));

        // Must pass `tj registry check` with exact single-host `permissions.network` (no "*" wildcard)!
        let manifest = PackageManifest {
            name: "pet-store".into(),
            kind: "bridge".into(),
            version: gen.version.clone(),
            trust: Trust::Community,
            source: "https://github.com/example/tj-pet-store".into(),
            entry: "plugin.tj".into(),
            url: None,
            commit: "a".repeat(40),
            sha256: registry::sha256_bytes(gen.code.as_bytes()),
            license: "MIT".into(),
            publisher: "example".into(),
            permissions: Permissions {
                network: vec![gen.host.clone()],
                plugins: Vec::new(),
                pii: Vec::new(),
                device: Vec::new(),
            },
            port: None,
            flagged: None,
        };
        assert!(registry::validate_manifest(&manifest, None, false).is_empty());
        assert!(registry::verify_package_source(&manifest, gen.code.as_bytes()).is_ok());
    }

    #[test]
    fn blocks_injection_wildcards_private_hosts_and_var_amplification() {
        let base_spec = |openapi: &str, server: &str| {
            json!({
                "openapi": openapi,
                "info": { "title": "T", "version": "1.0.0" },
                "servers": [{ "url": server }],
                "paths": { "/x": { "get": { "responses": { "204": { "description": "ok" } } } } }
            })
        };
        // BLOCKER 1: code injection in openapi version field.
        assert!(generate_openapi_bridge(&base_spec("3.0.3)\nexport function pwn() {}", "https://api.example.com"), None).is_err());
        // BLOCKER 2: wildcard, multi-host comma, query/fragment, and private/RFC1918/localhost hosts.
        for bad in [
            "http://api.example.com",
            "https://*/v1",
            "https://api.example.com,evil.example/v1",
            "https://api.example.com?a=1",
            "https://api.example.com#frag",
            "https://localhost./v1",
            "https://10.0.0.1/v1",
            "https://192.168.1.5/v1",
            "https://172.16.0.1/v1",
            "https://100.64.0.1/v1",
            "https://api.example.com/${evil}",
        ] {
            assert!(generate_openapi_bridge(&base_spec("3.0.3", bad), None).is_err(), "must reject {bad}");
        }
        // BLOCKER 3: recursive variable amplification does not re-expand and fails validation on leftover `{`.
        let bomb = json!({
            "openapi": "3.0.3",
            "info": { "title": "Bomb", "version": "1.0.0" },
            "servers": [{
                "url": "https://{a}.example.com",
                "variables": {
                    "a": { "default": "{b}{b}{b}{b}" },
                    "b": { "default": "{c}{c}{c}{c}" },
                    "c": { "default": "xxxx" }
                }
            }],
            "paths": { "/x": { "get": { "responses": { "204": { "description": "ok" } } } } }
        });
        assert!(generate_openapi_bridge(&bomb, None).is_err());
    }
}
