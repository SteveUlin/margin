//! Feeds: registered scripts that keep artifacts current without the AI (design.md "Feeds").
//!
//! A feed is a name, a command and a trigger. The command's stdout speaks the same language as the
//! AI: each line is one op (`{"op":"set",…}`), an array of ops (applied all or none), or a whole
//! component (`{"type":"section","id":"jj",…}`), which replaces the component with that id in
//! place. Triggers:
//! - `interval`: run every N seconds; one run's lines land as one revision.
//! - `watch`: run once at start, then whenever a watched path changes (inotify, debounced).
//! - `stream`: a long-running process; lines land as they arrive, coalesced to one write per
//!   `min_gap`, and the process restarts with backoff when it exits.
//!
//! Ownership is the fourth owner kind: a feed owns every id under its prefix in its artifact; the
//! store rejects its writes outside the prefix and everyone else's writes inside it (store.rs
//! `check_owner`). The supervisor (daemon.rs) runs the workers; this file holds what one worker does,
//! so `margin feed run` can do it in the foreground with the same code.

use crate::doc::{Block, Op};
use crate::store::{write_atomic, Store, Writer};
use crate::util::{epoch_now, now_rfc3339, rfc3339, valid_id};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::ffi::CString;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Trigger {
    Interval { every: f64 },
    Watch { paths: Vec<String> },
    Stream,
}

impl Trigger {
    pub fn describe(&self) -> String {
        match self {
            Trigger::Interval { every } => format!("every {}", fmt_secs(*every)),
            Trigger::Watch { paths } => {
                let names: Vec<String> = paths.iter().map(|p| short_path(p)).collect();
                format!("watch {}", names.join(" "))
            }
            Trigger::Stream => "stream".into(),
        }
    }
}

fn short_path(p: &str) -> String {
    let parts: Vec<&str> = p.trim_end_matches('/').rsplit('/').take(2).collect();
    parts.into_iter().rev().collect::<Vec<_>>().join("/")
}

pub fn fmt_secs(s: f64) -> String {
    if s >= 3600.0 && s % 3600.0 == 0.0 {
        format!("{}h", s / 3600.0)
    } else if s >= 60.0 && s % 60.0 == 0.0 {
        format!("{}m", s / 60.0)
    } else if s.fract() == 0.0 {
        format!("{}s", s as i64)
    } else {
        format!("{s:.1}s")
    }
}

fn d_gap() -> f64 {
    1.0
}
fn d_timeout() -> f64 {
    60.0
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Feed {
    pub name: String,
    /// argv, run directly (no shell), in `cwd`.
    pub cmd: Vec<String>,
    pub trigger: Trigger,
    pub artifact: String,
    /// The id prefix this feed owns in `artifact`; `*` = the whole artifact.
    pub owns: String,
    #[serde(default)]
    pub paused: bool,
    pub cwd: String,
    /// Rate limit: at least this many seconds between two writes.
    #[serde(default = "d_gap")]
    pub min_gap: f64,
    /// A one-shot run (interval, watch) is killed after this long.
    #[serde(default = "d_timeout")]
    pub timeout: f64,
    #[serde(default)]
    pub added: String,
}

impl Feed {
    pub fn writer(&self) -> Writer {
        Writer::Feed { name: self.name.clone(), owns: self.owns.clone() }
    }
    /// The feed's health line in its artifact. It sits inside the prefix, so the feed may write it.
    pub fn status_id(&self) -> String {
        status_id(&self.name, &self.owns)
    }
    /// Same feed apart from pause state (a changed command or trigger restarts the worker).
    pub fn same_job(&self, o: &Feed) -> bool {
        Feed { paused: false, ..self.clone() } == Feed { paused: false, ..o.clone() }
    }
}

pub fn status_id(name: &str, owns: &str) -> String {
    if owns == "*" {
        format!("{name}-feed")
    } else if owns.ends_with(['-', '_']) {
        format!("{owns}feed")
    } else {
        format!("{owns}-feed")
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Registry {
    #[serde(default)]
    pub feeds: Vec<Feed>,
}

/// Do two prefixes claim any common id? (`*` overlaps everything.)
pub fn overlaps(a: &str, b: &str) -> bool {
    a == "*" || b == "*" || a.starts_with(b) || b.starts_with(a)
}

impl Registry {
    pub fn path(dir: &Path) -> PathBuf {
        dir.join("feeds.json")
    }
    /// The registry, or empty when there is none. A file that exists but won't parse reads as
    /// empty here, which is only safe for display (`feed ls`, `show`). Anything that acts on the
    /// answer (edits, the daemon's reconcile, ownership checks) goes through
    /// [`Registry::load_strict`]: an "empty" registry there stopped every feed and unlocked their ids.
    pub fn load(dir: &Path) -> Registry {
        Self::load_strict(dir).unwrap_or_default()
    }

    /// Like `load`, but only a missing feeds.json is empty; one that won't read or parse is an
    /// error, so an edit can't save an empty registry over it and lose every feed.
    pub fn load_strict(dir: &Path) -> Result<Registry, String> {
        match std::fs::read_to_string(Self::path(dir)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Registry::default()),
            Err(e) => Err(format!("{}: {e}", Self::path(dir).display())),
            Ok(s) => serde_json::from_str(&s).map_err(|e| format!("{} is corrupt ({e}); fix or remove it", Self::path(dir).display())),
        }
    }
    pub fn save(&self, dir: &Path) -> Result<(), String> {
        write_atomic(&Self::path(dir), serde_json::to_string_pretty(self).unwrap().as_bytes()).map_err(|e| e.to_string())
    }
    pub fn get(&self, name: &str) -> Option<&Feed> {
        self.feeds.iter().find(|f| f.name == name)
    }
    pub fn claims(&self, art: &str) -> Vec<crate::store::Claim> {
        self.feeds
            .iter()
            .filter(|f| f.artifact == art)
            .map(|f| crate::store::Claim { feed: f.name.clone(), owns: f.owns.clone() })
            .collect()
    }
    pub fn add(&mut self, f: Feed, replace: bool) -> Result<(), String> {
        if !valid_id(&f.name) {
            return Err(format!("bad feed name {:?}: letters, digits, - and _", f.name));
        }
        if !valid_id(&f.artifact) {
            return Err(format!("bad artifact id {:?}", f.artifact));
        }
        if f.owns != "*" && !valid_id(&f.owns) {
            return Err(format!("bad --owns prefix {:?}: an id prefix (letters, digits, - and _) or *", f.owns));
        }
        if f.cmd.is_empty() {
            return Err("no command: margin feed add <name> … -- <cmd…>".into());
        }
        if let Trigger::Interval { every } = f.trigger {
            if every < 0.2 {
                return Err("--every must be at least 0.2s".into());
            }
        }
        if let Some(i) = self.feeds.iter().position(|x| x.name == f.name) {
            if !replace {
                return Err(format!("feed {:?} exists; `margin feed rm {}` first, or pass --replace", f.name, f.name));
            }
            self.feeds.remove(i);
        }
        if let Some(o) = self.feeds.iter().find(|x| x.artifact == f.artifact && overlaps(&x.owns, &f.owns)) {
            return Err(format!(
                "prefix {:?} overlaps feed {} (owns {:?} in {}); one id has one owner",
                f.owns, o.name, o.owns, o.artifact
            ));
        }
        self.feeds.push(f);
        Ok(())
    }
    pub fn remove(&mut self, name: &str) -> Result<Feed, String> {
        let i = self.feeds.iter().position(|f| f.name == name).ok_or_else(|| format!("no feed {name:?}"))?;
        Ok(self.feeds.remove(i))
    }
    pub fn set_paused(&mut self, name: &str, p: bool) -> Result<(), String> {
        self.feeds.iter_mut().find(|f| f.name == name).map(|f| f.paused = p).ok_or_else(|| format!("no feed {name:?}"))
    }
}

/// Change the registry under the project lock.
pub fn edit_registry<T>(st: &Store, f: impl FnOnce(&mut Registry) -> Result<T, String>) -> Result<T, String> {
    let _l = st.lock().map_err(|e| e.to_string())?;
    let mut r = Registry::load_strict(&st.dir())?;
    let out = f(&mut r)?;
    r.save(&st.dir())?;
    Ok(out)
}


/// Parse a whole `margin apply` input: one JSON value (an op, an op array, a component), or
/// several lines in the feed grammar. All lines together form one all-or-nothing write.
pub fn parse_input(s: &str) -> Result<Vec<Op>, String> {
    if let Ok(v) = serde_json::from_str::<Value>(s) {
        return match v {
            Value::Array(a) => Ok(a.into_iter().map(parse_value).collect::<Result<Vec<_>, _>>()?.into_iter().flatten().collect()),
            v => parse_value(v),
        };
    }
    let mut ops = Vec::new();
    for (i, l) in s.lines().enumerate() {
        ops.extend(parse_line(l).map_err(|e| format!("line {}: {e}", i + 1))?);
    }
    Ok(ops)
}

/// One line of feed output → ops. Blank lines and `#` comments are skipped.
/// A feed's toast line, `{"toast": "tests passed", "level": "ok", "at": "ci-1", "ttl": 8}`: sent
/// beside the document (toast.rs), never an op. True when the line was one.
pub fn toast_line(st: &Store, art: &str, feed: &str, line: &str, log: &Log) -> bool {
    let t = line.trim();
    if !t.starts_with('{') || !t.contains("\"toast\"") {
        return false;
    }
    let Ok(v) = serde_json::from_str::<Value>(t) else { return false };
    let Some(text) = v["toast"].as_str() else { return false };
    if let Err(e) = crate::toast::send(st, art, text, v["level"].as_str().unwrap_or("info"), v["at"].as_str().unwrap_or(""), v["ttl"].as_f64(), &format!("feed:{feed}")) {
        log.line(&format!("bad toast: {e}"));
    }
    true
}

pub fn parse_line(line: &str) -> Result<Vec<Op>, String> {
    let t = line.trim();
    if t.is_empty() || t.starts_with('#') {
        return Ok(vec![]);
    }
    let v: Value = serde_json::from_str(t).map_err(|e| format!("not JSON ({e}): {}", vellum::text::truncate(t, 80)))?;
    match v {
        Value::Array(a) => Ok(a.into_iter().map(parse_value).collect::<Result<Vec<_>, _>>()?.into_iter().flatten().collect()),
        v => parse_value(v),
    }
}

/// One value of the line grammar. Beyond the stored op shapes: `set` takes `fields` (several at
/// once, null deletes), and a bare component takes `parent` (where to add it when it's new).
fn parse_value(mut v: Value) -> Result<Vec<Op>, String> {
    // a writer may stamp every value with the format version; only 1 exists
    if let Some(m) = v.as_object_mut() {
        match m.remove("v") {
            None => {}
            Some(n) if n == 1 => {}
            Some(n) => return Err(format!("version {n}: this margin reads v 1")),
        }
    }
    if v.get("op").is_some() {
        if v["op"] == "set" {
            if let Some(Value::Object(fields)) = v.get("fields").cloned() {
                let id = v.get("id").and_then(|x| x.as_str()).ok_or("set needs an id")?.to_string();
                if fields.is_empty() {
                    return Err("set with empty fields".into());
                }
                return Ok(fields.into_iter().map(|(field, value)| Op::Set { id: id.clone(), field, value }).collect());
            }
        }
        return serde_json::from_value(v).map(|o| vec![o]).map_err(|e| format!("bad op: {e}"));
    }
    if v.get("type").is_some() {
        let parent = match v.as_object_mut().and_then(|m| m.remove("parent")) {
            None | Some(Value::Null) => None,
            Some(Value::String(p)) => Some(p),
            Some(p) => return Err(format!("parent must be a component id, got {p}")),
        };
        let block: Block = serde_json::from_value(v).map_err(|e| format!("bad component: {e}"))?;
        if block.id.is_empty() {
            return Err("a whole component needs an id (it replaces the component with that id)".into());
        }
        return Ok(vec![Op::Replace { block, parent }]);
    }
    Err("each line is an op ({\"op\":…}), an array of ops, or a component ({\"type\":…,\"id\":…})".into())
}

/// Restart backoff: 1, 2, 4 … 60 s after consecutive failures; a run that stayed up for
/// `healthy` seconds wipes the slate, so a feed that crashes once a day restarts in 1 s.
#[derive(Clone, Debug, Default)]
pub struct Backoff {
    pub fails: u32,
}

impl Backoff {
    pub const MAX: f64 = 60.0;
    pub fn delay(&self) -> f64 {
        if self.fails == 0 {
            0.0
        } else {
            2f64.powi(self.fails.min(16) as i32 - 1).min(Self::MAX)
        }
    }
    /// Record a run's end; returns how long to wait before the next start.
    pub fn record(&mut self, ok: bool, ran_for: f64, healthy: f64) -> f64 {
        if ran_for >= healthy {
            self.fails = 0;
        }
        if ok {
            self.fails = 0;
        } else {
            self.fails += 1;
        }
        self.delay()
    }
}

/// Write rate limit: at most one write per `gap` seconds.
#[derive(Clone, Debug)]
pub struct Gate {
    pub gap: f64,
    pub last: f64,
}

impl Gate {
    pub fn new(gap: f64) -> Gate {
        Gate { gap, last: f64::NEG_INFINITY }
    }
    pub fn wait(&self, now: f64) -> f64 {
        (self.last + self.gap - now).max(0.0)
    }
    pub fn pass(&mut self, now: f64) {
        self.last = now;
    }
}

/// A feed's state, for `margin feed ls` (status file) and the pane (the `feed` component).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Status {
    pub state: String,
    pub since: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    #[serde(default)]
    pub pid: u32,
    #[serde(default)]
    pub restarts: u32,
    #[serde(default)]
    pub runs: u64,
    #[serde(default)]
    pub writes: u64,
    #[serde(default)]
    pub last_write: String,
    #[serde(default)]
    pub last_error: String,
    /// When the command last ran to its end (a stream: when its lines were last taken), apart
    /// from `last_write`: a run that printed what is already there writes nothing, and "wrote
    /// never" beside a feed that runs every 10 s read as "not running".
    #[serde(default)]
    pub last_run: String,
    /// How that run went: `wrote`, `unchanged` or `failed` (the reason is `last_error`).
    #[serde(default)]
    pub last_outcome: String,
}

impl Status {
    /// The last run for `margin feed ls`: `ran 2s ago · unchanged (wrote 3m ago)`, `ran 2s ago ·
    /// wrote`, `failed 5s ago: <the last stderr line>`, or `not run yet`.
    pub fn last_run_text(&self, now: f64) -> String {
        let ago = |t: &str| crate::util::parse_rfc3339(t).map(|t| crate::util::ago(now - t));
        let Some(ran) = ago(&self.last_run) else {
            // a status file from before runs were tracked still knows its last write
            return ago(&self.last_write).map(|w| format!("wrote {w}")).unwrap_or_else(|| "not run yet".into());
        };
        match self.last_outcome.as_str() {
            "failed" => format!("failed {ran}: {}", vellum::text::truncate(&self.last_error, 90)),
            "wrote" => format!("ran {ran} · wrote"),
            _ => match ago(&self.last_write) {
                Some(w) => format!("ran {ran} · unchanged (wrote {w})"),
                None => format!("ran {ran} · unchanged"),
            },
        }
    }
}

/// `margin feed rm` and `margin feed pause`: say so on the feed's health line now. The daemon's
/// worker does the same as it stops, but with no daemon up (or no worker: the feed was already
/// paused) nobody else would, and the line went on saying `paused` or `live` for a feed that
/// was gone. A health line that isn't in the artifact is left out.
pub fn mark_health(st: &Store, f: &Feed, state: &str) {
    let sid = f.status_id();
    let Ok(d) = st.load(&f.artifact) else { return };
    if d.find(&sid).map(|b| b.kind != "feed" || b.s("state") == state).unwrap_or(true) {
        return;
    }
    let set = |field: &str, value: Value| Op::Set { id: sid.clone(), field: field.into(), value };
    let ops = vec![set("state", json!(state)), set("since", json!(now_rfc3339())), set("until", Value::Null), set("note", Value::Null)];
    let _ = st.feed_write(&f.artifact, &[ops], &f.writer(), &[]);
}

pub fn feeds_dir(st: &Store) -> PathBuf {
    st.dir().join("feeds")
}
pub fn status_path(st: &Store, name: &str) -> PathBuf {
    feeds_dir(st).join(format!("{name}.status.json"))
}
pub fn log_path(st: &Store, name: &str) -> PathBuf {
    feeds_dir(st).join(format!("{name}.log"))
}
pub fn read_status(st: &Store, name: &str) -> Option<Status> {
    std::fs::read_to_string(status_path(st, name)).ok().and_then(|s| serde_json::from_str(&s).ok())
}

/// Per-feed log: daemon notes plus the command's stderr, capped at 512 KB (one rotation).
#[derive(Clone)]
pub struct Log {
    path: PathBuf,
    echo: bool,
    lock: Arc<Mutex<()>>,
}

impl Log {
    pub fn new(path: PathBuf, echo: bool) -> Log {
        Log { path, echo, lock: Arc::new(Mutex::new(())) }
    }
    pub fn line(&self, s: &str) {
        let _g = self.lock.lock();
        if self.echo {
            eprintln!("{s}");
        }
        if let Some(d) = self.path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        if std::fs::metadata(&self.path).map(|m| m.len() > 512 * 1024).unwrap_or(false) {
            let _ = std::fs::rename(&self.path, self.path.with_extension("log.1"));
        }
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&self.path) {
            let _ = writeln!(f, "{} {s}", now_rfc3339());
        }
    }
}

/// Shared between the supervisor and one worker.
#[derive(Default)]
pub struct Ctl {
    pub stop: AtomicBool,
    /// Why it was stopped: "removed", "paused", "stopped" (daemon exit), "restart" (job changed).
    pub reason: Mutex<String>,
}

impl Ctl {
    pub fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed) || crate::daemon::exiting()
    }
    pub fn stop(&self, why: &str) {
        *self.reason.lock().unwrap() = why.into();
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// One feed being run: by a daemon worker thread, or by `margin feed run` in the foreground.
pub struct Worker {
    pub st: Store,
    pub f: Feed,
    pub ctl: Arc<Ctl>,
    pub log: Log,
    pub status: Status,
    gate: Gate,
    /// `margin feed run`: print each write to stdout.
    pub verbose: bool,
    last_stderr: Arc<Mutex<String>>,
}

impl Worker {
    pub fn new(st: Store, f: Feed, ctl: Arc<Ctl>, foreground: bool) -> Worker {
        let log = Log::new(log_path(&st, &f.name), foreground);
        let gate = Gate::new(f.min_gap.max(0.05));
        Worker { st, f, ctl, log, status: Status::default(), gate, verbose: foreground, last_stderr: Default::default() }
    }

    /// The status file belongs to the daemon's worker. A one-off `margin feed run` keeps its
    /// status in memory: writing it reset the daemon's live copy, so `feed ls` read `starting`.
    fn save_status(&self) {
        if !self.verbose {
            let _ = write_atomic(&status_path(&self.st, &self.f.name), serde_json::to_string_pretty(&self.status).unwrap().as_bytes());
        }
    }

    /// Change state: the status file and the pane's health line. Not the event log: a job's
    /// state is state (it is in the status file), and the log is for facts someone may react to
    /// (a handler watching the log was re-run by its own lifecycle lines, 2026-11-10 review).
    pub fn set_state(&mut self, state: &str, until: Option<f64>, err: Option<String>) {
        let changed = self.status.state != state;
        if changed {
            self.status.since = now_rfc3339();
        }
        self.status.state = state.into();
        self.status.until = until.map(rfc3339);
        if let Some(e) = err {
            self.status.last_error = e;
        }
        self.save_status();
        // the health line and lifecycle events describe the supervised feed, not a one-off run
        if self.verbose {
            return;
        }
        if changed || until.is_some() {
            self.write_health();
        }
    }

    fn health_block(&self) -> Block {
        let mut f = serde_json::Map::new();
        f.insert("name".into(), json!(self.f.name));
        f.insert("state".into(), json!(self.status.state));
        f.insert("trigger".into(), json!(self.f.trigger.describe()));
        f.insert("since".into(), json!(self.status.since));
        if let Some(u) = &self.status.until {
            f.insert("until".into(), json!(u));
        }
        if !self.status.last_write.is_empty() {
            f.insert("last".into(), json!(self.status.last_write));
        }
        if self.status.restarts > 0 {
            f.insert("restarts".into(), json!(self.status.restarts));
        }
        if matches!(self.status.state.as_str(), "restarting" | "failed") && !self.status.last_error.is_empty() {
            f.insert("note".into(), json!(vellum::text::truncate(&self.status.last_error, 120)));
        }
        Block { kind: "feed".into(), id: self.f.status_id(), children: vec![], f }
    }

    /// The health line sits just above the feed's root component (the one whose id is the prefix),
    /// as a sibling, so the feed replacing its root never wipes it.
    fn write_health(&mut self) {
        let b = self.health_block();
        // a feed never creates its artifact: with none there is no health line to write, and
        // the run itself fails with the message that names `margin new`
        if !self.st.exists(&self.f.artifact) {
            return;
        }
        let doc = self.st.load(&self.f.artifact).ok();
        let exists = doc.as_ref().and_then(|d| d.find(&b.id)).is_some();
        let op = if exists {
            Op::Replace { block: b, parent: None }
        } else {
            let root = doc.as_ref().filter(|d| d.blocks.iter().any(|x| x.id == self.f.owns)).map(|_| self.f.owns.clone());
            Op::Add { parent: None, block: b, before: root, after: None }
        };
        let (_, errs) = self.st.feed_write(&self.f.artifact, &[vec![op]], &self.f.writer(), &[]);
        for e in errs {
            self.log.line(&format!("health line: {e}"));
        }
    }

    /// Note how a run ended, for `margin feed ls` (the status file) and, with `log`, as a line
    /// of `margin feed logs`.
    fn ran(&mut self, outcome: &str, detail: &str, log: bool) {
        self.status.last_run = now_rfc3339();
        self.status.last_outcome = outcome.into();
        self.save_status();
        if log {
            self.log.line(&format!("run: {outcome}{}", if detail.is_empty() { String::new() } else { format!(" ({detail})") }));
        }
    }

    /// Apply parsed groups as one write (rate limited). Returns true if content changed.
    fn apply(&mut self, groups: &[Vec<Op>]) -> bool {
        if groups.is_empty() {
            return false;
        }
        let now = epoch_now();
        let wait = self.gate.wait(now);
        if wait > 0.0 {
            self.sleep(wait);
        }
        self.gate.pass(epoch_now());
        let stamp = now_rfc3339();
        let sid = self.f.status_id();
        let mut on_change = vec![Op::Set { id: sid.clone(), field: "last".into(), value: json!(stamp) }];
        // the health line moves above the root once the root exists
        if let Ok(d) = self.st.load(&self.f.artifact) {
            if d.find(&sid).is_none() {
                on_change.clear();
            }
        }
        let (rev, errs) = self.st.feed_write(&self.f.artifact, groups, &self.f.writer(), &on_change);
        for e in &errs {
            self.log.line(&format!("rejected: {e}"));
        }
        if let Some(r) = rev {
            self.status.writes += 1;
            self.status.last_write = stamp;
            if self.verbose {
                println!("{} rev {r}", self.f.artifact);
            }
            self.save_status();
            // first write: put the health line above the root now that the root exists
            if !self.verbose && self.st.load(&self.f.artifact).map(|d| d.find(&sid).is_none()).unwrap_or(false) {
                self.write_health();
            }
            true
        } else {
            if self.verbose && errs.is_empty() {
                println!("{} unchanged", self.f.artifact);
            }
            false
        }
    }

    /// Sleep up to `secs`, waking early when stopped.
    pub fn sleep(&self, secs: f64) {
        let end = epoch_now() + secs;
        while !self.ctl.stopped() {
            let left = end - epoch_now();
            if left <= 0.0 {
                break;
            }
            std::thread::sleep(Duration::from_secs_f64(left.min(0.1)));
        }
    }

    fn spawn(&mut self) -> Result<Child, String> {
        let mut c = Command::new(&self.f.cmd[0]);
        c.args(&self.f.cmd[1..])
            .current_dir(&self.f.cwd)
            .env("MARGIN_FEED", &self.f.name)
            .env("MARGIN_ARTIFACT", &self.f.artifact)
            .env("MARGIN_OWNS", &self.f.owns)
            .env("MARGIN_PROJECT", &self.st.project)
            .env("MARGIN_HOME", &self.st.home)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        unsafe {
            c.pre_exec(|| {
                // its own process group, so a stop kills the whole pipeline it may have started;
                // and it dies with the daemon even if the daemon is killed hard
                libc::setpgid(0, 0);
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
        let mut ch = c.spawn().map_err(|e| format!("{}: {e}", self.f.cmd[0]))?;
        self.status.pid = ch.id();
        self.status.runs += 1;
        let err = ch.stderr.take().unwrap();
        let log = self.log.clone();
        let last = self.last_stderr.clone();
        std::thread::spawn(move || {
            // lossy: one bad byte must not stop the reader, or the child blocks on a full pipe
            for l in lossy_lines(err) {
                log.line(&format!("stderr: {l}"));
                *last.lock().unwrap() = l;
            }
        });
        Ok(ch)
    }

    fn kill(ch: &mut Child) {
        let pg = ch.id() as i32;
        unsafe { libc::kill(-pg, libc::SIGTERM) };
        for _ in 0..20 {
            if matches!(ch.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        unsafe { libc::kill(-pg, libc::SIGKILL) };
        let _ = ch.wait();
    }

    fn exit_text(&self, code: Option<i32>, sig: Option<i32>) -> String {
        let how = match (code, sig) {
            (Some(c), _) => format!("exit {c}"),
            (None, Some(s)) => format!("signal {s}"),
            _ => "ended".into(),
        };
        let last = self.last_stderr.lock().unwrap().clone();
        if last.is_empty() {
            how
        } else {
            format!("{how}: {last}")
        }
    }

    /// Run the command to completion (interval, watch). Ok(true) = it changed the artifact.
    /// A feed's output is an update: with no artifact to update the run fails before the
    /// command starts, with the store's one message for a missing artifact (`feed ls` and the
    /// log show it), and nothing is created.
    pub fn run_once(&mut self) -> Result<bool, String> {
        *self.last_stderr.lock().unwrap() = String::new();
        if !self.st.exists(&self.f.artifact) {
            return Err(self.st.missing(&self.f.artifact));
        }
        let mut ch = self.spawn()?;
        let mut out = ch.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            // lossy, like the stream path: read_to_string dropped the whole run on one bad byte
            let mut b = Vec::new();
            let _ = out.read_to_end(&mut b);
            let _ = tx.send(String::from_utf8_lossy(&b).into_owned());
        });
        let start = epoch_now();
        let status = loop {
            if let Ok(Some(s)) = ch.try_wait() {
                break s;
            }
            if self.ctl.stopped() {
                Self::kill(&mut ch);
                return Ok(false);
            }
            if epoch_now() - start > self.f.timeout {
                Self::kill(&mut ch);
                return Err(format!("timed out after {}", fmt_secs(self.f.timeout)));
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let text = rx.recv_timeout(Duration::from_secs(2)).unwrap_or_default();
        use std::os::unix::process::ExitStatusExt;
        if !status.success() {
            return Err(self.exit_text(status.code(), status.signal()));
        }
        let mut groups = Vec::new();
        for l in text.lines() {
            if toast_line(&self.st, &self.f.artifact, &self.f.name, l, &self.log) {
                continue;
            }
            match parse_line(l) {
                Ok(ops) if !ops.is_empty() => groups.push(ops),
                Ok(_) => {}
                Err(e) => self.log.line(&format!("bad line: {e}")),
            }
        }
        let changed = self.apply(&groups);
        // each run's outcome is a line of the log: "is it running?" is answered there too
        let lines = groups.len();
        self.ran(if changed { "wrote" } else { "unchanged" }, &format!("{lines} line{}", if lines == 1 { "" } else { "s" }), true);
        Ok(changed)
    }

    /// The worker loop for this feed's trigger, until stopped.
    pub fn run(&mut self) {
        self.status = read_status(&self.st, &self.f.name).map(|s| Status { pid: 0, ..s }).unwrap_or_default();
        self.status.state = String::new();
        self.log.line(&format!("start: {} ({})", self.f.cmd.join(" "), self.f.trigger.describe()));
        match self.f.trigger.clone() {
            Trigger::Stream => self.run_stream(),
            Trigger::Interval { every } => self.run_periodic(Some(every), None),
            Trigger::Watch { paths } => {
                // a watched path that doesn't exist yet (`.jj` before `jj init`) retries with
                // backoff; failing at once made the supervisor respawn this worker every 300 ms
                let mut bo = Backoff::default();
                while !self.ctl.stopped() {
                    match Watcher::new(&self.f.cwd, &paths) {
                        Ok(w) => {
                            self.run_periodic(None, Some(w));
                            break;
                        }
                        Err(e) => {
                            let d = bo.record(false, 0.0, 60.0);
                            self.log.line(&format!("watch: {e}; retry in {}", fmt_secs(d)));
                            self.set_state("restarting", Some(epoch_now() + d), Some(e));
                            self.sleep(d);
                        }
                    }
                }
            }
        }
        let why = self.ctl.reason.lock().unwrap().clone();
        let why = if why.is_empty() { "stopped".to_string() } else { why };
        if why != "restart" {
            self.status.pid = 0;
            self.set_state(&why, None, None);
        }
        self.log.line(&format!("stop: {why}"));
    }

    fn run_periodic(&mut self, every: Option<f64>, watch: Option<Watcher>) {
        let mut bo = Backoff::default();
        self.set_state("live", None, None);
        let mut due = true;
        while !self.ctl.stopped() {
            if due {
                due = false;
                let t0 = epoch_now();
                match self.run_once() {
                    Ok(_) => {
                        bo.record(true, epoch_now() - t0, 30.0);
                        if self.status.state != "live" {
                            self.set_state("live", None, None);
                        }
                    }
                    Err(e) if !self.ctl.stopped() => {
                        let d = bo.record(false, epoch_now() - t0, 30.0);
                        self.status.restarts += 1;
                        self.status.last_error = e.clone();
                        self.ran("failed", "", false);
                        self.log.line(&format!("run: failed: {e}; next try in {}", fmt_secs(d)));
                        self.set_state("restarting", Some(epoch_now() + d), Some(e));
                        self.sleep(d);
                        due = true;
                        continue;
                    }
                    Err(_) => {}
                }
            }
            match (&every, &watch) {
                (Some(n), _) => {
                    self.sleep(*n);
                    due = true;
                }
                (None, Some(w)) => {
                    if w.wait(200) {
                        // debounce: let a burst of changes (jj writes several files) settle
                        settle(|ms| w.wait(ms), || self.ctl.stopped(), 2.0);
                        due = true;
                    }
                }
                _ => break,
            }
        }
    }

    fn run_stream(&mut self) {
        let mut bo = Backoff::default();
        while !self.ctl.stopped() {
            let t0 = epoch_now();
            *self.last_stderr.lock().unwrap() = String::new();
            let mut ch = match self.spawn() {
                Ok(c) => c,
                Err(e) => {
                    let d = bo.record(false, 0.0, 30.0);
                    self.log.line(&format!("spawn failed: {e}; retry in {}", fmt_secs(d)));
                    self.set_state("restarting", Some(epoch_now() + d), Some(e));
                    self.sleep(d);
                    continue;
                }
            };
            self.set_state("live", None, None);
            let out = ch.stdout.take().unwrap();
            // bounded: a process that prints faster than we parse blocks on its own stdout
            // (backpressure) instead of growing our memory without limit
            let (tx, rx) = mpsc::sync_channel::<String>(1024);
            std::thread::spawn(move || {
                // lossy: a non-UTF-8 byte used to end the reader, kill the child by SIGPIPE and
                // lose every later line
                for l in lossy_lines(out) {
                    if tx.send(l).is_err() {
                        break;
                    }
                }
            });
            let mut pending: Vec<Vec<Op>> = Vec::new();
            let mut closed = false;
            let (tst, tart, tname) = (Store::new(self.st.home.clone(), self.st.project.clone()), self.f.artifact.clone(), self.f.name.clone());
            let take = |pending: &mut Vec<Vec<Op>>, l: &str, log: &Log| {
                if toast_line(&tst, &tart, &tname, l, log) {
                    return;
                }
                match parse_line(l) {
                    Ok(ops) if !ops.is_empty() => coalesce(pending, ops),
                    Ok(_) => {}
                    Err(e) => log.line(&format!("bad line: {e}")),
                }
            };
            loop {
                if self.ctl.stopped() {
                    Self::kill(&mut ch);
                    return;
                }
                // wait for a line, but no longer than the gate needs before the next write
                let tmo = if pending.is_empty() { 0.1 } else { self.gate.wait(epoch_now()).clamp(0.005, 0.1) };
                match rx.recv_timeout(Duration::from_secs_f64(tmo)) {
                    Ok(l) => take(&mut pending, &l, &self.log),
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => closed = true,
                }
                // drain what's queued, but never forever: a flood must not starve the stop check
                // or the write. Lines arriving faster than min_gap coalesce into one write.
                for _ in 0..512 {
                    match rx.try_recv() {
                        Ok(l) => take(&mut pending, &l, &self.log),
                        Err(_) => break,
                    }
                }
                if !pending.is_empty() && (self.gate.wait(epoch_now()) <= 0.0 || closed) {
                    let g = std::mem::take(&mut pending);
                    // a stream's "run" is each batch of lines it hands over; not logged, or a
                    // busy stream would write a log line a second
                    if !self.st.exists(&self.f.artifact) {
                        // the same failure as a periodic run's, logged once per outage
                        let e = self.st.missing(&self.f.artifact);
                        if self.status.last_outcome != "failed" {
                            self.log.line(&format!("run: failed: {e}"));
                        }
                        self.status.last_error = e;
                        self.ran("failed", "", false);
                    } else {
                        let changed = self.apply(&g);
                        self.ran(if changed { "wrote" } else { "unchanged" }, "", false);
                    }
                }
                if closed {
                    break;
                }
            }
            // stdout closed: give the process a moment to exit, then stop it. Waiting forever here
            // blocked this worker, and the supervisor's join on it wedged every other feed
            let mut st = None;
            for _ in 0..40 {
                if let Ok(Some(s)) = ch.try_wait() {
                    st = Some(s);
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            if st.is_none() {
                self.log.line("closed stdout but kept running; stopping it");
                Self::kill(&mut ch);
                st = ch.try_wait().ok().flatten();
            }
            use std::os::unix::process::ExitStatusExt;
            let ran = epoch_now() - t0;
            let ok = st.map(|s| s.success()).unwrap_or(false);
            let why = self.exit_text(st.and_then(|s| s.code()), st.and_then(|s| s.signal()));
            if self.ctl.stopped() {
                return;
            }
            // a stream should run forever: any exit is a crash, a clean one included
            let d = bo.record(false, ran, 30.0).max(if ok { 1.0 } else { 0.0 });
            self.status.restarts += 1;
            self.status.last_error = why.clone();
            self.ran("failed", "", false);
            self.log.line(&format!("exited ({why}) after {}; restart in {}", fmt_secs((ran * 10.0).round() / 10.0), fmt_secs(d)));
            self.set_state("restarting", Some(epoch_now() + d), Some(why));
            self.sleep(d);
        }
    }
}

/// Debounce: return once `more` (wait up to N ms for another event) comes back quiet, but no
/// later than `cap` seconds in, and at once when stopped. Uncapped and blind to the stop, a path
/// that changed every 100 ms held the worker here for good: the feed ran once, and the
/// supervisor's join on it wedged `feed rm`, new feeds and SIGTERM.
fn settle(mut more: impl FnMut(i32) -> bool, stopped: impl Fn() -> bool, cap: f64) {
    let end = epoch_now() + cap;
    while !stopped() && epoch_now() < end && more(250) {}
}

/// Lines of a pipe, decoding invalid UTF-8 as U+FFFD instead of stopping at it.
fn lossy_lines<R: Read>(r: R) -> impl Iterator<Item = String> {
    let mut br = BufReader::new(r);
    std::iter::from_fn(move || {
        let mut buf = Vec::new();
        match br.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => None,
            Ok(_) => {
                while matches!(buf.last(), Some(b'\n' | b'\r')) {
                    buf.pop();
                }
                Some(String::from_utf8_lossy(&buf).into_owned())
            }
        }
    })
}

/// Queue one line's ops for the next write. A whole-component replace supersedes a queued replace
/// of the same id, so a feed that reprints its state 1000 times between writes costs one apply,
/// not 1000. Anything else queues in order.
pub fn coalesce(pending: &mut Vec<Vec<Op>>, ops: Vec<Op>) {
    fn subtree(b: &Block, out: &mut Vec<String>) {
        out.push(b.id.clone());
        for c in &b.children {
            subtree(c, out);
        }
    }
    if let [Op::Replace { block, .. }] = &ops[..] {
        let id = block.id.clone();
        if let Some(i) = pending.iter().rposition(|g| matches!(&g[..], [Op::Replace { block, .. }] if block.id == id)) {
            // the new replace jumps over everything queued after the old one, so it may only jump
            // over replaces of disjoint subtrees; any other op (an Add or Move into this block, a
            // Set on its child) has an order that matters, and the earlier prefix check missed those
            let mut mine = Vec::new();
            subtree(block, &mut mine);
            if let [Op::Replace { block: old, .. }] = &pending[i][..] {
                subtree(old, &mut mine);
            }
            let disjoint = pending[i + 1..].iter().all(|g| match &g[..] {
                [Op::Replace { block: other, .. }] => {
                    let mut theirs = Vec::new();
                    subtree(other, &mut theirs);
                    !theirs.iter().any(|t| mine.contains(t))
                }
                _ => false,
            });
            if disjoint {
                pending[i] = ops;
                return;
            }
        }
    }
    pending.push(ops);
}

/// inotify on a feed's watched paths. A watched file is watched through its directory too, because
/// editors and jj replace files by rename, which ends a watch on the old inode.
pub struct Watcher {
    fd: i32,
}

impl Watcher {
    pub fn new(cwd: &str, paths: &[String]) -> Result<Watcher, String> {
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if fd < 0 {
            return Err("inotify_init1 failed".into());
        }
        let mask = libc::IN_CLOSE_WRITE | libc::IN_MOVED_TO | libc::IN_MOVED_FROM | libc::IN_CREATE | libc::IN_DELETE | libc::IN_MODIFY | libc::IN_ATTRIB;
        let mut n = 0;
        for p in paths {
            let p = Path::new(cwd).join(p);
            let mut targets = vec![p.clone()];
            if p.is_file() {
                if let Some(d) = p.parent() {
                    targets.push(d.to_path_buf());
                }
            }
            for t in targets {
                if let Ok(c) = CString::new(t.to_string_lossy().as_bytes()) {
                    if unsafe { libc::inotify_add_watch(fd, c.as_ptr(), mask) } >= 0 {
                        n += 1;
                    }
                }
            }
        }
        if n == 0 {
            unsafe { libc::close(fd) };
            return Err(format!("nothing to watch: {} (relative to {cwd})", paths.join(", ")));
        }
        Ok(Watcher { fd })
    }
    /// Wait up to `ms` for a change; drains and returns true if one came.
    pub fn wait(&self, ms: i32) -> bool {
        let mut p = libc::pollfd { fd: self.fd, events: libc::POLLIN, revents: 0 };
        let n = unsafe { libc::poll(&mut p, 1, ms) };
        if n <= 0 {
            return false;
        }
        let mut buf = [0u8; 4096];
        let mut any = false;
        while unsafe { libc::read(self.fd, buf.as_mut_ptr() as *mut _, buf.len()) } > 0 {
            any = true;
        }
        any
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::{plan, temp_store};
    use crate::store::{check_owner, Claim};

    fn feed(name: &str, owns: &str) -> Feed {
        Feed {
            name: name.into(),
            cmd: vec!["true".into()],
            trigger: Trigger::Stream,
            artifact: "plan".into(),
            owns: owns.into(),
            paused: false,
            cwd: "/".into(),
            min_gap: 1.0,
            timeout: 60.0,
            added: String::new(),
        }
    }

    #[test]
    fn registry_persists_and_refuses_overlap() {
        let st = temp_store();
        edit_registry(&st, |r| r.add(feed("jj", "jj"), false)).unwrap();
        edit_registry(&st, |r| r.add(feed("agents", "ag-"), false)).unwrap();
        let r = Registry::load(&st.dir());
        assert_eq!(r.feeds.len(), 2);
        assert_eq!(r.claims("plan").len(), 2);
        assert!(r.claims("other").is_empty());
        // one id, one owner: "jj-x" would be claimed twice
        let e = edit_registry(&st, |r| r.add(feed("jj2", "jj-"), false)).unwrap_err();
        assert!(e.contains("overlaps feed jj"), "{e}");
        assert!(edit_registry(&st, |r| r.add(feed("all", "*"), false)).is_err());
        assert!(edit_registry(&st, |r| r.add(feed("jj", "zz"), false)).unwrap_err().contains("exists"));
        edit_registry(&st, |r| r.set_paused("jj", true)).unwrap();
        assert!(Registry::load(&st.dir()).get("jj").unwrap().paused);
        edit_registry(&st, |r| r.remove("jj").map(|_| ())).unwrap();
        assert!(Registry::load(&st.dir()).get("jj").is_none());
        assert!(edit_registry(&st, |r| r.add(feed("bad.name", "x"), false)).is_err());
        std::fs::remove_dir_all(&st.home).ok();
    }

    /// A feeds.json that won't parse is an error, not an empty registry: edits refuse and leave
    /// the file alone, and the strict load that ownership checks use fails instead of saying
    /// "nobody owns anything".
    #[test]
    fn corrupt_registry_is_an_error_not_empty() {
        let st = temp_store();
        // missing is fine: no feeds
        assert_eq!(Registry::load_strict(&st.dir()), Ok(Registry::default()));
        edit_registry(&st, |r| r.add(feed("jj", "jj"), false)).unwrap();
        let p = Registry::path(&st.dir());
        // a hand edit's trailing comma
        let bad = std::fs::read_to_string(&p).unwrap().replacen("\n  ]", ",\n  ]", 1);
        assert!(bad.contains(",\n  ]"), "{bad}");
        std::fs::write(&p, &bad).unwrap();
        let e = Registry::load_strict(&st.dir()).unwrap_err();
        assert!(e.contains("feeds.json") && e.contains("trailing comma"), "{e}");
        for r in [
            edit_registry(&st, |r| r.add(feed("agents", "ag-"), false)),
            edit_registry(&st, |r| r.remove("jj").map(|_| ())),
            edit_registry(&st, |r| r.set_paused("jj", true)),
            edit_registry(&st, |r| r.set_paused("jj", false)),
        ] {
            assert_eq!(r.unwrap_err(), e);
        }
        assert_eq!(std::fs::read_to_string(&p).unwrap(), bad, "a refused edit must not rewrite the file");
        std::fs::remove_dir_all(&st.home).ok();
    }

    #[test]
    fn debounce_is_capped_and_stops() {
        // events that never let up: run anyway once the cap passes
        let t0 = epoch_now();
        let mut n = 0;
        settle(
            |_| {
                n += 1;
                std::thread::sleep(Duration::from_millis(5));
                true
            },
            || false,
            0.2,
        );
        let dt = epoch_now() - t0;
        assert!(n > 1 && (0.2..1.0).contains(&dt), "{n} waits in {dt}s");
        // a stop ends it mid-burst, cap or no cap
        let n = std::cell::Cell::new(0);
        settle(
            |_| {
                n.set(n.get() + 1);
                true
            },
            || n.get() >= 3,
            3600.0,
        );
        assert_eq!(n.get(), 3);
        // a burst that settles ends at the first quiet wait
        let mut left = 2;
        settle(
            |_| {
                left -= 1;
                left >= 0
            },
            || false,
            3600.0,
        );
        assert_eq!(left, -1);
    }

    /// A run that changes nothing still counts as a run: the status says when it ran and how it
    /// went, apart from the last write, and the log has a line per run.
    #[test]
    fn a_run_is_tracked_apart_from_a_write() {
        let st = temp_store();
        st.create(crate::doc::Doc::empty("plan")).unwrap();
        let mut f = feed("jj", "jj");
        f.trigger = Trigger::Interval { every: 60.0 };
        f.min_gap = 0.0;
        f.cmd = vec!["printf".into(), r#"{"type":"section","id":"jj","title":"t"}\n"#.into()];
        let mut w = Worker::new(Store::new(st.home.clone(), st.project.clone()), f.clone(), Arc::new(Ctl::default()), false);
        assert_eq!(Status::default().last_run_text(epoch_now()), "not run yet");
        assert_eq!(w.run_once(), Ok(true));
        let s = read_status(&st, "jj").unwrap();
        let at = |s: &Status| crate::util::parse_rfc3339(&s.last_run).unwrap() + 2.0;
        assert_eq!((s.last_outcome.as_str(), s.last_run_text(at(&s)).as_str()), ("wrote", "ran 2s ago · wrote"), "{s:?}");
        // the same output again: it ran, and wrote nothing
        assert_eq!(w.run_once(), Ok(false));
        let s = read_status(&st, "jj").unwrap();
        let text = s.last_run_text(at(&s));
        assert!(text.starts_with("ran 2s ago · unchanged (wrote ") && text.ends_with("s ago)"), "{text}");
        // registered over content that is already there: never wrote, and still visibly ran
        let fresh = Status { last_run: now_rfc3339(), last_outcome: "unchanged".into(), ..Status::default() };
        assert_eq!(fresh.last_run_text(epoch_now() + 2.0), "ran 2s ago · unchanged");
        let failed = Status { last_run: now_rfc3339(), last_outcome: "failed".into(), last_error: "exit 1: cannot read prs.json".into(), ..Status::default() };
        assert_eq!(failed.last_run_text(epoch_now() + 5.0), "failed 5s ago: exit 1: cannot read prs.json");
        let old = Status { last_write: now_rfc3339(), ..Status::default() };
        assert_eq!(old.last_run_text(epoch_now() + 60.0), "wrote 1m ago", "a status file from before runs were tracked");
        let log = std::fs::read_to_string(log_path(&st, "jj")).unwrap();
        assert!(log.contains("run: wrote (1 line)") && log.contains("run: unchanged (1 line)"), "{log}");
        std::fs::remove_dir_all(&st.home).ok();
    }

    /// `feed rm` / `feed pause` with no worker to do it: the health line says so, once.
    #[test]
    fn rm_and_pause_mark_the_health_line_themselves() {
        let st = temp_store();
        let f = feed("jj", "jj");
        let block = |v: Value| -> Block { serde_json::from_value(v).unwrap() };
        let health = block(json!({"type": "feed", "id": "jj-feed", "name": "jj", "state": "live", "until": "2026-11-10T00:00:00Z", "note": "exit 1"}));
        st.create(crate::doc::Doc::empty("plan")).unwrap();
        st.feed_write("plan", &[vec![Op::Add { parent: None, block: health, before: None, after: None }]], &f.writer(), &[]);
        mark_health(&st, &f, "paused");
        let d = st.load("plan").unwrap();
        let h = d.find("jj-feed").unwrap();
        assert_eq!((h.s("state"), h.f.contains_key("until"), h.f.contains_key("note")), ("paused", false, false));
        // the registry has forgotten the feed by the time rm marks it
        mark_health(&st, &f, "removed");
        let d = st.load("plan").unwrap();
        assert_eq!(d.find("jj-feed").unwrap().s("state"), "removed");
        let rev = d.rev;
        mark_health(&st, &f, "removed");
        assert_eq!(st.load("plan").unwrap().rev, rev, "already says so: no write");
        // no health line, no artifact: nothing to do, nothing created
        mark_health(&st, &feed("x", "x"), "removed");
        mark_health(&st, &Feed { artifact: "absent".into(), ..feed("y", "y") }, "removed");
        assert!(st.load("plan").unwrap().find("x-feed").is_none() && !st.exists("absent"));
        std::fs::remove_dir_all(&st.home).ok();
    }

    /// A watched file replaced by rename (how editors, `jq … > tmp && mv tmp file` and jj write)
    /// is still watched: the watch on its directory outlives the inode.
    #[test]
    fn a_watched_file_replaced_by_rename_is_still_watched() {
        let dir = std::env::temp_dir().join(format!("margin-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("data.json"), "1").unwrap();
        let w = Watcher::new(&dir.to_string_lossy(), &["data.json".to_string()]).unwrap();
        // wait for quiet first: something else on the machine may touch a new file in the temp
        // directory (an attribute change is an event too), and that must not count as ours
        let settle = |w: &Watcher| {
            for _ in 0..30 {
                if !w.wait(200) {
                    break;
                }
            }
        };
        settle(&w);
        for round in 0..3 {
            std::fs::write(dir.join("data.tmp"), format!("{round}")).unwrap();
            settle(&w);
            // the rename alone: the old inode's watch ends here, the directory's goes on
            std::fs::rename(dir.join("data.tmp"), dir.join("data.json")).unwrap();
            assert!(w.wait(2000), "replacement {round} was seen");
            settle(&w);
        }
        // and a plain write to the file that is there now
        std::fs::write(dir.join("data.json"), "x").unwrap();
        assert!(w.wait(2000));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A feed updates its artifact and never creates it: a run with no artifact fails with the
    /// message that names `margin new` (and the name it was probably meant to be), the status
    /// and log show it, and nothing is created, health line included.
    #[test]
    fn a_run_against_a_missing_artifact_fails_without_creating_it() {
        let st = temp_store();
        st.create(crate::doc::Doc::empty("plan")).unwrap();
        let mut f = Feed { artifact: "plans".into(), ..feed("jj", "jj") };
        f.trigger = Trigger::Interval { every: 60.0 };
        f.min_gap = 0.0;
        f.cmd = vec!["printf".into(), r#"{"type":"section","id":"jj","title":"t"}\n"#.into()];
        let mut w = Worker::new(Store::new(st.home.clone(), st.project.clone()), f.clone(), Arc::new(Ctl::default()), false);
        let want = "no artifact \"plans\" in project proj: create it with margin new plans (did you mean \"plan\"?)";
        assert_eq!(w.run_once(), Err(want.to_string()));
        w.set_state("live", None, None);
        w.write_health();
        assert!(!st.exists("plans") && !st.art("plans").exists(), "nothing created");
        assert_eq!(st.load("plan").unwrap().rev, 1, "and the artifact beside it is untouched");
        // the supervisor's loop records it as a failed run: `feed ls` shows the message
        let ctl = Arc::new(Ctl::default());
        let mut w = Worker::new(Store::new(st.home.clone(), st.project.clone()), f.clone(), ctl.clone(), false);
        let h = std::thread::spawn(move || {
            w.run();
        });
        let t0 = epoch_now();
        while read_status(&st, "jj").map(|s| s.last_outcome != "failed").unwrap_or(true) && epoch_now() - t0 < 10.0 {
            std::thread::sleep(Duration::from_millis(20));
        }
        ctl.stop("stopped");
        h.join().unwrap();
        let s = read_status(&st, "jj").unwrap();
        assert_eq!((s.last_outcome.as_str(), s.last_error.as_str()), ("failed", want));
        assert!(s.last_run_text(epoch_now()).contains("create it with margin new plans"), "{}", s.last_run_text(epoch_now()));
        let log = std::fs::read_to_string(log_path(&st, "jj")).unwrap();
        assert!(log.contains(&format!("run: failed: {want}")), "{log}");
        assert!(!st.exists("plans"));
        // once it is created, the same feed writes
        st.create(crate::doc::Doc::empty("plans")).unwrap();
        let mut w = Worker::new(Store::new(st.home.clone(), st.project.clone()), f, Arc::new(Ctl::default()), false);
        assert_eq!(w.run_once(), Ok(true));
        assert!(st.load("plans").unwrap().find("jj").is_some());
        std::fs::remove_dir_all(&st.home).ok();
    }

    /// One non-UTF-8 byte in a one-shot run's output costs that byte, not the run.
    #[test]
    fn run_once_survives_bad_bytes() {
        let st = temp_store();
        st.create(crate::doc::Doc::empty("plan")).unwrap();
        let mut f = feed("jj", "jj");
        f.trigger = Trigger::Interval { every: 60.0 };
        f.min_gap = 0.0;
        let out = r#"{"type":"section","id":"jj","title":"a\377b"}\n{"type":"text","id":"jj-2","text":"ok"}\n"#;
        f.cmd = vec!["printf".into(), out.into()];
        let mut w = Worker::new(Store::new(st.home.clone(), st.project.clone()), f, Arc::new(Ctl::default()), false);
        assert_eq!(w.run_once(), Ok(true));
        let d = st.load("plan").unwrap();
        assert_eq!(d.find("jj").unwrap().s("title"), "a\u{fffd}b");
        assert!(d.find("jj-2").is_some());
        std::fs::remove_dir_all(&st.home).ok();
    }

    #[test]
    fn lines_parse_as_ops_arrays_or_components() {
        assert!(matches!(parse_line(r#"{"op":"set","id":"jj-1","field":"text","value":"x"}"#).unwrap()[..], [Op::Set { .. }]));
        assert_eq!(parse_line(r#"[{"op":"rm","id":"a"},{"op":"rm","id":"b"}]"#).unwrap().len(), 2);
        assert!(matches!(&parse_line(r#"{"type":"row","id":"jj-1","text":"hi"}"#).unwrap()[..], [Op::Replace { block, .. }] if block.id == "jj-1"));
        assert!(parse_line("").unwrap().is_empty());
        assert!(parse_line("# comment").unwrap().is_empty());
        assert!(parse_line("hello").is_err());
        assert!(parse_line(r#"{"type":"row","text":"no id"}"#).is_err());
        // apply's extras: multi-field set, a component's parent, several lines as one write
        assert_eq!(parse_line(r#"{"op":"set","id":"t1","fields":{"a":"x","b":null}}"#).unwrap().len(), 2);
        assert!(matches!(&parse_line(r#"{"type":"todo","id":"t9","parent":"steps"}"#).unwrap()[..], [Op::Replace { parent: Some(p), block }] if p == "steps" && !block.f.contains_key("parent")));
        assert_eq!(parse_input("{\"op\":\"rm\",\"id\":\"a\"}\n# c\n[{\"op\":\"rm\",\"id\":\"b\"}]").unwrap().len(), 2);
        assert!(parse_input("{\"op\":\"rm\",\"id\":\"a\"}\nnope").unwrap_err().starts_with("line 2"));
        assert!(parse_line(r#"{"what":1}"#).is_err());
    }

    #[test]
    fn ownership_four_ways() {
        let st = temp_store();
        st.put(plan(), None).unwrap();
        edit_registry(&st, |r| r.add(feed("jj", "jj"), false)).unwrap();
        let w = feed("jj", "jj").writer();
        let row = |id: &str| parse_line(&format!(r#"{{"type":"section","id":"{id}","title":"x"}}"#)).unwrap();
        // the feed writes inside its prefix …
        let (rev, errs) = st.feed_write("plan", &[row("jj")], &w, &[]);
        assert!(rev.is_some() && errs.is_empty(), "{errs:?}");
        assert_eq!(st.load("plan").unwrap().by, "feed:jj");
        // … but not outside it, and not into Claude's section
        let (_, errs) = st.feed_write("plan", &[row("t1x")], &w, &[]);
        assert!(errs[0].contains("outside its prefix"), "{errs:?}");
        let add = Op::Add { parent: Some("steps".into()), block: serde_json::from_str(r#"{"type":"text","id":"jj-n"}"#).unwrap(), before: None, after: None };
        let (_, errs) = st.feed_write("plan", &[vec![add]], &w, &[]);
        assert!(errs[0].contains("only insert into its own"), "{errs:?}");
        // one bad line doesn't sink the good one beside it
        let (rev, errs) = st.feed_write("plan", &[row("zz"), row("jj-two")], &w, &[]);
        assert!(rev.is_some() && errs.len() == 1);
        // Claude can't write, remove or insert into the feed's ids
        let e = st.patch("plan", &[Op::Set { id: "jj".into(), field: "title".into(), value: json!("mine") }], None).unwrap_err();
        assert!(e.contains("belongs to feed `jj`"), "{e}");
        assert!(st.patch("plan", &[Op::Rm { id: "jj-two".into() }], None).is_err());
        let add = Op::Add { parent: Some("jj".into()), block: serde_json::from_str(r#"{"type":"text","id":"c1"}"#).unwrap(), before: None, after: None };
        assert!(st.patch("plan", &[add], None).is_err());
        // a whole-document put must carry the feed's components through unchanged
        let mut d = st.load("plan").unwrap();
        d.title = "renamed".into();
        st.put(d.clone(), None).unwrap();
        d.blocks.retain(|b| b.id != "jj");
        assert!(st.put(d, None).unwrap_err().contains("belongs to feed"));
        // Claude's own ids are still Claude's
        st.patch("plan", &[Op::Set { id: "t1".into(), field: "note".into(), value: json!("ok") }], None).unwrap();
        // sulin's decisions can't land on a feed component
        assert!(st.decide("plan", "jj", "pin", json!(true), "key", None).unwrap_err().contains("read-only"));
        // a feed never sets the title unless it owns the whole artifact
        let c = [Claim { feed: "jj".into(), owns: "jj".into() }];
        assert!(check_owner(&Op::Title { value: "x".into() }, &st.load("plan").unwrap(), &w, &c).is_err());
        std::fs::remove_dir_all(&st.home).ok();
    }

    /// Re-printing the same state is not a revision, and no revision leaves a copy behind.
    #[test]
    fn an_unchanged_feed_write_is_not_a_revision() {
        let st = temp_store();
        st.create(crate::doc::Doc::empty("plan")).unwrap();
        let w = feed("jj", "jj").writer();
        for i in 0..5 {
            let l = format!(r#"{{"type":"text","id":"jj","text":"run {i}"}}"#);
            st.feed_write("plan", &[parse_line(&l).unwrap()], &w, &[]);
        }
        let l = r#"{"type":"text","id":"jj","text":"run 4"}"#;
        assert_eq!(st.feed_write("plan", &[parse_line(l).unwrap()], &w, &[]).0, None);
        assert_eq!(st.load("plan").unwrap().rev, 6, "the create, then five feed writes");
        assert!(!st.art("plan").join("history.jsonl").exists());
        std::fs::remove_dir_all(&st.home).ok();
    }

    #[test]
    fn backoff_doubles_caps_and_forgives() {
        let mut b = Backoff::default();
        let ds: Vec<f64> = (0..8).map(|_| b.record(false, 0.5, 30.0)).collect();
        assert_eq!(ds, vec![1.0, 2.0, 4.0, 8.0, 16.0, 32.0, 60.0, 60.0]);
        // a run that stayed up past `healthy` resets: the next crash waits 1 s again
        assert_eq!(b.record(false, 45.0, 30.0), 1.0);
        assert_eq!(b.record(true, 0.1, 30.0), 0.0);
        let mut g = Gate::new(1.0);
        assert_eq!(g.wait(100.0), 0.0);
        g.pass(100.0);
        assert!((g.wait(100.25) - 0.75).abs() < 1e-9);
    }

    #[test]
    fn floods_coalesce() {
        let mut p = Vec::new();
        for i in 0..1000 {
            coalesce(&mut p, parse_line(&format!(r#"{{"type":"row","id":"b-1","text":"n {i}"}}"#)).unwrap());
        }
        coalesce(&mut p, parse_line(r#"{"op":"set","id":"b-2","field":"text","value":"x"}"#).unwrap());
        assert_eq!(p.len(), 2);
        assert!(matches!(&p[0][..], [Op::Replace { block, .. }] if block.s("text") == "n 999"));
    }

    #[test]
    fn lossy_lines_survive_bad_bytes() {
        let v: Vec<String> = lossy_lines(&b"a\n\xff b\r\nc"[..]).collect();
        assert_eq!(v, vec!["a".to_string(), "\u{fffd} b".into(), "c".into()]);
    }

    /// A replace can't jump over an op that touches its block: the stream's last word wins.
    #[test]
    fn coalesce_keeps_order_with_other_ops() {
        let mut p = Vec::new();
        coalesce(&mut p, parse_line(r#"{"type":"section","id":"s","children":[{"type":"row","id":"c","text":"a"}]}"#).unwrap());
        coalesce(&mut p, parse_line(r#"{"op":"set","id":"c","field":"text","value":"x"}"#).unwrap());
        coalesce(&mut p, parse_line(r#"{"type":"section","id":"s","children":[{"type":"row","id":"c","text":"FINAL"}]}"#).unwrap());
        assert_eq!(p.len(), 3, "the last replace stays last");
        // disjoint replaces still collapse
        let mut q = Vec::new();
        for i in 0..3 {
            coalesce(&mut q, parse_line(&format!(r#"{{"type":"row","id":"a","text":"{i}"}}"#)).unwrap());
            coalesce(&mut q, parse_line(&format!(r#"{{"type":"row","id":"b","text":"{i}"}}"#)).unwrap());
        }
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn status_ids_stay_in_the_prefix() {
        for (n, o) in [("jj", "jj"), ("a", "ag-"), ("w", "*"), ("x", "x_")] {
            let id = status_id(n, o);
            assert!(crate::store::owns(o, &id) && valid_id(&id), "{id}");
        }
    }
}
