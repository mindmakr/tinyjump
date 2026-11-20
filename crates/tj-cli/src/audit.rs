//! The audit log (docs/trust.md, section 4): who did what to which object, the outcome and
//! the time taken. Never values, arguments, results, error text, tokens or URLs.
//!
//! - One file per UTC day in `<data>/audit/`, one JSON line per entry, with sorted keys and no
//!   whitespace. Each line's `mac` is an HMAC-SHA256 of the previous line's `mac` and this
//!   line, under a key kept apart from the data (`TJ_AUDIT_KEY_FILE`). Editing, removing or
//!   reordering a line breaks the chain.
//! - Every day file starts with a `checkpoint`, and one follows every 1,000 entries and every
//!   hour: the chain head, signed with Ed25519. Checkpoints are also appended to
//!   `checkpoints.jsonl`. A copy of it kept elsewhere lets `tj audit verify` catch a history
//!   rewritten, or cut short, even by someone holding the key.
//! - Caller and object ids are written as pseudonyms: a random token per id, mapped in
//!   `<data>/subjects/` (one file per id, named by a keyed hash). Deleting a mapping leaves
//!   that person's entries anonymous without touching the log.
//! - A background thread writes a batch at a time and syncs each batch to disk. When the
//!   queue is full, entries are counted and a `dropped` entry says how many. Security events
//!   (refusals, admin actions) wait until they are on disk; when they cannot be written, the
//!   request fails.
//! - Day files older than `TJ_AUDIT_DAYS` (default 365) are deleted. What is left still
//!   verifies, because the oldest file starts with a signed checkpoint.

use crate::engine::{now_ms, Failure};
use crate::identity::{hex, random};
use ring::hmac;
use ring::signature::{self, Ed25519KeyPair, KeyPair};
use serde_json::{Map, Value as Json};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, sync_channel, Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tj_core::runtime::protocol::Caller;

pub const KEY_ENV: &str = "TJ_AUDIT_KEY_FILE";
const DAYS_ENV: &str = "TJ_AUDIT_DAYS";
const DEFAULT_DAYS: u64 = 365;
const QUEUE: usize = 10_000;
const BATCH: usize = 1_000;
const CHECKPOINT_EVERY: u64 = 1_000;
const CHECKPOINT_MS: u64 = 3_600_000;
const SECURITY_WAIT: Duration = Duration::from_secs(10);
const DAY_MS: u64 = 86_400_000;
/// Pseudonyms remembered in memory; past this the cache starts over (the files stay).
const MAX_NAMES: usize = 100_000;
/// What the first checkpoint follows.
const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// Who did something.
pub enum Actor {
    /// The server itself: timers, and alarms nobody scheduled.
    System,
    /// Someone with the admin key (or anyone, on `tj dev`).
    Admin,
    Anonymous,
    /// A credential was sent but did not verify.
    Invalid,
    Person(String),
}

impl Actor {
    pub fn of(caller: &Caller) -> Actor {
        match caller {
            Caller::Trusted(_) => Actor::System,
            Caller::Remote { id, .. } => Actor::id(Some(id)),
        }
    }

    /// A caller id as the runtime reports it: none for the system, empty when anonymous.
    pub fn id(id: Option<&str>) -> Actor {
        match id {
            None => Actor::System,
            Some("") => Actor::Anonymous,
            Some(id) => Actor::Person(id.to_string()),
        }
    }
}

enum Field {
    /// A caller id: written as its pseudonym.
    Subject(String),
    /// `Class/id`: the id written as its pseudonym.
    Object(String),
    Text(String),
    Num(u64),
    Bool(bool),
}

/// One thing that happened. Values are only ever names, numbers and pseudonyms.
pub struct Entry {
    at: u64,
    kind: &'static str,
    fields: Vec<(&'static str, Field)>,
}

impl Entry {
    pub fn new(kind: &'static str) -> Entry {
        Entry { at: now_ms() as u64, kind, fields: Vec::new() }
    }

    pub fn who(mut self, actor: Actor) -> Entry {
        let f = match actor {
            Actor::System => Field::Text("system".into()),
            Actor::Admin => Field::Text("admin".into()),
            Actor::Anonymous => Field::Text("anonymous".into()),
            Actor::Invalid => Field::Text("invalid credential".into()),
            Actor::Person(id) => Field::Subject(id),
        };
        self.fields.push(("who", f));
        self
    }

    pub fn object(mut self, object: &str) -> Entry {
        self.fields.push(("object", Field::Object(object.to_string())));
        self
    }

    pub fn text(mut self, key: &'static str, value: &str) -> Entry {
        self.fields.push((key, Field::Text(value.to_string())));
        self
    }

    pub fn num(mut self, key: &'static str, value: u64) -> Entry {
        self.fields.push((key, Field::Num(value)));
        self
    }

    pub fn ok(mut self, ok: bool) -> Entry {
        self.fields.push(("ok", Field::Bool(ok)));
        self
    }
}

enum Job {
    Record(Entry),
    /// A security event: answered once it is on disk.
    Wait(Entry, Sender<bool>),
}

/// Where entries go. Cheap to clone; every clone feeds the same writer.
#[derive(Clone)]
pub struct Audit {
    tx: SyncSender<Job>,
    dropped: Arc<AtomicU64>,
}

impl Audit {
    /// Opens the audit log of the objects in `data` and starts its writer. Also returns a line
    /// for the startup banner: where the key is.
    pub fn start(data: &Path) -> Result<(Audit, String), String> {
        let days = match std::env::var(DAYS_ENV) {
            Ok(v) => v.trim().parse().ok().filter(|&d| d > 0).ok_or(format!("{DAYS_ENV} must be a whole number of days, at least 1"))?,
            Err(_) => DEFAULT_DAYS,
        };
        let key = key_path(data);
        let (keys, created) = Keys::load(&key, true)?;
        let dropped = Arc::new(AtomicU64::new(0));
        let writer = Writer::open(data, keys, days, dropped.clone()).map_err(|e| format!("cannot open the audit log in {}: {e}", data.join("audit").display()))?;
        let (tx, rx) = sync_channel(QUEUE);
        thread::Builder::new().name("tj-audit".into()).spawn(move || run(writer, &rx)).map_err(|e| format!("cannot start the audit log: {e}"))?;
        let apart = match (fs::canonicalize(&key), fs::canonicalize(data)) {
            (Ok(k), Ok(d)) => !k.starts_with(d),
            _ => false,
        };
        let mut about = key.display().to_string();
        if created {
            about += " (new: back it up; without it the log cannot be verified)";
        }
        if !apart {
            about += &format!(" (with the data: set {KEY_ENV} to keep it apart)");
        }
        Ok((Audit { tx, dropped }, about))
    }

    /// Queues an entry. When the queue is full it is counted, and a `dropped` entry says so.
    pub fn record(&self, e: Entry) {
        if let Err(TrySendError::Full(_)) = self.tx.try_send(Job::Record(e)) {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Writes a security event and waits until it is on disk. When it cannot be written, the
    /// caller must refuse what it was about to do.
    pub fn security(&self, e: Entry) -> Result<(), Failure> {
        let unavailable = || (503, "the audit log cannot be written, so this is refused for now; see the server's log".to_string());
        let (ack, done) = channel();
        self.tx.send(Job::Wait(e, ack)).map_err(|_| unavailable())?;
        match done.recv_timeout(SECURITY_WAIT) {
            Ok(true) => Ok(()),
            _ => Err(unavailable()),
        }
    }
}

/// The key file: `TJ_AUDIT_KEY_FILE`, else `audit.key` in the data folder.
fn key_path(data: &Path) -> PathBuf {
    std::env::var_os(KEY_ENV).filter(|v| !v.is_empty()).map_or_else(|| data.join("audit.key"), PathBuf::from)
}

/// Three keys derived from one secret: the chain's HMAC key, the pseudonym file names' HMAC
/// key, and the Ed25519 key that signs checkpoints.
struct Keys {
    chain: hmac::Key,
    subjects: hmac::Key,
    signing: Ed25519KeyPair,
}

impl Keys {
    fn from_secret(secret: &[u8]) -> Keys {
        let master = hmac::Key::new(hmac::HMAC_SHA256, secret);
        let derive = |label: &str| hmac::sign(&master, label.as_bytes());
        Keys {
            chain: hmac::Key::new(hmac::HMAC_SHA256, derive("tj audit chain").as_ref()),
            subjects: hmac::Key::new(hmac::HMAC_SHA256, derive("tj audit subjects").as_ref()),
            signing: Ed25519KeyPair::from_seed_unchecked(derive("tj audit signing").as_ref()).expect("a 32-byte seed"),
        }
    }

    /// Reads the key file; with `create`, a missing one is made with a new random secret.
    /// Whether it was created.
    fn load(path: &Path, create: bool) -> Result<(Keys, bool), String> {
        let (text, created) = match fs::read_to_string(path) {
            Ok(text) => (text, false),
            Err(e) if e.kind() == io::ErrorKind::NotFound && create => {
                let secret = hex(&random::<32>()?);
                let write = || -> io::Result<()> {
                    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
                        private_dir(dir)?;
                    }
                    let mut f = private_file().write(true).create_new(true).open(path)?;
                    f.write_all(secret.as_bytes())?;
                    f.sync_all()
                };
                write().map_err(|e| format!("cannot create the audit key {}: {e}", path.display()))?;
                (secret, true)
            }
            Err(e) => return Err(format!("cannot read the audit key {}: {e}", path.display())),
        };
        let secret = unhex(text.trim()).filter(|s| s.len() == 32).ok_or_else(|| format!("{} must hold 64 hex characters (32 random bytes)", path.display()))?;
        Ok((Keys::from_secret(&secret), created))
    }

    fn mac(&self, prev: &str, line: &str) -> String {
        let mut ctx = hmac::Context::with_key(&self.chain);
        ctx.update(prev.as_bytes());
        ctx.update(line.as_bytes());
        hex(ctx.sign().as_ref())
    }

    fn public(&self) -> String {
        hex(self.signing.public_key().as_ref())
    }
}

/// The defined bytes of an entry: keys sorted, no whitespace. Entries are flat.
fn canonical(m: &Map<String, Json>) -> String {
    let mut keys: Vec<&String> = m.keys().collect();
    keys.sort();
    Json::Object(keys.into_iter().map(|k| (k.clone(), m[k].clone())).collect()).to_string()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

/// The UTC date of epoch milliseconds, as `YYYY-MM-DD`.
fn day(ms: u64) -> String {
    // Days to civil date (Howard Hinnant's algorithm), for dates after 1970.
    let z = ms / DAY_MS + 719_468;
    let (era, doe) = (z / 146_097, z % 146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + u64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

fn private_dir(dir: &Path) -> io::Result<()> {
    let mut b = fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
    b.create(dir)
}

/// Options for a file only the server's user can read.
fn private_file() -> OpenOptions {
    #[allow(unused_mut)] // only Unix sets a mode
    let mut o = OpenOptions::new();
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    o
}

/// A new directory entry must reach the disk too. Windows cannot sync a folder.
fn sync_dir(_dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    File::open(_dir)?.sync_all()?;
    Ok(())
}

/// The day files in `dir`, oldest first.
fn day_files(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)?
        .filter_map(|e| e.ok().filter(|e| e.file_type().is_ok_and(|t| t.is_file())).map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl") && p.file_stem().and_then(|s| s.to_str()).is_some_and(|s| s.len() == 10 && s.as_bytes()[4] == b'-'))
        .collect();
    files.sort();
    Ok(files)
}

struct Writer {
    dir: PathBuf,
    subjects: PathBuf,
    keys: Keys,
    days: u64,
    /// The day file being written, and its day.
    file: Option<(String, File)>,
    /// Lines not yet written to it, and checkpoints not yet copied to `checkpoints.jsonl`.
    buf: String,
    checkpoints: String,
    n: u64,
    prev: String,
    since_checkpoint: u64,
    last_checkpoint: u64,
    names: HashMap<String, String>,
    dropped: Arc<AtomicU64>,
}

impl Writer {
    fn open(data: &Path, keys: Keys, days: u64, dropped: Arc<AtomicU64>) -> io::Result<Writer> {
        let (dir, subjects) = (data.join("audit"), data.join("subjects"));
        private_dir(&dir)?;
        private_dir(&subjects)?;
        fs::write(dir.join("signing.pub"), keys.public() + "\n")?;
        let mut w = Writer { dir, subjects, keys, days, file: None, buf: String::new(), checkpoints: String::new(), n: 0, prev: GENESIS.into(), since_checkpoint: 0, last_checkpoint: now_ms() as u64, names: HashMap::new(), dropped };
        w.recover()?;
        Ok(w)
    }

    /// Continues from the last complete line on disk. A line torn by a crash is cut off; a
    /// file with nothing in it is removed.
    fn recover(&mut self) -> io::Result<()> {
        self.file = None;
        self.buf.clear();
        self.checkpoints.clear();
        self.n = 0;
        self.prev = GENESIS.into();
        for path in day_files(&self.dir)?.into_iter().rev() {
            let text = fs::read_to_string(&path)?;
            let complete = text.rfind('\n').map_or(0, |i| i + 1);
            if complete < text.len() {
                OpenOptions::new().write(true).open(&path)?.set_len(complete as u64)?;
            }
            if let Some(last) = text[..complete].lines().last() {
                let damaged = || io::Error::new(io::ErrorKind::InvalidData, format!("{} ends with a damaged entry; run `tj audit verify`", path.display()));
                let m: Map<String, Json> = serde_json::from_str(last).map_err(|_| damaged())?;
                self.n = m.get("n").and_then(Json::as_u64).ok_or_else(damaged)?;
                self.prev = m.get("mac").and_then(Json::as_str).ok_or_else(damaged)?.to_string();
                return Ok(());
            }
            fs::remove_file(&path)?;
        }
        Ok(())
    }

    /// Writes a batch and syncs it. Security events hear whether it worked; ordinary entries
    /// that could not be written are counted as dropped.
    fn write(&mut self, jobs: Vec<Job>, now: u64) {
        let (mut entries, mut acks, mut ordinary) = (Vec::with_capacity(jobs.len()), Vec::new(), 0);
        for job in jobs {
            match job {
                Job::Record(e) => {
                    ordinary += 1;
                    entries.push(e);
                }
                Job::Wait(e, ack) => {
                    acks.push(ack);
                    entries.push(e);
                }
            }
        }
        let dropped = self.dropped.swap(0, Ordering::Relaxed);
        let r = (|| -> io::Result<()> {
            for e in entries {
                self.day_file(now)?;
                self.entry(e)?;
                if self.since_checkpoint >= CHECKPOINT_EVERY {
                    self.checkpoint(now);
                }
            }
            if dropped > 0 {
                self.day_file(now)?;
                self.entry(Entry { at: now, kind: "dropped", fields: vec![("count", Field::Num(dropped))] })?;
            }
            if self.since_checkpoint > 0 && now >= self.last_checkpoint + CHECKPOINT_MS {
                self.day_file(now)?;
                self.checkpoint(now);
            }
            self.flush()
        })();
        if let Err(e) = &r {
            eprintln!("[tj] audit log: cannot write: {e}");
            self.dropped.fetch_add(ordinary + dropped, Ordering::Relaxed);
            if let Err(e) = self.recover() {
                eprintln!("[tj] audit log: {e}");
            }
        }
        for ack in acks {
            let _ = ack.send(r.is_ok());
        }
    }

    /// Opens the file for `now`'s day if it is not open. A new file starts with a checkpoint,
    /// and is when days past the retention period are deleted.
    fn day_file(&mut self, now: u64) -> io::Result<()> {
        let today = day(now);
        if self.file.as_ref().is_some_and(|(d, _)| *d == today) {
            return Ok(());
        }
        self.flush()?;
        let file = private_file().create(true).append(true).open(self.dir.join(format!("{today}.jsonl")))?;
        let fresh = file.metadata()?.len() == 0;
        self.file = Some((today, file));
        if fresh {
            sync_dir(&self.dir)?;
            self.checkpoint(now);
            let oldest = day(now.saturating_sub(self.days * DAY_MS));
            for old in day_files(&self.dir)?.into_iter().filter(|p| p.file_stem().and_then(|s| s.to_str()).is_some_and(|s| s < oldest.as_str())) {
                fs::remove_file(old)?;
            }
        }
        Ok(())
    }

    /// Adds a line to the chain.
    fn append(&mut self, mut m: Map<String, Json>) -> String {
        self.n += 1;
        m.insert("n".into(), self.n.into());
        let mac = self.keys.mac(&self.prev, &canonical(&m));
        m.insert("mac".into(), mac.clone().into());
        self.prev = mac;
        self.since_checkpoint += 1;
        let line = canonical(&m) + "\n";
        self.buf.push_str(&line);
        line
    }

    fn checkpoint(&mut self, now: u64) {
        let mut m = Map::new();
        m.insert("at".into(), now.into());
        m.insert("kind".into(), "checkpoint".into());
        m.insert("n".into(), (self.n + 1).into());
        m.insert("head".into(), self.prev.clone().into());
        let sig = self.keys.signing.sign(canonical(&m).as_bytes());
        m.insert("sig".into(), hex(sig.as_ref()).into());
        let line = self.append(m);
        self.checkpoints.push_str(&line);
        self.since_checkpoint = 0;
        self.last_checkpoint = now;
    }

    fn entry(&mut self, e: Entry) -> io::Result<()> {
        let mut m = Map::new();
        m.insert("at".into(), e.at.into());
        m.insert("kind".into(), e.kind.into());
        for (key, field) in e.fields {
            let value = match field {
                Field::Subject(id) => self.pseudonym(&id)?.into(),
                Field::Object(o) => match o.split_once('/') {
                    Some((class, id)) => format!("{class}/{}", self.pseudonym(id)?).into(),
                    None => self.pseudonym(&o)?.into(),
                },
                Field::Text(t) => t.into(),
                Field::Num(n) => n.into(),
                Field::Bool(b) => b.into(),
            };
            m.insert(key.into(), value);
        }
        self.append(m);
        Ok(())
    }

    /// The token that stands for `id` in the log: random, made the first time `id` is seen.
    fn pseudonym(&mut self, id: &str) -> io::Result<String> {
        if let Some(t) = self.names.get(id) {
            return Ok(t.clone());
        }
        let path = self.subjects.join(hex(&hmac::sign(&self.keys.subjects, id.as_bytes()).as_ref()[..16]));
        let token = match fs::read_to_string(&path) {
            Ok(t) => t.trim().to_string(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let t = format!("~{}", hex(&random::<8>().map_err(io::Error::other)?));
                let mut f = private_file().write(true).create_new(true).open(&path)?;
                f.write_all(t.as_bytes())?;
                f.sync_all()?;
                sync_dir(&self.subjects)?;
                t
            }
            Err(e) => return Err(e),
        };
        if self.names.len() >= MAX_NAMES {
            self.names.clear();
        }
        self.names.insert(id.to_string(), token.clone());
        Ok(token)
    }

    fn flush(&mut self) -> io::Result<()> {
        let Some((_, file)) = &mut self.file else { return Ok(()) };
        if self.buf.is_empty() {
            return Ok(());
        }
        file.write_all(self.buf.as_bytes())?;
        file.sync_data()?;
        self.buf.clear();
        if !self.checkpoints.is_empty() {
            let mut copy = private_file().create(true).append(true).open(self.dir.join("checkpoints.jsonl"))?;
            copy.write_all(self.checkpoints.as_bytes())?;
            copy.sync_data()?;
            self.checkpoints.clear();
        }
        Ok(())
    }
}

/// The writer thread: a batch at a time, and a checkpoint when an hour has passed.
fn run(mut w: Writer, rx: &Receiver<Job>) {
    loop {
        let first = if w.since_checkpoint > 0 {
            let wait = (w.last_checkpoint + CHECKPOINT_MS).saturating_sub(now_ms() as u64);
            rx.recv_timeout(Duration::from_millis(wait))
        } else {
            rx.recv().map_err(|_| RecvTimeoutError::Disconnected)
        };
        let mut jobs = match first {
            Ok(job) => vec![job],
            Err(RecvTimeoutError::Timeout) => Vec::new(),
            Err(RecvTimeoutError::Disconnected) => return,
        };
        while jobs.len() < BATCH {
            match rx.try_recv() {
                Ok(job) => jobs.push(job),
                Err(_) => break,
            }
        }
        w.write(jobs, now_ms() as u64);
    }
}

/// `tj audit verify`: every line's HMAC and order, every checkpoint's signature, and, given
/// a copy of `checkpoints.jsonl` kept elsewhere, that the log still holds each of them.
pub fn verify(data: &Path, copy: Option<&Path>) -> Result<String, String> {
    let (keys, _) = Keys::load(&key_path(data), false)?;
    verify_with(&keys, data, copy)
}

fn verify_with(keys: &Keys, data: &Path, copy: Option<&Path>) -> Result<String, String> {
    let dir = data.join("audit");
    let files = day_files(&dir).map_err(|e| format!("no audit log in {}: {e}", dir.display()))?;
    let public = keys.public();
    let signer = signature::UnparsedPublicKey::new(&signature::ED25519, unhex(&public).unwrap_or_default());
    let mut prev: Option<String> = None;
    let (mut first, mut last) = (None, 0u64);
    let mut checkpoints: HashMap<u64, String> = HashMap::new();
    let mut torn = false;
    for (fi, path) in files.iter().enumerate() {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let text = fs::read_to_string(path).map_err(|e| format!("{name}: {e}"))?;
        for (li, raw) in text.split_inclusive('\n').enumerate() {
            let at = format!("{name}:{}", li + 1);
            let Some(line) = raw.strip_suffix('\n') else {
                // The server stopped while writing; it cuts this off when it starts again.
                if fi + 1 == files.len() {
                    torn = true;
                    break;
                }
                return Err(format!("{at}: incomplete line"));
            };
            let mut m: Map<String, Json> = serde_json::from_str(line).map_err(|e| format!("{at}: not an entry: {e}"))?;
            let mac = m.remove("mac").and_then(|v| v.as_str().map(str::to_string)).ok_or(format!("{at}: no mac"))?;
            let n = m.get("n").and_then(Json::as_u64).ok_or(format!("{at}: no entry number"))?;
            let kind = m.get("kind").and_then(Json::as_str).unwrap_or("").to_string();
            let head = m.get("head").and_then(Json::as_str).map(str::to_string);
            if li == 0 {
                let head = head.clone().filter(|_| kind == "checkpoint").ok_or(format!("{at}: a day file must start with a checkpoint"))?;
                match &prev {
                    Some(p) if *p != head => return Err(format!("{at}: does not continue the day before: a file is missing or was changed")),
                    Some(_) => {}
                    // The oldest file left: where it starts is vouched for by its signature.
                    None => prev = Some(head),
                }
            } else if n != last + 1 {
                return Err(format!("{at}: entry {n} follows entry {last}: entries were removed or reordered"));
            }
            let before = prev.clone().unwrap_or_default();
            if keys.mac(&before, &canonical(&m)) != mac {
                return Err(format!("{at}: entry {n} was changed (its HMAC does not match)"));
            }
            if kind == "checkpoint" {
                let sig = m.remove("sig").and_then(|v| v.as_str().and_then(unhex)).ok_or(format!("{at}: checkpoint without a signature"))?;
                if head.as_deref() != Some(before.as_str()) || signer.verify(canonical(&m).as_bytes(), &sig).is_err() {
                    return Err(format!("{at}: checkpoint {n} is not signed by this log's key"));
                }
                checkpoints.insert(n, mac.clone());
            }
            first.get_or_insert(n);
            last = n;
            prev = Some(mac);
        }
    }
    let Some(first) = first else { return Err(format!("no audit entries in {}", dir.display())) };
    let mut report = format!(
        "audit log ok: entries {first} to {last} in {} day file(s), {} checkpoint(s)\nsigning key {public}\n",
        files.len(),
        checkpoints.len()
    );
    if torn {
        report += "the last line is incomplete (the server stopped while writing); it is cut off when the server starts\n";
    }
    if let Some(copy) = copy {
        let text = fs::read_to_string(copy).map_err(|e| format!("cannot read {}: {e}", copy.display()))?;
        let (mut matched, mut pruned) = (0, 0);
        for (i, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
            let at = format!("{}:{}", copy.display(), i + 1);
            let m: Map<String, Json> = serde_json::from_str(line).map_err(|e| format!("{at}: not a checkpoint: {e}"))?;
            let (n, mac) = (m.get("n").and_then(Json::as_u64).unwrap_or(0), m.get("mac").and_then(Json::as_str).unwrap_or(""));
            match checkpoints.get(&n) {
                Some(local) if local == mac => matched += 1,
                Some(_) => return Err(format!("{at}: checkpoint {n} differs from the log: its history was rewritten")),
                None if n < first => pruned += 1,
                None => return Err(format!("{at}: checkpoint {n} is missing from the log: entries were removed")),
            }
        }
        report += &format!("{matched} checkpoint(s) match the copy; {pruned} are older than the log kept\n");
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const T0: u64 = 1_763_596_800_000; // 2025-11-20T00:00:00Z

    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tj-audit-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn keys() -> Keys {
        Keys::from_secret(&[7; 32])
    }

    fn writer(data: &Path, days: u64) -> Writer {
        Writer::open(data, keys(), days, Arc::default()).expect("opens")
    }

    fn call(who: &str, object: &str) -> Job {
        Job::Record(Entry::new("call").who(Actor::Person(who.into())).object(object).text("method", "add").num("status", 200).num("ms", 3))
    }

    fn log(data: &Path) -> String {
        day_files(&data.join("audit")).unwrap().iter().map(|p| fs::read_to_string(p).unwrap()).collect()
    }

    fn rewrite(data: &Path, f: impl Fn(&mut Vec<String>)) {
        let path = day_files(&data.join("audit")).unwrap().pop().unwrap();
        let mut lines: Vec<String> = fs::read_to_string(&path).unwrap().lines().map(str::to_string).collect();
        f(&mut lines);
        fs::write(&path, lines.join("\n") + "\n").unwrap();
    }

    #[test]
    fn the_chain_catches_edits_removals_and_reordering() {
        let data = folder("chain");
        let mut w = writer(&data, 365);
        w.write((0..5).map(|i| call("ana", &format!("Cart/{i}"))).collect(), T0);
        assert!(verify_with(&keys(), &data, None).unwrap().starts_with("audit log ok: entries 1 to 6"));
        let original = log(&data);

        rewrite(&data, |l| l[3] = l[3].replace("\"status\":200", "\"status\":403"));
        assert!(verify_with(&keys(), &data, None).unwrap_err().contains("entry 4 was changed"));
        rewrite(&data, |l| *l = original.lines().map(str::to_string).collect());
        rewrite(&data, |l| {
            l.remove(3);
        });
        assert!(verify_with(&keys(), &data, None).unwrap_err().contains("entry 5 follows entry 3"));
        rewrite(&data, |l| *l = original.lines().map(str::to_string).collect());
        rewrite(&data, |l| l.swap(2, 3));
        assert!(verify_with(&keys(), &data, None).is_err());
        // Another key cannot vouch for it.
        rewrite(&data, |l| *l = original.lines().map(str::to_string).collect());
        assert!(verify_with(&Keys::from_secret(&[8; 32]), &data, None).is_err());
        fs::remove_dir_all(data).unwrap();
    }

    #[test]
    fn ids_are_written_as_pseudonyms() {
        let data = folder("names");
        let mut w = writer(&data, 365);
        w.write(vec![call("ana@example.com", "Cart/ana@example.com"), call("ana@example.com", "Cart/bo")], T0);
        let text = log(&data);
        assert!(!text.contains("ana") && !text.contains("bo\""), "{text}");
        let lines: Vec<Json> = text.lines().skip(1).map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines[0]["who"], lines[1]["who"], "the same person, the same token");
        assert_eq!(lines[0]["object"], json!(format!("Cart/{}", lines[0]["who"].as_str().unwrap())), "ids map alike wherever they appear");
        assert_eq!(lines[1]["method"], "add");

        // Forgetting a person: their mapping goes, and the same id gets a new token.
        fs::remove_dir_all(data.join("subjects")).unwrap();
        let mut w = writer(&data, 365);
        w.write(vec![call("ana@example.com", "Cart/x")], T0 + 1);
        let again: Json = serde_json::from_str(log(&data).lines().last().unwrap()).unwrap();
        assert_ne!(again["who"], lines[0]["who"]);
        assert!(verify_with(&keys(), &data, None).is_ok());
        fs::remove_dir_all(data).unwrap();
    }

    /// Recomputes every HMAC and signature after a change, as someone holding the key could.
    fn forge(data: &Path, k: &Keys, f: impl Fn(&mut Vec<String>)) {
        rewrite(data, |lines| {
            f(lines);
            let mut prev = GENESIS.to_string();
            for (i, line) in lines.iter_mut().enumerate() {
                let mut m: Map<String, Json> = serde_json::from_str(line).unwrap();
                m.remove("mac");
                m.insert("n".into(), (i as u64 + 1).into());
                if m.remove("sig").is_some() {
                    m.insert("head".into(), prev.clone().into());
                    let sig = k.signing.sign(canonical(&m).as_bytes());
                    m.insert("sig".into(), hex(sig.as_ref()).into());
                }
                let mac = k.mac(&prev, &canonical(&m));
                m.insert("mac".into(), mac.clone().into());
                prev = mac;
                *line = canonical(&m);
            }
        });
    }

    #[test]
    fn a_copy_of_the_checkpoints_catches_a_rewrite_by_the_key_holder() {
        let data = folder("anchor");
        let mut w = writer(&data, 365);
        w.write((0..2500).map(|i| call("ana", &format!("Cart/{}", i % 3))).collect(), T0);
        let copy = data.join("elsewhere.jsonl");
        fs::copy(data.join("audit/checkpoints.jsonl"), &copy).unwrap();
        let original = log(&data);
        assert_eq!(fs::read_to_string(&copy).unwrap().lines().count(), 3, "at the start and every 1,000 entries");
        assert!(verify_with(&keys(), &data, Some(&copy)).unwrap().contains("3 checkpoint(s) match the copy"));

        // With the key, a changed history verifies on its own ...
        forge(&data, &keys(), |l| l[10] = l[10].replace("\"status\":200", "\"status\":201"));
        assert!(verify_with(&keys(), &data, None).is_ok());
        // ... but not against the copy kept elsewhere.
        assert!(verify_with(&keys(), &data, Some(&copy)).unwrap_err().contains("history was rewritten"));
        // Nor does a log cut short.
        forge(&data, &keys(), |l| *l = original.lines().take(1500).map(str::to_string).collect());
        assert!(verify_with(&keys(), &data, Some(&copy)).unwrap_err().contains("missing from the log"));
        fs::remove_dir_all(data).unwrap();
    }

    #[test]
    fn days_roll_over_old_days_are_deleted_and_the_rest_still_verifies() {
        let data = folder("days");
        let mut w = writer(&data, 2);
        for d in 0..4 {
            w.write(vec![call("ana", "Cart/a")], T0 + d * DAY_MS + 5);
        }
        let names: Vec<String> = day_files(&data.join("audit")).unwrap().iter().map(|p| p.file_stem().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["2025-11-21", "2025-11-22", "2025-11-23"]);
        let report = verify_with(&keys(), &data, Some(&data.join("audit/checkpoints.jsonl"))).unwrap();
        assert!(report.contains("entries 3 to 8") && report.contains("1 are older than the log kept"), "{report}");
        fs::remove_dir_all(data).unwrap();
    }

    #[test]
    fn a_torn_last_line_is_cut_off_and_the_chain_continues() {
        let data = folder("torn");
        writer(&data, 365).write(vec![call("ana", "Cart/a")], T0);
        let path = day_files(&data.join("audit")).unwrap().pop().unwrap();
        OpenOptions::new().append(true).open(&path).unwrap().write_all(b"{\"at\":1,\"ki").unwrap();
        assert!(verify_with(&keys(), &data, None).unwrap().contains("the last line is incomplete"));
        writer(&data, 365).write(vec![call("ana", "Cart/b")], T0 + 1);
        assert!(verify_with(&keys(), &data, None).unwrap().starts_with("audit log ok: entries 1 to 3"));
        fs::remove_dir_all(data).unwrap();
    }

    #[test]
    fn nothing_is_lost_silently() {
        let data = folder("lost");
        let dropped = Arc::new(AtomicU64::new(0));
        let mut w = Writer::open(&data, keys(), 365, dropped.clone()).unwrap();
        // The disk refuses: a security event hears so, and the ordinary entry is counted.
        let blocker = data.join(format!("audit/{}.jsonl", day(T0)));
        fs::create_dir_all(&blocker).unwrap();
        let (ack, done) = channel();
        w.write(vec![call("ana", "Cart/a"), Job::Wait(Entry::new("refused").who(Actor::Invalid), ack)], T0);
        assert!(!done.recv().unwrap(), "not on disk, so the request is refused");
        assert_eq!(dropped.load(Ordering::Relaxed), 1);
        // Once it can write again, the log says how many entries it lost.
        fs::remove_dir(&blocker).unwrap();
        dropped.fetch_add(2, Ordering::Relaxed);
        w.write(vec![call("ana", "Cart/b")], T0 + 1);
        assert!(log(&data).contains("\"count\":3,\"kind\":\"dropped\""), "{}", log(&data));
        assert!(verify_with(&keys(), &data, None).is_ok());
        fs::remove_dir_all(data).unwrap();
    }

    #[test]
    fn days_are_utc_dates() {
        assert_eq!(day(0), "1970-01-01");
        assert_eq!(day(T0), "2025-11-20");
        assert_eq!(day(951_782_400_000), "2000-02-29");
        assert_eq!(day(4_107_542_400_000), "2100-03-01");
    }
}
