//! Golden-image PNG screenshot decoding, encoding, and pixel-level diffing for `tj review`.
//!
//! Pure Rust using `flate2` (miniz_oxide) and `crc32fast`, with bounded dimensions,
//! incremental decompression, and strict chunk validation so malformed PNGs cannot exhaust memory.

use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use std::io::{Read, Write};
use std::path::Path;

const PNG_SIG: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
const MAX_DIM: u32 = 16_384;
const MAX_PIXELS: usize = 64_000_000;
const MAX_IDAT_BYTES: usize = 64 * 1024 * 1024;
/// Default per-channel tolerance (out of 255) to absorb 1-LSB subpixel antialiasing jitter.
pub const DEFAULT_CHANNEL_TOLERANCE: u8 = 2;
/// Default maximum allowed changed pixel percentage (`0.1%`).
pub const DEFAULT_THRESHOLD_PCT: f64 = 0.1;

/// Decoded 8-bit RGBA image (`pixels.len() == width * height * 4`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Comparison result between a golden baseline image and an actual screenshot.
#[derive(Debug, Clone)]
pub struct DiffReport {
    pub expected_w: u32,
    pub expected_h: u32,
    pub actual_w: u32,
    pub actual_h: u32,
    pub size_mismatch: bool,
    pub total_pixels: u64,
    pub diff_pixels: u64,
    pub diff_percent: f64,
    /// Populated only when requested (`build_diff == true`) and union bounds fit `MAX_PIXELS`.
    pub diff_image: Option<Image>,
}

impl DiffReport {
    pub fn passed(&self, threshold_pct: f64) -> bool {
        !self.size_mismatch && self.diff_percent <= threshold_pct
    }

    pub fn summary(&self, threshold_pct: f64) -> String {
        if self.size_mismatch {
            format!(
                "golden: dimension mismatch (expected {}x{}, got {}x{}; {}/{} pixels differ, {:.3}%)",
                self.expected_w, self.expected_h, self.actual_w, self.actual_h, self.diff_pixels, self.total_pixels, self.diff_percent
            )
        } else {
            format!(
                "golden: {}/{} pixels differ ({:.3}%, threshold {:.3}%, {}x{})",
                self.diff_pixels, self.total_pixels, self.diff_percent, threshold_pct, self.actual_w, self.actual_h
            )
        }
    }
}

/// Decodes a standard 8-bit non-interlaced PNG (Gray, RGB, Indexed, GrayAlpha, or RGBA).
pub fn decode_png(bytes: &[u8]) -> Result<Image, String> {
    if bytes.len() < 8 || &bytes[..8] != PNG_SIG {
        return Err("not a PNG image (bad signature)".into());
    }
    let mut pos = 8usize;
    let mut width = 0u32;
    let mut height = 0u32;
    let mut color_type = 0u8;
    let mut saw_ihdr = false;
    let mut saw_plte = false;
    let mut saw_trns = false;
    let mut saw_idat = false;
    let mut idat_closed = false;
    let mut saw_iend = false;
    let mut palette: Vec<[u8; 3]> = Vec::new();
    let mut trns_palette: Vec<u8> = Vec::new();
    let mut trns_gray: Option<u8> = None;
    let mut trns_rgb: Option<[u8; 3]> = None;
    let mut idat: Vec<u8> = Vec::new();

    while pos < bytes.len() {
        if pos + 12 > bytes.len() {
            return Err("truncated PNG chunk header".into());
        }
        let len = u32::from_be_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]]) as usize;
        let ctype = &bytes[pos + 4..pos + 8];
        let data_start = pos + 8;
        let data_end = data_start.checked_add(len).ok_or("PNG chunk length overflow")?;
        let crc_end = data_end.checked_add(4).ok_or("PNG chunk CRC overflow")?;
        if crc_end > bytes.len() {
            return Err("truncated PNG chunk data".into());
        }
        let expected_crc = u32::from_be_bytes([bytes[data_end], bytes[data_end + 1], bytes[data_end + 2], bytes[data_end + 3]]);
        let actual_crc = crc32fast::hash(&bytes[pos + 4..data_end]);
        if actual_crc != expected_crc {
            return Err(format!("PNG CRC mismatch in chunk {:?}", String::from_utf8_lossy(ctype)));
        }
        let data = &bytes[data_start..data_end];
        if saw_idat && ctype != b"IDAT" {
            idat_closed = true;
        }
        match ctype {
            b"IHDR" => {
                if saw_ihdr || data.len() != 13 {
                    return Err("invalid PNG IHDR chunk".into());
                }
                width = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
                height = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
                let bit_depth = data[8];
                color_type = data[9];
                let compression = data[10];
                let filter = data[11];
                let interlace = data[12];
                if width == 0 || height == 0 || width > MAX_DIM || height > MAX_DIM {
                    return Err(format!("unsupported PNG dimensions {width}x{height} (max {MAX_DIM}x{MAX_DIM})"));
                }
                let pixels = (width as usize).saturating_mul(height as usize);
                if pixels > MAX_PIXELS {
                    return Err(format!("PNG pixel count {pixels} exceeds limit {MAX_PIXELS}"));
                }
                if bit_depth != 8 {
                    return Err(format!("unsupported PNG bit depth {bit_depth} (expected 8-bit)"));
                }
                if !matches!(color_type, 0 | 2 | 3 | 4 | 6) {
                    return Err(format!("unsupported PNG color type {color_type}"));
                }
                if compression != 0 || filter != 0 {
                    return Err("unsupported PNG compression or filter method".into());
                }
                if interlace != 0 {
                    return Err("interlaced (Adam7) PNGs are not supported".into());
                }
                saw_ihdr = true;
            }
            b"PLTE" => {
                if !saw_ihdr || saw_plte || saw_idat || data.len() % 3 != 0 || data.is_empty() || data.len() > 256 * 3 {
                    return Err("invalid PNG PLTE palette".into());
                }
                palette = data.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
                saw_plte = true;
            }
            b"tRNS" => {
                if !saw_ihdr || saw_trns || saw_idat {
                    return Err("invalid PNG tRNS chunk placement".into());
                }
                match color_type {
                    0 => {
                        if data.len() != 2 {
                            return Err("invalid PNG tRNS length for grayscale".into());
                        }
                        let v = u16::from_be_bytes([data[0], data[1]]);
                        if v <= 255 {
                            trns_gray = Some(v as u8);
                        }
                    }
                    2 => {
                        if data.len() != 6 {
                            return Err("invalid PNG tRNS length for RGB".into());
                        }
                        let r = u16::from_be_bytes([data[0], data[1]]);
                        let g = u16::from_be_bytes([data[2], data[3]]);
                        let b = u16::from_be_bytes([data[4], data[5]]);
                        if r <= 255 && g <= 255 && b <= 255 {
                            trns_rgb = Some([r as u8, g as u8, b as u8]);
                        }
                    }
                    3 => {
                        if !saw_plte || data.len() > palette.len() {
                            return Err("invalid PNG tRNS chunk for indexed palette".into());
                        }
                        trns_palette = data.to_vec();
                    }
                    _ => return Err("PNG tRNS chunk is not allowed for images with an alpha channel".into()),
                }
                saw_trns = true;
            }
            b"IDAT" => {
                if !saw_ihdr || idat_closed {
                    return Err("invalid PNG IDAT chunk ordering".into());
                }
                if idat.len().saturating_add(data.len()) > MAX_IDAT_BYTES {
                    return Err("PNG compressed data exceeds size limit".into());
                }
                idat.extend_from_slice(data);
                saw_idat = true;
            }
            b"IEND" => {
                if !saw_ihdr || !data.is_empty() {
                    return Err("invalid PNG IEND chunk".into());
                }
                saw_iend = true;
                break;
            }
            _ => {
                if !saw_ihdr {
                    return Err("PNG chunk appeared before IHDR".into());
                }
            }
        }
        pos = crc_end;
    }

    if !saw_ihdr || !saw_iend || idat.is_empty() {
        return Err("incomplete PNG image (missing IHDR, IDAT, or IEND)".into());
    }
    if color_type == 3 && palette.is_empty() {
        return Err("indexed PNG missing PLTE chunk".into());
    }

    let bpp: usize = match color_type {
        0 | 3 => 1,
        2 => 3,
        4 => 2,
        6 => 4,
        _ => unreachable!(),
    };
    let stride = (width as usize).checked_mul(bpp).ok_or("PNG stride overflow")?;
    let expected_raw = (height as usize).checked_mul(stride + 1).ok_or("PNG buffer overflow")?;

    // Grow incrementally so a tiny corrupt IDAT claiming huge IHDR dimensions cannot trigger an eager multi-hundred-MiB allocation.
    let mut raw = Vec::with_capacity(expected_raw.min(64 * 1024));
    let mut decoder = ZlibDecoder::new(idat.as_slice()).take((expected_raw + 1) as u64);
    decoder.read_to_end(&mut raw).map_err(|e| format!("PNG zlib decode failed: {e}"))?;
    if raw.len() != expected_raw {
        return Err(format!("PNG decompressed size mismatch (expected {expected_raw}, got {})", raw.len()));
    }

    let mut prev = vec![0u8; stride];
    let mut cur = vec![0u8; stride];
    let mut pixels = Vec::with_capacity((width as usize) * (height as usize) * 4);

    for row in 0..height as usize {
        let row_start = row * (stride + 1);
        let filter = raw[row_start];
        let src = &raw[row_start + 1..row_start + 1 + stride];
        match filter {
            0 => cur.copy_from_slice(src),
            1 => {
                for i in 0..stride {
                    let a = if i >= bpp { cur[i - bpp] } else { 0 };
                    cur[i] = src[i].wrapping_add(a);
                }
            }
            2 => {
                for i in 0..stride {
                    cur[i] = src[i].wrapping_add(prev[i]);
                }
            }
            3 => {
                for i in 0..stride {
                    let a = if i >= bpp { u16::from(cur[i - bpp]) } else { 0 };
                    let b = u16::from(prev[i]);
                    cur[i] = src[i].wrapping_add(((a + b) / 2) as u8);
                }
            }
            4 => {
                for i in 0..stride {
                    let a = if i >= bpp { cur[i - bpp] } else { 0 };
                    let b = prev[i];
                    let c = if i >= bpp { prev[i - bpp] } else { 0 };
                    cur[i] = src[i].wrapping_add(paeth(a, b, c));
                }
            }
            other => return Err(format!("invalid PNG scanline filter {other}")),
        }

        match color_type {
            0 => {
                for &g in &cur {
                    let a = if trns_gray == Some(g) { 0 } else { 255 };
                    pixels.extend_from_slice(&[g, g, g, a]);
                }
            }
            2 => {
                for rgb in cur.chunks_exact(3) {
                    let a = if trns_rgb == Some([rgb[0], rgb[1], rgb[2]]) { 0 } else { 255 };
                    pixels.extend_from_slice(&[rgb[0], rgb[1], rgb[2], a]);
                }
            }
            3 => {
                for &idx in &cur {
                    let [r, g, b] = *palette.get(idx as usize).ok_or("PNG palette index out of range")?;
                    let a = trns_palette.get(idx as usize).copied().unwrap_or(255);
                    pixels.extend_from_slice(&[r, g, b, a]);
                }
            }
            4 => {
                for ga in cur.chunks_exact(2) {
                    pixels.extend_from_slice(&[ga[0], ga[0], ga[0], ga[1]]);
                }
            }
            6 => pixels.extend_from_slice(&cur),
            _ => unreachable!(),
        }
        std::mem::swap(&mut prev, &mut cur);
    }

    Ok(Image { width, height, pixels })
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let a = i32::from(a);
    let b = i32::from(b);
    let c = i32::from(c);
    let p = a + b - c;
    let pa = (p - a).abs();
    let pb = (p - b).abs();
    let pc = (p - c).abs();
    if pa <= pb && pa <= pc {
        a as u8
    } else if pb <= pc {
        b as u8
    } else {
        c as u8
    }
}

/// Encodes an 8-bit RGBA [`Image`] into a standard PNG byte stream.
pub fn encode_png(img: &Image) -> Result<Vec<u8>, String> {
    let expected = (img.width as usize).saturating_mul(img.height as usize).saturating_mul(4);
    if img.width == 0 || img.height == 0 || img.pixels.len() != expected {
        return Err("invalid RGBA image dimensions or buffer length".into());
    }
    let mut out = Vec::with_capacity(64 + img.pixels.len() / 2);
    out.extend_from_slice(PNG_SIG);

    let mut ihdr = [0u8; 13];
    ihdr[0..4].copy_from_slice(&img.width.to_be_bytes());
    ihdr[4..8].copy_from_slice(&img.height.to_be_bytes());
    ihdr[8] = 8; // 8-bit
    ihdr[9] = 6; // RGBA
    write_chunk(&mut out, b"IHDR", &ihdr);

    let stride = (img.width as usize) * 4;
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    for row in img.pixels.chunks_exact(stride) {
        enc.write_all(&[0u8]).map_err(|e| e.to_string())?;
        enc.write_all(row).map_err(|e| e.to_string())?;
    }
    let compressed = enc.finish().map_err(|e| e.to_string())?;
    write_chunk(&mut out, b"IDAT", &compressed);
    write_chunk(&mut out, b"IEND", &[]);
    Ok(out)
}

fn write_chunk(out: &mut Vec<u8>, ctype: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let crc_start = out.len();
    out.extend_from_slice(ctype);
    out.extend_from_slice(data);
    let crc = crc32fast::hash(&out[crc_start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// Compares `expected` against `actual` pixel by pixel. Allocates a red-highlighted visual diff
/// image only when `build_diff` is `true` and the union bounds fit within `MAX_PIXELS`.
pub fn compare(expected: &Image, actual: &Image, channel_tolerance: u8, build_diff: bool) -> DiffReport {
    let w = expected.width.max(actual.width);
    let h = expected.height.max(actual.height);
    let size_mismatch = expected.width != actual.width || expected.height != actual.height;
    let total_pixels = u64::from(w) * u64::from(h);
    let overlap_w = expected.width.min(actual.width);
    let overlap_h = expected.height.min(actual.height);
    let overlap_pixels = u64::from(overlap_w) * u64::from(overlap_h);
    let can_alloc_diff = build_diff && (w as usize).checked_mul(h as usize).is_some_and(|p| p <= MAX_PIXELS);

    let mut diff_pixels = total_pixels - overlap_pixels;
    let mut diff_buf = if can_alloc_diff {
        let mut buf = vec![0u8; (w as usize) * (h as usize) * 4];
        if size_mismatch {
            // Pre-fill out-of-overlap border as red changed pixels.
            for px in buf.chunks_exact_mut(4) {
                px.copy_from_slice(&[255, 32, 32, 255]);
            }
        }
        Some(buf)
    } else {
        None
    };

    for y in 0..overlap_h {
        for x in 0..overlap_w {
            let ei = ((y as usize) * (expected.width as usize) + (x as usize)) * 4;
            let ai = ((y as usize) * (actual.width as usize) + (x as usize)) * 4;
            let e = &expected.pixels[ei..ei + 4];
            let a = &actual.pixels[ai..ai + 4];
            let changed = e[0].abs_diff(a[0]) > channel_tolerance
                || e[1].abs_diff(a[1]) > channel_tolerance
                || e[2].abs_diff(a[2]) > channel_tolerance
                || e[3].abs_diff(a[3]) > channel_tolerance;
            if changed {
                diff_pixels += 1;
            }
            if let Some(buf) = diff_buf.as_mut() {
                let dst = ((y as usize) * (w as usize) + (x as usize)) * 4;
                if changed {
                    buf[dst..dst + 4].copy_from_slice(&[255, 32, 32, 255]);
                } else {
                    let luma = ((u16::from(a[0]) * 30 + u16::from(a[1]) * 59 + u16::from(a[2]) * 11) / 100) as u8;
                    let dimmed = (u16::from(luma) / 3 + 170).min(255) as u8;
                    buf[dst..dst + 4].copy_from_slice(&[dimmed, dimmed, dimmed, 255]);
                }
            }
        }
    }

    let diff_percent = if total_pixels == 0 {
        0.0
    } else {
        (diff_pixels as f64 / total_pixels as f64) * 100.0
    };

    DiffReport {
        expected_w: expected.width,
        expected_h: expected.height,
        actual_w: actual.width,
        actual_h: actual.height,
        size_mismatch,
        total_pixels,
        diff_pixels,
        diff_percent,
        diff_image: diff_buf.map(|pixels| Image { width: w, height: h, pixels }),
    }
}

/// Runs the golden-image baseline update or comparison workflow used by `tj review --golden`.
pub fn check_or_update(
    golden_path: &Path,
    actual_png: &[u8],
    update_golden: bool,
    threshold_pct: f64,
    diff_out: Option<&Path>,
) -> Result<String, String> {
    let actual_img = decode_png(actual_png)?;
    if update_golden {
        if let Some(parent) = golden_path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        std::fs::write(golden_path, actual_png).map_err(|e| format!("cannot write {}: {e}", golden_path.display()))?;
        return Ok(format!(
            "updated golden screenshot: {} ({}x{})",
            golden_path.display(),
            actual_img.width,
            actual_img.height
        ));
    }
    let expected_png = std::fs::read(golden_path).map_err(|e| {
        format!("cannot read golden screenshot {}: {e} (pass --update-golden to create it)", golden_path.display())
    })?;
    let expected_img = decode_png(&expected_png)?;
    let report = compare(&expected_img, &actual_img, DEFAULT_CHANNEL_TOLERANCE, diff_out.is_some());
    let mut msg = report.summary(threshold_pct);
    if report.passed(threshold_pct) {
        // Remove any stale diff artifact left behind by a previous failing run.
        if let Some(diff_path) = diff_out {
            let _ = std::fs::remove_file(diff_path);
        }
        return Ok(msg);
    }
    if let Some(diff_path) = diff_out {
        match &report.diff_image {
            Some(img) => {
                if let Some(parent) = diff_path.parent().filter(|p| !p.as_os_str().is_empty()) {
                    std::fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
                }
                let diff_png = encode_png(img)?;
                std::fs::write(diff_path, diff_png).map_err(|e| format!("cannot write diff image {}: {e}", diff_path.display()))?;
                msg.push_str(&format!("\nwrote visual diff: {}", diff_path.display()));
            }
            None => {
                msg.push_str("\nskipped visual diff image: union dimensions exceed pixel limit");
            }
        }
    }
    Err(format!("golden screenshot mismatch ({msg})"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, rgba: [u8; 4]) -> Image {
        let mut pixels = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..(w * h) {
            pixels.extend_from_slice(&rgba);
        }
        Image { width: w, height: h, pixels }
    }

    fn make_png(w: u32, h: u32, color_type: u8, extra_chunks: &[(&[u8; 4], &[u8])], raw_scanlines: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(PNG_SIG);
        let mut ihdr = [0u8; 13];
        ihdr[0..4].copy_from_slice(&w.to_be_bytes());
        ihdr[4..8].copy_from_slice(&h.to_be_bytes());
        ihdr[8] = 8;
        ihdr[9] = color_type;
        write_chunk(&mut out, b"IHDR", &ihdr);
        for &(ct, data) in extra_chunks {
            write_chunk(&mut out, ct, data);
        }
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(raw_scanlines).unwrap();
        write_chunk(&mut out, b"IDAT", &enc.finish().unwrap());
        write_chunk(&mut out, b"IEND", &[]);
        out
    }

    #[test]
    fn round_trips_rgba_png_and_diffs_pixels_accurately() {
        let base = solid(10, 10, [240, 240, 240, 255]);
        let png = encode_png(&base).expect("encodes");
        let decoded = decode_png(&png).expect("decodes");
        assert_eq!(decoded, base);

        // Identical image -> 0% diff, no diff buffer allocated when build_diff == false
        let same = compare(&base, &decoded, DEFAULT_CHANNEL_TOLERANCE, false);
        assert!(same.passed(DEFAULT_THRESHOLD_PCT));
        assert_eq!(same.diff_pixels, 0);
        assert!(same.diff_image.is_none());

        // Tiny 1-LSB antialiasing shift stays within tolerance
        let mut jitter = base.clone();
        jitter.pixels[0] = 241;
        let tol = compare(&base, &jitter, DEFAULT_CHANNEL_TOLERANCE, false);
        assert!(tol.passed(DEFAULT_THRESHOLD_PCT));
        assert_eq!(tol.diff_pixels, 0);

        // Real 1-pixel visual change (1% of 100 pixels > 0.1% threshold) fails and writes red diff pixel
        let mut changed = base.clone();
        changed.pixels[0..4].copy_from_slice(&[10, 20, 30, 255]);
        let report = compare(&base, &changed, DEFAULT_CHANNEL_TOLERANCE, true);
        assert!(!report.passed(DEFAULT_THRESHOLD_PCT));
        assert_eq!(report.diff_pixels, 1);
        assert!((report.diff_percent - 1.0).abs() < 1e-6);
        let diff_img = report.diff_image.as_ref().expect("diff image built");
        assert_eq!(&diff_img.pixels[0..4], &[255, 32, 32, 255]);
        let diff_png = encode_png(diff_img).expect("diff png encodes");
        assert_eq!(decode_png(&diff_png).expect("diff png decodes"), *diff_img);

        // Dimension mismatch always fails even if threshold is 100%
        let smaller = solid(8, 10, [240, 240, 240, 255]);
        let dim = compare(&base, &smaller, DEFAULT_CHANNEL_TOLERANCE, true);
        assert!(dim.size_mismatch);
        assert!(!dim.passed(100.0));
        assert_eq!(dim.diff_pixels, 20);

        // C-057.1 OOM guard: mismatched aspect ratios (16384x1 vs 1x16384 -> 268M union pixels)
        // must NOT allocate a 1 GiB diff buffer, while still reporting exact diff count!
        let wide = solid(16_384, 1, [10, 20, 30, 255]);
        let tall = solid(1, 16_384, [10, 20, 30, 255]);
        let huge = compare(&wide, &tall, DEFAULT_CHANNEL_TOLERANCE, true);
        assert!(huge.size_mismatch);
        assert!(huge.diff_image.is_none(), "union > MAX_PIXELS must skip diff buffer allocation");
        assert_eq!(huge.total_pixels, 16_384 * 16_384);
        assert_eq!(huge.diff_pixels, 16_384 * 16_384 - 1);
    }

    #[test]
    fn decodes_all_png_filters_color_types_trns_and_chunk_validation() {
        // 2x5 RGB PNG exercising every filter type (0..=4) + RGB tRNS key ([10, 20, 30]).
        let rows: [[u8; 6]; 5] = [
            [10, 20, 30, 40, 50, 60],
            [15, 25, 35, 55, 75, 95],
            [20, 30, 40, 60, 80, 100],
            [25, 35, 45, 70, 90, 110],
            [30, 40, 50, 80, 100, 120],
        ];
        let mut raw = Vec::new();
        for (r, row) in rows.iter().enumerate() {
            let filter = r as u8;
            raw.push(filter);
            for i in 0..6 {
                let a = if i >= 3 { row[i - 3] } else { 0 };
                let b = if r > 0 { rows[r - 1][i] } else { 0 };
                let c = if r > 0 && i >= 3 { rows[r - 1][i - 3] } else { 0 };
                let pred = match filter {
                    0 => 0,
                    1 => a,
                    2 => b,
                    3 => ((u16::from(a) + u16::from(b)) / 2) as u8,
                    4 => paeth(a, b, c),
                    _ => unreachable!(),
                };
                raw.push(row[i].wrapping_sub(pred));
            }
        }
        let rgb_trns = [0, 10, 0, 20, 0, 30];
        let out = make_png(2, 5, 2, &[(b"tRNS", &rgb_trns)], &raw);
        let decoded = decode_png(&out).expect("all 5 PNG filters decode");
        assert_eq!((decoded.width, decoded.height), (2, 5));
        assert_eq!(&decoded.pixels[0..8], &[10, 20, 30, 0, 40, 50, 60, 255], "RGB tRNS pixel gets alpha 0");
        assert_eq!(&decoded.pixels[32..40], &[30, 40, 50, 255, 80, 100, 120, 255]);

        // Corrupt a payload byte inside IDAT -> CRC mismatch rejected
        let mut bad = out.clone();
        let mid = bad.len() - 20;
        bad[mid] ^= 0xff;
        assert!(decode_png(&bad).unwrap_err().contains("CRC mismatch"));

        // Color type 0 (Grayscale) + tRNS
        let gray = make_png(2, 1, 0, &[(b"tRNS", &[0, 42])], &[0, 42, 99]);
        assert_eq!(decode_png(&gray).unwrap().pixels, vec![42, 42, 42, 0, 99, 99, 99, 255]);

        // Color type 4 (GrayAlpha)
        let ga = make_png(2, 1, 4, &[], &[0, 50, 128, 200, 255]);
        assert_eq!(decode_png(&ga).unwrap().pixels, vec![50, 50, 50, 128, 200, 200, 200, 255]);

        // Color type 3 (Indexed) + PLTE + tRNS
        let idx = make_png(2, 1, 3, &[(b"PLTE", &[255, 0, 0, 0, 255, 0]), (b"tRNS", &[128])], &[0, 0, 1]);
        assert_eq!(decode_png(&idx).unwrap().pixels, vec![255, 0, 0, 128, 0, 255, 0, 255]);

        // Reject tRNS on RGBA (color type 6) and tRNS before PLTE on Indexed (color type 3)
        let bad_trns_rgba = make_png(1, 1, 6, &[(b"tRNS", &[0, 0])], &[0, 1, 2, 3, 4]);
        assert!(decode_png(&bad_trns_rgba).unwrap_err().contains("tRNS"));
        let bad_trns_order = make_png(1, 1, 3, &[(b"tRNS", &[128]), (b"PLTE", &[255, 0, 0])], &[0, 0]);
        assert!(decode_png(&bad_trns_order).unwrap_err().contains("tRNS"));
    }

    #[test]
    fn check_or_update_workflow_creates_baseline_cleans_stale_diff_and_writes_diff_on_failure() {
        let dir = std::env::temp_dir().join(format!("tj-golden-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let golden = dir.join("screens/home.png");
        let diff = dir.join("out/home.diff.png");
        let base_img = solid(8, 8, [200, 200, 200, 255]);
        let base_png = encode_png(&base_img).unwrap();

        // Missing golden gives clear fix hint
        let missing = check_or_update(&golden, &base_png, false, DEFAULT_THRESHOLD_PCT, None).unwrap_err();
        assert!(missing.contains("--update-golden"), "{missing}");

        // --update-golden writes the baseline cleanly
        let upd = check_or_update(&golden, &base_png, true, DEFAULT_THRESHOLD_PCT, None).expect("updates");
        assert!(upd.contains("8x8"), "{upd}");
        assert!(golden.exists());

        // Changed screenshot fails and emits a valid red-highlighted diff PNG
        let mut regressed = base_img.clone();
        regressed.pixels[0..4].copy_from_slice(&[0, 0, 0, 255]);
        let reg_png = encode_png(&regressed).unwrap();
        let err = check_or_update(&golden, &reg_png, false, DEFAULT_THRESHOLD_PCT, Some(&diff)).unwrap_err();
        assert!(err.contains("golden screenshot mismatch") && err.contains("1/64"), "{err}");
        let diff_decoded = decode_png(&std::fs::read(&diff).unwrap()).expect("diff PNG valid");
        assert_eq!(&diff_decoded.pixels[0..4], &[255, 32, 32, 255]);

        // Subsequent passing run (even with 1 changed pixel <= 5% threshold, or 0 changed pixels) removes stale diff!
        let ok = check_or_update(&golden, &reg_png, false, 5.0, Some(&diff)).expect("within threshold");
        assert!(ok.contains("1/64 pixels differ"), "{ok}");
        assert!(!diff.exists(), "passing run must remove stale --diff artifact");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
