//! The work overview's subagent rows (ag1): what each subagent of this pane's session is for,
//! what it's doing, what it has done, and what it found, read from the files Claude Code keeps per
//! subagent. Nothing here writes the document: the pane builds the rows on every reload, so any
//! number of subagents never contend for doc.json.
//!
//! Claude Code keeps, beside a session's transcript `<session>.jsonl`, a `<session>/subagents/`
//! directory with `agent-<id>.meta.json` (description, agentType, finished) and the subagent's
//! transcript `agent-<id>.jsonl`. Every line of that transcript carries the `promptId` of the
//! parent turn that spawned it, so subagents launched together (a fan-out: a PR review's
//! reviewers) share one, which is how they're grouped.

//!
//! Two things are derived on top of that for the one-line view (ag-phase): a `Phase` (planning,
//! coding, testing, reviewing, waiting, done) from the kinds of its recent tool calls, and its
//! plan progress from the plan section it keeps in the pane. Both come from what is already on
//! disk, so they cost the subagent no tokens.

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::io::{Read as _, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Agent {
    pub id: String,
    pub description: String,
    pub kind: String,
    pub finished: bool,
    /// The parent turn that spawned it: the fan-out key.
    pub prompt: String,
    /// RFC 3339 of its first line.
    pub started: String,
    /// Tool calls by name.
    pub tools: BTreeMap<String, u32>,
    /// Its latest words: what it said it's doing (while running) or its result (once finished).
    pub said: String,
    /// Its steps in order: a tool call each, labelled the way the live strip labels them, with a
    /// run of the same tool folded into one (`Read ×4`). Kept for counts; the pane shows `notes`.
    pub steps: Vec<Step>,
    /// What it said along the way, in its own words: the first line of each text it wrote. The
    /// tracker's chain (sulin wants the high level, not the tool calls, 2026-11-08).
    pub notes: Vec<String>,
    /// RFC 3339 of its last line: with `started`, how long it ran.
    pub ended: String,
    /// Its plan in the pane, `(artifact, section id)`: the first section it created with
    /// `margin add <artifact> - section <id>`. Only where the plan is; the counts are read from
    /// the document each time (`plan_progress`), because plans change as the work goes.
    pub plan: Option<(String, String)>,
    /// What `phase` is derived from: its recent calls by kind, and what is still in flight.
    pub track: Track,
    /// How far through its plan it was when the pane last loaded its agents (`for_pane`): reading
    /// it takes a document from the store, so it is read per reload and never per frame.
    pub progress: Option<Progress>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Step {
    pub tool: String,
    pub label: String,
    pub n: u32,
}

impl Agent {
    pub fn tool_count(&self) -> u32 {
        self.tools.values().sum()
    }
}

/// A fan-out: the subagents one turn spawned, oldest first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Group {
    pub prompt: String,
    pub agents: Vec<Agent>,
}

impl Group {
    pub fn done(&self) -> usize {
        self.agents.iter().filter(|a| a.finished).count()
    }
}

/// The subagents directory beside a session's transcript.
pub fn dir_for(transcript: &Path) -> PathBuf {
    transcript.with_extension("").join("subagents")
}

/// Read one subagent from its meta file and transcript.
///
/// The transcript is read incrementally: a pane re-reads every subagent whenever any of them
/// writes, and transcripts run to megabytes, so the parsed agent is kept per path with the byte
/// offset it was parsed to, and a later read parses only the bytes appended since (`CACHE`).
pub fn read(meta: &Path) -> Option<Agent> {
    let m: Value = serde_json::from_slice(&std::fs::read(meta).ok()?).ok()?;
    let id = meta.file_name()?.to_str()?.trim_start_matches("agent-").trim_end_matches(".meta.json").to_string();
    let head = Agent {
        id: id.clone(),
        description: m["description"].as_str().unwrap_or("").to_string(),
        kind: m["agentType"].as_str().unwrap_or("").to_string(),
        finished: m["finished"].as_bool().unwrap_or(false),
        ..Default::default()
    };
    let path = meta.with_file_name(format!("agent-{id}.jsonl"));
    let (len, mtime) = std::fs::metadata(&path).map(|md| (md.len(), md.modified().ok())).unwrap_or((0, None));
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let c = cache.get_or_insert_with(HashMap::new).entry(path.clone()).or_default();
    // Claude Code only appends, so a longer file continues the parse; anything else (shorter, or
    // the same length with a new mtime) is a different file under the same name: start over
    if len < c.offset || (len == c.offset && mtime != c.mtime && c.offset > 0) {
        *c = Cached::default();
    }
    if c.offset == 0 {
        c.agent = head.clone();
    }
    if len > c.offset {
        let mut buf = Vec::new();
        let got = std::fs::File::open(&path).and_then(|mut f| {
            f.seek(SeekFrom::Start(c.offset))?;
            f.take(len - c.offset).read_to_end(&mut buf)
        });
        if got.is_ok() {
            // whole lines only: a line still being written is left for the next read, unless it
            // already parses (a transcript whose last line has no newline)
            let whole = buf.iter().rposition(|&b| b == b'\n').map(|i| i + 1).unwrap_or(0);
            let take = if serde_json::from_slice::<Value>(&buf[whole..]).is_ok() { buf.len() } else { whole };
            c.agent.ingest(&String::from_utf8_lossy(&buf[..take]));
            c.offset += take as u64;
        }
    }
    c.mtime = mtime;
    let mut a = c.agent.clone();
    (a.id, a.description, a.kind, a.finished) = (head.id, head.description, head.kind, head.finished);
    Some(a)
}

#[derive(Default)]
struct Cached {
    /// Bytes of the transcript already parsed into `agent`.
    offset: u64,
    mtime: Option<std::time::SystemTime>,
    agent: Agent,
}

static CACHE: Mutex<Option<HashMap<PathBuf, Cached>>> = Mutex::new(None);
/// The subagents of a session, grouped by the turn that spawned them, newest group first.
/// How long a finished agent stays listed in the pane.
pub const KEEP_FINISHED: f64 = 5.0 * 60.0;

/// Finished groups older than `keep` seconds are left out.
pub fn groups(dir: &Path, now: f64, keep: f64) -> Vec<Group> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut agents: Vec<Agent> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.to_str().map(|s| s.ends_with(".meta.json")).unwrap_or(false))
        .filter_map(|p| read(&p))
        .collect();
    agents.sort_by(|a, b| a.started.cmp(&b.started));
    let mut out: Vec<Group> = Vec::new();
    for a in agents {
        match out.iter_mut().find(|g| g.prompt == a.prompt && !a.prompt.is_empty()) {
            Some(g) => g.agents.push(a),
            None => out.push(Group { prompt: a.prompt.clone(), agents: vec![a] }),
        }
    }
    let age = |g: &Group| g.agents.iter().filter_map(|a| crate::util::parse_rfc3339(&a.started)).fold(0.0f64, f64::max);
    // a finished group leaves `keep` seconds after its last agent ENDED: long enough to see how
    // it went, short enough that the block lists what is running, not what once ran
    let ended = |g: &Group| g.agents.iter().filter_map(|a| crate::util::parse_rfc3339(&a.ended)).fold(0.0f64, f64::max);
    out.retain(|g| g.done() < g.agents.len() || now - ended(g).max(age(g)) < keep);
    out.sort_by(|a, b| age(b).partial_cmp(&age(a)).unwrap_or(std::cmp::Ordering::Equal));
    out
}

/// The groups a pane on `art` shows: the subagents of its linked sessions.
pub fn for_pane(st: &crate::store::Store, art: &str) -> Vec<Group> {
    let now = crate::util::epoch_now();
    let mut out: Vec<Group> = crate::link::all(st)
        .into_iter()
        .filter(|l| l.art == art && !l.transcript.is_empty())
        .flat_map(|l| groups(&dir_for(Path::new(&l.transcript)), now, KEEP_FINISHED))
        .collect();
    // each agent's plan progress, with every artifact that holds a plan loaded once
    let mut docs: HashMap<String, Option<(crate::doc::Doc, crate::store::State)>> = HashMap::new();
    for a in out.iter_mut().flat_map(|g| g.agents.iter_mut()) {
        let Some((plan_art, _)) = a.plan.clone() else { continue };
        let held = docs.entry(plan_art.clone()).or_insert_with(|| st.load(&plan_art).ok().map(|d| (d, st.state(&plan_art))));
        a.progress = held.as_ref().and_then(|(d, s)| a.plan_progress(d, s));
    }
    out
}

/// An agent parsed from transcript lines, for tests elsewhere that need a real phase.
#[cfg(test)]
pub fn parsed_for_test(description: &str, transcript: &str) -> Agent {
    let mut a = Agent { description: description.into(), ..Default::default() };
    a.ingest(transcript);
    a
}

impl Agent {
    /// Its work in a line, when it said nothing along the way: what kind of work, not which
    /// commands (`searched the web 22× · read 21 pages · wrote a report`).
    pub fn summary(&self) -> String {
        let n = |k: &str| self.tools.get(k).copied().unwrap_or(0);
        let mut parts = Vec::new();
        let mut add = |c: u32, one: &str, many: String| {
            if c == 1 {
                parts.push(one.to_string());
            } else if c > 1 {
                parts.push(many);
            }
        };
        add(n("WebSearch"), "searched the web", format!("searched the web {}×", n("WebSearch")));
        add(n("WebFetch"), "read a page", format!("read {} pages", n("WebFetch")));
        add(n("Read") + n("Grep") + n("Glob"), "looked at the code", format!("looked at the code {}×", n("Read") + n("Grep") + n("Glob")));
        add(n("Edit") + n("Write") + n("MultiEdit"), "changed a file", format!("changed files {}×", n("Edit") + n("Write") + n("MultiEdit")));
        add(n("Bash"), "ran a command", format!("ran {} commands", n("Bash")));
        parts.join(" · ")
    }

    /// How long it has run (or ran), as `2m`.
    pub fn took(&self, now: f64) -> String {
        let s = crate::util::parse_rfc3339(&self.started);
        let e = if self.finished { crate::util::parse_rfc3339(&self.ended) } else { Some(now) };
        match (s, e) {
            (Some(s), Some(e)) => crate::components::age_short((e - s).max(0.0)),
            _ => String::new(),
        }
    }
}

impl Agent {
    /// Fold more transcript lines into the agent. `description` and `kind` must already be set:
    /// they decide how a reviewer's reading counts.
    fn ingest(&mut self, text: &str) {
        for line in text.lines() {
            // most of a transcript's bytes are request records (`api-request-blob`), which carry
            // nothing shown here: a long line that names neither of the two types wanted is
            // skipped without parsing it
            if line.len() > 4096 && !line.contains(r#""type":"assistant""#) && !line.contains(r#""type":"user""#) {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
            let ty = v["type"].as_str().unwrap_or("");
            if self.prompt.is_empty() {
                self.prompt = v["promptId"].as_str().unwrap_or("").to_string();
            }
            if let Some(t) = v["timestamp"].as_str() {
                if self.started.is_empty() {
                    self.started = t.to_string();
                }
                self.ended = t.to_string();
            }
            if ty != "assistant" && ty != "user" {
                continue;
            }
            let at = v["timestamp"].as_str().and_then(crate::util::parse_rfc3339).unwrap_or(self.track.last);
            self.track.last = at;
            let content = v["message"]["content"].as_array();
            if ty == "user" {
                self.track.idle_turn = false;
                for c in content.into_iter().flatten().filter(|c| c["type"] == "tool_result") {
                    let id = c["tool_use_id"].as_str().unwrap_or("");
                    self.track.pending.retain(|p| p.id != id);
                }
                continue;
            }
            let mut called = false;
            for c in content.into_iter().flatten() {
                match c["type"].as_str() {
                    Some("tool_use") => {
                        called = true;
                        let name = c["name"].as_str().unwrap_or("?").to_string();
                        *self.tools.entry(name.clone()).or_default() += 1;
                        let label = crate::hook::label(&name, &c["input"]);
                        match self.steps.last_mut() {
                            // a run of one tool is one step: `Read ×4`, keeping the latest label
                            Some(last) if last.tool == name => {
                                last.n += 1;
                                if !label.is_empty() {
                                    last.label = label;
                                }
                            }
                            _ => self.steps.push(Step { tool: name.clone(), label, n: 1 }),
                        }
                        if self.plan.is_none() && name == "Bash" {
                            self.plan = plan_in(c["input"]["command"].as_str().unwrap_or(""));
                        }
                        let call = classify(&name, &c["input"]);
                        let reviewer = self.reviewer();
                        self.track.note(c["id"].as_str().unwrap_or(""), at, call, reviewer);
                    }
                    Some("text") => {
                        let s = c["text"].as_str().unwrap_or("").trim();
                        if !s.is_empty() {
                            self.said = s.to_string();
                            if let Some(first) = s.lines().map(|l| l.trim().trim_start_matches(['#', '-', '*', ' ']).replace("**", "")).find(|l| !l.is_empty()) {
                                self.notes.push(first);
                            }
                        }
                    }
                    _ => {}
                }
            }
            // a turn that ended without calling anything: it is done, or parked until a
            // background job or a message wakes it
            self.track.idle_turn = !called && v["message"]["stop_reason"] == "end_turn";
        }
    }

    /// An agent sent to review (or audit) reads as its work, where a builder reads to plan.
    fn reviewer(&self) -> bool {
        let d = format!("{} {}", self.description, self.kind).to_lowercase();
        d.contains("review") || d.contains("audit")
    }

    /// What kind of work it is doing now. See `Track` for how flicker is kept out.
    pub fn phase(&self, now: f64) -> Phase {
        if self.finished {
            return Phase::Done;
        }
        let t = &self.track;
        // the oldest call still in flight decides: that is what it is blocked behind
        if let Some(p) = t.pending.iter().min_by(|a, b| a.at.total_cmp(&b.at)) {
            let age = now - p.at;
            if age > WAIT_SECS {
                // a build or a test run that takes a few minutes is still testing; a sleep, a
                // watch, a nested agent, a permission prompt or anything else that long is waiting
                return if p.test && !p.waits && age < TEST_SECS { Phase::Testing } else { Phase::Waiting };
            }
        } else if t.last > 0.0 {
            let quiet = now - t.last;
            // nothing in flight and quiet: while it composes its next call a transcript is
            // silent for minutes (3m22s measured before a large Write), so only a parked turn
            // counts after WAIT_SECS; otherwise it takes STALL_SECS of silence
            if (t.idle_turn && quiet > WAIT_SECS) || quiet > STALL_SECS {
                return Phase::Waiting;
            }
        }
        t.phase
    }

    /// How long it has been waiting, in seconds, when `phase` is `Waiting`.
    pub fn waiting_for(&self, now: f64) -> Option<f64> {
        if self.phase(now) != Phase::Waiting {
            return None;
        }
        let since = self.track.pending.iter().map(|p| p.at).min_by(|a, b| a.total_cmp(b)).unwrap_or(self.track.last);
        Some((now - since).max(0.0))
    }

    /// What it is waiting on, in a word or two, when `phase` is `Waiting` and the transcript's
    /// tail says: the oldest call in flight is a retry loop around a build or a test run
    /// (`retrying tests`), a nested agent (`subagent`), a sleep (`sleep`), or a build or test
    /// run past `TEST_SECS` (`long build`, `long tests`); with nothing in flight, a turn that
    /// ended (`turn ended`). `None` for anything else (a permission prompt and a slow plain
    /// command look the same from here).
    pub fn waiting_why(&self, now: f64) -> Option<&'static str> {
        if self.phase(now) != Phase::Waiting {
            return None;
        }
        let t = &self.track;
        match t.pending.iter().min_by(|a, b| a.at.total_cmp(&b.at)) {
            Some(p) if !p.why.is_empty() => Some(p.why),
            Some(p) if p.test => Some(p.long),
            Some(_) => None,
            None => t.idle_turn.then_some("turn ended"),
        }
    }

    /// The row's name: the description it was started with, which is written to be read. Its
    /// plan section's id (`ag-new` once showed as a row named "new") is only the fallback.
    pub fn who(&self) -> String {
        if !self.description.trim().is_empty() {
            return self.description.clone();
        }
        match &self.plan {
            Some((_, sec)) => sec.strip_prefix("ag-").unwrap_or(sec).to_string(),
            None => String::new(),
        }
    }

    /// How far through its plan it is: the todos under its plan section in `doc`, counted now.
    /// `None` when it keeps no plan, when `doc` is another artifact, or when the section holds no
    /// todos (yet): the row then shows no bar.
    pub fn plan_progress(&self, doc: &crate::doc::Doc, state: &crate::store::State) -> Option<Progress> {
        let (art, sec) = self.plan.as_ref()?;
        if &doc.id != art {
            return None;
        }
        let mut p = Progress::default();
        let mut next = None;
        crate::doc::walk(&doc.find(sec)?.children, &mut |b, _, _| {
            if b.kind != "todo" {
                return;
            }
            p.total += 1;
            match crate::components::status_of(state, b).as_str() {
                // a skipped step is behind it too, the way the stop hook counts a plan finished
                "done" | "skipped" => p.done += 1,
                "doing" if p.current.is_empty() => p.current = b.s("text").to_string(),
                _ if next.is_none() => next = Some(b.s("text").to_string()),
                _ => {}
            }
        });
        if p.current.is_empty() {
            p.current = next.unwrap_or_default();
        }
        (p.total > 0).then_some(p)
    }

    /// `plan_progress` for a plan in any artifact of the store, loading it. For the pane's own
    /// artifact prefer `plan_progress` with the document already in hand.
    pub fn plan_progress_in(&self, st: &crate::store::Store) -> Option<Progress> {
        let (art, _) = self.plan.as_ref()?;
        self.plan_progress(&st.load(art).ok()?, &st.state(art))
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

/// The kind of work a subagent is doing, at the level sulin asked for (2026-11-10): not which
/// command, but whether it is still finding its way, writing, checking, or waiting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    /// Reading and searching: before its first change, or finding its way to the next one.
    #[default]
    Planning,
    /// Changing files.
    Coding,
    /// Building and running tests.
    Testing,
    /// Reading diffs, or reading back what it changed; all of a review agent's reading.
    Reviewing,
    /// Blocked: a long command, a sleep, a nested agent, a permission prompt, a background job.
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

/// A call in flight longer than this is something the agent is waiting on.
pub const WAIT_SECS: f64 = 30.0;
/// A build or test run counts as testing up to this long, then as waiting.
pub const TEST_SECS: f64 = 300.0;
/// Silence with nothing in flight counts as waiting after this (it is composing until then).
pub const STALL_SECS: f64 = 600.0;
/// How many recent calls vote on the phase.
const WINDOW: usize = 8;

/// The running state `Agent::phase` reads, updated a call at a time as the transcript is read.
///
/// Flicker is kept out in two ways. Calls that say nothing about the phase (ticking the plan,
/// `mkdir`, `date`) do not vote at all. And the phase is sticky: a new call changes it only when
/// the two newest votes agree with each other, or when one phase holds a strict plurality of the
/// last `WINDOW` votes. So a single odd call (one build in the middle of editing, one file read
/// between tests) never flips it, an edit/build/edit/build loop stays coding, and two calls of
/// a new kind in a row move it at once.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Track {
    phase: Phase,
    votes: VecDeque<Phase>,
    pending: Vec<Pending>,
    /// Epoch of its last assistant or user line.
    last: f64,
    idle_turn: bool,
    /// File names it has changed: reading one of them back is reviewing.
    edited: BTreeSet<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Pending {
    id: String,
    at: f64,
    test: bool,
    waits: bool,
    /// Why it blocks, where the call itself says (`Call::why`), and what a long run of it is.
    why: &'static str,
    long: &'static str,
}

impl Track {
    fn note(&mut self, id: &str, at: f64, call: Call, reviewer: bool) {
        self.pending.push(Pending { id: id.to_string(), at, test: call.class == Class::Test, waits: call.waits, why: call.why, long: call.long });
        let vote = match call.class {
            Class::None => return,
            Class::Edit => {
                self.edited.extend(call.paths);
                Phase::Coding
            }
            Class::Test => Phase::Testing,
            Class::Diff => Phase::Reviewing,
            Class::Read if reviewer || call.paths.iter().any(|p| self.edited.contains(p)) || self.edited.iter().any(|e| call.text.contains(e.as_str())) => Phase::Reviewing,
            Class::Read => Phase::Planning,
        };
        let prev = self.votes.back().copied();
        self.votes.push_back(vote);
        if self.votes.len() > WINDOW {
            self.votes.pop_front();
        }
        if self.votes.len() == 1 || prev == Some(vote) {
            self.phase = vote;
            return;
        }
        let count = |p: Phase| self.votes.iter().filter(|v| **v == p).count();
        let mut tally: Vec<(usize, Phase)> = [Phase::Planning, Phase::Coding, Phase::Testing, Phase::Reviewing].into_iter().map(|p| (count(p), p)).collect();
        tally.sort_by(|a, b| b.0.cmp(&a.0));
        if tally[0].0 > tally[1].0 {
            self.phase = tally[0].1;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Class {
    /// Says nothing about the phase.
    None,
    Read,
    Diff,
    Edit,
    Test,
}

/// One tool call, as the phase sees it.
#[derive(Debug)]
struct Call {
    class: Class,
    /// It blocks on purpose: a sleep, a watch, a nested agent.
    waits: bool,
    /// File names it touches (an edit's targets, a Read's file).
    paths: Vec<String>,
    /// A shell command's text, to spot a file it changed being read back.
    text: String,
    /// Why it blocks, when the call says so itself: `retrying tests` (a loop around a build or
    /// a test run), `sleep`, `subagent`. Empty otherwise.
    why: &'static str,
    /// What a build or a test run is called once it has run long: `long build`, `long tests`.
    long: &'static str,
}

fn base(path: &str) -> String {
    path.rsplit('/').next().unwrap_or("").to_string()
}

fn classify(tool: &str, input: &Value) -> Call {
    let file = |k: &str| input[k].as_str().filter(|s| !s.is_empty()).map(base).into_iter().collect::<Vec<_>>();
    let call = |class, waits, paths| Call { class, waits, paths, text: String::new(), why: "", long: "" };
    match tool {
        "Edit" | "Write" | "MultiEdit" => call(Class::Edit, false, file("file_path")),
        "NotebookEdit" => call(Class::Edit, false, file("notebook_path")),
        "Read" => call(Class::Read, false, file("file_path")),
        "Grep" | "Glob" | "LSP" | "WebSearch" | "WebFetch" => call(Class::Read, false, Vec::new()),
        "Agent" | "Task" => Call { why: "subagent", ..call(Class::None, true, Vec::new()) },
        "Monitor" | "TaskOutput" | "BashOutput" => call(Class::None, true, Vec::new()),
        "Bash" => shell(input["command"].as_str().unwrap_or("")),
        _ => call(Class::None, false, Vec::new()),
    }
}

/// Classify a shell command by the commands it runs, not by the words in it: it is split into
/// its commands (`;`, `&&`, `||`, newlines; only the head of a pipeline counts, so `| tail` is
/// not a read), each is classed by its first word, and the strongest class wins (a test over an
/// edit over a diff over a read). Subagents here change files mostly from the shell (268 Bash
/// calls to 8 Write across this session's 14), so `python3 - <<EOF … write(` and `cat > f <<EOF`
/// have to read as edits.
fn shell(cmd: &str) -> Call {
    let (outside, bodies) = strip_heredocs(cmd);
    let mut class = Class::None;
    let mut waits = false;
    let mut looped = false;
    for seg in segments(&outside) {
        looped |= matches!(seg.split_whitespace().next().map(|w| w.trim_start_matches(['(', '{'])), Some("until" | "while" | "for"));
        let words: Vec<&str> = head_words(&seg);
        let Some(first) = words.first() else { continue };
        let w0 = first.rsplit('/').next().unwrap_or("");
        let sub = words.iter().skip(1).find(|w| !w.starts_with(['-', '+'])).copied().unwrap_or("");
        let has = |set: &[&str]| words.iter().skip(1).any(|w| set.contains(w));
        let c = match w0 {
            _ if first.starts_with("tests/") || first.contains("/tests/") => Class::Test,
            "cargo" if ["test", "build", "check", "clippy", "run", "nextest", "bench", "t", "b", "c", "r"].contains(&sub) => Class::Test,
            "npm" | "pnpm" | "yarn" | "bun" if has(&["test", "build", "lint", "typecheck", "check"]) => Class::Test,
            "go" if ["test", "build", "vet"].contains(&sub) => Class::Test,
            "deno" if ["test", "check", "lint"].contains(&sub) => Class::Test,
            "nix" if has(&["build", "check"]) => Class::Test,
            "pytest" | "make" | "ninja" | "cmake" | "ctest" | "tsc" | "jest" | "vitest" | "just" | "tox" => Class::Test,
            "python" | "python3" | "bash" | "sh" | "node" | "deno" => {
                if seg.contains("<<") {
                    // a script on stdin: an edit when it writes or rewrites (agents here patch
                    // files with `s.replace(a, b)` scripts, some through an `edit(path, …)` helper)
                    if ["write(", "write_text(", "replace(", "edit(", "sub("].iter().any(|k| bodies.contains(k)) { Class::Edit } else { Class::None }
                } else if words.iter().skip(1).any(|w| w.contains("test")) {
                    Class::Test
                } else {
                    Class::None
                }
            }
            "jj" | "git" if ["diff", "show", "status", "st", "log", "interdiff", "evolog"].contains(&sub) => Class::Diff,
            "sed" | "perl" if words.iter().skip(1).any(|w| w.starts_with("-i") || w.starts_with("-pi")) => Class::Edit,
            "tee" | "patch" => Class::Edit,
            "cat" | "echo" | "printf" if redirects(&seg) => Class::Edit,
            "cat" | "sed" | "grep" | "rg" | "ugrep" | "ls" | "head" | "tail" | "wc" | "find" | "fd" | "jq" | "awk" | "cut" | "tree" | "stat" | "bat" | "nl" => Class::Read,
            "sleep" => {
                let n = sub.trim_end_matches(['s', 'm', 'h']).parse::<f64>().unwrap_or(0.0) * if sub.ends_with('m') { 60.0 } else if sub.ends_with('h') { 3600.0 } else { 1.0 };
                waits |= n >= 20.0;
                Class::None
            }
            _ => Class::None,
        };
        class = class.max(c);
    }
    let paths = if class == Class::Edit {
        // the files an edit names: path-like words with an extension, anywhere in the command
        cmd.split(|c: char| !(c.is_alphanumeric() || "_-./".contains(c)))
            .filter(|t| t.contains('/'))
            .map(base)
            .filter(|b| b.rsplit_once('.').map(|(n, e)| !n.is_empty() && (1..=5).contains(&e.len()) && e.chars().all(|c| c.is_alphanumeric())).unwrap_or(false))
            .collect()
    } else {
        Vec::new()
    };
    let why = if looped && class == Class::Test {
        "retrying tests"
    } else if waits {
        "sleep"
    } else {
        ""
    };
    let long = if outside.contains("test") { "long tests" } else { "long build" };
    Call { class, waits, paths, text: outside, why, long }
}

/// A command without its here-document bodies, and the bodies.
fn strip_heredocs(cmd: &str) -> (String, String) {
    let (mut outside, mut bodies) = (String::new(), String::new());
    let mut tag: Option<String> = None;
    for line in cmd.lines() {
        if let Some(t) = &tag {
            if line.trim() == t {
                tag = None;
            } else {
                bodies.push_str(line);
                bodies.push('\n');
            }
            continue;
        }
        outside.push_str(line);
        outside.push('\n');
        if let Some(i) = line.find("<<").filter(|i| !line[*i..].starts_with("<<<")) {
            let word: String = line[i + 2..].trim_start_matches(['-', '~', ' ', '\'', '"', '\\']).chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            tag = Some(word).filter(|w| !w.is_empty());
        }
    }
    (outside, bodies)
}

/// The commands of a shell line: split at `;`, `&&`, `||` and newlines outside quotes, each cut
/// at its first pipe.
fn segments(s: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let (mut quote, mut piped) = (None::<char>, false);
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match (quote, c) {
            (Some(q), _) if c == q => quote = None,
            (None, '\'' | '"') => quote = Some(c),
            (None, ';' | '\n') => {
                out.push(String::new());
                piped = false;
                continue;
            }
            (None, '&' | '|') if it.peek() == Some(&c) => {
                it.next();
                out.push(String::new());
                piped = false;
                continue;
            }
            (None, '|') => piped = true,
            _ => {}
        }
        if !piped {
            out.last_mut().unwrap().push(c);
        }
    }
    out
}

/// A command's words from the command itself on: past `(`, `do`, `then`, `until`, `time`,
/// `timeout 60` and `VAR=value` prefixes.
fn head_words(seg: &str) -> Vec<&str> {
    let mut it = seg.split_whitespace().map(|w| w.trim_start_matches(['(', '{'])).peekable();
    while let Some(w) = it.peek().copied() {
        let assign = w.split_once('=').map(|(k, _)| !k.is_empty() && k.chars().all(|c| c.is_alphanumeric() || c == '_')).unwrap_or(false);
        if w == "timeout" {
            it.next();
            it.next();
        } else if assign || ["", "do", "then", "else", "time", "!", "exec", "nohup", "until", "while", "if"].contains(&w) {
            it.next();
        } else {
            break;
        }
    }
    it.collect()
}

/// Whether a command redirects its output into a file (not `/dev/null`, not `2>&1`).
fn redirects(seg: &str) -> bool {
    let mut quote = None::<char>;
    let b: Vec<char> = seg.chars().collect();
    for (i, &c) in b.iter().enumerate() {
        match (quote, c) {
            (Some(q), _) if c == q => quote = None,
            (None, '\'' | '"') => quote = Some(c),
            (None, '>') => {
                let rest: String = b[i + 1..].iter().collect();
                let target = rest.trim_start_matches('>').trim_start();
                if !target.is_empty() && !target.starts_with('&') && !target.starts_with("/dev/") && !target.starts_with('>') {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// The plan a shell command creates: `(artifact, section)` from `margin add <artifact> - section
/// <id>`, with the binary spelled any way (`margin`, a path ending in `/margin`, a variable such
/// as `$M`). Failing that, the section a todo is added under (`margin add <artifact> <section>
/// todo …`), for an agent handed a section someone else made.
fn plan_in(cmd: &str) -> Option<(String, String)> {
    let clean = |w: &str| w.trim_matches(['\'', '"', '(', ')', ';']).to_string();
    let w: Vec<String> = cmd.split_whitespace().map(clean).collect();
    let margin = |t: &str| t == "margin" || t.ends_with("/margin") || t.starts_with('$');
    let at = |f: &dyn Fn(&[String]) -> bool| w.windows(6).find(|x| margin(&x[0]) && x[1] == "add" && f(x));
    at(&|x| x[3] == "-" && x[4] == "section")
        .map(|x| (x[2].clone(), x[5].clone()))
        .or_else(|| at(&|x| x[3] != "-" && x[4] == "todo").map(|x| (x[2].clone(), x[3].clone())))
        .filter(|(a, s)| crate::util::valid_id(a) && crate::util::valid_id(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(dir: &Path, id: &str, prompt: &str, ts: &str, finished: bool, lines: &[&str]) {
        std::fs::write(dir.join(format!("agent-{id}.meta.json")), format!(r#"{{"agentType":"general-purpose","description":"review {id}","finished":{finished}}}"#)).unwrap();
        let mut t = vec![format!(r#"{{"type":"user","promptId":"{prompt}","timestamp":"{ts}","message":{{"content":"go"}}}}"#)];
        t.extend(lines.iter().map(|s| s.to_string()));
        std::fs::write(dir.join(format!("agent-{id}.jsonl")), t.join("\n")).unwrap();
    }

    #[test]
    fn reads_and_groups_a_fan_out() {
        let d = std::env::temp_dir().join(format!("margin-agents-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let tool = |n: &str| format!(r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","name":"{n}"}}]}}}}"#);
        let said = |s: &str| format!(r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"{s}"}}]}}}}"#);
        agent(&d, "a1", "p1", "2026-11-08T10:00:00Z", true, &[&tool("Read"), &tool("Read"), &tool("Grep"), &said("found 2 bugs")]);
        agent(&d, "a2", "p1", "2026-11-08T10:00:01Z", false, &[&tool("Read"), &said("now checking the lock")]);
        agent(&d, "b1", "p0", "2026-11-08T09:00:00Z", true, &[&said("old")]);
        let now = crate::util::parse_rfc3339("2026-11-08T10:05:00Z").unwrap();
        let gs = groups(&d, now, 30.0 * 60.0);
        assert_eq!(gs.len(), 1, "the old finished group is left out: {gs:?}");
        let g = &gs[0];
        assert_eq!((g.agents.len(), g.done()), (2, 1));
        assert_eq!(g.agents[0].tool_count(), 3);
        assert_eq!(g.agents[0].steps.iter().map(|s| (s.tool.as_str(), s.n)).collect::<Vec<_>>(), [("Read", 2), ("Grep", 1)], "a run of one tool folds");
        assert_eq!(g.agents[0].said, "found 2 bugs");
        assert_eq!(g.agents[1].said, "now checking the lock");
        assert_eq!(g.agents[0].notes, ["found 2 bugs"]);
        assert_eq!(g.agents[0].summary(), "looked at the code 3×");
        let _ = std::fs::remove_dir_all(&d);
    }

    // ---- phase and plan (ag-phase) ----

    const T0: &str = "2026-11-10T18:00:00Z";

    fn at(secs: u32) -> String {
        format!("2026-11-10T18:{:02}:{:02}.250Z", secs / 60, secs % 60)
    }
    fn now(secs: u32) -> f64 {
        crate::util::parse_rfc3339(&at(secs)).unwrap()
    }
    /// A tool call at `secs` and, unless `open`, its result a second later.
    fn call(n: u32, secs: u32, tool: &str, input: Value, open: bool) -> String {
        let mut s = serde_json::json!({"type": "assistant", "timestamp": at(secs), "message": {"stop_reason": "tool_use", "content": [{"type": "tool_use", "id": format!("tu{n}"), "name": tool, "input": input}]}}).to_string();
        if !open {
            s.push('\n');
            s += &serde_json::json!({"type": "user", "timestamp": at(secs + 1), "message": {"content": [{"type": "tool_result", "tool_use_id": format!("tu{n}"), "content": "ok"}]}}).to_string();
        }
        s
    }
    fn sh(n: u32, secs: u32, cmd: &str) -> String {
        call(n, secs, "Bash", serde_json::json!({"command": cmd}), false)
    }
    fn edit(n: u32, secs: u32, file: &str) -> String {
        call(n, secs, "Edit", serde_json::json!({"file_path": file}), false)
    }
    fn look(n: u32, secs: u32, file: &str) -> String {
        call(n, secs, "Read", serde_json::json!({"file_path": file}), false)
    }
    /// An agent whose transcript is `lines`, parsed without touching the disk.
    fn parsed(desc: &str, lines: &[String]) -> Agent {
        let mut a = Agent { description: desc.into(), ..Default::default() };
        a.ingest(&format!("{{\"type\":\"user\",\"promptId\":\"p\",\"timestamp\":\"{T0}\",\"message\":{{\"content\":\"go\"}}}}\n"));
        a.ingest(&lines.join("\n"));
        a
    }
    fn numbered(f: impl Fn(u32, u32) -> String, from: u32, n: u32) -> Vec<String> {
        (from..from + n).map(|i| f(i, i * 2)).collect()
    }

    #[test]
    fn each_phase_is_recognized() {
        let reads = numbered(|i, s| if i % 2 == 0 { look(i, s, "/r/src/a.rs") } else { sh(i, s, "grep -n foo src/*.rs | head -20") }, 1, 4);
        let a = parsed("Add tabs", &reads);
        assert_eq!(a.phase(now(10)), Phase::Planning, "reading before any change");
        assert_eq!(parsed("Add tabs", &[]).phase(now(1)), Phase::Planning, "nothing yet: finding its way");

        let mut l = reads.clone();
        l.extend(numbered(|i, s| edit(i, s, "/r/src/a.rs"), 5, 3));
        assert_eq!(parsed("Add tabs", &l).phase(now(17)), Phase::Coding);

        l.extend(numbered(|i, s| sh(i, s, if i % 2 == 0 { "cargo test --workspace 2>&1 | tail -5" } else { "cd /r && cargo build 2>&1 | grep -E '^error' -A8" }), 8, 2));
        assert_eq!(parsed("Add tabs", &l).phase(now(21)), Phase::Testing);

        l.extend([sh(10, 22, "jj diff --stat"), look(11, 24, "/r/src/a.rs")]);
        assert_eq!(parsed("Add tabs", &l).phase(now(26)), Phase::Reviewing, "a diff, then reading back the file it changed");

        // a review agent never edits: its reading is the review
        assert_eq!(parsed("Review vellum toolkit crate", &reads).phase(now(10)), Phase::Reviewing);
        assert_eq!(Phase::Coding.label(), "coding");
        assert_eq!([Phase::Planning, Phase::Testing, Phase::Reviewing, Phase::Waiting, Phase::Done].map(Phase::label), ["planning", "testing", "reviewing", "waiting", "done"]);
    }

    #[test]
    fn shell_commands_are_classed_by_what_they_run() {
        let c = |cmd: &str| shell(cmd).class;
        assert_eq!(c("python3 - <<'E'\np='crates/margin/src/doc.rs'\ns=open(p).read()\nopen(p,'w').write(s.replace('a','b'))\nE"), Class::Edit);
        assert_eq!(c("cat > /tmp/x/run.py <<'EOF'\nimport os\ncargo test\nEOF"), Class::Edit, "a here-document's body is not commands");
        assert_eq!(c("sed -i 's/a/b/' src/a.rs"), Class::Edit);
        assert_eq!(c("sed -n 1,60p tests/e2e/ptyhost.py; grep -n \"cargo test\" README.md"), Class::Read, "reading a test, or a file that mentions one, is not testing");
        assert_eq!(c("cd /r && cargo build 2>&1 | tail -2 && date -u +%FT%TZ"), Class::Test);
        assert_eq!(c("timeout 120 python3 tests/e2e/idle.py"), Class::Test);
        assert_eq!(c("FOO=1 npm run test -- --watch=false"), Class::Test);
        assert_eq!(c("for i in 1 2; do ./target/debug/margin set margin t$i status doing >/dev/null; done"), Class::None, "ticking the plan says nothing");
        assert_eq!(c("python3 - <<'EOF'\nimport sys; sys.path.insert(0,'/tmp/s')\nfrom ed import edit\nedit('crates/margin/src/app.rs', [(a, b)])\nEOF"), Class::Edit);
        assert_eq!(c("python3 - <<'EOF'\nprint(open('a.json').read()[:200])\nEOF"), Class::None);
        assert_eq!(c("until cargo build 2>&1 | grep -q Finished; do sleep 10; done"), Class::Test);
        assert_eq!(c("jj diff -r @ | head -80"), Class::Diff);
        assert_eq!(c("git status"), Class::Diff);
        assert_eq!(c("./target/debug/margin add margin - section s | tail -3"), Class::None, "a pipe's tail is not a read");
        assert_eq!(c("cargo build 2>/dev/null; cat src/a.rs"), Class::Test, "the strongest class wins");
        assert!(shell("sleep 50; cargo build 2>&1 | tail").waits && !shell("sleep 2; cargo build").waits);
        assert_eq!(shell("python3 - <<'E'\nopen('crates/margin/src/doc.rs','w').write('x')\nE").paths, ["doc.rs"]);
    }

    #[test]
    fn one_stray_call_does_not_flip_the_phase() {
        let mut l = numbered(|i, s| edit(i, s, "/r/src/a.rs"), 1, 5);
        l.push(sh(6, 12, "cargo build 2>&1 | tail -3"));
        assert_eq!(parsed("x", &l).phase(now(14)), Phase::Coding, "one build amid edits");
        l.push(edit(7, 14, "/r/src/b.rs"));
        l.push(look(8, 16, "/r/src/never-seen.rs"));
        assert_eq!(parsed("x", &l).phase(now(18)), Phase::Coding, "one read of another file");
        // an edit/build loop stays coding however long it runs
        l.extend(numbered(|i, s| if i % 2 == 0 { sh(i, s, "cargo build") } else { edit(i, s, "/r/src/a.rs") }, 9, 12));
        let a = parsed("x", &l);
        assert_eq!(a.phase(now(44)), Phase::Coding);
        // two of a kind in a row move it at once
        l.extend([sh(21, 44, "cargo test"), sh(22, 46, "cargo test -p vellum")]);
        assert_eq!(parsed("x", &l).phase(now(48)), Phase::Testing);
        // and calls that say nothing never vote
        l.extend(numbered(|i, s| sh(i, s, "margin set margin t1 status done; mkdir -p out; date"), 24, 9));
        assert_eq!(parsed("x", &l).phase(now(70)), Phase::Testing);
    }

    #[test]
    fn a_call_long_in_flight_is_waiting() {
        let mut l = numbered(|i, s| edit(i, s, "/r/src/a.rs"), 1, 3);
        l.push(call(4, 10, "Bash", serde_json::json!({"command": "rsync -a big/ copy/"}), true));
        let a = parsed("x", &l);
        assert_eq!(a.phase(now(20)), Phase::Coding, "ten seconds in flight is just a command");
        assert_eq!(a.phase(now(10 + 31)), Phase::Waiting);
        assert_eq!(a.waiting_for(now(10 + 45)), Some(45.0));
        assert_eq!(a.waiting_for(now(20)), None);
        // its result ends the wait, and the phase is what it was
        let mut m = l.clone();
        m.push(serde_json::json!({"type": "user", "timestamp": at(100), "message": {"content": [{"type": "tool_result", "tool_use_id": "tu4", "content": "ok"}]}}).to_string());
        assert_eq!(parsed("x", &m).phase(now(101)), Phase::Coding);
        // composing the next call is silent for minutes: not waiting until it is long past that
        assert_eq!(parsed("x", &m).phase(now(100 + 240)), Phase::Coding);
        assert_eq!(parsed("x", &m).phase(now(100 + 601)), Phase::Waiting);

        // a long test run is testing, not waiting, until it is very long; behind a sleep it waits
        let mut t = numbered(|i, s| edit(i, s, "/r/src/a.rs"), 1, 3);
        t.push(call(4, 10, "Bash", serde_json::json!({"command": "cargo test --workspace"}), true));
        assert_eq!(parsed("x", &t).phase(now(10 + 120)), Phase::Testing);
        assert_eq!(parsed("x", &t).phase(now(10 + 301)), Phase::Waiting);
        t.pop();
        t.push(call(4, 10, "Bash", serde_json::json!({"command": "sleep 60; cargo build 2>&1 | tail"}), true));
        assert_eq!(parsed("x", &t).phase(now(10 + 40)), Phase::Waiting);

        // a turn that ended while it is unfinished: parked until a background job wakes it
        let mut p = numbered(|i, s| edit(i, s, "/r/src/a.rs"), 1, 3);
        p.push(serde_json::json!({"type": "assistant", "timestamp": at(10), "message": {"stop_reason": "end_turn", "content": [{"type": "text", "text": "Waiting for the suite."}]}}).to_string());
        assert_eq!(parsed("x", &p).phase(now(15)), Phase::Coding);
        assert_eq!(parsed("x", &p).phase(now(45)), Phase::Waiting);
    }

    /// A waiting agent says why when the transcript's tail tells: the call in flight is a retry
    /// loop, a sleep, a nested agent or a build that has run long; or the turn ended. A wait
    /// nothing explains (a slow plain command, long silence) gives no reason, and nothing is
    /// said while it is not waiting.
    #[test]
    fn a_waiting_agent_says_why_when_the_transcript_tells() {
        let waiting = |tool: &str, input: serde_json::Value, after: u32| {
            let mut l = numbered(|i, s| edit(i, s, "/r/src/a.rs"), 1, 3);
            l.push(call(4, 10, tool, input, true));
            let a = parsed("x", &l);
            (a.phase(now(10 + after)), a.waiting_why(now(10 + after)))
        };
        let sh = |cmd: &str, after: u32| waiting("Bash", serde_json::json!({"command": cmd}), after);
        assert_eq!(sh("until cargo test --workspace 2>&1 | grep -q 'test result: ok'; do sleep 30; done", 60), (Phase::Waiting, Some("retrying tests")));
        assert_eq!(sh("for i in 1 2 3; do cargo build && break; sleep 5; done", 301), (Phase::Waiting, Some("retrying tests")));
        assert_eq!(sh("sleep 600", 60), (Phase::Waiting, Some("sleep")));
        assert_eq!(sh("sleep 60; cargo build 2>&1 | tail", 40), (Phase::Waiting, Some("sleep")));
        assert_eq!(sh("cargo build --release", 301), (Phase::Waiting, Some("long build")));
        assert_eq!(sh("cargo test --workspace", 301), (Phase::Waiting, Some("long tests")));
        assert_eq!(sh("cargo test --workspace", 120), (Phase::Testing, None), "still testing: nothing to explain");
        assert_eq!(waiting("Agent", serde_json::json!({"description": "review the diff", "prompt": "…"}), 60), (Phase::Waiting, Some("subagent")));
        assert_eq!(sh("rsync -a big/ copy/", 60), (Phase::Waiting, None), "a slow command or a permission prompt: it cannot tell");
        // nothing in flight: a turn that ended is parked; plain silence has no reason
        let mut p = numbered(|i, s| edit(i, s, "/r/src/a.rs"), 1, 3);
        p.push(serde_json::json!({"type": "assistant", "timestamp": at(10), "message": {"stop_reason": "end_turn", "content": [{"type": "text", "text": "Waiting for the suite."}]}}).to_string());
        assert_eq!(parsed("x", &p).waiting_why(now(45)), Some("turn ended"));
        assert_eq!(parsed("x", &p).waiting_why(now(15)), None);
        let quiet = parsed("x", &numbered(|i, s| edit(i, s, "/r/src/a.rs"), 1, 3));
        assert_eq!((quiet.phase(now(3 + 700)), quiet.waiting_why(now(3 + 700))), (Phase::Waiting, None));
    }

    #[test]
    fn a_finished_agent_is_done() {
        let mut a = parsed("x", &[call(1, 2, "Bash", serde_json::json!({"command": "sleep 600"}), true)]);
        assert_eq!(a.phase(now(100)), Phase::Waiting);
        a.finished = true;
        assert_eq!(a.phase(now(100)), Phase::Done);
        assert_eq!(a.waiting_for(now(100)), None);
    }

    #[test]
    fn the_plan_section_is_found_for_each_spelling_of_the_binary() {
        let want = Some(("margin".to_string(), "ag-tabs".to_string()));
        for cmd in [
            "margin add margin - section ag-tabs title=\"Agent: tabs\"",
            "./target/debug/margin add margin - section ag-tabs title=\"Agent: tabs\" 2>&1 | tail -3",
            "cd /home/u/margin && /home/u/margin/target/release/margin add margin - section ag-tabs",
            "M=./target/debug/margin; $M add margin - section ag-tabs title=x; $M add margin ag-tabs todo t1 \"read\"",
            "\"$M\" add 'margin' - section \"ag-tabs\";",
            "cd /r && wc -l src/*.rs && (margin add margin - section ag-tabs title=x)",
            // handed a section someone else made: where its todos go
            "./target/debug/margin add margin ag-tabs todo ag-tabs-1 \"Read the code\"",
        ] {
            assert_eq!(plan_in(cmd), want, "{cmd}");
        }
        assert_eq!(plan_in("margin set margin ag-tabs-1 status done"), None);
        assert_eq!(plan_in("grep -n 'add margin - section' notes.md"), None, "not a margin command");
        // the first section it makes is its plan; later ones are content
        let a = parsed("x", &[sh(1, 2, "ls"), sh(2, 4, "margin add work - section ag-a title=A"), sh(3, 6, "margin add work - section findings title=F")]);
        assert_eq!(a.plan, Some(("work".into(), "ag-a".into())));
        assert_eq!(a.who(), "x", "the description names the row, not the section id");
        assert_eq!(parsed("Fix the parser", &[sh(1, 2, "ls")]).who(), "Fix the parser");
    }

    #[test]
    fn plan_progress_follows_the_plan_as_it_changes() {
        use crate::doc::{Block, Doc};
        let todo = |id: &str, text: &str, status: &str| -> Block { serde_json::from_value(serde_json::json!({"type": "todo", "id": id, "text": text, "status": status})).unwrap() };
        let mut doc: Doc = serde_json::from_value(serde_json::json!({"id": "work", "blocks": [
            {"type": "section", "id": "ag-a", "title": "Agent a", "children": [
                {"type": "todo", "id": "a1", "text": "Read", "status": "done"},
                {"type": "todo", "id": "a2", "text": "Build", "status": "doing"},
                {"type": "todo", "id": "a3", "text": "Test"}]},
            {"type": "section", "id": "other", "children": [{"type": "todo", "id": "o1", "text": "not its"}]}]})).unwrap();
        let mut state = crate::store::State::default();
        let a = parsed("x", &[sh(1, 2, "margin add work - section ag-a title=A")]);
        let p = a.plan_progress(&doc, &state).unwrap();
        assert_eq!((p.done, p.total, p.current.as_str()), (1, 3, "Build"));
        assert_eq!(p.bar(5), "▰▰▱▱▱");

        // a todo is added, one is removed: the counts are read, not remembered
        doc.blocks[0].children.push(todo("a4", "Report", ""));
        assert_eq!(a.plan_progress(&doc, &state).map(|p| (p.done, p.total)), Some((1, 4)));
        doc.blocks[0].children.remove(0);
        let p = a.plan_progress(&doc, &state).unwrap();
        assert_eq!((p.done, p.total, p.current.as_str()), (0, 3, "Build"));
        assert_eq!(p.bar(5), "▱▱▱▱▱");

        // a status sulin set in the pane (state) wins over the block's, as in the pane
        state.decisions.insert("a2".into(), serde_json::from_value(serde_json::json!({"kind": "status", "value": "done", "by": "sulin", "seq": 1, "at": T0, "via": "pane"})).unwrap());
        let p = a.plan_progress(&doc, &state).unwrap();
        assert_eq!((p.done, p.total, p.current.as_str()), (1, 3, "Test"), "no step is doing: the first not done is next");
        for b in &mut doc.blocks[0].children {
            b.f.insert("status".into(), "skipped".into());
        }
        let p = a.plan_progress(&doc, &state).unwrap();
        assert_eq!((p.done, p.total, p.current.as_str(), p.bar(4).as_str()), (3, 3, "", "▰▰▰▰"));
        assert_eq!(Progress { done: 1, total: 9, current: String::new() }.bar(5), "▰▱▱▱▱", "started shows");
        assert_eq!(Progress { done: 8, total: 9, current: String::new() }.bar(5), "▰▰▰▰▱", "only finished fills");

        // no plan, another artifact, a section gone or empty: no bar
        assert_eq!(parsed("x", &[sh(1, 2, "ls")]).plan_progress(&doc, &state), None);
        doc.id = "elsewhere".into();
        assert_eq!(a.plan_progress(&doc, &state), None);
        doc.id = "work".into();
        doc.blocks[0].children.clear();
        assert_eq!(a.plan_progress(&doc, &state), None);
        doc.blocks.remove(0);
        assert_eq!(a.plan_progress(&doc, &state), None);
    }

    #[test]
    fn read_parses_only_what_was_appended() {
        use std::io::Write;
        let d = std::env::temp_dir().join(format!("margin-agents-inc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let (meta, log) = (d.join("agent-i1.meta.json"), d.join("agent-i1.jsonl"));
        std::fs::write(&meta, r#"{"agentType":"general-purpose","description":"inc"}"#).unwrap();
        let first = format!("{{\"type\":\"user\",\"promptId\":\"p\",\"timestamp\":\"{T0}\",\"message\":{{\"content\":\"go\"}}}}\n{}\n", edit(1, 2, "/r/a.rs"));
        std::fs::write(&log, &first).unwrap();
        let a = read(&meta).unwrap();
        assert_eq!((a.tool_count(), a.prompt.as_str(), a.started.as_str()), (1, "p", T0));
        let offset = |p: &Path| CACHE.lock().unwrap().as_ref().unwrap()[p].offset;
        assert_eq!(offset(&log), first.len() as u64);

        // a line still being written is left for the next read, and nothing is counted twice
        let next = edit(2, 4, "/r/b.rs");
        let (head, tail) = next.split_at(40);
        let mut f = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
        f.write_all(head.as_bytes()).unwrap();
        assert_eq!(read(&meta).unwrap().tool_count(), 1);
        assert_eq!(offset(&log), first.len() as u64);
        f.write_all(tail.as_bytes()).unwrap();
        f.write_all(b"\n").unwrap();
        let a = read(&meta).unwrap();
        assert_eq!((a.tool_count(), a.phase(now(8))), (2, Phase::Coding));
        assert_eq!(read(&meta).unwrap(), a, "an unchanged file reads the same");
        assert_eq!(a.steps, [Step { tool: "Edit".into(), label: "Edit b.rs".into(), n: 2 }], "a run folds across reads");

        // the meta is read every time: finishing shows without the transcript changing
        std::fs::write(&meta, r#"{"agentType":"general-purpose","description":"inc","finished":true}"#).unwrap();
        assert_eq!(read(&meta).unwrap().phase(now(8)), Phase::Done);
        // a shorter file is another transcript: parsed from the top
        std::fs::write(&log, format!("{{\"type\":\"user\",\"promptId\":\"q\",\"timestamp\":\"{T0}\",\"message\":{{\"content\":\"go\"}}}}")).unwrap();
        let a = read(&meta).unwrap();
        assert_eq!((a.tool_count(), a.prompt.as_str()), (0, "q"));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Not a test: prints each agent's phase and progress for a real subagents directory, and
    /// what a first and a repeated read cost. `MARGIN_AGENTS_DIR=… cargo test live_table -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_table() {
        let Ok(dir) = std::env::var("MARGIN_AGENTS_DIR") else { return };
        let st = crate::store::Store::open_cwd();
        let now = crate::util::epoch_now();
        let t = std::time::Instant::now();
        let gs = groups(Path::new(&dir), now, 1e9);
        let cold = t.elapsed();
        let t = std::time::Instant::now();
        for _ in 0..10 {
            groups(Path::new(&dir), now, 1e9);
        }
        println!("first read {cold:?}, repeated read {:?}", t.elapsed() / 10);
        // what `read` did before: every line of every transcript parsed on every read
        let t = std::time::Instant::now();
        let mut bytes = 0;
        for e in std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).filter(|e| e.path().extension().map(|x| x == "jsonl").unwrap_or(false)) {
            let s = std::fs::read_to_string(e.path()).unwrap_or_default();
            bytes += s.len();
            s.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).count();
        }
        println!("parsing all {bytes} bytes, as before: {:?}", t.elapsed());
        for a in gs.iter().flat_map(|g| &g.agents) {
            let p = a.plan_progress_in(&st);
            let bar = p.as_ref().map(|p| format!("{} {}/{}  {}", p.bar(5), p.done, p.total, p.current)).unwrap_or_default();
            println!("{:<10} {:<34} {:<10} {:>4}  {:?} {bar}", a.who().chars().take(10).collect::<String>(), a.description, a.phase(now).label(), a.took(now), a.track.votes.iter().map(|v| &v.label()[..1]).collect::<String>());
        }
    }
}
