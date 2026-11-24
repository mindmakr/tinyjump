//! Sealed container / OCI Image Layout 1.0.0 packaging (`tj build --oci`):
//! wraps a compiled standalone tinyjump server executable (`TJAPP001`) and
//! optional static web assets in a deterministic, rootless `FROM scratch`
//! OCI image bundle (`oci-layout`, `index.json`, `blobs/sha256/*`, plus
//! optional `--assemble` tar archive and `Containerfile`) with zero Docker daemon.

use flate2::{Compression, GzBuilder};
use serde_json::json;
#[cfg(test)]
use serde_json::Value as Json;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use tj_core::crypto::sha256;

const NONROOT_UID: u32 = 65_532;
const NONROOT_GID: u32 = 65_532;
const MAX_TAR_SIZE: u64 = 0o777_7777_7777;

/// Architecture / executable metadata detected from a runner binary or `--arch`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OciArch {
    Amd64,
    Arm64,
    Arm,
}

impl OciArch {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "amd64" | "x86_64" => Ok(Self::Amd64),
            "arm64" | "aarch64" => Ok(Self::Arm64),
            "arm" | "armv7" | "armhf" => Ok(Self::Arm),
            other => Err(format!("unsupported --arch {other:?}; expected amd64, arm64, or arm")),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Amd64 => "amd64",
            Self::Arm64 => "arm64",
            Self::Arm => "arm",
        }
    }

    pub fn platform_str(self) -> &'static str {
        match self {
            Self::Amd64 => "linux/amd64",
            Self::Arm64 => "linux/arm64",
            Self::Arm => "linux/arm/v7",
        }
    }
}

/// Inspects an executable header to detect whether it is a Linux ELF binary and its CPU arch.
pub fn detect_elf_arch(bytes: &[u8]) -> Option<OciArch> {
    if bytes.len() < 20 || &bytes[0..4] != b"\x7fELF" {
        return None;
    }
    let e_machine = match bytes[5] {
        1 => u16::from_le_bytes([bytes[18], bytes[19]]),
        2 => u16::from_be_bytes([bytes[18], bytes[19]]),
        _ => return None,
    };
    match e_machine {
        0x003E => Some(OciArch::Amd64),
        0x00B7 => Some(OciArch::Arm64),
        0x0028 => Some(OciArch::Arm),
        _ => None,
    }
}

pub fn host_default_arch() -> OciArch {
    match std::env::consts::ARCH {
        "aarch64" => OciArch::Arm64,
        "arm" => OciArch::Arm,
        _ => OciArch::Amd64,
    }
}

pub struct OciOptions<'a> {
    pub layout_dir: Option<&'a Path>,
    pub archive_path: Option<&'a Path>,
    pub app_name: &'a str,
    pub title: &'a str,
    pub version: &'a str,
    pub app_sha256: &'a str,
    pub bytecode_bytes: usize,
    pub has_ui: bool,
    pub port: u16,
    pub arch: OciArch,
    pub executable_bytes: &'a [u8],
    pub web_assets: &'a [(&'a str, &'a [u8])],
}

pub struct OciOutput {
    pub bin_name: String,
    pub image_digest: String,
    pub config_digest: String,
    pub layer_digest: String,
    pub diff_id: String,
    pub layer_tar_bytes: usize,
    pub layer_gz_bytes: usize,
    pub web_files: Vec<String>,
    pub archive_path: Option<PathBuf>,
}

/// Sanitizes an app name for use as a container binary name (`/app/<bin_name>`).
pub fn sanitize_bin_name(raw: &str) -> String {
    let mut s: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    while s.starts_with(['.', '-']) {
        s.remove(0);
    }
    while s.ends_with(['.', '-']) {
        s.pop();
    }
    if s.is_empty() {
        s.push_str("app");
    }
    if s.len() > 64 {
        s.truncate(64);
    }
    s
}

/// Builds a deterministic rootless `FROM scratch` OCI Image Layout 1.0.0 bundle.
pub fn build_oci_image(opts: &OciOptions<'_>) -> Result<OciOutput, String> {
    let bin_name = sanitize_bin_name(opts.app_name);
    let mut entries: BTreeMap<String, (Vec<u8>, u32, bool)> = BTreeMap::new();
    entries.insert("app/".into(), (Vec::new(), 0o755, true));
    entries.insert(format!("app/{bin_name}"), (opts.executable_bytes.to_vec(), 0o755, false));
    entries.insert("data/".into(), (Vec::new(), 0o700, true));

    let mut web_files = Vec::new();
    if !opts.web_assets.is_empty() {
        entries.insert("web/".into(), (Vec::new(), 0o755, true));
        for &(rel, bytes) in opts.web_assets {
            let clean = validate_web_rel_path(rel)?;
            let mut prefix = String::from("web");
            let parts: Vec<&str> = clean.split('/').collect();
            for dir_seg in &parts[..parts.len() - 1] {
                prefix.push('/');
                prefix.push_str(dir_seg);
                entries.insert(format!("{prefix}/"), (Vec::new(), 0o755, true));
            }
            let tar_path = format!("web/{clean}");
            entries.insert(tar_path.clone(), (bytes.to_vec(), 0o644, false));
            web_files.push(clean);
        }
    }

    let mut layer_tar = TarBuilder::new();
    for (path, (data, mode, is_dir)) in &entries {
        if *is_dir {
            layer_tar.add_dir(path, *mode)?;
        } else {
            layer_tar.add_file(path, data, *mode)?;
        }
    }
    let layer_tar_bytes_vec = layer_tar.finish();
    let layer_tar_bytes = layer_tar_bytes_vec.len();
    let diff_id = format!("sha256:{}", hex_lower(&sha256(&layer_tar_bytes_vec)));

    let mut gz = GzBuilder::new().mtime(0).write(Vec::new(), Compression::default());
    gz.write_all(&layer_tar_bytes_vec)
        .map_err(|e| format!("gzip layer failed: {e}"))?;
    let layer_gz_vec = gz.finish().map_err(|e| format!("gzip layer finish failed: {e}"))?;
    let layer_gz_bytes = layer_gz_vec.len();
    let layer_digest = format!("sha256:{}", hex_lower(&sha256(&layer_gz_vec)));

    let has_web = !web_files.is_empty();
    let port_str = opts.port.to_string();
    let mut cmd = vec![
        "--host".to_string(),
        "0.0.0.0".to_string(),
        "--port".to_string(),
        port_str.clone(),
        "--data".to_string(),
        "/data".to_string(),
    ];
    if has_web {
        cmd.push("--web-dir".into());
        cmd.push("/web".into());
    }

    let port_key = format!("{}/tcp", opts.port);
    let config_json = json!({
        "created": "1970-01-01T00:00:00Z",
        "author": "tinyjump",
        "architecture": opts.arch.as_str(),
        "os": "linux",
        "config": {
            "User": format!("{NONROOT_UID}:{NONROOT_GID}"),
            "ExposedPorts": { port_key: {} },
            "Env": [
                "TJ_DATA=/data",
                format!("TJ_PORT={}", opts.port),
            ],
            "Entrypoint": [format!("/app/{bin_name}")],
            "Cmd": cmd,
            "Volumes": { "/data": {} },
            "WorkingDir": "/app",
            "Labels": {
                "org.opencontainers.image.title": opts.title,
                "org.opencontainers.image.version": opts.version,
                "org.opencontainers.image.description": format!("Sealed TinyJump AOT application unit ({bin_name})"),
                "io.tinyjump.app": bin_name,
                "io.tinyjump.bytecode.sha256": opts.app_sha256,
                "io.tinyjump.bytecode.bytes": opts.bytecode_bytes.to_string(),
                "io.tinyjump.ui": opts.has_ui.to_string(),
                "io.tinyjump.web": has_web.to_string(),
            },
        },
        "rootfs": {
            "type": "layers",
            "diff_ids": [diff_id],
        },
        "history": [{
            "created": "1970-01-01T00:00:00Z",
            "created_by": "tj build --oci",
            "comment": "Sealed AOT standalone TinyJump binary + optional static web bundle (FROM scratch)",
        }],
    });
    let config_bytes = serde_json::to_vec_pretty(&config_json).expect("config serializes");
    let config_digest = format!("sha256:{}", hex_lower(&sha256(&config_bytes)));

    let manifest_json = json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "config": {
            "mediaType": "application/vnd.oci.image.config.v1+json",
            "digest": config_digest,
            "size": config_bytes.len(),
        },
        "layers": [{
            "mediaType": "application/vnd.oci.image.layer.v1.tar+gzip",
            "digest": layer_digest,
            "size": layer_gz_bytes,
        }],
        "annotations": {
            "org.opencontainers.image.title": opts.title,
            "org.opencontainers.image.version": opts.version,
            "org.opencontainers.image.ref.name": format!("{bin_name}:{}", opts.version),
        },
    });
    let manifest_bytes = serde_json::to_vec_pretty(&manifest_json).expect("manifest serializes");
    let image_digest = format!("sha256:{}", hex_lower(&sha256(&manifest_bytes)));

    let index_json = json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.index.v1+json",
        "manifests": [{
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "digest": image_digest,
            "size": manifest_bytes.len(),
            "platform": {
                "architecture": opts.arch.as_str(),
                "os": "linux",
            },
            "annotations": {
                "org.opencontainers.image.ref.name": format!("{bin_name}:{}", opts.version),
            },
        }],
    });
    let index_bytes = serde_json::to_vec_pretty(&index_json).expect("index serializes");
    let oci_layout_bytes = b"{\"imageLayoutVersion\":\"1.0.0\"}\n";

    let mut blobs: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    blobs.insert(strip_sha_prefix(&layer_digest).to_string(), layer_gz_vec);
    blobs.insert(strip_sha_prefix(&config_digest).to_string(), config_bytes);
    blobs.insert(strip_sha_prefix(&image_digest).to_string(), manifest_bytes);

    if let Some(layout_dir) = opts.layout_dir {
        // Write clean OCI layout directory (reset stale blobs so rebuilds do not leave orphan digests).
        let blobs_dir = layout_dir.join("blobs").join("sha256");
        if layout_dir.join("blobs").exists() {
            fs::remove_dir_all(layout_dir.join("blobs"))
                .map_err(|e| format!("cannot clean stale blobs in {}: {e}", layout_dir.display()))?;
        }
        fs::create_dir_all(&blobs_dir)
            .map_err(|e| format!("cannot create {}: {e}", blobs_dir.display()))?;
        fs::write(layout_dir.join("oci-layout"), oci_layout_bytes)
            .map_err(|e| format!("cannot write oci-layout: {e}"))?;
        fs::write(layout_dir.join("index.json"), &index_bytes)
            .map_err(|e| format!("cannot write index.json: {e}"))?;
        for (hex, data) in &blobs {
            fs::write(blobs_dir.join(hex), data)
                .map_err(|e| format!("cannot write blob sha256:{hex}: {e}"))?;
        }
    }

    let archive_path = if let Some(tar_out) = opts.archive_path {
        if let Some(p) = tar_out.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(p).map_err(|e| format!("cannot create {}: {e}", p.display()))?;
        }
        let mut archive = TarBuilder::new();
        archive.add_dir("blobs/", 0o755)?;
        archive.add_dir("blobs/sha256/", 0o755)?;
        for (hex, data) in &blobs {
            archive.add_file(&format!("blobs/sha256/{hex}"), data, 0o644)?;
        }
        archive.add_file("index.json", &index_bytes, 0o644)?;
        archive.add_file("oci-layout", oci_layout_bytes, 0o644)?;
        fs::write(tar_out, archive.finish())
            .map_err(|e| format!("cannot write {}: {e}", tar_out.display()))?;
        Some(tar_out.to_path_buf())
    } else {
        None
    };

    Ok(OciOutput {
        bin_name,
        image_digest,
        config_digest,
        layer_digest,
        diff_id,
        layer_tar_bytes,
        layer_gz_bytes,
        web_files,
        archive_path,
    })
}

/// Renders a matching rootless `FROM scratch` `Containerfile` / `Dockerfile` reference
/// next to an OCI layout directory.
pub fn render_containerfile(bin_name: &str, title: &str, version: &str, app_sha256: &str, port: u16, has_web: bool) -> String {
    let web_copy = if has_web { "COPY web /web\n" } else { "" };
    let cmd_json = if has_web {
        format!("[\"--host\", \"0.0.0.0\", \"--port\", \"{port}\", \"--data\", \"/data\", \"--web-dir\", \"/web\"]")
    } else {
        format!("[\"--host\", \"0.0.0.0\", \"--port\", \"{port}\", \"--data\", \"/data\"]")
    };
    format!(
        "# Sealed rootless FROM scratch container definition for {title} ({bin_name}:{version})\n\
         # Preferred: deploy the daemonless OCI layout directly (`skopeo copy oci:./layout ...` or `--assemble` .tar).\n\
         # If building from unpacked `app/`{web_note} directories with `docker build`, run inside a staging folder holding `app/{bin_name}`{web_note2}:\n\
         FROM scratch\n\
         LABEL org.opencontainers.image.title=\"{escaped_title}\" \\\n\
               org.opencontainers.image.version=\"{version}\" \\\n\
               io.tinyjump.bytecode.sha256=\"{app_sha256}\"\n\
         COPY --chown=65532:65532 app/{bin_name} /app/{bin_name}\n\
         {web_copy}\
         ENV TJ_DATA=/data TJ_PORT={port}\n\
         WORKDIR /app\n\
         USER 65532:65532\n\
         VOLUME [\"/data\"]\n\
         EXPOSE {port}\n\
         ENTRYPOINT [\"/app/{bin_name}\"]\n\
         CMD {cmd_json}\n",
        escaped_title = title.replace(['\\', '"', '\n', '\r'], " "),
        web_note = if has_web { " and `web/`" } else { "" },
        web_note2 = if has_web { " and `web/`" } else { "" },
    )
}

/// Validates an OCI Image Layout 1.0.0 directory end-to-end:
/// `oci-layout`, `index.json`, manifest/config/layer SHA-256 digests and sizes,
/// gzip decompression, `rootfs.diff_ids`, and POSIX ustar header checksums.
#[cfg(test)]
pub fn verify_oci_layout(layout_dir: &Path) -> Result<Json, String> {
    let layout_raw = fs::read_to_string(layout_dir.join("oci-layout"))
        .map_err(|e| format!("missing oci-layout: {e}"))?;
    let layout_json: Json = serde_json::from_str(&layout_raw).map_err(|e| format!("invalid oci-layout JSON: {e}"))?;
    if layout_json["imageLayoutVersion"].as_str() != Some("1.0.0") {
        return Err(format!("expected imageLayoutVersion 1.0.0, got {}", layout_json["imageLayoutVersion"]));
    }
    let idx_raw = fs::read_to_string(layout_dir.join("index.json"))
        .map_err(|e| format!("missing index.json: {e}"))?;
    let idx: Json = serde_json::from_str(&idx_raw).map_err(|e| format!("invalid index.json: {e}"))?;
    if idx["schemaVersion"].as_u64() != Some(2) {
        return Err("index.json schemaVersion must be 2".into());
    }
    let manifest_desc = idx["manifests"]
        .as_array()
        .and_then(|a| a.first())
        .ok_or("index.json must contain at least one manifest descriptor")?;
    let manifest_bytes = read_verified_blob(layout_dir, manifest_desc)?;
    let manifest: Json = serde_json::from_slice(&manifest_bytes)
        .map_err(|e| format!("invalid OCI manifest JSON: {e}"))?;
    if manifest["schemaVersion"].as_u64() != Some(2) {
        return Err("manifest schemaVersion must be 2".into());
    }
    let config_bytes = read_verified_blob(layout_dir, &manifest["config"])?;
    let config: Json = serde_json::from_slice(&config_bytes)
        .map_err(|e| format!("invalid OCI config JSON: {e}"))?;
    let layers = manifest["layers"].as_array().ok_or("manifest layers must be an array")?;
    let diff_ids = config["rootfs"]["diff_ids"].as_array().ok_or("config rootfs.diff_ids must be an array")?;
    if layers.len() != diff_ids.len() || layers.is_empty() {
        return Err("layer count must match rootfs.diff_ids".into());
    }
    let mut tar_entries = Vec::new();
    for (idx, desc) in layers.iter().enumerate() {
        let gz_bytes = read_verified_blob(layout_dir, desc)?;
        let mut dec = flate2::read::GzDecoder::new(&gz_bytes[..]);
        let mut tar_bytes = Vec::new();
        std::io::Read::read_to_end(&mut dec, &mut tar_bytes)
            .map_err(|e| format!("gzip layer {idx} corrupt: {e}"))?;
        let got_diff = format!("sha256:{}", hex_lower(&sha256(&tar_bytes)));
        let want_diff = diff_ids[idx].as_str().ok_or("missing diff_id string")?;
        if got_diff != want_diff {
            return Err(format!("layer {idx} diff_id mismatch: expected {want_diff}, got {got_diff}"));
        }
        tar_entries.extend(parse_tar_entries(&tar_bytes)?);
    }
    Ok(json!({
        "manifestDigest": manifest_desc["digest"],
        "configDigest": manifest["config"]["digest"],
        "architecture": config["architecture"],
        "os": config["os"],
        "tarEntries": tar_entries,
    }))
}

#[cfg(test)]
fn read_verified_blob(layout_dir: &Path, desc: &Json) -> Result<Vec<u8>, String> {
    let digest = desc["digest"].as_str().ok_or("descriptor missing digest")?;
    let size = desc["size"].as_u64().ok_or("descriptor missing size")?;
    let hex = digest.strip_prefix("sha256:").ok_or_else(|| format!("unsupported digest {digest:?}"))?;
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("invalid sha256 digest {digest:?}"));
    }
    let bytes = fs::read(layout_dir.join("blobs/sha256").join(hex))
        .map_err(|e| format!("missing blob {digest}: {e}"))?;
    if bytes.len() as u64 != size {
        return Err(format!("blob {digest} size mismatch: expected {size}, got {}", bytes.len()));
    }
    let actual = hex_lower(&sha256(&bytes));
    if actual != hex {
        return Err(format!("blob {digest} sha256 mismatch: got {actual}"));
    }
    Ok(bytes)
}

fn validate_web_rel_path(rel: &str) -> Result<String, String> {
    let clean = rel.replace('\\', "/");
    if clean.is_empty() || clean.starts_with('/') || clean.contains('\0') {
        return Err(format!("invalid OCI web asset path {rel:?}"));
    }
    let parts: Vec<&str> = clean.split('/').collect();
    if parts.iter().any(|p| p.is_empty() || *p == "." || *p == "..") {
        return Err(format!("invalid OCI web asset path {rel:?}"));
    }
    // Defense in depth: never pack data directories or key material inside static web assets.
    if parts.iter().any(|p| *p == ".tj-data" || p.ends_with(".key")) {
        return Err(format!("refusing to pack sensitive asset {rel:?} inside OCI image"));
    }
    Ok(clean)
}

fn strip_sha_prefix(d: &str) -> &str {
    d.strip_prefix("sha256:").unwrap_or(d)
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Deterministic POSIX ustar writer (`mtime = 0`, `65532:65532` `nonroot`).
pub struct TarBuilder {
    buf: Vec<u8>,
}

impl TarBuilder {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    pub fn add_dir(&mut self, path: &str, mode: u32) -> Result<(), String> {
        let norm = normalize_tar_path(path, true)?;
        self.write_header(&norm, mode, 0, b'5')?;
        Ok(())
    }

    pub fn add_file(&mut self, path: &str, data: &[u8], mode: u32) -> Result<(), String> {
        let norm = normalize_tar_path(path, false)?;
        let size = data.len() as u64;
        if size > MAX_TAR_SIZE {
            return Err(format!("tar entry {norm:?} ({size} bytes) exceeds 8 GiB ustar limit"));
        }
        self.write_header(&norm, mode, size, b'0')?;
        self.buf.extend_from_slice(data);
        let pad = (512 - (data.len() % 512)) % 512;
        if pad > 0 {
            self.buf.resize(self.buf.len() + pad, 0);
        }
        Ok(())
    }

    pub fn finish(mut self) -> Vec<u8> {
        self.buf.resize(self.buf.len() + 1024, 0);
        self.buf
    }

    fn write_header(&mut self, path: &str, mode: u32, size: u64, typeflag: u8) -> Result<(), String> {
        let (prefix, name) = split_ustar_path(path)?;
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        write_octal(&mut h[100..108], u64::from(mode & 0o7777), 7);
        write_octal(&mut h[108..116], NONROOT_UID as u64, 7);
        write_octal(&mut h[116..124], NONROOT_GID as u64, 7);
        write_octal(&mut h[124..136], size, 11);
        write_octal(&mut h[136..148], 0, 11); // deterministic mtime = 0
        h[148..156].fill(b' ');
        h[156] = typeflag;
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        h[265..272].copy_from_slice(b"nonroot");
        h[297..304].copy_from_slice(b"nonroot");
        write_octal(&mut h[329..337], 0, 7);
        write_octal(&mut h[337..345], 0, 7);
        h[345..345 + prefix.len()].copy_from_slice(prefix.as_bytes());

        let sum: u32 = h.iter().map(|&b| b as u32).sum();
        let chk = format!("{:06o}\0 ", sum);
        h[148..156].copy_from_slice(chk.as_bytes());
        self.buf.extend_from_slice(&h);
        Ok(())
    }
}

fn normalize_tar_path(raw: &str, is_dir: bool) -> Result<String, String> {
    let trimmed = raw.trim_end_matches('/');
    if trimmed.is_empty() || trimmed.starts_with('/') || trimmed.contains('\\') || trimmed.contains('\0') {
        return Err(format!("invalid tar entry path {raw:?}"));
    }
    if trimmed.split('/').any(|p| p.is_empty() || p == "." || p == "..") {
        return Err(format!("unsafe tar entry path {raw:?}"));
    }
    Ok(if is_dir { format!("{trimmed}/") } else { trimmed.to_string() })
}

fn split_ustar_path(path: &str) -> Result<(&str, &str), String> {
    if path.len() <= 100 {
        return Ok(("", path));
    }
    if path.len() > 256 {
        return Err(format!("tar path {path:?} exceeds 256-byte ustar limit"));
    }
    for (idx, b) in path.bytes().enumerate().rev() {
        if b == b'/' && idx > 0 && idx <= 155 && (path.len() - idx - 1) <= 100 && (path.len() - idx - 1) > 0 {
            return Ok((&path[..idx], &path[idx + 1..]));
        }
    }
    Err(format!("tar path {path:?} cannot split into ustar prefix(<=155)/name(<=100)"))
}

fn write_octal(dst: &mut [u8], val: u64, digits: usize) {
    let s = format!("{val:0digits$o}\0", digits = digits);
    let n = s.len().min(dst.len());
    dst[..n].copy_from_slice(&s.as_bytes()[..n]);
}

/// Validates every header checksum and returns `(path, size)` entries inside a tar stream.
#[cfg(test)]
pub fn parse_tar_entries(bytes: &[u8]) -> Result<Vec<(String, u64)>, String> {
    if bytes.len() % 512 != 0 {
        return Err("tar archive is not 512-byte aligned".into());
    }
    let mut pos = 0usize;
    let mut out = Vec::new();
    while pos + 512 <= bytes.len() {
        let h = &bytes[pos..pos + 512];
        pos += 512;
        if h.iter().all(|&b| b == 0) {
            break;
        }
        let stored_chk = parse_octal(&h[148..156])?;
        let mut calc = 0u64;
        for (i, &b) in h.iter().enumerate() {
            calc += if (148..156).contains(&i) { b' ' as u64 } else { b as u64 };
        }
        if stored_chk != calc {
            return Err(format!("tar header checksum mismatch: expected {stored_chk}, got {calc}"));
        }
        let name = read_cstr(&h[..100]);
        let prefix = read_cstr(&h[345..500]);
        let full = if prefix.is_empty() { name.to_string() } else { format!("{prefix}/{name}") };
        let size = parse_octal(&h[124..136])?;
        out.push((full, size));
        let blocks = size.div_ceil(512) as usize;
        pos = pos.checked_add(blocks.checked_mul(512).ok_or("tar size overflow")?).ok_or("tar truncated")?;
        if pos > bytes.len() {
            return Err("tar entry extends past end of archive".into());
        }
    }
    Ok(out)
}

#[cfg(test)]
fn read_cstr(b: &[u8]) -> &str {
    let end = b.iter().position(|&x| x == 0).unwrap_or(b.len());
    std::str::from_utf8(&b[..end]).unwrap_or("")
}

#[cfg(test)]
fn parse_octal(b: &[u8]) -> Result<u64, String> {
    let s = std::str::from_utf8(b)
        .map_err(|_| "invalid tar octal field")?
        .trim_matches(|c: char| c == '\0' || c == ' ');
    if s.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(s, 8).map_err(|_| format!("invalid octal {s:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tj-oci-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn builds_valid_reproducible_oci_layout_and_tar_archive() {
        let dir = tmp("repro");
        let l1 = dir.join("l1");
        let t1 = dir.join("app1.tar");
        let l2 = dir.join("l2");
        let t2 = dir.join("app2.tar");

        // Minimal ELF64 x86_64 header + fake payload
        let mut fake_elf = vec![0u8; 64];
        fake_elf[0..4].copy_from_slice(b"\x7fELF");
        fake_elf[5] = 1;
        fake_elf[18..20].copy_from_slice(&0x003Eu16.to_le_bytes());
        assert_eq!(detect_elf_arch(&fake_elf), Some(OciArch::Amd64));

        let fake_wasm = b"\x00asm\x01\x00\x00\x00";
        let assets: Vec<(&str, &[u8])> = vec![
            ("index.html", b"<!doctype html>"),
            ("plugins/webrtc.js", b"export function createVideoCallComponents(){}"),
            ("tinyjump.js", b"export function start(){}"),
            ("tinyjump.wasm", fake_wasm),
        ];

        let r1 = build_oci_image(&OciOptions {
            layout_dir: Some(&l1),
            archive_path: Some(&t1),
            app_name: "Collab Studio!",
            title: "Collab Studio",
            version: "1.2.3",
            app_sha256: &"a".repeat(64),
            bytecode_bytes: 512,
            has_ui: true,
            port: 8080,
            arch: detect_elf_arch(&fake_elf).unwrap(),
            executable_bytes: &fake_elf,
            web_assets: &assets,
        })
        .unwrap();

        let r2 = build_oci_image(&OciOptions {
            layout_dir: Some(&l2),
            archive_path: Some(&t2),
            app_name: "Collab Studio!",
            title: "Collab Studio",
            version: "1.2.3",
            app_sha256: &"a".repeat(64),
            bytecode_bytes: 512,
            has_ui: true,
            port: 8080,
            arch: OciArch::Amd64,
            executable_bytes: &fake_elf,
            web_assets: &assets,
        })
        .unwrap();

        assert_eq!(r1.bin_name, "collab-studio");
        assert_eq!(r1.image_digest, r2.image_digest);
        assert_eq!(fs::read(&t1).unwrap(), fs::read(&t2).unwrap(), "OCI archives must be bit-for-bit reproducible");

        let verified = verify_oci_layout(&l1).unwrap();
        assert_eq!(verified["architecture"], json!("amd64"));
        let entry_names: Vec<String> = verified["tarEntries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e[0].as_str().unwrap().to_string())
            .collect();
        assert!(entry_names.contains(&"app/collab-studio".to_string()));
        assert!(entry_names.contains(&"data/".to_string()));
        assert!(entry_names.contains(&"web/tinyjump.wasm".to_string()));
        assert!(entry_names.contains(&"web/plugins/webrtc.js".to_string()));

        // Outer oci-archive tar is also valid ustar.
        let outer = parse_tar_entries(&fs::read(&t1).unwrap()).unwrap();
        assert!(outer.iter().any(|(n, _)| n == "oci-layout"));
        assert!(outer.iter().any(|(n, _)| n == "index.json"));

        // Direct tar-only output without writing a layout directory.
        let t_only = dir.join("tar-only.tar");
        let r3 = build_oci_image(&OciOptions {
            layout_dir: None,
            archive_path: Some(&t_only),
            app_name: "Collab Studio!",
            title: "Collab Studio",
            version: "1.2.3",
            app_sha256: &"a".repeat(64),
            bytecode_bytes: 512,
            has_ui: true,
            port: 8080,
            arch: OciArch::Amd64,
            executable_bytes: &fake_elf,
            web_assets: &assets,
        })
        .unwrap();
        assert_eq!(fs::read(&t_only).unwrap(), fs::read(&t1).unwrap());
        assert_eq!(r3.image_digest, r1.image_digest);

        // Reject unsafe asset paths & sensitive files.
        assert!(build_oci_image(&OciOptions {
            layout_dir: Some(&l1),
            archive_path: None,
            app_name: "app",
            title: "app",
            version: "0.1.0",
            app_sha256: &"0".repeat(64),
            bytecode_bytes: 1,
            has_ui: false,
            port: 4700,
            arch: OciArch::Arm64,
            executable_bytes: &fake_elf,
            web_assets: &[("../secret.key", b"x")],
        })
        .is_err());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn detects_elf_architectures_and_splits_long_ustar_paths() {
        let mut arm64 = vec![0u8; 32];
        arm64[0..4].copy_from_slice(b"\x7fELF");
        arm64[5] = 1;
        arm64[18..20].copy_from_slice(&0x00B7u16.to_le_bytes());
        assert_eq!(detect_elf_arch(&arm64), Some(OciArch::Arm64));

        let mut arm = vec![0u8; 32];
        arm[0..4].copy_from_slice(b"\x7fELF");
        arm[5] = 1;
        arm[18..20].copy_from_slice(&0x0028u16.to_le_bytes());
        assert_eq!(detect_elf_arch(&arm), Some(OciArch::Arm));

        let long_path = format!("{}/file.bin", "a".repeat(110));
        let (p, n) = split_ustar_path(&long_path).unwrap();
        assert_eq!(p.len(), 110);
        assert_eq!(n, "file.bin");
    }
}
