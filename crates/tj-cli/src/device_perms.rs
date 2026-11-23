//! Canonical device capability permissions (`docs/device-plugins.md`): single source of
//! truth shared across `tj registry check`, `tj.lock`, `tj check --manifest`, `GET /_tj/manifest`,
//! optional `permissions.json`, and `tj build --android` / `--ios` / `--web`.

use crate::registry;
use serde_json::{json, Value as Json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{ErrorKind, Read};
use std::path::Path;

pub const PERMISSIONS_FILE: &str = "permissions.json";
const MAX_PERMS_FILE_BYTES: usize = 64 * 1024;
const MAX_REASON_CHARS: usize = 240;

/// Canonical device capabilities accepted in `permissions.device` (manifests & `permissions.json`).
pub const ALLOWED_DEVICE_PERMISSIONS: &[&str] = &[
    "camera",
    "microphone",
    "location",
    "location-background",
    "notifications",
    "files",
    "photos",
    "contacts",
    "biometrics",
    "bluetooth",
    "sensors",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevicePermission {
    /// Custom human-readable usage reason from `permissions.json` (used in iOS `Info.plist`).
    pub reason: Option<String>,
    /// Where this permission was declared (e.g. `"tj:qr"`, `"permissions.json"`).
    pub sources: Vec<String>,
}

pub fn validate_device_permission(name: &str) -> Result<(), String> {
    if ALLOWED_DEVICE_PERMISSIONS.contains(&name) {
        Ok(())
    } else {
        Err(format!(
            "unknown device permission {name:?}; valid capabilities: {}",
            ALLOWED_DEVICE_PERMISSIONS.join(", ")
        ))
    }
}

/// Validates and merges device permissions declared in `tj.lock` (locked packages) and optional
/// `permissions.json` next to the app (`{ "device": { "<cap>": "<usage reason>" } }`).
pub fn load_app_device_permissions(app_dir: &Path) -> Result<BTreeMap<String, DevicePermission>, String> {
    let mut out: BTreeMap<String, DevicePermission> = BTreeMap::new();

    // 1. Collect from `tj.lock` (if present).
    if let Some(lock) = registry::read_lockfile(app_dir)? {
        for (pkg_name, pkg) in &lock.packages {
            let src = format!("tj:{pkg_name}");
            for cap in &pkg.permissions.device {
                validate_device_permission(cap)
                    .map_err(|e| format!("tj.lock package `{src}`: {e}"))?;
                let entry = out
                    .entry(cap.clone())
                    .or_insert_with(|| DevicePermission { reason: None, sources: Vec::new() });
                if !entry.sources.contains(&src) {
                    entry.sources.push(src.clone());
                }
            }
        }
    }

    // 2. Merge optional `permissions.json` next to `<app.tj>` (bounded read; fail closed on non-file/I/O errors).
    let perms_path = app_dir.join(PERMISSIONS_FILE);
    let mut file = match fs::File::open(&perms_path) {
        Ok(f) => f,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(format!("cannot open {}: {e}", perms_path.display())),
    };
    let mut bytes = Vec::new();
    file.by_ref()
        .take((MAX_PERMS_FILE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("cannot read {}: {e}", perms_path.display()))?;
    if bytes.len() > MAX_PERMS_FILE_BYTES {
        return Err(format!("{} exceeds {} KiB limit", perms_path.display(), MAX_PERMS_FILE_BYTES / 1024));
    }
    let val: Json = serde_json::from_slice(&bytes)
        .map_err(|e| format!("{} is not valid JSON: {e}", perms_path.display()))?;
    let obj = val
        .as_object()
        .ok_or_else(|| format!("{} must be a JSON object", perms_path.display()))?;
    for k in obj.keys() {
        if k != "device" {
            return Err(format!("{} contains unknown top-level key {k:?} (expected \"device\")", perms_path.display()));
        }
    }
    if let Some(dev) = obj.get("device") {
        let map = dev
            .as_object()
            .ok_or_else(|| format!("{}: \"device\" must be a JSON object mapping capability names to usage descriptions", perms_path.display()))?;
        for (cap, v) in map {
            validate_device_permission(cap)
                .map_err(|e| format!("{}: {e}", perms_path.display()))?;
            let reason = match v {
                Json::String(s) => {
                    let t = s.trim();
                    if t.is_empty() || t.chars().count() > MAX_REASON_CHARS || t.chars().any(char::is_control) {
                        return Err(format!(
                            "{}: usage description for {cap:?} must be 1..={MAX_REASON_CHARS} printable characters",
                            perms_path.display()
                        ));
                    }
                    Some(t.to_string())
                }
                Json::Bool(true) => None,
                _ => {
                    return Err(format!(
                        "{}: value for {cap:?} must be a usage-description string (or true)",
                        perms_path.display()
                    ));
                }
            };
            let entry = out
                .entry(cap.clone())
                .or_insert_with(|| DevicePermission { reason: None, sources: Vec::new() });
            if reason.is_some() {
                entry.reason = reason;
            }
            let src = PERMISSIONS_FILE.to_string();
            if !entry.sources.contains(&src) {
                entry.sources.push(src);
            }
        }
    }

    Ok(out)
}

/// JSON array of `{ name, sources, reason? }` entries for `tj check --manifest --json` and `GET /_tj/manifest`.
pub fn manifest_json(perms: &BTreeMap<String, DevicePermission>) -> Vec<Json> {
    perms
        .iter()
        .map(|(name, p)| {
            let mut obj = serde_json::Map::new();
            obj.insert("name".into(), json!(name));
            obj.insert("sources".into(), json!(p.sources));
            if let Some(ref r) = p.reason {
                obj.insert("reason".into(), json!(r));
            }
            Json::Object(obj)
        })
        .collect()
}

/// Built-in opt-in host device plugins shipped in `hosts/{web,android,ios}` and bundled
/// automatically by `tj build` and `tj dev` only when an app declares matching permissions.
pub fn bundled_host_plugins(perms: &BTreeMap<String, DevicePermission>) -> Vec<&'static str> {
    let mut out = Vec::new();
    if perms.contains_key("files") {
        out.push("files");
    }
    if perms.contains_key("notifications") {
        out.push("notifications");
    }
    if perms.contains_key("location") || perms.contains_key("location-background") {
        out.push("location");
    }
    if perms.contains_key("camera") {
        out.push("camera");
    }
    out
}

/// Whether a compiled app opts into the built-in WebRTC `<Native name="VideoCall" ... />` host
/// component: requires `camera` or `microphone` permission AND a compiled `<Native>` element with
/// a `"VideoCall"` string constant. Works identically for `.tj` sources and AOT `.tjb` bundles
/// while adding 0 bytes to `tj-core`.
pub fn uses_video_call(program: &tj_core::vm::program::Program, perms: &BTreeMap<String, DevicePermission>) -> bool {
    (perms.contains_key("camera") || perms.contains_key("microphone"))
        && program.elems.iter().any(|e| &*e.kind == "Native")
        && program
            .consts
            .iter()
            .any(|v| matches!(v, tj_core::vm::value::Value::Str(s) if &**s == "VideoCall"))
}

/// Generates deduplicated `<uses-permission>` and `<uses-feature>` XML lines for `AndroidManifest.xml`
/// covering `minSdk = 21` through `targetSdk = 35`.
pub fn android_manifest_xml(perms: &BTreeMap<String, DevicePermission>) -> String {
    let mut uses_perms: BTreeSet<&'static str> = BTreeSet::new();
    let mut raw_entries: BTreeSet<&'static str> = BTreeSet::new();
    let mut features: BTreeSet<&'static str> = BTreeSet::new();

    for cap in perms.keys() {
        match cap.as_str() {
            "camera" => {
                uses_perms.insert("android.permission.CAMERA");
                features.insert("android.hardware.camera");
            }
            "microphone" => {
                uses_perms.insert("android.permission.RECORD_AUDIO");
                features.insert("android.hardware.microphone");
            }
            "location" => {
                uses_perms.insert("android.permission.ACCESS_FINE_LOCATION");
                uses_perms.insert("android.permission.ACCESS_COARSE_LOCATION");
            }
            "location-background" => {
                uses_perms.insert("android.permission.ACCESS_FINE_LOCATION");
                uses_perms.insert("android.permission.ACCESS_COARSE_LOCATION");
                uses_perms.insert("android.permission.ACCESS_BACKGROUND_LOCATION");
            }
            "notifications" => {
                uses_perms.insert("android.permission.POST_NOTIFICATIONS");
            }
            "files" => {} // Scoped Storage Access Framework needs no broad storage permission.
            "photos" => {
                uses_perms.insert("android.permission.READ_MEDIA_IMAGES");
                uses_perms.insert("android.permission.READ_MEDIA_VISUAL_USER_SELECTED");
                raw_entries.insert(
                    "    <uses-permission android:name=\"android.permission.READ_EXTERNAL_STORAGE\" android:maxSdkVersion=\"32\" />\n",
                );
            }
            "contacts" => {
                uses_perms.insert("android.permission.READ_CONTACTS");
            }
            "biometrics" => {
                uses_perms.insert("android.permission.USE_BIOMETRIC");
                raw_entries.insert(
                    "    <uses-permission android:name=\"android.permission.USE_FINGERPRINT\" android:maxSdkVersion=\"27\" />\n",
                );
            }
            "bluetooth" => {
                raw_entries.insert(
                    "    <uses-permission android:name=\"android.permission.BLUETOOTH\" android:maxSdkVersion=\"30\" />\n",
                );
                raw_entries.insert(
                    "    <uses-permission android:name=\"android.permission.BLUETOOTH_ADMIN\" android:maxSdkVersion=\"30\" />\n",
                );
                raw_entries.insert(
                    "    <uses-permission android:name=\"android.permission.BLUETOOTH_SCAN\" android:usesPermissionFlags=\"neverForLocation\" />\n",
                );
                uses_perms.insert("android.permission.BLUETOOTH_CONNECT");
            }
            "sensors" => {
                uses_perms.insert("android.permission.ACTIVITY_RECOGNITION");
                uses_perms.insert("android.permission.BODY_SENSORS");
            }
            other => debug_assert!(false, "unhandled device permission {other:?}"),
        }
    }

    let mut out = String::new();
    for p in uses_perms {
        out.push_str(&format!("    <uses-permission android:name=\"{p}\" />\n"));
    }
    for line in raw_entries {
        out.push_str(line);
    }
    for f in features {
        out.push_str(&format!("    <uses-feature android:name=\"{f}\" android:required=\"false\" />\n"));
    }
    out
}

/// Generates deduplicated `<key>NS*UsageDescription</key><string>...</string>` entries for iOS `Info.plist`.
/// Custom descriptions from `permissions.json` always take precedence over generated fallbacks.
pub fn ios_plist_xml(perms: &BTreeMap<String, DevicePermission>, app_title: &str, escape_xml: fn(&str) -> String) -> String {
    // Track `(description, is_custom)` so custom descriptions overwrite earlier defaults.
    let mut keys: BTreeMap<&'static str, (String, bool)> = BTreeMap::new();
    let default_reason = |what: &str| format!("{app_title} uses {what}.");

    for (cap, perm) in perms {
        let mut insert = |plist_key: &'static str, fallback: &str| {
            let (text, is_custom) = match &perm.reason {
                Some(r) => (r.clone(), true),
                None => (default_reason(fallback), false),
            };
            match keys.get(plist_key) {
                Some((_, true)) => {}
                Some((_, false)) if !is_custom => {}
                _ => {
                    keys.insert(plist_key, (text, is_custom));
                }
            }
        };
        match cap.as_str() {
            "camera" => insert("NSCameraUsageDescription", "the camera"),
            "microphone" => insert("NSMicrophoneUsageDescription", "the microphone"),
            "location" => insert("NSLocationWhenInUseUsageDescription", "your location"),
            "location-background" => {
                insert("NSLocationWhenInUseUsageDescription", "your location");
                insert("NSLocationAlwaysAndWhenInUseUsageDescription", "your location in the background");
            }
            "photos" => insert("NSPhotoLibraryUsageDescription", "your photo library"),
            "contacts" => insert("NSContactsUsageDescription", "your contacts"),
            "biometrics" => insert("NSFaceIDUsageDescription", "Face ID to sign in"),
            "bluetooth" => insert("NSBluetoothAlwaysUsageDescription", "Bluetooth to connect to nearby devices"),
            "sensors" => insert("NSMotionUsageDescription", "motion sensors"),
            "notifications" | "files" => {}
            other => debug_assert!(false, "unhandled device permission {other:?}"),
        }
    }

    let mut out = String::new();
    for (k, (v, _)) in keys {
        out.push_str(&format!("    <key>{k}</key>\n    <string>{}</string>\n", escape_xml(&v)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tj-devperms-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn validates_merges_and_generates_android_and_ios_permissions() {
        assert!(validate_device_permission("camera").is_ok());
        assert!(validate_device_permission("location-background").is_ok());
        assert!(validate_device_permission("camra").is_err());

        // Exhaustiveness: every ALLOWED_DEVICE_PERMISSIONS entry must pass both generators cleanly.
        let all: BTreeMap<String, DevicePermission> = ALLOWED_DEVICE_PERMISSIONS
            .iter()
            .map(|c| (c.to_string(), DevicePermission { reason: None, sources: vec!["test".into()] }))
            .collect();
        let all_and = android_manifest_xml(&all);
        assert!(all_and.contains("android.hardware.microphone\" android:required=\"false\""));
        assert!(all_and.contains("android.permission.BLUETOOTH\" android:maxSdkVersion=\"30\""));
        assert!(all_and.contains("android.permission.USE_FINGERPRINT\" android:maxSdkVersion=\"27\""));
        assert!(all_and.contains("android.permission.READ_MEDIA_VISUAL_USER_SELECTED"));
        assert!(all_and.contains("android.permission.ACTIVITY_RECOGNITION"));
        let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
        let _ = ios_plist_xml(&all, "App", esc);

        let dir = tmp("merge");
        fs::write(
            dir.join("tj.lock"),
            r#"{
  "version": 1,
  "packages": {
    "qr": {
      "kind": "device-plugin",
      "version": "1.0.0",
      "trust": "official",
      "source": "https://github.com/mindmakr/tj-plugins",
      "entry": "plugin.tj",
      "commit": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
      "license": "MIT",
      "publisher": "mindmakr",
      "permissions": { "network": [], "plugins": ["qr"], "pii": [], "device": ["camera", "location", "notifications"] }
    }
  }
}"#,
        )
        .unwrap();
        // Arabic/UTF-8 description + custom location-background description overriding default location fallback.
        let arabic_reason = "يستخدم التطبيق الكاميرا لمسح رموز QR وإيصالات الدفع";
        fs::write(
            dir.join("permissions.json"),
            format!(r#"{{ "device": {{ "camera": "{arabic_reason}", "location-background": "Track delivery route" }} }}"#),
        )
        .unwrap();

        let merged = load_app_device_permissions(&dir).unwrap();
        assert_eq!(merged["camera"].reason.as_deref(), Some(arabic_reason));
        assert_eq!(merged["camera"].sources, vec!["tj:qr", "permissions.json"]);
        assert_eq!(merged["notifications"].sources, vec!["tj:qr"]);
        assert_eq!(bundled_host_plugins(&merged), vec!["notifications", "location", "camera"]);
        assert!(bundled_host_plugins(&BTreeMap::new()).is_empty());

        // `uses_video_call` checks compiled bytecode (works for .tj AND AOT .tjb, ignores comments).
        let call_prog = tj_core::compile("function App() { return <Column><Native name=\"VideoCall\" active={false} /></Column> }").unwrap();
        assert!(uses_video_call(&call_prog, &merged), "VideoCall + camera/mic opts in");
        assert!(!uses_video_call(&call_prog, &BTreeMap::new()), "missing camera/mic permission stays opted out");
        let aot_bytes = call_prog.to_bytes().unwrap();
        let aot_prog = tj_core::vm::program::Program::from_bytes(&aot_bytes).unwrap();
        assert!(uses_video_call(&aot_prog, &merged), "AOT .tjb bundle still detects VideoCall");
        let comment_only = tj_core::compile("// VideoCall in a comment\nfunction App() { return <Column><Text>hi</Text></Column> }").unwrap();
        assert!(!uses_video_call(&comment_only, &merged), "comment mentioning VideoCall does not opt in");

        let ios_xml = ios_plist_xml(&merged, "MyApp", esc);
        assert!(ios_xml.contains("<key>NSCameraUsageDescription</key>"));
        assert!(ios_xml.contains(arabic_reason));
        // Custom location-background description must win over default fallback for NSLocationWhenInUseUsageDescription.
        assert!(ios_xml.contains("<key>NSLocationWhenInUseUsageDescription</key>\n    <string>Track delivery route</string>"));
        assert!(ios_xml.contains("<key>NSLocationAlwaysAndWhenInUseUsageDescription</key>\n    <string>Track delivery route</string>"));

        // Unknown capability fails closed.
        fs::write(dir.join("permissions.json"), r#"{ "device": { "root-access": "nope" } }"#).unwrap();
        assert!(load_app_device_permissions(&dir).unwrap_err().contains("unknown device permission"));

        // A directory named permissions.json fails closed instead of being silently skipped.
        fs::remove_file(dir.join("permissions.json")).unwrap();
        fs::create_dir(dir.join("permissions.json")).unwrap();
        assert!(load_app_device_permissions(&dir).is_err());

        let _ = fs::remove_dir_all(&dir);
    }
}
