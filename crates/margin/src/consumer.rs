//! The seam between the core and a consumer of the person's pane events.
//!
//! The core is boards, an event log with cursors, handlers, owners, datasets and the pane. A
//! consumer is something that keeps a board on someone's behalf and reads what the person does in
//! the pane: a chat session of an assistant, for one. The core knows no consumer by name. An
//! adapter implements `Consumer` and installs it once at start-up (`install`); from then on the
//! core asks two things through this module, and nothing else:
//!
//! - who is working on this board (`on_board`): a display name, what its sessions are doing, its
//!   workers, a recap. The scene draws that; a board nobody keeps gets `Presence::default()` and
//!   draws none of it (no live strip, no stop control, no consumer's name anywhere);
//! - what to do as the person acts (`pane`, `review`, `pane_started`): per-pane callbacks the main
//!   loop runs each frame. With no adapter installed there are none, and with no session keeping
//!   any of the pane's boards the loop does not call them.
//!
//! Manners live on the adapter's side of this line, not here: marking the terminal tab when a
//! session needs the person, waking an idle session, never ringing the bell and opening a pane
//! without taking focus are behaviors of an adapter and of the host integration (`host.rs`). The
//! core has no rule about any of them; it only gives an adapter the moments to act in.

use crate::config::Config;
use crate::doc::Doc;
use crate::store::{Activity, State, Store};
use std::sync::OnceLock;

/// The kind of work a worker is doing: not which command, but whether it is still finding its
/// way, writing, checking, or waiting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    /// Reading and searching: before its first change, or finding its way to the next one.
    #[default]
    Planning,
    /// Changing files.
    Coding,
    /// Building and running tests.
    Testing,
    /// Reading diffs, or reading back what it changed.
    Reviewing,
    /// Blocked: a long command, a sleep, a nested worker, a prompt, a background job.
    Waiting,
    Done,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Phase::Planning => "planning",
            Phase::Coding => "coding",
            Phase::Testing => "testing",
            Phase::Reviewing => "reviewing",
            Phase::Waiting => "waiting",
            Phase::Done => "done",
        }
    }
}

/// How far through a plan: `done` of `total` steps, and the step in hand (the one marked doing,
/// else the first not done; empty once all are).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Progress {
    pub done: usize,
    pub total: usize,
    pub current: String,
}

impl Progress {
    /// A bar `width` cells wide, `▰▰▰▱▱`: a started plan shows at least one filled cell, and
    /// only a finished one fills the last.
    pub fn bar(&self, width: usize) -> String {
        let full = if self.total == 0 || width == 0 {
            0
        } else if self.done >= self.total {
            width
        } else {
            let n = (self.done * width + self.total / 2) / self.total;
            n.clamp((self.done > 0) as usize, width.saturating_sub(1))
        };
        "▰".repeat(full) + &"▱".repeat(width - full)
    }
}

/// One worker on a board, as the scene draws it: a row of name, phase, plan progress and elapsed
/// time, and when opened what it said along the way. Plain data: the adapter reads whatever it
/// reads (a transcript, a job table) and fills this in. Times are epoch seconds.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Worker {
    pub id: String,
    /// The row's name.
    pub name: String,
    /// What it was sent to do, shown in full when the row is opened and differs from `name`.
    pub description: String,
    pub finished: bool,
    pub started: Option<f64>,
    pub ended: Option<f64>,
    /// The phase it is in while nothing later in `turns` has come due.
    pub phase: Phase,
    /// What the passing of time alone will make of it, in order: from each time on, that phase
    /// (a call still in flight after half a minute is a wait, with no new data to say so).
    pub turns: Vec<(f64, Phase)>,
    /// Since when it has been blocked, for "waiting 2m" once its phase is `Waiting`.
    pub blocked_since: f64,
    /// What it is blocked on, in a word or two; shown only while it waits. Empty: unknown.
    pub blocked_on: String,
    pub progress: Option<Progress>,
    /// Its own words along the way, oldest first; a finished worker's last note is its result.
    pub notes: Vec<String>,
    /// Its work in a line, for a worker that said nothing along the way.
    pub summary: String,
}

impl Worker {
    pub fn phase(&self, now: f64) -> Phase {
        if self.finished {
            return Phase::Done;
        }
        self.turns.iter().rev().find(|(at, _)| now > *at).map(|(_, p)| *p).unwrap_or(self.phase)
    }

    /// How long it has been waiting, in seconds, when `phase` is `Waiting`.
    pub fn waiting_for(&self, now: f64) -> Option<f64> {
        (self.phase(now) == Phase::Waiting).then(|| (now - self.blocked_since).max(0.0))
    }

    /// What it waits on, when it waits and that is known.
    pub fn waiting_why(&self, now: f64) -> Option<&str> {
        (self.phase(now) == Phase::Waiting && !self.blocked_on.is_empty()).then_some(self.blocked_on.as_str())
    }

    /// How long it has run (or ran), as `2m`.
    pub fn took(&self, now: f64) -> String {
        match (self.started, if self.finished { self.ended } else { Some(now) }) {
            (Some(s), Some(e)) => crate::components::age_short((e - s).max(0.0)),
            _ => String::new(),
        }
    }
}

/// Workers started together (one fan-out).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Group {
    pub agents: Vec<Worker>,
}

impl Group {
    pub fn done(&self) -> usize {
        self.agents.iter().filter(|a| a.finished).count()
    }
}

/// Who is working on a board, for the scene. The default is "nobody": nothing of it is drawn.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Presence {
    /// The consumer's display name, as the pane writes it ("<name> is current", "stop <name>").
    /// Empty when no consumer keeps the board.
    pub name: String,
    /// A live session keeps the board.
    pub linked: bool,
    /// Only ended sessions keep it: when the last one ended (local HH:MM).
    pub ended: Option<String>,
    /// What its sessions are doing, newest first (the live strip, "waiting on you").
    pub activity: Vec<Activity>,
    /// Its latest recap of the work: (text, local time it was written).
    pub recap: Option<(String, String)>,
    /// The heading over the workers' rows.
    pub workers_title: String,
    pub workers: Vec<Group>,
    /// The person's decisions on this board that a live session has not been handed, if any.
    pub undelivered: Option<Undelivered>,
}

/// The person's decisions on a board that its consumer has not been handed. The core asks only
/// this; how a consumer is handed decisions, and how it is known to have been active, is the
/// adapter's to answer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Undelivered {
    /// How many decisions wait.
    pub count: usize,
    /// When the oldest of them was made (epoch seconds).
    pub oldest: f64,
    /// Has the consumer been at work since then, past a moment at which it would have been
    /// handed them? `Some(true)`: yes, so its delivery is broken. `Some(false)`: no, it has
    /// been idle or busy with one thing since, which is normal. `None`: nothing recorded says.
    pub active_since: Option<bool>,
}

/// How long decisions may wait before the pane says anything (seconds).
pub const UNDELIVERED_SECS: f64 = 60.0;

impl Undelivered {
    /// The line for the pane's head, and whether it is a warning: nothing while the decisions
    /// are young or the consumer is merely idle; a warning once it has worked on without them;
    /// a plain count when nothing recorded says either way (no claim about the cause).
    pub fn line(&self, name: &str, wall: f64) -> Option<(String, bool)> {
        if self.count == 0 || name.is_empty() || wall - self.oldest <= UNDELIVERED_SECS {
            return None;
        }
        let n = format!("{} decision{}", self.count, if self.count == 1 { "" } else { "s" });
        match self.active_since {
            Some(true) => Some((format!("{n} not delivered to {name}: its hooks are not running (restart the session, or /hooks)"), true)),
            Some(false) => None,
            None => Some((format!("{n} waiting for {name}"), false)),
        }
    }
    /// When `line` may first have something to say (epoch seconds), for the pane's wake-up.
    pub fn due(&self) -> f64 {
        self.oldest + UNDELIVERED_SECS
    }
}

/// What the pane's main loop hands a consumer each frame.
pub struct Tick<'a> {
    /// The activity of the sessions keeping the pane's boards, newest first.
    pub activity: &'a [Activity],
    /// The pane's clock (seconds since it started) and the wall clock (epoch seconds).
    pub t: f64,
    pub wall: f64,
    pub cfg: &'a Config,
    pub store: &'a Store,
    /// The newest event seq the pane has loaded.
    pub last_seq: u64,
    /// The person is in review mode in some tab.
    pub review: bool,
    /// Every tab's board and the person's decisions on it.
    pub boards: &'a [(&'a Doc, &'a State)],
    /// Is the pane on screen, as far as the host can tell.
    pub visible: &'a dyn Fn() -> Option<bool>,
}

/// A consumer's per-pane state: created when a pane starts, dropped when it leaves.
pub trait PaneWatch {
    /// Called on every frame of a pane that shows a board some session keeps. Returns when the
    /// pane should next wake for the consumer's sake (pane clock), or None to let it sleep.
    fn step(&mut self, tick: &Tick) -> Option<f64>;
}

pub trait Consumer: Sync {
    /// Who keeps `board`. None: no session of this consumer does.
    fn on_board(&self, st: &Store, board: &str) -> Option<Presence>;
    /// A pane is starting: the state it keeps for that pane.
    fn pane(&self) -> Box<dyn PaneWatch>;
    /// A pane starts in this store (before its first frame).
    fn pane_started(&self, st: &Store);
    /// The person turned review mode on or off in `pane` on `board`.
    fn review(&self, st: &Store, pane: &str, board: &str, on: bool);
}

static INSTALLED: OnceLock<&'static dyn Consumer> = OnceLock::new();

/// Plug an adapter in. Once per process; a second call is ignored.
pub fn install(c: &'static dyn Consumer) {
    let _ = INSTALLED.set(c);
}

/// Who keeps `board`: nobody (the default) unless an installed consumer says otherwise.
pub fn on_board(st: &Store, board: &str) -> Presence {
    INSTALLED.get().and_then(|c| c.on_board(st, board)).unwrap_or_default()
}

/// The installed consumer's state for a pane that is starting, if there is a consumer.
pub fn pane() -> Option<Box<dyn PaneWatch>> {
    INSTALLED.get().map(|c| c.pane())
}

pub fn pane_started(st: &Store) {
    if let Some(c) = INSTALLED.get() {
        c.pane_started(st);
    }
}

pub fn review(st: &Store, pane: &str, board: &str, on: bool) {
    if let Some(c) = INSTALLED.get() {
        c.review(st, pane, board, on);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_worker_turns_with_time_alone() {
        let w = Worker { phase: Phase::Coding, turns: vec![(130.0, Phase::Testing), (400.0, Phase::Waiting)], blocked_since: 100.0, blocked_on: "long tests".into(), started: Some(40.0), ..Default::default() };
        assert_eq!((w.phase(120.0), w.phase(131.0), w.phase(401.0)), (Phase::Coding, Phase::Testing, Phase::Waiting));
        assert_eq!((w.waiting_for(131.0), w.waiting_for(460.0)), (None, Some(360.0)));
        assert_eq!((w.waiting_why(131.0), w.waiting_why(460.0)), (None, Some("long tests")));
        assert_eq!(w.took(100.0), "1m");
        let done = Worker { finished: true, started: Some(0.0), ended: Some(45.0), ..w };
        assert_eq!((done.phase(1000.0), done.waiting_for(1000.0), done.took(1000.0).as_str()), (Phase::Done, None, "45s"));
    }

    /// The three states of decisions that wait, and the one where nothing says which it is:
    /// delivered (nothing), waiting on an idle consumer (nothing: that is normal), undelivered
    /// while the consumer worked on (the warning), and unknown (a plain count, no cause named).
    #[test]
    fn undelivered_decisions_warn_only_when_the_consumer_worked_on_without_them() {
        let u = |count, active_since| Undelivered { count, oldest: 1000.0, active_since };
        assert_eq!(u(0, Some(true)).line("Otto", 2000.0), None, "delivered");
        assert_eq!(u(2, Some(false)).line("Otto", 2000.0), None, "waiting, and the consumer is idle");
        assert_eq!(u(2, Some(true)).line("Otto", 1000.0 + UNDELIVERED_SECS), None, "young: it may yet be handed them");
        assert_eq!(u(2, Some(true)).line("Otto", 1061.0), Some(("2 decisions not delivered to Otto: its hooks are not running (restart the session, or /hooks)".to_string(), true)));
        assert_eq!(u(1, None).line("Otto", 1061.0), Some(("1 decision waiting for Otto".to_string(), false)), "no signal: no claim about hooks");
        assert_eq!(u(2, Some(true)).line("", 2000.0), None, "nobody keeps the board");
        assert_eq!(u(2, None).due(), 1060.0);
    }

    /// The core's source files: everything but the adapter, `main.rs` (which dispatches to it),
    /// `help.rs` (the embedded reference, with its topics about Claude Code), `demo.rs` (a sample
    /// of a session's plan) and `app.rs` (shared with another change in flight: its code reaches
    /// the adapter through this module only; two of its tests link a session to draw one).
    const CORE: &[&str] = &[
        "store", "doc", "handler", "owner", "data", "daemon", "doctor", "tabs", "detail", "components", "scene", "select", "graph", "mdoc", "theme", "toast", "util", "who", "example",
        "consumer", "host", "zj", "config", "diffblock",
    ];

    /// Core files that still name the adapter, with why: each is a file this change did not own.
    /// In an exempt file the word itself is not counted (its comments, messages and fixtures
    /// are another change's, some of it in flight), but the lines that reach into the adapter's
    /// code are, against the ceiling given: that number may only go down.
    const EXEMPT: &[(&str, usize, &str)] = &[
        // TODO: `Store::stop` collects the board's linked sessions through `link::all`, and three
        // lines of its tests describe events through `hook::describe` (both by the root aliases
        // in main.rs); the rest is comments, test names and the `claude` author in fixtures. The
        // inversion: a `consumer` call that names the sessions keeping a board.
        ("store", 4, "stop() reads the adapter's links; comments, tests and fixtures say Claude"),
        // TODO: the schema's values `claude` (a tone, a todo's owner, a decided row's `by`) and
        // the notes on them are stored-document vocabulary; renaming them is a schema change.
        ("doc", 0, "schema values and notes: the tone, owner and by named claude"),
        // TODO: `margin doctor` lists the adapter's dead rewake and review marks by calling
        // `wake::dead_marks` / `dead_review_marks`; the inversion is a `consumer` leftovers call.
        ("doctor", 10, "calls the adapter's wake module for its leftover marks"),
        // TODO: comments, and one refusal that says a status is "Claude's to change".
        ("select", 0, "comments and one message"),
        ("owner", 0, "a test's expected author, claude"),
        ("tabs", 0, "a comment"),
        ("mdoc", 0, "a comment"),
        ("util", 0, "a comment"),
        ("diffblock", 0, "the parked diff block's \"Claude reads it next turn\", drawn only on a linked board"),
        ("vellum/parts", 0, "a comment"),
    ];

    /// A line may carry the adapter's name where a stored document does: it says so with this.
    const MARKER: &str = "stored-name:";

    /// The lines of a source text that reach into the adapter's code, by its module or by the
    /// root aliases of its modules.
    fn reaching(text: &str) -> Vec<String> {
        let reach = ["crate::claude", "crate::hook::", "crate::wake::", "crate::rewake::", "crate::link::", "crate::agents::", "crate::init::", "crate::recap::", "crate::attention::"];
        text.lines().filter(|l| reach.iter().any(|r| l.contains(r))).map(|l| l.trim().to_string()).collect()
    }

    /// The lines that name the adapter at all: in code, a comment, a message or a fixture.
    fn naming(text: &str) -> Vec<String> {
        text.lines().filter(|l| !l.contains(MARKER) && l.to_lowercase().contains("claude")).map(|l| l.trim().to_string()).collect()
    }

    /// The boundary: no core source file names Claude or imports from the adapter, in code,
    /// comments or tests. A line that must carry the name because stored documents do is marked
    /// `stored-name:`; a file this change could not clean is exempt above, with its reason and a
    /// ceiling on the lines that reach into the adapter's code. The scan reads the sources themselves, so this file spells the name in pieces.
    #[test]
    fn no_core_file_names_the_adapter() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let vellum = src.join("../../vellum/src");
        let mut files: Vec<(String, std::path::PathBuf)> = CORE.iter().map(|f| (f.to_string(), src.join(format!("{f}.rs")))).collect();
        let mut stack = vec![vellum.clone()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().and_then(|x| x.to_str()) == Some("rs") {
                    let rel = p.strip_prefix(&vellum).unwrap().with_extension("");
                    files.push((format!("vellum/{}", rel.display()), p));
                }
            }
        }
        assert!(files.len() > CORE.len() + 5, "vellum's sources were not found");
        let mut bad = Vec::new();
        for (name, path) in &files {
            let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            // this file's own test names what it scans for
            let text = if name == "consumer" { text.split("#[cfg(test)]").next().unwrap().to_string() } else { text };
            let exempt = EXEMPT.iter().find(|(f, _, _)| f == name);
            let reach = reaching(&text);
            let allowed = exempt.map(|(_, n, _)| *n).unwrap_or(0);
            if reach.len() > allowed {
                bad.push(format!("{name} reaches into the adapter on {} line(s), {allowed} allowed\n    {}", reach.len(), reach.join("\n    ")));
            }
            let names = naming(&text);
            if exempt.is_none() && !names.is_empty() {
                bad.push(format!("{name} names the adapter on {} line(s)\n    {}", names.len(), names.join("\n    ")));
            }
        }
        assert!(bad.is_empty(), "core files name the adapter:\n{}", bad.join("\n"));
        for (f, _, why) in EXEMPT {
            assert!(files.iter().any(|(n, _)| n == f) && !why.is_empty(), "{f} is exempt but not scanned");
        }
        // the scan sees what it should
        let sample = "let a = 1;\n// Claude reads it\nuse crate::claude::link;\nlet l = crate::link::all(st);\n\"claude\" => blue, // stored-name: a tone\n";
        assert_eq!((naming(sample).len(), reaching(sample).len()), (2, 2));
    }

    /// A board nobody keeps: the default presence, which the scene draws nothing from.
    #[test]
    fn nobody_is_the_default() {
        let p = Presence::default();
        assert!(p.name.is_empty() && !p.linked && p.activity.is_empty() && p.workers.is_empty() && p.recap.is_none());
    }
}
