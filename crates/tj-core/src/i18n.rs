//! Translations and locales (`locales/*.json`, `t("key", { count })`, CLDR plurals,
//! base-locale fallback, and pseudo-localization). Zero I/O and zero ICU data.

use crate::modules::File;
use crate::syntax::{Diagnostic, Span};
use crate::vm::value::Record;
use serde_json::Value as Json;
use std::rc::Rc;

/// CLDR cardinal plural categories supported in `locales/*.json`, in fixed slot order.
pub const PLURAL_FORMS: [&str; 6] = ["zero", "one", "two", "few", "many", "other"];
pub const PLURAL_OTHER: usize = 5;

/// Right-to-left primary language subtags (matches Web, Android, and iOS hosts).
pub const RTL_LANGUAGES: &[&str] = &["ar", "he", "fa", "ur", "ps", "sd", "ug", "yi", "dv", "ku"];

/// One translation entry: either a plain template string or a 6-slot CLDR plural table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Text(Rc<str>),
    Plural(Box<[Option<Rc<str>>; 6]>),
}

/// One locale's sorted key-to-message table (binary-searched with zero extra map codegen).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocaleTable {
    pub tag: Rc<str>,
    pub entries: Vec<(Rc<str>, Message)>,
}

impl LocaleTable {
    pub fn get(&self, key: &str) -> Option<&Message> {
        self.entries.iter().find(|(k, _)| k.as_ref() == key).map(|(_, m)| m)
    }
}

/// Compiled locale message tables attached to a [`crate::vm::program::Program`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Locales {
    /// Normalized base locale tag (e.g. `"en"`), or empty when the app has no locale files.
    pub base: Rc<str>,
    /// Per-locale message tables, sorted by normalized locale tag.
    pub tables: Vec<LocaleTable>,
}

/// Maps a CLDR plural category name to its slot index in `PLURAL_FORMS`.
pub fn plural_index(name: &str) -> Option<usize> {
    PLURAL_FORMS.iter().position(|&f| f == name)
}

/// Extracts the locale tag from a bundle path like `"locales/en.json"` or `"locales/en-GB.json"`.
pub fn locale_tag_from_path(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("locales/")?;
    let tag = rest.strip_suffix(".json")?;
    if tag.is_empty() || tag.contains('/') || tag.contains('\\') {
        return None;
    }
    Some(tag)
}

/// Whether `path` is a locale message file inside a bundle (`locales/<tag>.json`).
pub fn is_locale_file(path: &str) -> bool {
    locale_tag_from_path(path).is_some()
}

/// Normalizes a BCP-47 locale tag for lookup (`"en_GB"` -> `"en-gb"`).
pub fn normalize_tag(tag: &str) -> String {
    tag.trim().replace('_', "-").to_ascii_lowercase()
}

/// Primary language subtag (`"ar-SA"` -> `"ar"`).
pub fn primary_language(locale: &str) -> String {
    locale
        .trim()
        .split(['-', '_'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// Whether `locale` reads right-to-left (Arabic, Hebrew, Persian, Urdu, ...).
pub fn is_rtl_locale(locale: &str) -> bool {
    let lang = primary_language(locale);
    RTL_LANGUAGES.contains(&lang.as_str())
}

/// Extracts unique `{placeholder}` names from a message template, respecting `{{` / `}}` escapes.
pub fn placeholders(template: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'{' {
                i += 2;
                continue;
            }
            if let Some(rel_end) = template[i + 1..].find('}') {
                let name = &template[i + 1..i + 1 + rel_end];
                if is_placeholder_name(name) && !out.iter().any(|p| p == name) {
                    out.push(name.to_string());
                }
                i += rel_end + 2;
                continue;
            }
        } else if bytes[i] == b'}' && i + 1 < bytes.len() && bytes[i + 1] == b'}' {
            i += 2;
            continue;
        }
        i += 1;
    }
    out
}

fn is_placeholder_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

/// Every placeholder name required by `msg`. Plural messages always require `"count"`.
pub fn required_placeholders(msg: &Message) -> Vec<String> {
    match msg {
        Message::Text(s) => placeholders(s),
        Message::Plural(forms) => {
            let mut out = vec!["count".to_string()];
            for tmpl in forms.iter().flatten() {
                for p in placeholders(tmpl) {
                    if !out.contains(&p) {
                        out.push(p);
                    }
                }
            }
            out
        }
    }
}

/// Selects the CLDR cardinal plural category slot index (`PLURAL_FORMS`) for `n` in `locale`.
pub fn plural_category_index(locale: &str, n: f64) -> usize {
    let lang = primary_language(locale);
    let abs = n.abs();
    let is_int = abs.is_finite() && abs.fract() == 0.0;
    let i = if is_int { abs as u64 } else { 0 };
    match lang.as_str() {
        "ar" => {
            if !is_int {
                5
            } else if i == 0 {
                0
            } else if i == 1 {
                1
            } else if i == 2 {
                2
            } else if (3..=10).contains(&(i % 100)) {
                3
            } else if (11..=99).contains(&(i % 100)) {
                4
            } else {
                5
            }
        }
        "ru" | "uk" | "sr" | "hr" | "bs" => {
            if !is_int {
                5
            } else if i % 10 == 1 && i % 100 != 11 {
                1
            } else if (2..=4).contains(&(i % 10)) && !(12..=14).contains(&(i % 100)) {
                3
            } else {
                4
            }
        }
        "pl" => {
            if !is_int {
                5
            } else if i == 1 {
                1
            } else if (2..=4).contains(&(i % 10)) && !(12..=14).contains(&(i % 100)) {
                3
            } else {
                4
            }
        }
        "cs" | "sk" => {
            if !is_int {
                5
            } else if i == 1 {
                1
            } else if (2..=4).contains(&i) {
                3
            } else {
                5
            }
        }
        "fr" | "pt" | "hi" | "fa" | "bn" => {
            if is_int && (i == 0 || i == 1) {
                1
            } else {
                5
            }
        }
        "zh" | "ja" | "ko" | "vi" | "th" | "id" | "ms" | "tr" => 5,
        _ => {
            if is_int && i == 1 {
                1
            } else {
                5
            }
        }
    }
}

/// Selects the CLDR cardinal plural category name for `n` in `locale`.
pub fn plural_category(locale: &str, n: f64) -> &'static str {
    PLURAL_FORMS[plural_category_index(locale, n)]
}

/// Parses every `locales/<tag>.json` file in a bundle into [`Locales`].
pub fn parse_locale_files(src: &str, files: &[File]) -> Result<Locales, Vec<Diagnostic>> {
    if files.is_empty() {
        return Ok(Locales::default());
    }
    let mut errors = Vec::new();
    let mut tables: Vec<LocaleTable> = Vec::with_capacity(files.len());
    for f in files {
        let Some(raw_tag) = locale_tag_from_path(&f.name) else { continue };
        let span = Span { start: f.start as u32, end: f.end.min(f.start.saturating_add(1)) as u32 };
        let tag = normalize_tag(raw_tag);
        if tag == "pseudo" {
            errors.push(
                Diagnostic::new("locale tag 'pseudo' is reserved for built-in pseudo-localization", span)
                    .hint("use a BCP-47 locale tag like locales/en.json or locales/ar.json"),
            );
            continue;
        }
        if tables.iter().any(|t| t.tag.as_ref() == tag) {
            errors.push(Diagnostic::new(format!("duplicate locale file for '{tag}'"), span));
            continue;
        }
        let text = &src[f.start..f.end];
        let parsed: Json = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(e) => {
                errors.push(Diagnostic::new(format!("invalid JSON in {}: {e}", f.name), span));
                continue;
            }
        };
        let Some(obj) = parsed.as_object() else {
            errors.push(
                Diagnostic::new(format!("{} must be a JSON object mapping keys to translations", f.name), span)
                    .hint("write { \"greeting\": \"Hello\" }"),
            );
            continue;
        };
        let mut entries: Vec<(Rc<str>, Message)> = Vec::with_capacity(obj.len());
        for (k, v) in obj {
            match v {
                Json::String(s) => {
                    entries.push((Rc::from(k.as_str()), Message::Text(Rc::from(s.as_str()))));
                }
                Json::Object(m) => {
                    let mut forms: [Option<Rc<str>>; 6] = Default::default();
                    let mut ok = true;
                    for (fk, fv) in m {
                        let Some(idx) = plural_index(fk) else {
                            errors.push(
                                Diagnostic::new(format!("unknown plural form '{fk}' in {} for key '{k}'", f.name), span)
                                    .hint("supported plural forms: zero, one, two, few, many, other"),
                            );
                            ok = false;
                            continue;
                        };
                        match fv.as_str() {
                            Some(s) => forms[idx] = Some(Rc::from(s)),
                            None => {
                                errors.push(Diagnostic::new(format!("plural form '{fk}' for '{k}' in {} must be a string", f.name), span));
                                ok = false;
                            }
                        }
                    }
                    if forms[PLURAL_OTHER].is_none() {
                        errors.push(
                            Diagnostic::new(format!("plural message '{k}' in {} must include an \"other\" form", f.name), span)
                                .hint("add an \"other\" fallback template, e.g. \"other\": \"{count} items\""),
                        );
                        ok = false;
                    }
                    if ok {
                        entries.push((Rc::from(k.as_str()), Message::Plural(Box::new(forms))));
                    }
                }
                _ => {
                    errors.push(
                        Diagnostic::new(format!("translation '{k}' in {} must be a string or plural object", f.name), span)
                            .hint("use a string like \"Hello\" or a plural object like { \"one\": \"1 item\", \"other\": \"{count} items\" }"),
                    );
                }
            }
        }
        tables.push(LocaleTable { tag: Rc::from(tag.as_str()), entries });
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    let base = if tables.iter().any(|t| t.tag.as_ref() == "en") {
        Rc::from("en")
    } else if let Some(t) = tables.iter().find(|t| t.tag.starts_with("en-")) {
        t.tag.clone()
    } else {
        tables.first().map(|t| t.tag.clone()).unwrap_or_else(|| Rc::from(""))
    };
    Ok(Locales { base, tables })
}

impl Locales {
    pub fn is_empty(&self) -> bool {
        self.tables.is_empty()
    }

    pub fn table(&self, tag: &str) -> Option<&LocaleTable> {
        self.tables.iter().find(|t| t.tag.as_ref() == tag)
    }

    pub fn base_table(&self) -> Option<&LocaleTable> {
        self.table(&self.base)
    }

    pub fn tags(&self) -> impl Iterator<Item = &str> {
        self.tables.iter().map(|t| t.tag.as_ref())
    }

    /// Resolves `key` for `locale` with base-language and base-locale fallback, CLDR plural
    /// selection, `{placeholder}` interpolation, and built-in `"pseudo"` pseudo-localization.
    pub fn translate(&self, locale: &str, key: &str, params: Option<&Record>) -> String {
        if self.tables.is_empty() {
            return key.to_string();
        }
        let pseudo = locale.eq_ignore_ascii_case("pseudo");
        let norm = normalize_tag(locale);
        let lang = primary_language(locale);
        let (eff_locale, msg) = if pseudo {
            match self.base_table().and_then(|t| t.get(key)) {
                Some(m) => (self.base.as_ref(), m),
                None => return key.to_string(),
            }
        } else if let Some(m) = self.table(&norm).and_then(|t| t.get(key)) {
            (norm.as_str(), m)
        } else if lang != norm && self.table(&lang).and_then(|t| t.get(key)).is_some() {
            let m = self.table(&lang).and_then(|t| t.get(key)).unwrap();
            (lang.as_str(), m)
        } else if let Some(m) = self.base_table().and_then(|t| t.get(key)) {
            (self.base.as_ref(), m)
        } else {
            return key.to_string();
        };
        let template = match msg {
            Message::Text(s) => s.as_ref(),
            Message::Plural(forms) => {
                let count = params
                    .and_then(|r| r.get("count"))
                    .and_then(|v| match v {
                        crate::vm::value::Value::Num(n) => Some(*n),
                        _ => None,
                    })
                    .unwrap_or(0.0);
                if count == 0.0 {
                    if let Some(z) = &forms[0] {
                        z.as_ref()
                    } else {
                        let cat = plural_category_index(eff_locale, count);
                        forms[cat].as_deref().or(forms[PLURAL_OTHER].as_deref()).unwrap_or(key)
                    }
                } else {
                    let cat = plural_category_index(eff_locale, count);
                    forms[cat].as_deref().or(forms[PLURAL_OTHER].as_deref()).unwrap_or(key)
                }
            }
        };
        interpolate(template, params, pseudo)
    }

    /// Check-time parity warnings: missing keys, extra keys, and plural/placeholder mismatches.
    #[cfg(any(test, feature = "check"))]
    pub fn warnings(&self) -> Vec<String> {
        let mut out = Vec::new();
        let Some(base_table) = self.base_table() else { return out };
        for table in &self.tables {
            if table.tag == self.base {
                continue;
            }
            let loc = table.tag.as_ref();
            for (k, base_msg) in &base_table.entries {
                match table.get(k) {
                    None => out.push(format!("locales/{loc}.json is missing key '{k}' (falls back to '{}')", self.base)),
                    Some(msg) => {
                        match (base_msg, msg) {
                            (Message::Plural(_), Message::Text(_)) => {
                                out.push(format!("locales/{loc}.json key '{k}' is a string, but base locale '{}' uses a plural object", self.base));
                            }
                            (Message::Text(_), Message::Plural(_)) => {
                                out.push(format!("locales/{loc}.json key '{k}' is a plural object, but base locale '{}' uses a string", self.base));
                            }
                            _ => {}
                        }
                        let base_ph = required_placeholders(base_msg);
                        for p in required_placeholders(msg) {
                            if !base_ph.contains(&p) {
                                out.push(format!("locales/{loc}.json key '{k}' uses unknown placeholder '{{{p}}}' not in base locale '{}'", self.base));
                            }
                        }
                    }
                }
            }
            for (k, _) in &table.entries {
                if base_table.get(k).is_none() {
                    out.push(format!("locales/{loc}.json has key '{k}' not present in base locale '{}'", self.base));
                }
            }
        }
        out
    }
}

fn interpolate(template: &str, params: Option<&Record>, pseudo: bool) -> String {
    let mut out = String::with_capacity(template.len() + if pseudo { template.len() / 2 + 4 } else { 0 });
    if pseudo {
        out.push('[');
    }
    let mut literal_chars = 0usize;
    let bytes = template.as_bytes();
    let mut i = 0;
    let mut chunk_start = 0;
    let flush = |chunk: &str, out: &mut String, literal_chars: &mut usize| {
        if pseudo {
            for ch in chunk.chars() {
                out.push(accent(ch));
                *literal_chars += 1;
            }
        } else {
            out.push_str(chunk);
        }
    };
    while i < bytes.len() {
        if bytes[i] == b'{' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'{' {
                flush(&template[chunk_start..i], &mut out, &mut literal_chars);
                out.push('{');
                i += 2;
                chunk_start = i;
                continue;
            }
            if let Some(rel_end) = template[i + 1..].find('}') {
                let name = &template[i + 1..i + 1 + rel_end];
                if is_placeholder_name(name) {
                    flush(&template[chunk_start..i], &mut out, &mut literal_chars);
                    match params.and_then(|r| r.get(name)) {
                        Some(v) => out.push_str(&v.display()),
                        None => {
                            out.push('{');
                            out.push_str(name);
                            out.push('}');
                        }
                    }
                    i += rel_end + 2;
                    chunk_start = i;
                    continue;
                }
            }
        } else if bytes[i] == b'}' && i + 1 < bytes.len() && bytes[i + 1] == b'}' {
            flush(&template[chunk_start..i], &mut out, &mut literal_chars);
            out.push('}');
            i += 2;
            chunk_start = i;
            continue;
        }
        i += 1;
    }
    flush(&template[chunk_start..], &mut out, &mut literal_chars);
    if pseudo {
        // Expand ~35% so pseudo-locale reviews catch overflow and truncation.
        let pad = (literal_chars * 35 / 100).max(2);
        out.push(' ');
        for n in 0..pad {
            out.push(if n % 2 == 0 { '·' } else { '~' });
        }
        out.push(']');
    }
    out
}

fn accent(ch: char) -> char {
    match ch {
        'a' => 'á',
        'e' => 'é',
        'i' => 'í',
        'o' => 'ó',
        'u' => 'ú',
        'y' => 'ý',
        'c' => 'ç',
        'n' => 'ñ',
        's' => 'š',
        'z' => 'ž',
        'A' => 'Á',
        'E' => 'É',
        'I' => 'Í',
        'O' => 'Ó',
        'U' => 'Ú',
        'C' => 'Ç',
        'N' => 'Ñ',
        'S' => 'Š',
        'Z' => 'Ž',
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::value::Value;

    #[test]
    fn cldr_plurals_cover_arabic_russian_french_english_and_cjk() {
        assert_eq!(plural_category("ar", 0.0), "zero");
        assert_eq!(plural_category("ar", 1.0), "one");
        assert_eq!(plural_category("ar", 2.0), "two");
        assert_eq!(plural_category("ar", 5.0), "few");
        assert_eq!(plural_category("ar", 11.0), "many");
        assert_eq!(plural_category("ar", 100.0), "other");

        assert_eq!(plural_category("ru", 1.0), "one");
        assert_eq!(plural_category("ru", 21.0), "one");
        assert_eq!(plural_category("ru", 2.0), "few");
        assert_eq!(plural_category("ru", 5.0), "many");
        assert_eq!(plural_category("ru", 11.0), "many");

        assert_eq!(plural_category("fr", 0.0), "one");
        assert_eq!(plural_category("fr", 1.0), "one");
        assert_eq!(plural_category("fr", 2.0), "other");

        assert_eq!(plural_category("en", 0.0), "other");
        assert_eq!(plural_category("en", 1.0), "one");
        assert_eq!(plural_category("en", 2.0), "other");

        assert_eq!(plural_category("ja", 1.0), "other");
        assert!(is_rtl_locale("ar"));
        assert!(is_rtl_locale("ar-SA"));
        assert!(is_rtl_locale("ur_PK"));
        assert!(!is_rtl_locale("en-US"));
    }

    #[test]
    fn fallback_plurals_and_pseudo_localization() {
        let src = crate::modules::bundle(&[
            ("app.tj".into(), "function App() { return <Text>hi</Text> }".into()),
            (
                "locales/en.json".into(),
                r#"{"hello":"Hello {name}!","tasks.remaining":{"zero":"All done","one":"1 task left","other":"{count} tasks left"}}"#.into(),
            ),
            (
                "locales/ar.json".into(),
                r#"{"tasks.remaining":{"zero":"لا مهام","one":"مهمة واحدة","two":"مهمتان","few":"{count} مهام","many":"{count} مهمة","other":"{count} مهمة"}}"#.into(),
            ),
        ]);
        let files: Vec<File> = crate::modules::files(&src).into_iter().filter(|f| is_locale_file(&f.name)).collect();
        let loc = parse_locale_files(&src, &files).expect("parses");

        let mut p = Record::new();
        p.insert(Rc::from("name"), Value::str("Ana"));
        assert_eq!(loc.translate("en", "hello", Some(&p)), "Hello Ana!");
        // Missing in ar -> falls back to base locale en
        assert_eq!(loc.translate("ar-SA", "hello", Some(&p)), "Hello Ana!");

        let mut c = Record::new();
        c.insert(Rc::from("count"), Value::Num(2.0));
        assert_eq!(loc.translate("ar-SA", "tasks.remaining", Some(&c)), "مهمتان");
        c.insert(Rc::from("count"), Value::Num(5.0));
        assert_eq!(loc.translate("ar", "tasks.remaining", Some(&c)), "5 مهام");
        c.insert(Rc::from("count"), Value::Num(0.0));
        assert_eq!(loc.translate("en", "tasks.remaining", Some(&c)), "All done");

        let pseudo = loc.translate("pseudo", "hello", Some(&p));
        assert!(pseudo.starts_with('[') && pseudo.ends_with(']'));
        assert!(pseudo.contains("Ana"), "interpolated value stays intact: {pseudo}");
        assert!(pseudo.chars().count() > "Hello Ana!".chars().count() + 3, "pseudo expands length: {pseudo}");

        let warns = loc.warnings();
        assert_eq!(warns.len(), 1);
        assert!(warns[0].contains("locales/ar.json is missing key 'hello'"));
    }
}
