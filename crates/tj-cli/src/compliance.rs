//! Compliance packs (docs/compliance.md): rules for a region or an industry, kept as data
//! files outside tinyjump. `tj check --compliance <pack.json>` reports each control as met,
//! not met (with the fix), checked when the server starts, or a duty for a person;
//! `tj serve --compliance <pack.json>` refuses to start while a control is not met.
//!
//! A pack only names controls; what each checked control means is defined here, once. A
//! pack may extend others, and only adds to them.

use serde_json::Value as Json;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use tj_core::vm::program::Program;

/// The controls tinyjump can check, and what each means.
const CHECKS: &[(&str, &str)] = &[
    ("lawful-basis", "every purpose personal data is held for names its lawful basis (`const lawfulBasis`)"),
    ("special-basis", "data in a special category is held only on consent, or a person confirms another condition"),
    ("retention", "every object holding personal data is erased after a set time (`@retain`)"),
    ("subject-rights", "people can get a copy of their data and have it erased (`tj privacy`, `tj:privacy`)"),
    ("records", "the record of processing can be written (`tj check --privacy --record`)"),
    ("audit-log", "who read or changed personal data is logged, tamper-evident (on every server)"),
    ("encryption-at-rest", "stored data is encrypted (`TJ_KEY_FILE`); checked when the server starts"),
];

/// A pack, with the packs it extends merged in.
#[derive(Debug, Default)]
pub struct Pack {
    pub names: Vec<String>,
    pub title: String,
    /// Each pack it is made of: name, date in force, and who reviewed it (`None`: a draft).
    pub reviews: Vec<(String, String, Option<String>)>,
    pub special: BTreeSet<String>,
    pub controls: Vec<Control>,
    pub not_covered: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Control {
    pub kind: Kind,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    /// One of [`CHECKS`].
    Check(String),
    /// A duty only a person can do (appointing an officer, an assessment, a contract).
    Person(String),
}

/// Where a control stands.
#[derive(Debug, PartialEq)]
pub enum Verdict {
    Met(String),
    NotMet(String),
    /// Checked by `tj serve --compliance`, not by `tj check`.
    AtServe,
    Person,
}

impl Pack {
    /// Loads `path` and every pack it extends (paths relative to the file).
    pub fn load(path: &Path) -> Result<Pack, String> {
        let mut pack = Pack::default();
        pack.add(path, &mut Vec::new())?;
        Ok(pack)
    }

    fn add(&mut self, path: &Path, seen: &mut Vec<PathBuf>) -> Result<(), String> {
        let at = path.canonicalize().map_err(|e| format!("cannot read the pack {}: {e}", path.display()))?;
        if seen.contains(&at) {
            return Err(format!("the pack {} extends itself", path.display()));
        }
        seen.push(at.clone());
        let text = std::fs::read_to_string(&at).map_err(|e| format!("cannot read the pack {}: {e}", path.display()))?;
        let json: Json = serde_json::from_str(&text).map_err(|e| format!("{} is not JSON: {e}", path.display()))?;
        let bad = |what: &str| format!("{}: {what}", path.display());
        let name = json["name"].as_str().filter(|n| !n.is_empty()).ok_or_else(|| bad("needs a \"name\""))?;
        for parent in strings(&json["extends"]).map_err(|e| bad(&format!("\"extends\" {e}")))? {
            self.add(&at.parent().unwrap_or(Path::new(".")).join(parent), seen)?;
        }
        // Parents are added first, so the pack given last names the whole.
        self.title = json["title"].as_str().unwrap_or(name).to_string();
        self.names.push(name.to_string());
        let effective = json["effective"].as_str().filter(|d| !d.is_empty()).ok_or_else(|| bad("needs \"effective\", the date the rules apply from"))?;
        self.reviews.push((name.to_string(), effective.to_string(), json["reviewedBy"].as_str().map(str::to_string)));
        self.special.extend(strings(&json["special"]).map_err(|e| bad(&format!("\"special\" {e}")))?);
        self.not_covered.extend(strings(&json["notCovered"]).map_err(|e| bad(&format!("\"notCovered\" {e}")))?);
        let controls = json["controls"].as_array().ok_or_else(|| bad("needs \"controls\", a list"))?;
        for (i, c) in controls.iter().enumerate() {
            let bad = |what: &str| bad(&format!("control {}: {what}", i + 1));
            let source = c["source"].as_str().filter(|s| !s.trim().is_empty()).ok_or_else(|| bad("every control cites its source (the law and article)"))?;
            let kind = match (c["check"].as_str(), c["person"].as_str()) {
                (Some(check), None) if CHECKS.iter().any(|(k, _)| *k == check) => Kind::Check(check.to_string()),
                (Some(check), None) => return Err(bad(&format!("tinyjump cannot check \"{check}\"; it can check {}, and anything else is a \"person\" control", CHECKS.iter().map(|(k, _)| *k).collect::<Vec<_>>().join(", ")))),
                (None, Some(duty)) => Kind::Person(duty.to_string()),
                _ => return Err(bad("give either \"check\" (what tinyjump checks) or \"person\" (a duty for a person)")),
            };
            let control = Control { kind, source: source.to_string() };
            if !self.controls.contains(&control) {
                self.controls.push(control);
            }
        }
        Ok(())
    }

    /// Each control with its verdict, for an app (`encrypted`: known only to a server).
    pub fn evaluate(&self, src: &str, program: &Program, encrypted: Option<bool>) -> Vec<(&Control, Verdict)> {
        let bases = tj_core::lawful_bases(src);
        let held: Vec<_> = program.objects.iter().filter(|o| !o.pii.is_empty()).collect();
        let basis_of = |purpose: &str| bases.as_ref().and_then(|b| b.iter().find(|(p, _)| p == purpose).map(|(_, b)| b.clone()));
        self.controls
            .iter()
            .map(|c| {
                let Kind::Check(check) = &c.kind else { return (c, Verdict::Person) };
                let v = match check.as_str() {
                    _ if held.is_empty() => Verdict::Met("the app holds no personal data".into()),
                    "lawful-basis" => match &bases {
                        Some(_) => Verdict::Met("every purpose names its basis".into()),
                        None => {
                            let used: BTreeSet<&str> = held.iter().flat_map(|o| o.pii.iter().flat_map(|p| p.purposes.iter().map(|s| &**s))).collect();
                            let example: Vec<String> = used.iter().map(|p| format!("{p}: \"contract\"")).collect();
                            Verdict::NotMet(format!("declare the lawful basis of each purpose: const lawfulBasis = {{ {} }}", example.join(", ")))
                        }
                    },
                    "special-basis" => {
                        let mut doubtful = Vec::new();
                        let mut any = false;
                        for o in &held {
                            for p in o.pii.iter().filter(|p| self.special.contains(&*p.category)) {
                                any = true;
                                let not_consent: Vec<String> = p.purposes.iter().filter(|purpose| basis_of(purpose).as_deref() != Some("consent")).map(|p| p.to_string()).collect();
                                if !not_consent.is_empty() {
                                    doubtful.push(format!("{}.{} ({}) for {}", o.name, p.field, p.category, not_consent.join(", ")));
                                }
                            }
                        }
                        if !any {
                            Verdict::Met("the app holds no special-category data".into())
                        } else if doubtful.is_empty() {
                            Verdict::Met("special categories are held only on consent".into())
                        } else {
                            Verdict::NotMet(format!("special-category data held without consent: {}. Make those purposes consent-based, or have a person confirm another condition and record it", doubtful.join("; ")))
                        }
                    }
                    "retention" => {
                        let unbounded: Vec<&str> = held.iter().filter(|o| o.retain.is_none()).map(|o| &*o.name).collect();
                        if unbounded.is_empty() {
                            Verdict::Met("every object holding personal data has @retain".into())
                        } else {
                            Verdict::NotMet(format!("kept forever: {}. Add @retain(\"2y\") (or how long you need) to each", unbounded.join(", ")))
                        }
                    }
                    "subject-rights" => Verdict::Met("built in: tj privacy export|erase, and tj:privacy in app code".into()),
                    "records" => Verdict::Met("built in: tj check --privacy --record".into()),
                    "audit-log" => Verdict::Met("built in on every server".into()),
                    "encryption-at-rest" => match encrypted {
                        Some(true) => Verdict::Met("TJ_KEY_FILE is set".into()),
                        Some(false) => Verdict::NotMet("set TJ_KEY_FILE to encrypt stored data (docs/privacy.md)".into()),
                        None => Verdict::AtServe,
                    },
                    other => unreachable!("loading accepts only known checks, not {other}"),
                };
                (c, v)
            })
            .collect()
    }

    /// The report `tj check --compliance` prints, and whether every checked control is met.
    pub fn report(&self, verdicts: &[(&Control, Verdict)]) -> (String, bool) {
        let effective: Vec<String> = self.reviews.iter().map(|(n, from, _)| format!("{n} from {from}")).collect();
        let mut out = format!("{} ({})\n", self.title, effective.join(", "));
        out += "This report is not legal advice. ";
        let drafts: Vec<&str> = self.reviews.iter().filter(|(_, _, by)| by.is_none()).map(|(n, _, _)| n.as_str()).collect();
        if drafts.is_empty() {
            let by: Vec<String> = self.reviews.iter().map(|(n, _, by)| format!("{n} by {}", by.as_deref().unwrap_or_default())).collect();
            out += &format!("Reviewed: {}.\n\n", by.join(", "));
        } else {
            out += &format!("Not reviewed by counsel: {}.\n\n", drafts.join(", "));
        }
        let mut ok = true;
        for (c, v) in verdicts {
            let (mark, what, detail) = match (&c.kind, v) {
                (Kind::Check(k), Verdict::Met(why)) => ("met", k.as_str(), why.clone()),
                (Kind::Check(k), Verdict::NotMet(fix)) => {
                    ok = false;
                    ("NOT MET", k.as_str(), fix.clone())
                }
                (Kind::Check(k), _) => ("at serve", k.as_str(), "checked by tj serve --compliance".into()),
                (Kind::Person(duty), _) => ("person", "", duty.clone()),
            };
            let what = if what.is_empty() { String::new() } else { format!("{what}: ") };
            out += &format!("  {mark:<9} {what}{detail}  [{}]\n", c.source);
        }
        if !self.not_covered.is_empty() {
            out += "\nNot covered by this pack:\n";
            for n in &self.not_covered {
                out += &format!("  - {n}\n");
            }
        }
        (out, ok)
    }
}

fn strings(v: &Json) -> Result<Vec<String>, String> {
    match v {
        Json::Null => Ok(Vec::new()),
        Json::Array(a) => a.iter().map(|s| s.as_str().map(str::to_string).ok_or("must list strings".to_string())).collect(),
        _ => Err("must be a list of strings".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tj-packs-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn packs_need_sources_known_checks_and_only_add_when_extended() {
        let d = dir("load");
        let write = |f: &str, s: &str| std::fs::write(d.join(f), s).unwrap();
        write("base.json", r#"{ "name": "base", "effective": "2020-01-01", "special": ["health"], "controls": [{ "check": "retention", "source": "Law art. 1" }] }"#);
        write("health.json", r#"{ "name": "health", "effective": "2024-01-01", "title": "Health", "reviewedBy": "A. Counsel", "extends": ["base.json"], "controls": [{ "check": "retention", "source": "Law art. 1" }, { "person": "Appoint an officer", "source": "Law art. 2" }], "notCovered": ["clinical trials"] }"#);
        let p = Pack::load(&d.join("health.json")).unwrap();
        assert_eq!(p.names, ["base", "health"]);
        assert_eq!(p.title, "Health");
        assert_eq!(p.controls.len(), 2, "the same control twice is kept once");
        assert!(p.special.contains("health"));
        write("nosource.json", r#"{ "name": "x", "effective": "2020-01-01", "controls": [{ "check": "retention" }] }"#);
        assert!(Pack::load(&d.join("nosource.json")).unwrap_err().contains("cites its source"));
        write("unknown.json", r#"{ "name": "x", "effective": "2020-01-01", "controls": [{ "check": "be-nice", "source": "s" }] }"#);
        assert!(Pack::load(&d.join("unknown.json")).unwrap_err().contains("cannot check \"be-nice\""));
        write("loop.json", r#"{ "name": "x", "effective": "2020-01-01", "extends": ["loop.json"], "controls": [] }"#);
        assert!(Pack::load(&d.join("loop.json")).unwrap_err().contains("extends itself"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn controls_are_met_not_met_at_serve_or_for_a_person() {
        let d = dir("eval");
        let all: Vec<String> = CHECKS.iter().map(|(k, _)| format!(r#"{{ "check": "{k}", "source": "art. {k}" }}"#)).collect();
        std::fs::write(d.join("p.json"), format!(r#"{{ "name": "p", "effective": "2018-05-25", "special": ["health"], "controls": [{}, {{ "person": "Appoint a DPO", "source": "art. 37" }}] }}"#, all.join(", "))).unwrap();
        let pack = Pack::load(&d.join("p.json")).unwrap();
        let src = "@server @read(\"owner\") object Patient {\n  @pii(\"health\", \"care\") notes: string = \"\"\n}\n";
        let program = tj_core::compile(src).unwrap();
        let (r, ok) = pack.report(&pack.evaluate(src, &program, None));
        assert!(!ok);
        assert!(r.starts_with("p (p from 2018-05-25)\nThis report is not legal advice. Not reviewed by counsel: p."), "{r}");
        assert!(r.contains("NOT MET   lawful-basis: declare the lawful basis of each purpose: const lawfulBasis = { care: \"contract\" }"), "{r}");
        assert!(r.contains("NOT MET   special-basis: special-category data held without consent: Patient.notes (health) for care"), "{r}");
        assert!(r.contains("NOT MET   retention: kept forever: Patient."), "{r}");
        assert!(r.contains("at serve  encryption-at-rest") && r.contains("person    Appoint a DPO  [art. 37]"), "{r}");

        let src = "const lawfulBasis = { care: \"consent\" }\n@server @read(\"owner\") @retain(\"2y\") object Patient {\n  @pii(\"health\", \"care\") notes: string = \"\"\n}\n";
        let program = tj_core::compile(src).unwrap();
        let (r, ok) = pack.report(&pack.evaluate(src, &program, Some(true)));
        assert!(ok && !r.contains("NOT MET"), "{r}");
        let (r, ok) = pack.report(&pack.evaluate(src, &program, Some(false)));
        assert!(!ok && r.contains("NOT MET   encryption-at-rest: set TJ_KEY_FILE"), "{r}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
