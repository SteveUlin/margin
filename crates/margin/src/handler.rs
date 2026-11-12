//! Handlers: registered commands the daemon runs when an event they asked for is appended.
//!
//! A handler is a name, a command and one or more event-name globs (`--on`). It has no timer and
//! watches no file of its author's: scheduling lives outside margin, and reaches a handler as an event
//! (`margin emit reviews.refresh` from cron). The pane never runs anything either: a key on a
//! row appends an event, and whichever handler asked for that name does the work.
//!
//! The contract (`margin help handlers` states it for script authors):
//! - started by argv exec with no shell, in the directory it was registered from;
//! - stdin: the pending matching events since the handler's cursor, one JSON object per line;
//! - stdout: JSON lines, each an op or a component for its board (the line grammar of `margin apply`), a
//!   toast, or `{"emit": "name", "value": …}`;
//! - exit 0: the ops apply, the emits are appended and the cursor moves past the last event it
//!   was given, in one locked step (`Store::handler_commit`);
//! - a non-zero exit or a timeout: nothing applies and the cursor stays; it is retried with
//!   backoff, and after `--retries` more failures the batch is passed over with a
//!   `handler.failed` event, so one bad event never blocks the ones behind it.
//!
//! Delivery is therefore at-least-once: a handler must be safe to run twice on one event, and
//! every event carries its `seq` to dedupe on.
//!
//! This file also holds what one run needs around it: the per-handler log, the stop flag the
//! daemon raises, and the retry backoff.
//!
//! A worker never wakes to ask whether there is work. It blocks (watch.rs) until the event log
//! changes, its retry is due, or it is told to stop; while its command runs it blocks on the
//! child's pid fd, the stop flag and the run's timeout.

use crate::doc::{parse_line, Op};
use crate::owner::fmt_secs;
use crate::store::{write_atomic, Event, Store, Writer};
use crate::util::{epoch_now, now_rfc3339, valid_id};
use crate::watch::{PidFd, Waker};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

fn d_timeout() -> f64 {
    60.0
}
fn d_retries() -> u32 {
    3
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Handler {
    pub name: String,
    /// argv, run directly (no shell), in `cwd`. A first word of exactly `margin` runs this binary.
    pub cmd: Vec<String>,
    /// Event-name globs: the handler is run for an appended event whose kind matches one.
    pub on: Vec<String>,
    /// The board its stdout writes, and the id prefix it owns there (both empty: it writes none).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub board: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub owns: String,
    #[serde(default)]
    pub paused: bool,
    pub cwd: String,
    /// The PATH of the shell that registered it: what its command and the tools it calls are
    /// found on, whoever started the daemon.
    #[serde(default)]
    pub path: String,
    /// A run is killed after this long.
    #[serde(default = "d_timeout")]
    pub timeout: f64,
    /// How many times a failed run is retried before the events it was given are passed over.
    #[serde(default = "d_retries")]
    pub retries: u32,
    #[serde(default)]
    pub added: String,
}

impl Handler {
    /// Who its events and writes are logged as.
    pub fn by(&self) -> String {
        format!("handler:{}", self.name)
    }
    /// Its place in the event log: the named cursor `handler:<name>`
    /// (`margin events --cursor handler:<name> --no-commit` lists what waits for it).
    pub fn cursor(&self) -> String {
        format!("handler:{}", self.name)
    }
    pub fn writer(&self) -> Writer {
        Writer::Named { by: self.by(), owns: self.owns.clone() }
    }
    /// Is this event one of the handler's? A glob in `--on` matches the event's name. Never an
    /// event the handler emitted itself (it would loop); a `handler.*` event only through a glob
    /// that says `handler.` itself; a passive kind (seen, view, fold) only by its exact name,
    /// and a board's view events also by a glob that names the board (`reviews.*`). So `--on
    /// '*'` means every fact, not the runner's or the pane's noise.
    pub fn wants(&self, e: &Event) -> bool {
        e.by != self.by() && self.on.iter().any(|g| matches(g, &e.kind))
    }
}

/// Does the glob `g` of an `--on` select events named `kind`? A passive kind only by its exact
/// name, with one exception: the pane's view events (`<board>.shown|hidden|detail`) are also
/// selected by a glob that names their board outright (`reviews.*`), since a provider that asks
/// for everything about its board means them too. `*.open` and `*` name no board and do not.
pub fn matches(g: &str, kind: &str) -> bool {
    let names_the_board = || g.split_once('.').map(|(b, _)| !b.contains('*') && !b.is_empty()).unwrap_or(false);
    crate::util::glob_match(g, kind)
        && (!kind.starts_with("handler.") || g.starts_with("handler."))
        && (!crate::store::is_passive(kind) || g == kind || (crate::store::is_view(kind) && names_the_board()))
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Registry {
    #[serde(default)]
    pub handlers: Vec<Handler>,
}

impl Registry {
    pub fn path(dir: &Path) -> PathBuf {
        dir.join("handlers.json")
    }
    /// The registry, or empty when there is none or it won't parse: for display only.
    pub fn load(dir: &Path) -> Registry {
        Self::load_strict(dir).unwrap_or_default()
    }
    /// Only a missing handlers.json is empty; one that won't read or parse is an error, so an
    /// edit can't save an empty registry over it and ownership checks can't read "nothing owned".
    pub fn load_strict(dir: &Path) -> Result<Registry, String> {
        match std::fs::read_to_string(Self::path(dir)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Registry::default()),
            Err(e) => Err(format!("{}: {e}", Self::path(dir).display())),
            Ok(s) => serde_json::from_str(&s).map_err(|e| format!("{} is corrupt ({e}); fix or remove it", Self::path(dir).display())),
        }
    }
    pub fn save(&self, dir: &Path) -> Result<(), String> {
        crate::store::write_durable(&Self::path(dir), serde_json::to_string_pretty(self).unwrap().as_bytes()).map_err(|e| e.to_string())
    }
    pub fn get(&self, name: &str) -> Option<&Handler> {
        self.handlers.iter().find(|h| h.name == name)
    }
    /// The handlers that would be run for an event named `kind` (paused ones included: they
    /// take it when resumed).
    pub fn for_kind(&self, kind: &str) -> Vec<&Handler> {
        self.handlers.iter().filter(|h| h.on.iter().any(|g| matches(g, kind))).collect()
    }
    pub fn claims(&self, art: &str) -> Vec<crate::store::Claim> {
        self.handlers.iter().filter(|h| h.board == art && !h.owns.is_empty()).map(|h| crate::store::Claim { feed: h.name.clone(), owns: h.owns.clone(), kind: "handler" }).collect()
    }
    /// Every claim in the registry, with its board.
    pub fn all_claims(&self) -> Vec<(String, crate::store::Claim)> {
        self.handlers.iter().filter(|h| !h.owns.is_empty()).map(|h| (h.board.clone(), crate::store::Claim { feed: h.name.clone(), owns: h.owns.clone(), kind: "handler" })).collect()
    }
    fn check(h: &Handler) -> Result<(), String> {
        if !valid_id(&h.name) {
            return Err(format!("bad handler name {:?}: letters, digits, - and _", h.name));
        }
        if h.cmd.is_empty() {
            return Err(format!("no command: margin handler add {} --on <event glob> -- <cmd…>", h.name));
        }
        if h.on.is_empty() {
            return Err("a handler needs the events it handles: --on <name or glob> (reviews.open, '*.open', 'ci.*')".into());
        }
        if let Some(g) = h.on.iter().find(|g| g.is_empty() || g.contains(char::is_whitespace)) {
            return Err(format!("bad --on {g:?}: an event name or a glob of one (reviews.open, 'ci.*')"));
        }
        match (h.board.is_empty(), h.owns.is_empty()) {
            (true, false) => return Err("--owns needs the board it is in: --board B".into()),
            (false, true) => return Err("--board needs the id prefix the handler writes there: --owns PREFIX".into()),
            (false, false) => {
                if !valid_id(&h.board) {
                    return Err(format!("bad board id {:?}", h.board));
                }
                if h.owns != "*" && !valid_id(&h.owns) {
                    return Err(format!("bad --owns prefix {:?}: an id prefix (letters, digits, - and _) or *", h.owns));
                }
            }
            _ => {}
        }
        if h.timeout <= 0.0 {
            return Err("--timeout must be more than 0".into());
        }
        Ok(())
    }
    /// Register a new handler; a name that exists is refused (`set` changes one).
    pub fn add(&mut self, h: Handler) -> Result<(), String> {
        Self::check(&h)?;
        if self.get(&h.name).is_some() {
            return Err(format!("handler {:?} exists: change it with margin handler set {} …, or margin handler rm {} first", h.name, h.name, h.name));
        }
        self.handlers.push(h);
        Ok(())
    }
    /// Change a registered handler in place; a missing one is refused (`add` creates).
    pub fn set(&mut self, h: Handler) -> Result<(), String> {
        Self::check(&h)?;
        let i = self.handlers.iter().position(|x| x.name == h.name).ok_or_else(|| missing(&h.name))?;
        self.handlers[i] = h;
        Ok(())
    }
    pub fn remove(&mut self, name: &str) -> Result<Handler, String> {
        let i = self.handlers.iter().position(|h| h.name == name).ok_or_else(|| missing(name))?;
        Ok(self.handlers.remove(i))
    }
    pub fn set_paused(&mut self, name: &str, p: bool) -> Result<(), String> {
        self.handlers.iter_mut().find(|h| h.name == name).map(|h| h.paused = p).ok_or_else(|| missing(name))
    }
}

/// The one message for a handler that isn't registered ("no handler …": exit code 3).
pub fn missing(name: &str) -> String {
    format!("no handler {name:?}: margin handler ls lists them, margin handler add {name} --on <event> -- <cmd…> creates one")
}

/// Change the registry under the project lock.
///
/// A handler that stops owning a prefix (removed, or set to another board or prefix) gives its
/// blocks back to the board's layout first, where they are ordinary content again; one that
/// starts owning a prefix takes the blocks already under it into its own file (store.rs).
pub fn edit_registry<T>(st: &Store, f: impl FnOnce(&mut Registry) -> Result<T, String>) -> Result<T, String> {
    let _l = st.lock().map_err(|e| e.to_string())?;
    let mut r = Registry::load_strict(&st.dir())?;
    let before = r.all_claims();
    let out = f(&mut r)?;
    st.claims_going_locked(&before, &r.all_claims())?;
    r.save(&st.dir())?;
    st.claims_came_locked(&before, &r.all_claims());
    Ok(out)
}

/// A handler's state, for `margin handler ls` and `margin daemon status`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Status {
    /// `live` (waiting for events), `retrying`, `stopped` (the daemon stopped), `paused`, `removed`.
    pub state: String,
    pub since: String,
    /// The next retry (RFC 3339), while `retrying`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    /// The running command's pid, 0 when none is.
    #[serde(default)]
    pub pid: u32,
    /// When the run in flight started (RFC 3339); empty when none is.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub run_started: String,
    #[serde(default)]
    pub runs: u64,
    #[serde(default)]
    pub last_run: String,
    /// `ok`, `failed` (the reason is `last_error`), `skipped` (gave up on a batch), `stopped`
    /// (the daemon was stopping: not a failure).
    #[serde(default)]
    pub last_outcome: String,
    #[serde(default)]
    pub last_error: String,
    /// Failed runs in a row on the batch at the head of its queue. Kept across a daemon
    /// restart, so an event that always fails is still passed over after `--retries` retries.
    #[serde(default)]
    pub attempts: u32,
    /// The seq its last successful run took the cursor to.
    #[serde(default)]
    pub last_seq: u64,
}

impl Status {
    /// The last run in a few words: `running for 3s`, `ran 2s ago · ok`, `failed 5s ago: …`.
    pub fn last_run_text(&self, now: f64) -> String {
        let ago = |t: &str| crate::util::parse_rfc3339(t).map(|t| crate::util::ago(now - t));
        if let Some(t) = crate::util::parse_rfc3339(&self.run_started) {
            return format!("running for {}", fmt_secs((now - t).max(0.0).floor()));
        }
        match (ago(&self.last_run), self.last_outcome.as_str()) {
            (None, _) => "not run yet".into(),
            (Some(r), "failed") | (Some(r), "skipped") => format!("{} {r}: {}", self.last_outcome, vellum::text::truncate(&self.last_error, 90)),
            (Some(r), o) => format!("ran {r} · {o}"),
        }
    }
}

pub fn dir(st: &Store) -> PathBuf {
    st.dir().join("handlers")
}
pub fn status_path(st: &Store, name: &str) -> PathBuf {
    dir(st).join(format!("{name}.status.json"))
}
pub fn log_path(st: &Store, name: &str) -> PathBuf {
    dir(st).join(format!("{name}.log"))
}
pub fn read_status(st: &Store, name: &str) -> Option<Status> {
    std::fs::read_to_string(status_path(st, name)).ok().and_then(|s| serde_json::from_str(&s).ok())
}

/// The events waiting for a handler: after its cursor, matching its globs. Its cursor is
/// created at the end of the log the first time anyone asks, so a new handler hears what
/// happens from its registration on.
pub fn pending(st: &Store, h: &Handler) -> Vec<Event> {
    let from = st.cursor_or_create(&h.cursor());
    st.events_since(from).into_iter().filter(|e| h.wants(e)).collect()
}

/// A status a dead daemon left behind: a handler recorded as running whose process is gone.
/// Called when a worker starts and by `margin doctor`; returns what it corrected.
pub fn settle_status(s: &mut Status) -> bool {
    if s.run_started.is_empty() && s.pid == 0 {
        return false;
    }
    s.run_started.clear();
    s.pid = 0;
    s.last_outcome = "interrupted".into();
    true
}

/// A handler's toast line, `{"toast": "tests passed", "level": "ok", "at": "ci-1", "ttl": 8}`: sent
/// beside the document (toast.rs), never an op. True when the line was one.
pub fn toast_line(st: &Store, art: &str, handler: &str, line: &str, log: &Log) -> bool {
    let t = line.trim();
    if !t.starts_with('{') || !t.contains("\"toast\"") {
        return false;
    }
    let Ok(v) = serde_json::from_str::<Value>(t) else { return false };
    let Some(text) = v["toast"].as_str() else { return false };
    if let Err(e) = crate::toast::send(st, art, text, v["level"].as_str().unwrap_or("info"), v["at"].as_str().unwrap_or(""), v["ttl"].as_f64(), &format!("handler:{handler}")) {
        log.line(&format!("bad toast: {e}"));
    }
    true
}

/// Restart backoff: 1, 2, 4 … 60 s after consecutive failures; a run that stayed up for
/// `healthy` seconds wipes the slate, so a command that fails once a day is retried in 1 s.
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

/// Per-handler log: daemon notes plus the command's stderr, capped at 512 KB (one rotation).
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

/// Shared between the daemon and one worker.
#[derive(Default)]
pub struct Ctl {
    pub stop: AtomicBool,
    /// Why it was stopped: "removed", "paused", "stopped" (daemon exit), "restart" (job changed).
    pub reason: Mutex<String>,
    /// Raised with `stop`, and never lowered: what a blocked worker is woken by.
    pub wake: Waker,
    /// Finish the run in hand and take no more events (`drain`): the daemon is about to become
    /// another build (daemon.rs). The running command is left alone, unlike a stop.
    pub draining: AtomicBool,
    /// Raised with `drain`: wakes a worker that is waiting for events, never one waiting on its
    /// command (that wait ends when the command does).
    pub drain_wake: Waker,
}

impl Ctl {
    pub fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed) || crate::daemon::exiting()
    }
    pub fn stop(&self, why: &str) {
        *self.reason.lock().unwrap() = why.into();
        self.stop.store(true, Ordering::Relaxed);
        self.wake.wake();
    }
    /// End the worker after the run it is in, if any: its command is not signalled, its cursor
    /// moves as after any run, and what is still queued stays pending for the next worker.
    pub fn drain(&self) {
        *self.reason.lock().unwrap() = "restart".into();
        self.draining.store(true, Ordering::Relaxed);
        self.drain_wake.wake();
    }
    pub fn draining(&self) -> bool {
        self.draining.load(Ordering::Relaxed)
    }
    /// What ends a worker's wait from outside: this handler's stop, and the daemon's exit.
    fn interrupts(&self) -> [i32; 2] {
        [self.wake.fd(), crate::daemon::exit_fd()]
    }
}

/// One line of a handler's stdout that is an emit: `{"emit": "name", "value": …}`.
fn emit_line(line: &str) -> Option<Result<(String, Value), String>> {
    let t = line.trim();
    if !t.starts_with('{') || !t.contains("\"emit\"") {
        return None;
    }
    let v: Value = serde_json::from_str(t).ok()?;
    if v.get("op").is_some() || v.get("type").is_some() {
        return None;
    }
    let name = v.get("emit")?;
    Some(match name.as_str() {
        Some(n) if n.starts_with("handler.") => Err(format!("emit {n:?}: names starting with handler. are the runner's own")),
        Some(n) if crate::store::valid_event_name(n) => Ok((n.to_string(), v.get("value").cloned().unwrap_or(Value::Null))),
        _ => Err(format!("emit {name}: an event name is letters, digits and . _ - :")),
    })
}

/// How a run ended, when it did not fail.
#[derive(Debug, PartialEq)]
pub enum Ran {
    /// Exit 0, and its output was committed. `changed`: its board's content did.
    Done { changed: bool },
    /// The worker was told to stop (daemon shutdown, pause, rm) and killed the command: nothing
    /// was applied, no cursor moved, and it is not a failure.
    Stopped,
}

/// One handler being run: by a daemon worker thread, or by `margin handler run` in the foreground.
pub struct Worker {
    pub st: Store,
    pub h: Handler,
    pub ctl: Arc<Ctl>,
    pub log: Log,
    pub status: Status,
    /// `margin handler run`: print what happens, and leave the daemon's status file alone.
    pub verbose: bool,
    last_stderr: Arc<Mutex<String>>,
    /// Closed when the running command's stderr has been read to its end.
    stderr_done: Option<mpsc::Receiver<()>>,
}

impl Worker {
    pub fn new(st: Store, h: Handler, ctl: Arc<Ctl>, foreground: bool) -> Worker {
        let log = Log::new(log_path(&st, &h.name), foreground);
        Worker { st, h, ctl, log, status: Status::default(), verbose: foreground, last_stderr: Default::default(), stderr_done: None }
    }

    fn save_status(&self) {
        if !self.verbose {
            let _ = write_atomic(&status_path(&self.st, &self.h.name), serde_json::to_string_pretty(&self.status).unwrap().as_bytes());
        }
    }

    fn set_state(&mut self, state: &str, until: Option<f64>) {
        if self.status.state != state {
            self.status.since = now_rfc3339();
        }
        self.status.state = state.into();
        self.status.until = until.map(crate::util::rfc3339);
        self.save_status();
    }

    /// Start the command: argv exec with no shell, in the directory it was registered from. Its
    /// environment is the daemon's, with PATH replaced by the registering shell's and the
    /// MARGIN_* variables added.
    fn spawn(&mut self) -> Result<Child, String> {
        // `margin …` as a handler's command is this binary, wherever the daemon was started from
        let prog = if self.h.cmd[0] == "margin" { crate::help::bin_path().display().to_string() } else { self.h.cmd[0].clone() };
        let mut c = Command::new(&prog);
        c.args(&self.h.cmd[1..]).current_dir(&self.h.cwd);
        if !self.h.path.is_empty() {
            c.env("PATH", &self.h.path);
        }
        c.env("MARGIN_HANDLER", &self.h.name)
            .env("MARGIN_PROJECT", &self.st.project)
            .env("MARGIN_HOME", &self.st.home)
            .env("MARGIN_BIN", crate::help::bin_path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if !self.h.board.is_empty() {
            c.env("MARGIN_BOARD", &self.h.board).env("MARGIN_OWNS", &self.h.owns);
        }
        unsafe {
            c.pre_exec(|| {
                // its own process group, so a stop kills whatever it started; and it dies with
                // the daemon even if the daemon is killed hard
                libc::setpgid(0, 0);
                #[cfg(target_os = "linux")]
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                // the daemon ignores SIGPIPE; a script expects the default
                libc::signal(libc::SIGPIPE, libc::SIG_DFL);
                Ok(())
            });
        }
        let mut ch = c.spawn().map_err(|e| format!("{prog}: {e}"))?;
        self.status.pid = ch.id();
        self.status.runs += 1;
        let err = ch.stderr.take().unwrap();
        let log = self.log.clone();
        let last = self.last_stderr.clone();
        let (done, rx) = mpsc::channel::<()>();
        self.stderr_done = Some(rx);
        std::thread::spawn(move || {
            let _done = done;
            let mut b = std::io::BufReader::new(err);
            let mut buf = Vec::new();
            use std::io::BufRead;
            // lossy: one bad byte must not stop the reader, or the child blocks on a full pipe
            while matches!(b.read_until(b'\n', &mut buf), Ok(n) if n > 0) {
                let l = String::from_utf8_lossy(&buf).trim_end().to_string();
                log.line(&format!("stderr: {l}"));
                *last.lock().unwrap() = l;
                buf.clear();
            }
        });
        Ok(ch)
    }

    /// TERM to the whole process group, a grace period, then KILL.
    fn kill(ch: &mut Child, pidfd: &PidFd) {
        let pg = ch.id() as i32;
        unsafe { libc::kill(-pg, libc::SIGTERM) };
        let end = Instant::now() + Duration::from_secs(1);
        loop {
            if matches!(ch.try_wait(), Ok(Some(_))) {
                return;
            }
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            pidfd.wait(&[], left);
        }
        unsafe { libc::kill(-pg, libc::SIGKILL) };
        let _ = ch.wait();
    }

    /// One run, start to commit. `events` go to the command's stdin. Exit 0 commits its output
    /// in one locked step, with the cursor when `move_cursor`; anything else is an Err and
    /// commits nothing. `dry`: print what would be committed and write nothing.
    pub fn exec(&mut self, events: &[Event], dry: bool, move_cursor: bool) -> Result<Ran, String> {
        *self.last_stderr.lock().unwrap() = String::new();
        if !self.h.board.is_empty() && !self.st.exists(&self.h.board) {
            return Err(self.st.missing(&self.h.board));
        }
        let mut ch = self.spawn()?;
        self.status.run_started = now_rfc3339();
        self.save_status();
        if let Some(mut si) = ch.stdin.take() {
            // from its own thread: a command that never reads stdin must not block the run
            let text: String = events.iter().map(|e| serde_json::to_string(e).unwrap() + "\n").collect();
            std::thread::spawn(move || {
                let _ = si.write_all(text.as_bytes());
            });
        }
        let mut out = ch.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut b = Vec::new();
            let _ = out.read_to_end(&mut b);
            let _ = tx.send(String::from_utf8_lossy(&b).into_owned());
        });
        // the wait for it: until the child ends, a stop is raised, or the run's time is up
        let pidfd = PidFd::open(ch.id());
        let start = Instant::now();
        let ended = loop {
            if let Ok(Some(s)) = ch.try_wait() {
                break Ok(s);
            }
            if self.ctl.stopped() {
                Self::kill(&mut ch, &pidfd);
                break Err(None);
            }
            let left = self.h.timeout - start.elapsed().as_secs_f64();
            if left <= 0.0 {
                Self::kill(&mut ch, &pidfd);
                break Err(Some(format!("timed out after {}", fmt_secs(self.h.timeout))));
            }
            pidfd.wait(&self.ctl.interrupts(), Duration::try_from_secs_f64(left).unwrap_or(Duration::MAX));
        };
        drop(pidfd);
        self.status.run_started.clear();
        self.status.pid = 0;
        let status = match ended {
            Ok(s) => s,
            Err(None) => {
                // stopping is not failing: nothing applied, the cursor where it was
                self.status.last_run = now_rfc3339();
                self.status.last_outcome = "stopped".into();
                self.save_status();
                self.log.line("run: stopped (the events stay pending)");
                return Ok(Ran::Stopped);
            }
            Err(Some(e)) => return Err(e),
        };
        let text = rx.recv_timeout(Duration::from_secs(2)).unwrap_or_default();
        // The exit is seen the moment it happens, which can be before the reader thread has
        // logged the command's last words: wait for the end of its stderr (bounded: something
        // the command left running may hold the pipe open), so a failure carries its reason.
        if let Some(done) = self.stderr_done.take() {
            let _ = done.recv_timeout(Duration::from_secs(1));
        }
        use std::os::unix::process::ExitStatusExt;
        if !status.success() {
            let how = match (status.code(), status.signal()) {
                (Some(c), _) => format!("exit {c}"),
                (None, Some(s)) => format!("signal {s}"),
                _ => "ended".into(),
            };
            let last = self.last_stderr.lock().unwrap().clone();
            return Err(if last.is_empty() { how } else { format!("{how}: {last}") });
        }
        let (mut groups, mut emits, mut toasts, mut kept): (Vec<Vec<Op>>, Vec<(String, Value)>, Vec<String>, Vec<String>) = Default::default();
        for l in text.lines() {
            let t = l.trim();
            if t.starts_with('{') && t.contains("\"toast\"") && serde_json::from_str::<Value>(t).map(|v| v["toast"].is_string()).unwrap_or(false) {
                toasts.push(l.to_string());
                continue;
            }
            match emit_line(l) {
                Some(Ok(e)) => {
                    emits.push(e);
                    kept.push(t.to_string());
                    continue;
                }
                Some(Err(e)) => {
                    self.log.line(&format!("bad line: {e}"));
                    continue;
                }
                None => {}
            }
            match parse_line(l) {
                Ok(ops) if !ops.is_empty() => {
                    groups.push(ops);
                    kept.push(t.to_string());
                }
                Ok(_) => {}
                Err(e) => self.log.line(&format!("bad line: {e}")),
            }
        }
        let cursor = events.last().map(|e| e.seq).filter(|_| move_cursor);
        if dry {
            for e in events {
                println!("given  {}", serde_json::to_string(e).unwrap());
            }
            for l in kept.iter().chain(&toasts) {
                println!("would  {}", l.trim());
            }
            if let Some(c) = cursor {
                println!("would  move cursor {} to {c}", self.h.cursor());
            }
            if kept.is_empty() && toasts.is_empty() {
                println!("would  change nothing (it printed no op, component or emit)");
            }
            return Ok(Ran::Done { changed: false });
        }
        for l in &toasts {
            if self.h.board.is_empty() {
                self.log.line("bad line: a toast needs a board (--board B --owns PREFIX)");
            } else {
                toast_line(&self.st, &self.h.board, &self.h.name, l, &self.log);
            }
        }
        let cur = self.h.cursor();
        let (rev, errs) = self.st.handler_commit(&self.h.board, &groups, &self.h.writer(), &emits, cursor.map(|c| (cur.as_str(), c)));
        for e in &errs {
            self.log.line(&format!("rejected: {e}"));
        }
        if self.verbose {
            match rev {
                // the handler's own file: the board's rev is its layout's, and did not move
                Some(r) => println!("{}: {} rev {r}", self.h.board, self.h.by()),
                None if !self.h.board.is_empty() && errs.is_empty() => println!("{} unchanged", self.h.board),
                None => {}
            }
            for (n, _) in &emits {
                println!("emitted {n}");
            }
        }
        self.status.last_run = now_rfc3339();
        self.status.last_outcome = "ok".into();
        if let Some(c) = cursor {
            self.status.last_seq = c;
        }
        self.save_status();
        let n = events.len();
        self.log.line(&format!(
            "run: ok ({n} event{}{}, {} line{}{})",
            if n == 1 { "" } else { "s" },
            events.last().map(|e| format!(" to seq {}", e.seq)).unwrap_or_default(),
            kept.len(),
            if kept.len() == 1 { "" } else { "s" },
            if rev.is_some() { ", wrote" } else { "" }
        ));
        Ok(Ran::Done { changed: rev.is_some() })
    }

    /// The worker loop: wait for events the handler wants, run it on them, until stopped.
    pub fn run(&mut self) {
        self.status = read_status(&self.st, &self.h.name).unwrap_or_default();
        if settle_status(&mut self.status) {
            // the daemon died with a run in flight: its events are still pending, so they are
            // simply given again (the handler is safe to run twice by contract)
            self.log.line("the last daemon stopped mid-run: the events it was given are pending again");
        }
        self.log.line(&format!("start: {} (on {})", self.h.cmd.join(" "), self.h.on.join(" ")));
        self.set_state("live", None);
        let mut bo = Backoff { fails: self.status.attempts };
        let mut retry_at = 0.0f64;
        // Armed before the first look at the log: an event appended between a look and the wait
        // after it is already queued on the watch, and that wait returns at once.
        let [stop, exit] = self.ctl.interrupts();
        let mut watch = self.st.watch_events().interrupt(stop).interrupt(exit).interrupt(self.ctl.drain_wake.fd());
        if let Some(why) = watch.fallback() {
            self.log.line(&format!("{why}; checking the event log every {} ms instead", crate::watch::FALLBACK_TICK.as_millis()));
        }
        // what the log held when this worker last looked, and the matches not yet handled
        let mut scanned = self.st.cursor_or_create(&self.h.cursor());
        let mut queue: Vec<Event> = Vec::new();
        while !self.ctl.stopped() && !self.ctl.draining() {
            let last = self.st.last_seq();
            if last > scanned {
                queue.extend(self.st.events_since(scanned).into_iter().filter(|e| self.h.wants(e)));
                scanned = last;
            }
            let now = epoch_now();
            if queue.is_empty() || now < retry_at {
                // nothing to run yet: sleep until the log changes, the retry is due (when one
                // is), or a stop. No clock wakes an idle worker.
                let due = (!queue.is_empty()).then(|| Duration::try_from_secs_f64(retry_at - now).unwrap_or(Duration::ZERO));
                watch.wait(due);
                continue;
            }
            // `margin handler run` may have handled some from the foreground
            let at = self.st.cursor(&self.h.cursor()).unwrap_or(0);
            queue.retain(|e| e.seq > at);
            if queue.is_empty() {
                continue;
            }
            let evs = queue.clone();
            let hi = evs[evs.len() - 1].seq;
            let t0 = epoch_now();
            match self.exec(&evs, false, true) {
                Ok(Ran::Stopped) => break,
                Ok(Ran::Done { .. }) => {
                    queue.retain(|e| e.seq > hi);
                    bo = Backoff::default();
                    self.status.attempts = 0;
                    self.set_state("live", None);
                }
                Err(e) => {
                    let stderr = self.last_stderr.lock().unwrap().clone();
                    self.status.attempts += 1;
                    self.status.last_run = now_rfc3339();
                    self.status.last_error = e.clone();
                    if self.status.attempts > self.h.retries {
                        // a poison event must not block every later one: pass over what this
                        // run was given, say so in the log where a handler can react, move on
                        let v = json!({"handler": self.h.name, "error": e, "stderr": stderr, "attempts": self.status.attempts, "skipped": {"from": evs[0].seq, "to": hi, "count": evs.len()}});
                        self.st.handler_failed(&self.h.board, &self.h.by(), v, (&self.h.cursor(), hi));
                        self.log.line(&format!("run: failed: {e}; gave up after {} attempts: passed over {} event{} (seq {} to {hi}) and appended handler.failed", self.status.attempts, evs.len(), if evs.len() == 1 { "" } else { "s" }, evs[0].seq));
                        queue.retain(|x| x.seq > hi);
                        self.status.attempts = 0;
                        self.status.last_outcome = "skipped".into();
                        bo = Backoff::default();
                        self.set_state("live", None);
                        continue;
                    }
                    let d = bo.record(false, epoch_now() - t0, 30.0);
                    self.status.last_outcome = "failed".into();
                    self.log.line(&format!("run: failed: {e}; retry {} of {} in {}", self.status.attempts, self.h.retries, fmt_secs(d)));
                    retry_at = epoch_now() + d;
                    self.set_state("retrying", Some(retry_at));
                }
            }
        }
        let why = self.ctl.reason.lock().unwrap().clone();
        let why = if why.is_empty() { "stopped".to_string() } else { why };
        if why != "restart" {
            self.status.pid = 0;
            self.status.run_started.clear();
            self.set_state(&why, None);
        }
        self.log.line(&format!("stop: {why}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::temp_store;

    pub fn handler(name: &str, on: &[&str], sh: &str) -> Handler {
        Handler {
            name: name.into(),
            cmd: vec!["sh".into(), "-c".into(), sh.into()],
            on: on.iter().map(|s| s.to_string()).collect(),
            board: String::new(),
            owns: String::new(),
            paused: false,
            cwd: std::env::temp_dir().display().to_string(),
            path: String::new(),
            timeout: 20.0,
            retries: 2,
            added: String::new(),
        }
    }

    fn worker(st: &Store, h: &Handler) -> Worker {
        Worker::new(Store::new(st.home.clone(), st.project.clone()), h.clone(), Arc::new(Ctl::default()), false)
    }

    fn emit(st: &Store, name: &str, v: Value) -> u64 {
        st.emit(name, "", "", v, "you", "emit").unwrap().seq
    }

    /// Run the worker loop in a thread until `done` holds (or 15 s), then stop it.
    fn run_until(st: &Store, h: &Handler, done: impl Fn() -> bool) {
        let ctl = Arc::new(Ctl::default());
        let mut w = Worker::new(Store::new(st.home.clone(), st.project.clone()), h.clone(), ctl.clone(), false);
        let t = std::thread::spawn(move || w.run());
        let t0 = epoch_now();
        while !done() && epoch_now() - t0 < 15.0 {
            std::thread::sleep(Duration::from_millis(20));
        }
        ctl.stop("stopped");
        t.join().unwrap();
    }

    #[test]
    fn registry_adds_once_sets_only_what_exists_and_checks() {
        let st = temp_store();
        let h = handler("open", &["*.open"], "true");
        edit_registry(&st, |r| r.add(h.clone())).unwrap();
        assert!(edit_registry(&st, |r| r.add(h.clone())).unwrap_err().contains("exists: change it with margin handler set open"));
        assert!(edit_registry(&st, |r| r.set(handler("nope", &["x"], "true"))).unwrap_err().starts_with("no handler \"nope\""));
        edit_registry(&st, |r| r.set(Handler { on: vec!["reviews.open".into()], ..h.clone() })).unwrap();
        assert_eq!(Registry::load(&st.dir()).get("open").unwrap().on, ["reviews.open"]);
        for (bad, why) in [
            (Handler { on: vec![], ..h.clone() }, "needs the events"),
            (Handler { cmd: vec![], ..h.clone() }, "no command"),
            (Handler { name: "a.b".into(), ..h.clone() }, "bad handler name"),
            (Handler { owns: "pr".into(), ..h.clone() }, "--owns needs the board"),
            (Handler { board: "b".into(), ..h.clone() }, "--board needs the id prefix"),
            (Handler { on: vec!["two words".into()], ..h.clone() }, "bad --on"),
        ] {
            assert!(Registry::default().add(bad).unwrap_err().contains(why), "{why}");
        }
        edit_registry(&st, |r| r.set_paused("open", true)).unwrap();
        assert!(Registry::load(&st.dir()).get("open").unwrap().paused);
        edit_registry(&st, |r| r.remove("open").map(|_| ())).unwrap();
        assert!(Registry::load(&st.dir()).handlers.is_empty());
        // a registry that won't parse is an error, never "no handlers"
        std::fs::write(Registry::path(&st.dir()), "{").unwrap();
        assert!(edit_registry(&st, |r| r.add(h.clone())).unwrap_err().contains("handlers.json"));
        std::fs::remove_dir_all(&st.home).ok();
    }

    /// What `--on` selects: the name by glob, never the handler's own emits, the runner's
    /// `handler.*` events only when asked by that prefix, a passive kind only by its exact name.
    #[test]
    fn a_handler_wants_its_names_and_never_its_own_events() {
        let ev = |kind: &str, by: &str| Event { v: 1, seq: 1, at: String::new(), artifact: String::new(), id: String::new(), kind: kind.into(), value: Value::Null, by: by.into(), via: "emit".into(), rev: 0 };
        let h = handler("h", &["reviews.*", "ci.red"], "true");
        assert!(h.wants(&ev("reviews.open", "you")) && h.wants(&ev("ci.red", "cron")));
        assert!(!h.wants(&ev("ci.green", "you")) && !h.wants(&ev("reviews", "you")));
        assert!(!h.wants(&ev("reviews.open", "handler:h")), "its own emit");
        assert!(h.wants(&ev("reviews.open", "handler:other")));
        let all = handler("all", &["*"], "true");
        assert!(all.wants(&ev("anything", "you")));
        assert!(!all.wants(&ev("handler.failed", "handler:h")) && !all.wants(&ev("seen", "you")) && !all.wants(&ev("fold", "you")));
        assert!(handler("f", &["handler.*"], "true").wants(&ev("handler.failed", "handler:h")));
        assert!(handler("s", &["seen"], "true").wants(&ev("seen", "you")));
        // the pane's view events: by exact name, or by a glob that names their board
        for view in ["reviews.shown", "reviews.hidden", "reviews.detail"] {
            assert!(matches("reviews.*", view) && matches(view, view) && matches("reviews.d*", view) == (view == "reviews.detail"), "{view}");
            assert!(!matches("*", view) && !matches("*.detail", view) && !matches("*.*", view) && !matches("rev*.detail", view) && !matches("ci.*", view), "{view}");
        }
        assert!(matches("*.open", "reviews.open") && !matches("reviews.*", "handler.failed") && !matches("seen.*", "seen"));
    }

    /// The contract, start to finish: the handler gets exactly the matching events since its
    /// cursor on stdin; on exit 0 its component is applied under its ownership, its emit is
    /// appended (and not handed back to it), and its cursor is past the last event given.
    #[test]
    fn a_run_gets_the_matching_events_and_commits_in_one_step() {
        let st = temp_store();
        st.create(crate::doc::Doc::empty("board")).unwrap();
        let seen = st.home.join("seen.jsonl");
        let sh = format!(r#"cat >> {}; printf '%s\n' '{{"type":"text","id":"h-1","text":"handled"}}' '{{"emit":"mine.done","value":{{"n":1}}}}'"#, seen.display());
        let h = Handler { board: "board".into(), owns: "h".into(), ..handler("h", &["mine.*"], &sh) };
        edit_registry(&st, |r| r.add(h.clone())).unwrap();
        emit(&st, "mine.old", json!(0));
        // the cursor is created at the end of the log: history is not replayed
        assert!(pending(&st, &h).is_empty());
        let a = emit(&st, "mine.one", json!(1));
        emit(&st, "other.one", json!(2));
        let b = emit(&st, "mine.two", json!(3));
        assert_eq!(pending(&st, &h).iter().map(|e| e.seq).collect::<Vec<_>>(), [a, b]);
        run_until(&st, &h, || st.cursor("handler:h") == Some(b));
        let got: Vec<Value> = std::fs::read_to_string(&seen).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(got.iter().map(|e| (e["seq"].as_u64().unwrap(), e["kind"].as_str().unwrap().to_string())).collect::<Vec<_>>(), [(a, "mine.one".to_string()), (b, "mine.two".to_string())], "one run, both events, nothing else");
        assert_eq!(got[0]["value"], json!(1));
        let d = st.load("board").unwrap();
        assert_eq!((d.find("h-1").unwrap().s("text"), d.rev, d.owned["handler:h"].rev), ("handled", 1, 1), "in the handler's own file: the layout's rev did not move");
        assert!(st.art("board").join("owned/handler.h.json").exists());
        let done: Vec<Event> = st.events().into_iter().filter(|e| e.kind == "mine.done").collect();
        assert_eq!(done.len(), 1, "its own emit matches its glob and is not handed back to it");
        assert_eq!((done[0].by.as_str(), done[0].via.as_str(), done[0].artifact.as_str(), &done[0].value), ("handler:h", "handler", "board", &json!({"n": 1})));
        assert_eq!(st.cursor("handler:h"), Some(b));
        let s = read_status(&st, "h").unwrap();
        assert_eq!((s.last_outcome.as_str(), s.last_seq, s.state.as_str(), s.attempts), ("ok", b, "stopped", 0));
        // others may not write its ids; it may not write outside them
        let e = st.patch("board", &[Op::Set { id: "h-1".into(), field: "text".into(), value: json!("mine") }], None).unwrap_err();
        assert!(e.contains("belongs to handler `h`"), "{e}");
        std::fs::remove_dir_all(&st.home).ok();
    }

    /// A worker sleeps on the log's watch, with no clock to save it: every event must wake it,
    /// the one appended while it was busy with the last (between its look at the log and its
    /// next wait) included. Each emit here lands the moment the previous run committed, which
    /// is that window.
    #[test]
    fn an_event_emitted_as_the_worker_goes_to_sleep_is_still_handled() {
        let st = temp_store();
        let h = handler("h", &["go"], "cat > /dev/null");
        // one already waiting when the worker starts: before its watch exists
        st.cursor_or_create("handler:h");
        let first = emit(&st, "go", json!(0));
        let ctl = Arc::new(Ctl::default());
        let mut w = Worker::new(Store::new(st.home.clone(), st.project.clone()), h.clone(), ctl.clone(), false);
        let t = std::thread::spawn(move || w.run());
        let handled = |seq: u64| {
            let t0 = Instant::now();
            while st.cursor("handler:h") != Some(seq) && t0.elapsed() < Duration::from_secs(10) {
                std::thread::yield_now();
            }
            st.cursor("handler:h") == Some(seq)
        };
        assert!(handled(first), "the event from before the worker started");
        for n in 1..=6 {
            let seq = emit(&st, "go", json!(n));
            assert!(handled(seq), "event {n} was never handled: the worker slept through it");
        }
        // idle now, and blocked: a stop still ends it at once
        let t0 = Instant::now();
        ctl.stop("stopped");
        t.join().unwrap();
        assert!(t0.elapsed() < Duration::from_millis(500), "a stop wakes a sleeping worker: {:?}", t0.elapsed());
        assert_eq!(read_status(&st, "h").unwrap().runs, 7);
        std::fs::remove_dir_all(&st.home).ok();
    }

    /// A failing handler: nothing applies and the cursor stays; after the retries the batch is
    /// passed over with a `handler.failed` event, and the events behind it are handled.
    #[test]
    fn a_failing_run_keeps_the_cursor_then_is_passed_over() {
        let st = temp_store();
        st.create(crate::doc::Doc::empty("board")).unwrap();
        let tries = st.home.join("tries");
        // fails while the batch holds the poison event; its output must never land
        let sh = format!(r#"in=$(cat); echo x >> {}; echo '{{"type":"text","id":"h-1","text":"half"}}'; case "$in" in *poison*) echo "cannot handle poison" >&2; exit 7;; esac; echo '{{"type":"text","id":"h-2","text":"good"}}'"#, tries.display());
        let h = Handler { board: "board".into(), owns: "h".into(), retries: 1, ..handler("h", &["job"], &sh) };
        st.cursor_or_create("handler:h");
        let bad = emit(&st, "job", json!("poison"));
        // one failed run by hand: Err, nothing written, cursor where it was
        let mut w = worker(&st, &h);
        let evs = pending(&st, &h);
        assert_eq!(w.exec(&evs, false, true).unwrap_err(), "exit 7: cannot handle poison");
        assert_eq!(st.cursor("handler:h"), Some(0));
        assert!(st.load("board").unwrap().find("h-1").is_none(), "a failed run applies nothing");
        std::fs::remove_file(&tries).unwrap();
        // the loop: first run + 1 retry, then it gives up on that batch
        run_until(&st, &h, || st.events().iter().any(|e| e.kind == "handler.failed"));
        assert_eq!(std::fs::read_to_string(&tries).unwrap().lines().count(), 2, "the run and --retries 1 retry");
        let f = st.events().into_iter().find(|e| e.kind == "handler.failed").unwrap();
        assert_eq!((f.by.as_str(), f.via.as_str()), ("handler:h", "handler"));
        assert_eq!(f.value["handler"], "h");
        assert_eq!(f.value["stderr"], "cannot handle poison");
        assert_eq!(f.value["skipped"], json!({"from": bad, "to": bad, "count": 1}));
        assert_eq!(st.cursor("handler:h"), Some(bad), "the cursor moved past the poison event");
        assert!(st.load("board").unwrap().find("h-1").is_none());
        let s = read_status(&st, "h").unwrap();
        assert_eq!((s.last_outcome.as_str(), s.attempts), ("skipped", 0));
        // the next event is handled as if nothing had happened
        let good = emit(&st, "job", json!("fine"));
        run_until(&st, &h, || st.cursor("handler:h") == Some(good));
        assert_eq!(st.load("board").unwrap().find("h-2").unwrap().s("text"), "good");
        let log = std::fs::read_to_string(log_path(&st, "h")).unwrap();
        assert!(log.contains("retry 1 of 1") && log.contains("gave up after 2 attempts") && log.contains("stderr: cannot handle poison"), "{log}");
        std::fs::remove_dir_all(&st.home).ok();
    }

    /// Stopping the worker mid-run (the daemon got SIGTERM) kills the command's whole process
    /// group, applies nothing, leaves the cursor, and is not a failure; the next worker gets
    /// the same events again.
    #[test]
    fn a_stop_mid_run_is_not_a_failure_and_the_events_stay_pending() {
        let st = temp_store();
        let marks = st.home.join("marks");
        let child_pid = st.home.join("child.pid");
        // a child of its own (sleep) that must die with it
        let sh = format!("cat > /dev/null; echo started >> {}; sleep 30 & echo $! > {}; wait", marks.display(), child_pid.display());
        let h = handler("slow", &["go"], &sh);
        st.cursor_or_create("handler:slow");
        let seq = emit(&st, "go", Value::Null);
        run_until(&st, &h, || child_pid.exists() && std::fs::read_to_string(&child_pid).map(|s| s.trim().parse::<i32>().is_ok()).unwrap_or(false));
        let pid: i32 = std::fs::read_to_string(&child_pid).unwrap().trim().parse().unwrap();
        let t0 = epoch_now();
        while unsafe { libc::kill(pid, 0) } == 0 && epoch_now() - t0 < 5.0 {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(unsafe { libc::kill(pid, 0) } != 0, "the handler's own child was stopped with it");
        assert_eq!(st.cursor("handler:slow"), Some(seq - 1), "the cursor did not move");
        let s = read_status(&st, "slow").unwrap();
        assert_eq!((s.state.as_str(), s.last_outcome.as_str(), s.attempts, s.pid, s.run_started.as_str()), ("stopped", "stopped", 0, 0, ""));
        assert!(!st.events().iter().any(|e| e.kind == "handler.failed"));
        assert_eq!(pending(&st, &h).len(), 1, "still pending for the next daemon");
        // a status left by a daemon that died hard is corrected, and the events are given again
        let mut dead = s.clone();
        dead.pid = 999_999;
        dead.run_started = now_rfc3339();
        write_atomic(&status_path(&st, "slow"), serde_json::to_string(&dead).unwrap().as_bytes()).unwrap();
        let quick = Handler { cmd: vec!["sh".into(), "-c".into(), format!("cat > /dev/null; echo again >> {}", marks.display())], ..h.clone() };
        run_until(&st, &quick, || st.cursor("handler:slow") == Some(seq));
        assert_eq!(std::fs::read_to_string(&marks).unwrap(), "started\nagain\n");
        std::fs::remove_dir_all(&st.home).ok();
    }

    /// `--dry-run` prints what a run would commit and writes nothing: no op, no emit, no cursor.
    #[test]
    fn a_dry_run_writes_nothing() {
        let st = temp_store();
        st.create(crate::doc::Doc::empty("board")).unwrap();
        let h = Handler { board: "board".into(), owns: "h".into(), ..handler("h", &["x"], r#"cat >/dev/null; echo '{"type":"text","id":"h-1","text":"t"}'; echo '{"emit":"y"}'"#) };
        st.cursor_or_create("handler:h");
        emit(&st, "x", Value::Null);
        let mut w = Worker::new(Store::new(st.home.clone(), st.project.clone()), h.clone(), Arc::new(Ctl::default()), true);
        assert_eq!(w.exec(&pending(&st, &h), true, true), Ok(Ran::Done { changed: false }));
        assert_eq!((st.load("board").unwrap().rev, st.events().len(), st.cursor("handler:h")), (1, 1, Some(0)));
        assert!(read_status(&st, "h").is_none(), "a foreground run leaves the daemon's status alone");
        std::fs::remove_dir_all(&st.home).ok();
    }

    /// A handler's environment: the registering PATH and the MARGIN_* variables; a timeout kills
    /// the run and counts as a failure.
    #[test]
    fn the_environment_and_the_timeout() {
        let st = temp_store();
        let out = st.home.join("env");
        let h = Handler { path: "/usr/bin:/bin:/registered".into(), ..handler("envy", &["x"], &format!("cat >/dev/null; echo \"$MARGIN_HANDLER|$MARGIN_PROJECT|$MARGIN_HOME|$PATH|${{MARGIN_BIN:+bin}}|${{MARGIN_BOARD:-none}}\" > {}", out.display())) };
        st.cursor_or_create("handler:envy");
        emit(&st, "x", Value::Null);
        let mut w = worker(&st, &h);
        let evs = pending(&st, &h);
        w.exec(&evs, false, true).unwrap();
        assert_eq!(std::fs::read_to_string(&out).unwrap().trim(), format!("envy|proj|{}|/usr/bin:/bin:/registered|bin|none", st.home.display()));
        let slow = Handler { timeout: 0.3, ..handler("slow", &["x"], "sleep 20") };
        let mut w = worker(&st, &slow);
        assert_eq!(w.exec(&evs, false, false).unwrap_err(), "timed out after 0.3s");
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
    }
}
