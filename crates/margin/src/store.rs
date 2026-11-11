//! The store: one directory per project, one per artifact, three owners kept in three files.
//!
//! ```text
//! $MARGIN_HOME (~/.local/state/margin)/<project>/
//!   events.jsonl          append-only, every artifact
//!   .lock                 flock: every write takes it
//!   delivered/<session>   last event seq handed to that Claude session
//!   feeds.json            registered feeds (feed.rs)   feeds/<name>.log|.status.json   daemon.lock|.pid
//!   <artifact>/doc.json   content (Claude)        state.json  decisions (sulin)
//!              history.jsonl  every old revision  view/<pane>.json  folds, focus, scroll (the pane)
//! ```
//!
//! Writes go to a temp file and `rename` into place, so the renderer never reads half a file.

use crate::doc::{Doc, Op};
use crate::util::{now_rfc3339, slug};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Decision {
    pub kind: String,
    pub value: Value,
    pub by: String,
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    /// Event seq that recorded this decision (for "has Claude read it yet").
    #[serde(default)]
    pub seq: u64,
    /// On sulin's mark on a todo: the status Claude had set beneath it, restored when he clears
    /// the mark (one decision slot per id, so without this a clear lost Claude's status).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub under: Option<Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct State {
    #[serde(default)]
    pub decisions: BTreeMap<String, Decision>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct View {
    #[serde(default)]
    pub folds: BTreeMap<String, bool>,
    #[serde(default)]
    pub focus: Option<String>,
    /// Scroll anchor: the first visible component and the rows scrolled into it.
    #[serde(default)]
    pub anchor: Option<String>,
    #[serde(default)]
    pub anchor_off: i32,
    #[serde(default)]
    pub seen_rev: u64,
    #[serde(default)]
    pub seen_at: String,
    /// The artifacts this pane shows as tabs, the pane's own first (empty: just the one). Kept in
    /// the view file of the pane's own artifact; `margin tab` edits it and the pane follows.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tabs: Vec<String>,
    /// The tab on screen (an artifact id; empty: the first).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tab: String,
    /// The pinned head is folded (`zh`): each of its lines one row, cut, instead of wrapped.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub head_folded: bool,
    /// The detail panel is open (`d`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub detail: bool,
    /// A `tabs` component's section on show, by the component's id (default: its first).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub subtabs: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Event {
    #[serde(default = "crate::util::v1")]
    pub v: u64,
    pub seq: u64,
    pub at: String,
    pub artifact: String,
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub value: Value,
    pub by: String,
    pub via: String,
    #[serde(default)]
    pub rev: u64,
}

/// Passive kinds are logged but never pushed into the prompt (design §2).
pub fn is_passive(kind: &str) -> bool {
    matches!(kind, "fold" | "unfold" | "seen" | "open" | "close" | "focus" | "view" | "feed" | "toasttest")
}

pub struct Store {
    pub home: PathBuf,
    pub project: String,
    /// Who a CLI content write is logged as (`by` on the doc, history and status decisions):
    /// `claude`, `sulin`, or another name (`margin --by`). Feeds log as `feed:<name>` regardless.
    pub author: String,
}

/// Who is writing content. sulin's decisions go through `decide`, never through here.
#[derive(Clone, Debug, PartialEq)]
pub enum Writer {
    Claude,
    /// A registered feed and the id prefix it owns in its artifact (`*` = the whole artifact).
    /// The prefix travels with the writer, so the daemon can still mark a removed feed's status
    /// line "removed" after the registry has forgotten it.
    Feed { name: String, owns: String },
}

impl Writer {
    pub fn by(&self) -> String {
        match self {
            Writer::Claude => "claude".into(),
            Writer::Feed { name, .. } => format!("feed:{name}"),
        }
    }
}

/// A feed's claim on part of an artifact: every id starting with `owns`.
#[derive(Clone, Debug, PartialEq)]
pub struct Claim {
    pub feed: String,
    pub owns: String,
}

/// Does prefix `p` cover `id`? `*` covers everything; the empty prefix covers nothing. A prefix
/// is a namespace, not a string prefix: `ci` covers `ci` and `ci-…` / `ci_…`, never `city`
/// (2026-11-06 review: `--owns ci` claimed Claude's `city`, `circle`, `cite`). A prefix that
/// already ends in `-` or `_` (`ag-`) covers what starts with it.
pub fn owns(p: &str, id: &str) -> bool {
    if p == "*" {
        return true;
    }
    if p.is_empty() {
        return false;
    }
    if p.ends_with('-') || p.ends_with('_') {
        return id.starts_with(p);
    }
    id == p || id.strip_prefix(p).map(|r| r.starts_with('-') || r.starts_with('_') || r.starts_with('/')).unwrap_or(false)
}

/// The ownership rule for one op (design.md "Feeds"): a feed writes only inside its prefix, and
/// nobody else writes there. `touched` includes descendants and the parent an op inserts into, so
/// removing or moving a section can't carry a feed's components away with it.
pub fn check_owner(op: &Op, doc: &Doc, w: &Writer, claims: &[Claim]) -> Result<(), String> {
    let (ids, parent) = op.touched(doc);
    match w {
        Writer::Feed { name, owns: p } => {
            if matches!(op, Op::Title { .. }) && p != "*" {
                return Err(format!("feed {name} owns the ids {p} and {p}-…, not the artifact title"));
            }
            if let Some(id) = ids.iter().find(|id| !owns(p, id)) {
                return Err(format!("feed {name} may only write the ids {p} and {p}-…; {id:?} is outside its prefix"));
            }
            if let Some(par) = parent.filter(|par| !owns(p, par)) {
                return Err(format!("feed {name} may only insert into its own components or the top level, not into {par:?}"));
            }
        }
        Writer::Claude => {
            let mine = |id: &str| claims.iter().find(|c| owns(&c.owns, id));
            if matches!(op, Op::Title { .. }) {
                if let Some(c) = claims.iter().find(|c| c.owns == "*") {
                    return Err(feed_owned_msg("the title", c));
                }
            }
            for id in ids.iter().chain(parent.iter()) {
                if let Some(c) = mine(id) {
                    return Err(feed_owned_msg(&format!("{id:?}"), c));
                }
            }
        }
    }
    Ok(())
}

fn feed_owned_msg(what: &str, c: &Claim) -> String {
    let span = if c.owns == "*" { "this whole artifact".to_string() } else { format!("the ids {0} and {0}-… here", c.owns) };
    format!(
        "{what} belongs to feed `{}`, which owns {span} and rewrites it on its own; write outside that prefix, or stop the feed first (`margin feed pause {}` / `margin feed rm {}`)",
        c.feed, c.feed, c.feed
    )
}

/// The reactions a row note can carry (r3; sulin picked all five, 2026-11-08).
pub const NOTE_REACTIONS: &[&str] = &["more", "less", "keep", "disagree", "why"];

pub fn note_key(row: &str, quote: &str) -> String {
    let mut h: u32 = 0x811c9dc5;
    for b in quote.trim().bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x01000193);
    }
    format!("{row}#note#{:04x}", h & 0xffff)
}

/// The row a note key belongs to.
pub fn note_row(key: &str) -> &str {
    key.split("#note#").next().unwrap_or(key)
}

/// A row's content fingerprint: a note made on one version of the row is answered by the next.
pub fn row_hash(b: &crate::doc::Block) -> String {
    let mut h: u32 = 0x811c9dc5;
    for c in format!("{}\u{1}{}", b.s("text"), b.s("title")).bytes() {
        h ^= c as u32;
        h = h.wrapping_mul(0x01000193);
    }
    format!("{h:08x}")
}

/// Where a todo's start and finish times live in state.json: a `time` decision whose value is
/// `{started, done}` (RFC 3339), written by `Store::patch` when Claude sets the step's status.
pub fn step_time_key(id: &str) -> String {
    format!("{id}#time")
}

/// Is this note still waiting: its row exists and hasn't changed since he wrote it?
pub fn note_open(d: &Doc, key: &str, v: &Decision) -> bool {
    d.find(note_row(key)).map(|b| v.value["h"].as_str() == Some(&row_hash(b))).unwrap_or(false)
}

/// sulin's open notes on `row`.
pub fn notes_on<'a>(d: &Doc, st: &'a State, row: &str) -> Vec<(&'a String, &'a Decision)> {
    let pre = format!("{row}#note#");
    st.decisions.iter().filter(|(k, v)| k.starts_with(&pre) && v.kind == "note" && note_open(d, k, v)).collect()
}

pub struct Lock(fs::File);

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

pub fn default_home() -> PathBuf {
    if let Ok(h) = std::env::var("MARGIN_HOME") {
        return PathBuf::from(h);
    }
    let base = std::env::var("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/state"));
    base.join("margin")
}

/// The project: `$MARGIN_PROJECT`, else the nearest jj/git root above `cwd`, else `cwd`, as a slug.
pub fn project_for(cwd: &Path) -> String {
    if let Ok(p) = std::env::var("MARGIN_PROJECT") {
        return p;
    }
    let mut d = Some(cwd);
    while let Some(p) = d {
        if p.join(".jj").exists() || p.join(".git").exists() {
            return slug(&p.to_string_lossy());
        }
        d = p.parent();
    }
    slug(&cwd.to_string_lossy())
}

/// What one Claude session (or one of its subagents) is doing, as its tool hooks last saw it.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Activity {
    pub session: String,
    /// The subagent's id when a subagent made the call; empty for the main session.
    #[serde(default)]
    pub agent: String,
    /// "running" (a tool call is in flight), "thinking" (between calls), "idle" (sulin's turn).
    pub state: String,
    #[serde(default)]
    pub tool: String,
    #[serde(default)]
    pub text: String,
    /// RFC 3339; the pane ticks the age from it.
    pub since: String,
}

pub fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_data().ok();
    }
    fs::rename(&tmp, path)
}

fn read_json<T: for<'a> Deserialize<'a> + Default>(p: &Path) -> T {
    fs::read_to_string(p).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

/// The read half of a read-modify-write. A missing file is the default; one that exists but
/// won't read or parse is an error, never the default: the write that follows would make the
/// reset permanent (an unparseable state.json came back holding only the newest decision).
fn read_json_strict<T: for<'a> Deserialize<'a> + Default>(p: &Path) -> Result<T, String> {
    let s = match fs::read_to_string(p) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(T::default()),
        Err(e) => return Err(format!("{}: {e}; nothing was written", p.display())),
    };
    serde_json::from_str(&s).map_err(|e| format!("{}: {e}; nothing was written (fix the file or move it aside)", p.display()))
}

/// A session id as a file name (the same mapping link.rs, wake.rs and recap.rs use).
fn session_key(session: &str) -> String {
    session.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect()
}

impl Store {
    pub fn new(home: PathBuf, project: String) -> Store {
        Store { home, project, author: "claude".into() }
    }

    pub fn open_cwd() -> Store {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        Store::new(default_home(), project_for(&cwd))
    }

    pub fn dir(&self) -> PathBuf {
        self.home.join(&self.project)
    }
    pub fn art(&self, a: &str) -> PathBuf {
        self.dir().join(a)
    }
    pub fn doc_path(&self, a: &str) -> PathBuf {
        self.art(a).join("doc.json")
    }
    pub fn state_path(&self, a: &str) -> PathBuf {
        self.art(a).join("state.json")
    }
    pub fn view_path(&self, a: &str, pane: &str) -> PathBuf {
        self.art(a).join("view").join(format!("{pane}.json"))
    }
    pub fn events_path(&self) -> PathBuf {
        self.dir().join("events.jsonl")
    }

    pub fn lock(&self) -> std::io::Result<Lock> {
        fs::create_dir_all(self.dir())?;
        let f = fs::OpenOptions::new().create(true).truncate(false).write(true).open(self.dir().join(".lock"))?;
        // std's File::lock is flock(2) on Linux; unlike the raw call it reports failure
        f.lock()?;
        Ok(Lock(f))
    }

    pub fn exists(&self, a: &str) -> bool {
        self.doc_path(a).exists()
    }

    /// The one message for an artifact this project doesn't have. Every command that takes an
    /// existing artifact fails with it, so the fix (`margin new`) is named wherever the mistake
    /// shows, and a mistyped name points at the artifact it was probably meant to be (a typo
    /// used to create a second board: writes created what they didn't find).
    pub fn missing(&self, a: &str) -> String {
        let near = self
            .names()
            .into_iter()
            .filter(|n| n != a)
            .map(|n| (crate::util::edit_distance(a, &n), n))
            .filter(|(d, _)| *d <= 2)
            .min();
        let hint = near.map(|(_, n)| format!(" (did you mean {n:?}?)")).unwrap_or_default();
        format!("no artifact {a:?} in project {}: create it with margin new {a}{hint}", self.project)
    }

    /// The ids of this project's artifacts, from the directory alone (no document is parsed).
    fn names(&self) -> Vec<String> {
        let Ok(rd) = fs::read_dir(self.dir()) else { return vec![] };
        let mut v: Vec<String> = rd.flatten().filter(|e| e.path().join("doc.json").exists()).filter_map(|e| e.file_name().into_string().ok()).filter(|n| !n.starts_with('.')).collect();
        v.sort();
        v
    }

    pub fn load(&self, a: &str) -> Result<Doc, String> {
        let s = fs::read_to_string(self.doc_path(a)).map_err(|_| self.missing(a))?;
        let d: Doc = serde_json::from_str(&s).map_err(|e| format!("{a}/doc.json: {e}"))?;
        // design §3 "Versions": a newer format is refused, not read as v 1 and rewritten
        if d.v != 1 {
            return Err(format!("{a}/doc.json: version {}: this margin reads v 1", d.v));
        }
        Ok(d)
    }

    /// For display: an unparseable state.json reads as empty. Never write this back.
    pub fn state(&self, a: &str) -> State {
        read_json(&self.state_path(a))
    }

    /// For a read-modify-write of state.json (call it before anything else is written).
    fn state_rw(&self, a: &str) -> Result<State, String> {
        read_json_strict(&self.state_path(a))
    }

    pub fn view(&self, a: &str, pane: &str) -> View {
        read_json(&self.view_path(a, pane))
    }

    pub fn save_view(&self, a: &str, pane: &str, v: &View) {
        let _ = write_atomic(&self.view_path(a, pane), serde_json::to_string_pretty(v).unwrap().as_bytes());
    }

    fn save_doc_locked(&self, d: &mut Doc, old: Option<&Doc>, by: &str) -> Result<u64, String> {
        if let Some(o) = old {
            // Every revision Claude wrote is kept whole. A feed rewrites often, so a feed write
            // after its own previous write keeps only a marker line: the history then holds every
            // Claude revision in full plus the last state each feed left, not thousands of copies.
            let line = if by.starts_with("feed:") && o.by == by {
                serde_json::to_string(&serde_json::json!({"rev": o.rev, "by": o.by, "at": o.updated, "compact": true})).unwrap()
            } else {
                serde_json::to_string(o).unwrap()
            };
            let mut f = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.art(&d.id).join("history.jsonl"))
                .map_err(|e| e.to_string())?;
            writeln!(f, "{line}").map_err(|e| e.to_string())?;
        }
        d.rev = old.map(|o| o.rev).unwrap_or(0) + 1;
        d.updated = now_rfc3339();
        d.by = by.to_string();
        if let (true, Ok(s)) = (by == "claude", std::env::var("CLAUDE_CODE_SESSION_ID")) {
            if !d.sessions.contains(&s) {
                d.sessions.push(s);
            }
        }
        write_atomic(&self.doc_path(&d.id), serde_json::to_string_pretty(d).unwrap().as_bytes()).map_err(|e| e.to_string())?;
        self.prune_claude_orphans(d);
        Ok(d.rev)
    }

    /// Drop decisions Claude (or a feed) made on components that no longer exist: a status on a
    /// row Claude removed is Claude's own bookkeeping, not something sulin decided, so it would
    /// only pile up as orphans. sulin's own decisions stay until Claude has been handed them.
    fn prune_claude_orphans(&self, d: &Doc) {
        // an unparseable state.json is left exactly as it is
        let Ok(mut st) = self.state_rw(&d.id) else { return };
        let ids: std::collections::HashSet<String> = d.ids().into_iter().collect();
        let before = st.decisions.len();
        // a note is answered once Claude rewrites (or removes) its row
        st.decisions.retain(|k, v| v.kind != "note" || note_open(d, k, v));
        // sulin's decision on a removed row stays only until Claude has been handed it: the
        // orphan exists to say "Claude removed this before reading what you decided"
        let handed = self.delivered_max();
        // a removed step's times go with it
        st.decisions.retain(|k, v| v.kind != "time" || k.split('#').next().map(|id| ids.contains(id)).unwrap_or(false));
        st.decisions.retain(|k, v| k.contains('#') || ids.contains(k) || (crate::who::is_human(&v.by) && v.seq > handed));
        if st.decisions.len() != before {
            let _ = write_atomic(&self.state_path(&d.id), serde_json::to_string_pretty(&st).unwrap().as_bytes());
        }
    }

    /// The store's whole-document write: create or replace, whichever applies. The CLI never
    /// calls it for a document someone named: `margin new` goes through `create` and
    /// `margin put` / a whole-document `margin apply` through `replace`, which each allow one
    /// of the two. What is left here is `margin demo` (which resets its own artifact) and code
    /// that has just loaded the document it writes back. With `base_rev`, refuse if the stored
    /// rev differs.
    pub fn put(&self, d: Doc, base_rev: Option<u64>) -> Result<u64, String> {
        let _l = self.lock().map_err(|e| e.to_string())?;
        self.put_locked(d, base_rev)
    }

    /// `margin new`: create an artifact, refusing one that exists. Decided under the lock, so
    /// of two concurrent creates exactly one succeeds and the other is told it exists (decided
    /// outside it, both took the create branch and the second replaced the first's document:
    /// 2026-11-09 review, 39 of 40 parallel runs).
    pub fn create(&self, d: Doc) -> Result<u64, String> {
        let _l = self.lock().map_err(|e| e.to_string())?;
        if self.exists(&d.id) {
            let a = &d.id;
            let rev = self.load(a).map(|o| format!(" (rev {})", o.rev)).unwrap_or_default();
            return Err(format!("{a} already exists{rev}; change it with margin apply/add/set, or remove it with margin rm {a} --yes"));
        }
        self.put_locked(d, None)
    }

    /// `margin put` and a whole-document `margin apply`: replace the document of an artifact
    /// that exists. A missing one is an error that names `margin new`, never a create.
    pub fn replace(&self, d: Doc, base_rev: Option<u64>) -> Result<u64, String> {
        let _l = self.lock().map_err(|e| e.to_string())?;
        if !self.exists(&d.id) {
            return Err(self.missing(&d.id));
        }
        self.put_locked(d, base_rev)
    }

    fn put_locked(&self, mut d: Doc, base_rev: Option<u64>) -> Result<u64, String> {
        if !crate::util::valid_id(&d.id) {
            return Err(format!("bad artifact id {:?}", d.id));
        }
        if d.v != 1 {
            return Err(format!("document version {}: this margin reads v 1", d.v));
        }
        d.normalize()?;
        d.blocks.iter().try_for_each(crate::doc::check_block)?;
        // a doc that exists but won't parse is not a missing doc: replacing it would reset the rev
        // and skip archiving what was there
        let old = if self.doc_path(&d.id).exists() { Some(self.load(&d.id)?) } else { None };
        check_base(old.as_ref(), base_rev)?;
        // a whole-document put must leave every feed-owned component exactly as the feed left it
        let claims = self.claims_strict(&d.id)?;
        if !claims.is_empty() {
            let mut ids: Vec<String> = d.ids();
            if let Some(o) = &old {
                ids.extend(o.ids());
            }
            for id in ids {
                if let Some(c) = claims.iter().find(|c| owns(&c.owns, &id)) {
                    if old.as_ref().and_then(|o| o.find(&id)) != d.find(&id) {
                        return Err(feed_owned_msg(&format!("{id:?}"), c) + "; `put` must carry feed components through unchanged, so prefer id-addressed ops");
                    }
                }
            }
            if let (Some(c), Some(o)) = (claims.iter().find(|c| c.owns == "*"), &old) {
                if o.title != d.title {
                    return Err(feed_owned_msg("the title", c));
                }
            }
        }
        let rev = self.save_doc_locked(&mut d, old.as_ref(), &self.author)?;
        Ok(rev)
    }

    /// The feeds that own part of artifact `a` (from feeds.json; paused feeds still own theirs).
    pub fn claims(&self, a: &str) -> Vec<Claim> {
        crate::feed::Registry::load(&self.dir()).claims(a)
    }

    /// Like `claims`, for Claude's and sulin's writes: a feeds.json that won't parse is an error,
    /// not "nothing is owned" (a write into a feed's ids would land, and the feed would fight it).
    pub fn claims_strict(&self, a: &str) -> Result<Vec<Claim>, String> {
        Ok(crate::feed::Registry::load_strict(&self.dir())?.claims(a))
    }

    /// A feed's write: groups of ops (one group per stdout line), each applied all-or-nothing, so
    /// one bad line is rejected and logged without losing its neighbours. `on_change` ops (the
    /// feed's "last write" stamp) apply only when the groups changed content, so a feed that
    /// re-prints the same state costs no revision. A feed updates its artifact and never creates
    /// it: a missing one fails the write with the message that names `margin new`.
    pub fn feed_write(&self, a: &str, groups: &[Vec<Op>], w: &Writer, on_change: &[Op]) -> (Option<u64>, Vec<String>) {
        let mut errs = Vec::new();
        let _l = match self.lock() {
            Ok(l) => l,
            Err(e) => return (None, vec![e.to_string()]),
        };
        let old = match self.load(a) {
            Ok(d) => d,
            Err(e) => return (None, vec![e]),
        };
        let mut d = old.clone();
        let claims = self.claims(a);
        let base_blocks = d.blocks.clone();
        let base_title = d.title.clone();
        for g in groups {
            let mut t = d.clone();
            let r: Result<(), String> = g.iter().try_for_each(|op| {
                check_owner(op, &t, w, &claims)?;
                t.apply(op)
            });
            match r {
                Ok(()) => d = t,
                Err(e) => errs.push(e),
            }
        }
        if d.blocks == base_blocks && d.title == base_title {
            return (None, errs);
        }
        for op in on_change {
            let mut t = d.clone();
            if check_owner(op, &t, w, &claims).and_then(|_| t.apply(op)).is_ok() {
                d = t;
            }
        }
        match self.save_doc_locked(&mut d, Some(&old), &w.by()) {
            Ok(rev) => (Some(rev), errs),
            Err(e) => {
                errs.push(e);
                (None, errs)
            }
        }
    }

    /// Apply ops to an artifact that exists, atomically: all land or none do (a missing artifact
    /// is `missing`'s error, never a create). A todo's `status` is a shared field (design §2):
    /// it goes to state.json with `by: claude`, and overruling sulin's own tick is logged.
    pub fn patch(&self, a: &str, ops: &[Op], base_rev: Option<u64>) -> Result<u64, String> {
        let _l = self.lock().map_err(|e| e.to_string())?;
        self.patch_locked(a, ops, base_rev)
    }

    fn patch_locked(&self, a: &str, ops: &[Op], base_rev: Option<u64>) -> Result<u64, String> {
        let old = self.load(a)?;
        check_base(Some(&old), base_rev)?;
        let claims = self.claims_strict(a)?;
        let mut d = old.clone();
        let mut status_writes = Vec::new();
        for op in ops {
            check_owner(op, &d, &Writer::Claude, &claims)?;
            if let Op::Set { id, field, value } = op {
                if field == "status" && d.find(id).map(|b| b.kind == "todo").unwrap_or(false) {
                    crate::doc::check_field(crate::doc::kind("todo").unwrap(), field, value).map_err(|e| format!("{id}: {e}"))?;
                    status_writes.push((id.clone(), value.clone()));
                    continue;
                }
            }
            d.apply(op)?;
            // taking a question's default records when, so the card can say "took cli at 21:40"
            if let Op::Set { id, field, value } = op {
                if field == "applied" && value == &json!(true) && d.find(id).map(|b| b.kind == "question" && b.s("applied_at").is_empty()).unwrap_or(false) {
                    d.apply(&Op::Set { id: id.clone(), field: "applied_at".into(), value: json!(now_rfc3339()) })?;
                }
            }
        }
        // before the doc is saved, so a state.json that won't parse fails the whole write
        if !status_writes.is_empty() {
            self.state_rw(a)?;
        }
        let content_changed = d.blocks != old.blocks || d.title != old.title;
        let rev = if content_changed { self.save_doc_locked(&mut d, Some(&old), &self.author)? } else { old.rev };
        if !status_writes.is_empty() {
            let mut st = self.state_rw(a)?;
            for (id, v) in status_writes {
                if let Some(prev) = st.decisions.get(&id) {
                    if crate::who::is_human(&prev.by) && !crate::who::is_human(&self.author) && prev.value != v {
                        self.append_event_locked(a, &id, "overruled", v.clone(), &self.author, "cli", rev)?;
                    }
                }
                // when the step started and finished, kept beside its status under `<id>#time`,
                // so the pane shows how long it took without reading history and a status write
                // still leaves the content rev alone. `started` is stamped once, on the first
                // doing; `done` once per finish (a step set back to doing loses it)
                let key = step_time_key(&id);
                let mut t = st.decisions.get(&key).map(|d| d.value.clone()).filter(Value::is_object).unwrap_or_else(|| json!({}));
                let was = t.clone();
                match v.as_str() {
                    Some("doing") => {
                        t.as_object_mut().unwrap().remove("done");
                        if t["started"].is_null() {
                            t["started"] = json!(now_rfc3339());
                        }
                    }
                    Some("done") if t["done"].is_null() => t["done"] = json!(now_rfc3339()),
                    _ => {}
                }
                if t != was {
                    st.decisions.insert(key, Decision { kind: "time".into(), value: t, by: self.author.clone(), at: now_rfc3339(), reason: None, via: Some("cli".into()), seq: 0, under: None });
                }
                st.decisions.insert(
                    id.clone(),
                    Decision { kind: "status".into(), value: v, by: self.author.clone(), at: now_rfc3339(), reason: None, via: Some("cli".into()), seq: 0, under: None },
                );
            }
            write_atomic(&self.state_path(a), serde_json::to_string_pretty(&st).unwrap().as_bytes()).map_err(|e| e.to_string())?;
        }
        Ok(rev)
    }

    /// sulin's note on a row (row notes, r3): a reaction (more, less, keep, disagree, why) and
    /// optionally the text he selected. Keyed `<row>#note#<quote hash>`, so one row holds a note
    /// per quote and a reaction on the same quote replaces the last. `reaction` "" takes it back.
    pub fn note(&self, a: &str, row: &str, reaction: &str, quote: &str, via: &str) -> Result<Event, String> {
        let _l = self.lock().map_err(|e| e.to_string())?;
        let d = self.load(a)?;
        let b = d.find(row).ok_or_else(|| format!("no component {row:?} in {a}"))?;
        if !reaction.is_empty() && !NOTE_REACTIONS.contains(&reaction) {
            return Err(format!("a note is one of {}, not {reaction:?}", NOTE_REACTIONS.join(" ")));
        }
        let key = note_key(row, quote);
        let (kind, value) = if reaction.is_empty() {
            ("unnote", Value::Null)
        } else {
            ("note", json!({"r": reaction, "quote": quote, "h": row_hash(b)}))
        };
        let mut st = self.state_rw(a)?;
        let ev = self.append_event_locked(a, &key, kind, value.clone(), crate::who::HUMAN, via, d.rev)?;
        if reaction.is_empty() {
            st.decisions.remove(&key);
        } else {
            st.decisions.insert(key, Decision { kind: "note".into(), value, by: crate::who::HUMAN.into(), at: ev.at.clone(), reason: None, via: Some(via.into()), seq: ev.seq, under: None });
        }
        write_atomic(&self.state_path(a), serde_json::to_string_pretty(&st).unwrap().as_bytes()).map_err(|e| e.to_string())?;
        Ok(ev)
    }

    /// sulin's decision (a tick, pick, strike, verdict) from the pane or from `margin select`:
    /// write state.json and append the event, under one lock.
    pub fn decide(&self, a: &str, id: &str, kind: &str, value: Value, via: &str, reason: Option<String>) -> Result<Event, String> {
        let _l = self.lock().map_err(|e| e.to_string())?;
        let d = self.load(a)?;
        if d.find(id).is_none() {
            return Err(format!("no component {id:?} in {a}"));
        }
        if let Some(c) = self.claims_strict(a)?.iter().find(|c| owns(&c.owns, id)) {
            return Err(format!("{id:?} is kept current by feed `{}`; its components are read-only", c.feed));
        }
        let mut st = self.state_rw(a)?;
        let ev = self.append_event_locked(a, id, kind, value.clone(), crate::who::HUMAN, via, d.rev)?;
        let prev = st.decisions.get(id).cloned();
        let mut dec = Decision { kind: kind.into(), value, by: crate::who::HUMAN.into(), at: ev.at.clone(), reason, via: Some(via.into()), seq: ev.seq, under: None };
        if kind == "tick" {
            dec.kind = "status".into();
            // keep Claude's status under the mark: his own, or the one already kept under his
            dec.under = prev.as_ref().and_then(|p| if crate::who::is_human(&p.by) { p.under.clone() } else if p.kind == "status" { Some(p.value.clone()) } else { None });
        }
        let key = id.to_string();
        if kind == "untick" {
            // the step is Claude's again, at the status Claude last gave it
            match prev.and_then(|p| p.under) {
                Some(v) => {
                    st.decisions.insert(key, Decision { kind: "status".into(), value: v, by: "claude".into(), at: ev.at.clone(), reason: None, via: None, seq: 0, under: None });
                }
                None => {
                    st.decisions.remove(&key);
                }
            }
        } else if kind == "unstrike" || kind == "unverdict" || kind == "unhold" || kind == "untalk" {
            st.decisions.remove(&key);
        } else {
            st.decisions.insert(key, dec);
        }
        write_atomic(&self.state_path(a), serde_json::to_string_pretty(&st).unwrap().as_bytes()).map_err(|e| e.to_string())?;
        Ok(ev)
    }

    /// sulin's answer to Claude's proposal on `id` (`margin propose`): one event (kind `proposal`,
    /// value `{accepted, verb, value}`) and a resolution keyed `<id>#proposal` holding the
    /// proposal's `at`, so that proposal stops showing while a newer one would show again. The
    /// accepted change itself goes through `decide` like any of his decisions (the caller does it
    /// first, so a proposal that no longer applies stays open).
    pub fn resolve_proposal(&self, a: &str, id: &str, proposal: &Value, accepted: bool, via: &str) -> Result<Event, String> {
        let _l = self.lock().map_err(|e| e.to_string())?;
        let d = self.load(a)?;
        let mut st = self.state_rw(a)?;
        let v = json!({"accepted": accepted, "verb": proposal["verb"], "value": proposal["value"]});
        let ev = self.append_event_locked(a, id, "proposal", v, crate::who::HUMAN, via, d.rev)?;
        let dec = Decision { kind: "proposal".into(), value: json!({"at": proposal["at"], "accepted": accepted}), by: crate::who::HUMAN.into(), at: ev.at.clone(), reason: None, via: Some(via.into()), seq: ev.seq, under: None };
        st.decisions.insert(format!("{id}#proposal"), dec);
        write_atomic(&self.state_path(a), serde_json::to_string_pretty(&st).unwrap().as_bytes()).map_err(|e| e.to_string())?;
        Ok(ev)
    }

    /// A passive or lifecycle event that changes no decision (fold, seen, open).
    /// sulin takes a step as his own (`o` in the pane), or hands it back: a decision keyed
    /// `<id>#own`, so it never collides with the step's status in its own slot.
    pub fn decide_own(&self, a: &str, id: &str, own: bool, via: &str) -> Result<Event, String> {
        let _l = self.lock().map_err(|e| e.to_string())?;
        let d = self.load(a)?;
        if d.find(id).map(|b| b.kind != "todo").unwrap_or(true) {
            return Err(format!("{id} isn't a todo in {a}"));
        }
        let mut st = self.state_rw(a)?;
        let ev = self.append_event_locked(a, id, if own { "own" } else { "unown" }, json!(own), crate::who::HUMAN, via, d.rev)?;
        st.decisions.insert(format!("{id}#own"), Decision { kind: "own".into(), value: json!(own), by: crate::who::HUMAN.into(), at: ev.at.clone(), reason: None, via: Some(via.into()), seq: ev.seq, under: None });
        write_atomic(&self.state_path(a), serde_json::to_string_pretty(&st).unwrap().as_bytes()).map_err(|e| e.to_string())?;
        Ok(ev)
    }

    /// The stop signal: sulin pressed stop in the pane. While the flag is up, every tool call in
    /// this project is refused (the PreToolUse hook), subagents' included; it drops when he next
    /// sends a message, since then he's talking. The event keeps it in the log.
    pub fn stop(&self, a: &str, via: &str) -> Result<(), String> {
        let rev = self.load(a).map(|d| d.rev).unwrap_or(0);
        {
            let _l = self.lock().map_err(|e| e.to_string())?;
            self.append_event_locked(a, "", "stop", json!(true), crate::who::HUMAN, via, rev)?;
        }
        // the sessions linked to this pane (and so their subagents, which share their ids); a
        // pane nobody is linked to stops the project
        let sessions: Vec<String> = crate::link::all(self).into_iter().filter(|l| l.art == a).map(|l| l.session).collect();
        write_atomic(&self.dir().join("stop"), serde_json::to_string(&json!({"at": now_rfc3339(), "sessions": sessions})).unwrap().as_bytes()).map_err(|e| e.to_string())
    }

    /// Is the stop flag up for this session? (Any session, when the stop named none.)
    pub fn stopped_for(&self, session: &str) -> bool {
        let Ok(b) = fs::read(self.dir().join("stop")) else { return false };
        let v: Value = serde_json::from_slice(&b).unwrap_or(Value::Null);
        match v["sessions"].as_array() {
            Some(a) if !a.is_empty() => a.iter().any(|x| x.as_str() == Some(session)),
            _ => true,
        }
    }

    /// Stop one subagent (its row's stop in the work overview): its tool calls are refused until
    /// sulin's next message; the main session and the other subagents run on.
    pub fn stop_agent(&self, a: &str, agent: &str, via: &str) -> Result<(), String> {
        let rev = self.load(a).map(|d| d.rev).unwrap_or(0);
        let _l = self.lock().map_err(|e| e.to_string())?;
        self.append_event_locked(a, "", "stop-agent", json!(agent), crate::who::HUMAN, via, rev)?;
        let p = self.dir().join("stop-agents");
        let mut ids: Vec<String> = fs::read(&p).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        if !ids.iter().any(|x| x == agent) {
            ids.push(agent.to_string());
        }
        write_atomic(&p, serde_json::to_string(&ids).unwrap().as_bytes()).map_err(|e| e.to_string())
    }

    pub fn agent_stopped(&self, agent: &str) -> bool {
        fs::read(self.dir().join("stop-agents")).ok().and_then(|b| serde_json::from_slice::<Vec<String>>(&b).ok()).map(|v| v.iter().any(|x| x == agent)).unwrap_or(false)
    }

    pub fn stopped(&self) -> bool {
        self.dir().join("stop").exists()
    }

    pub fn clear_stop(&self) {
        let _ = fs::remove_file(self.dir().join("stop"));
        let _ = fs::remove_file(self.dir().join("stop-agents"));
    }

    pub fn log(&self, a: &str, id: &str, kind: &str, value: Value, via: &str) -> Result<Event, String> {
        let _l = self.lock().map_err(|e| e.to_string())?;
        let rev = self.load(a).map(|d| d.rev).unwrap_or(0);
        self.append_event_locked(a, id, kind, value, crate::who::HUMAN, via, rev)
    }

    /// A feed's lifecycle event (start, exit, restart, stop), `by: feed:<name>`. Passive: it never
    /// reaches Claude's prompt; `margin events --kind feed` lists them.
    pub fn log_feed(&self, a: &str, feed: &str, value: Value) -> Result<Event, String> {
        let _l = self.lock().map_err(|e| e.to_string())?;
        let rev = self.load(a).map(|d| d.rev).unwrap_or(0);
        self.append_event_locked(a, "", "feed", value, &format!("feed:{feed}"), "daemon", rev)
    }

    fn append_event_locked(&self, a: &str, id: &str, kind: &str, value: Value, by: &str, via: &str, rev: u64) -> Result<Event, String> {
        let seq = self.last_seq() + 1;
        let ev = Event { v: 1, seq, at: now_rfc3339(), artifact: a.into(), id: id.into(), kind: kind.into(), value, by: by.into(), via: via.into(), rev };
        let mut line = serde_json::to_string(&ev).unwrap() + "\n";
        // a crash mid-append leaves a torn last line: end it first, or this event would join it,
        // read as garbage too, and have its seq handed out again
        if fs::read(self.events_path()).map(|b| b.last().map(|c| *c != b'\n').unwrap_or(false)).unwrap_or(false) {
            line.insert(0, '\n');
        }
        let mut f = fs::OpenOptions::new().create(true).append(true).open(self.events_path()).map_err(|e| e.to_string())?;
        // one write, so a line is whole or absent short of a crash inside the call
        f.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
        Ok(ev)
    }

    pub fn events(&self) -> Vec<Event> {
        String::from_utf8_lossy(&fs::read(self.events_path()).unwrap_or_default()).lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
    }

    pub fn last_seq(&self) -> u64 {
        // the file is small and append-only. The highest seq on any parseable line, not the last
        // line's: a torn or hand-edited line is skipped and never lowers what's handed out next.
        // Lossy, so a torn line of bad UTF-8 doesn't read as an empty log and restart seq at 1.
        String::from_utf8_lossy(&fs::read(self.events_path()).unwrap_or_default())
            .lines()
            .filter_map(|l| serde_json::from_str::<Event>(l).ok())
            .map(|e| e.seq)
            .max()
            .unwrap_or(0)
    }

    /// The last event seq handed to `session`. A session margin hasn't seen starts where the
    /// furthest-along session is: actions no session has read yet still reach it, while ones
    /// already handed to another session aren't replayed as binding (a fresh session got four
    /// old picks, two of them for one question, 2026-11-06).
    pub fn delivered(&self, session: &str) -> u64 {
        fs::read_to_string(self.delivered_path(session))
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or_else(|| self.delivered_max())
    }

    fn delivered_path(&self, session: &str) -> PathBuf {
        self.dir().join("delivered").join(session_key(session))
    }

    /// A hook's first sight of `session`: write its cursor down where `delivered` puts a new
    /// session. Without the file that fallback is taken afresh on every read, so it slides past
    /// whatever another session reads in the meantime and this one never hears it.
    /// `create_new`, not a rename: it must never replace a cursor a concurrent hook just moved
    /// (an empty file left by a crash in between reads as the fallback, i.e. the same value).
    pub fn first_seen(&self, session: &str) {
        let p = self.delivered_path(session);
        if p.exists() || fs::create_dir_all(self.dir().join("delivered")).is_err() {
            return;
        }
        let at = self.delivered_max();
        if let Ok(mut f) = fs::OpenOptions::new().write(true).create_new(true).open(&p) {
            let _ = f.write_all(at.to_string().as_bytes());
        }
    }

    pub fn set_delivered(&self, session: &str, seq: u64) {
        let _ = write_atomic(&self.delivered_path(session), seq.to_string().as_bytes());
    }

    /// Where the tool hooks record what each Claude session is doing (one file per session or
    /// subagent), outside doc.json so a tool call bumps no revision and breaks no `--base-rev`.
    pub fn activity_dir(&self) -> PathBuf {
        self.dir().join("activity")
    }

    pub fn set_activity(&self, a: &Activity) {
        let key = if a.agent.is_empty() { a.session.clone() } else { format!("{}.{}", a.session, a.agent) };
        let key: String = key.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '_' }).collect();
        let _ = fs::create_dir_all(self.activity_dir());
        if let Ok(j) = serde_json::to_vec(a) {
            let _ = write_atomic(&self.activity_dir().join(format!("{key}.json")), &j);
        }
    }

    /// What the main session `session` was last recorded doing.
    pub fn activity(&self, session: &str) -> Option<Activity> {
        let key: String = session.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '_' }).collect();
        serde_json::from_slice(&fs::read(self.activity_dir().join(format!("{key}.json"))).ok()?).ok()
    }

    /// Every recorded activity, newest first.
    pub fn activities(&self) -> Vec<Activity> {
        let mut v: Vec<Activity> = fs::read_dir(self.activity_dir())
            .map(|rd| rd.filter_map(|e| e.ok()).filter_map(|e| serde_json::from_slice(&fs::read(e.path()).ok()?).ok()).collect())
            .unwrap_or_default();
        v.sort_by(|a: &Activity, b: &Activity| b.since.cmp(&a.since));
        v
    }

    /// Highest seq any session has been handed (for "Claude read it").
    pub fn delivered_max(&self) -> u64 {
        fs::read_dir(self.dir().join("delivered"))
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter_map(|e| fs::read_to_string(e.path()).ok()?.trim().parse::<u64>().ok())
                    .max()
                    .unwrap_or(0)
            })
            .unwrap_or(0)
    }

    pub fn list(&self) -> Vec<Doc> {
        let mut v: Vec<Doc> = fs::read_dir(self.dir())
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter(|e| e.path().join("doc.json").exists())
                    .filter_map(|e| self.load(&e.file_name().to_string_lossy()).ok())
                    .collect()
            })
            .unwrap_or_default();
        v.sort_by(|a, b| b.updated.cmp(&a.updated));
        v
    }

    /// Move one artifact's directory (document, decisions, history, views) into `.archive/`,
    /// under `<id>-<date>` (with `-2`, `-3`… when that name is taken). The store never deletes:
    /// `margin rm <art>` and `margin gc` both end here, and moving the directory back restores it.
    pub fn archive(&self, a: &str) -> Result<PathBuf, String> {
        let _l = self.lock().map_err(|e| e.to_string())?;
        self.archive_locked(a)
    }

    fn archive_locked(&self, a: &str) -> Result<PathBuf, String> {
        if !crate::util::valid_id(a) || !self.exists(a) {
            return Err(self.missing(a));
        }
        let dir = self.dir().join(".archive");
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let base = format!("{a}-{}", &now_rfc3339()[..10]);
        let dest = (1..).map(|n| dir.join(if n == 1 { base.clone() } else { format!("{base}-{n}") })).find(|p| !p.exists()).unwrap();
        fs::rename(self.art(a), &dest).map_err(|e| format!("archive {a}: {e}"))?;
        Ok(dest)
    }

    /// Move artifacts untouched for `days` into `.archive/` (never deletes). Returns moved names.
    pub fn archive_stale(&self, days: u32) -> Vec<String> {
        if days == 0 {
            return vec![];
        }
        let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(days as u64 * 86400);
        let mut moved = Vec::new();
        for d in self.list() {
            let newest = ["doc.json", "state.json"]
                .iter()
                .filter_map(|f| fs::metadata(self.art(&d.id).join(f)).ok()?.modified().ok())
                .chain(fs::read_dir(self.art(&d.id).join("view")).into_iter().flatten().filter_map(|e| e.ok()?.metadata().ok()?.modified().ok()))
                .max();
            if newest.map(|t| t < cutoff).unwrap_or(false) && self.archive(&d.id).is_ok() {
                moved.push(d.id.clone());
            }
        }
        moved
    }
}

fn check_base(old: Option<&Doc>, base: Option<u64>) -> Result<(), String> {
    if let Some(b) = base {
        let cur = old.map(|d| d.rev).unwrap_or(0);
        if cur != b {
            return Err(format!(
                "stale base: you edited rev {b} but the artifact is at rev {cur}; run `margin show` and redo the change on rev {cur}"
            ));
        }
    }
    Ok(())
}

/// Every project directory under the store home (for resolving tokens without a known project).
pub fn projects(home: &Path) -> Vec<String> {
    fs::read_dir(home)
        .map(|rd| rd.filter_map(|e| e.ok()).filter(|e| e.path().is_dir()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default()
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn owns_is_a_namespace_not_a_string_prefix() {
        for id in ["ci", "ci-1", "ci_x", "ci-a-b"] {
            assert!(owns("ci", id), "{id}");
        }
        for id in ["city", "circle", "cite", "c", ""] {
            assert!(!owns("ci", id), "{id}");
        }
        assert!(owns("ag-", "ag-feed") && !owns("ag-", "ag"));
        assert!(owns("*", "anything") && !owns("", "x"));
    }

    pub fn temp_store() -> Store {
        let d = std::env::temp_dir().join(format!("margin-test-{}-{}", std::process::id(), rand_suffix()));
        Store::new(d, "proj".into())
    }
    fn rand_suffix() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        N.fetch_add(1, Ordering::Relaxed) * 7919 + (crate::util::epoch_now() * 1e6) as u64 % 100000
    }
    pub fn plan() -> Doc {
        serde_json::from_value(json!({"id": "plan", "title": "fix", "blocks": [
            {"type": "section", "id": "steps", "title": "Steps", "children": [
                {"type": "todo", "id": "t1", "text": "one"}, {"type": "todo", "id": "t2", "text": "two"}]},
            {"type": "question", "id": "Q1", "text": "MCP?", "options": ["cli", "mcp"], "default": "cli"}
        ]}))
        .unwrap()
    }

    #[test]
    fn revs_history_and_base_check() {
        let s = temp_store();
        assert_eq!(s.put(plan(), None).unwrap(), 1);
        let set = Op::Set { id: "t1".into(), field: "note".into(), value: json!("x") };
        assert_eq!(s.patch("plan", &[set.clone()], Some(1)).unwrap(), 2);
        let e = s.patch("plan", &[set], Some(1)).unwrap_err();
        assert!(e.contains("stale base"), "{e}");
        let hist = fs::read_to_string(s.art("plan").join("history.jsonl")).unwrap();
        assert_eq!(hist.lines().count(), 1);
        // a failing op in a batch leaves the doc untouched
        let bad = [Op::Set { id: "t1".into(), field: "note".into(), value: json!("y") }, Op::Rm { id: "zz".into() }];
        assert!(s.patch("plan", &bad, None).is_err());
        assert_eq!(s.load("plan").unwrap().find("t1").unwrap().s("note"), "x");
        fs::remove_dir_all(&s.home).ok();
    }

    #[test]
    fn decisions_are_sulins_and_status_is_shared() {
        let s = temp_store();
        s.put(plan(), None).unwrap();
        let ev = s.decide("plan", "Q1", "pick", json!("mcp"), "select", None).unwrap();
        assert_eq!(ev.seq, 1);
        assert_eq!(s.state("plan").decisions["Q1"].value, json!("mcp"));
        s.decide("plan", "t1", "tick", json!("done"), "key", None).unwrap();
        // Claude marks t1 doing: lands in state, and logs that it overruled sulin
        s.patch("plan", &[Op::Set { id: "t1".into(), field: "status".into(), value: json!("doing") }], None).unwrap();
        let st = s.state("plan");
        assert_eq!(st.decisions["t1"].by, "claude");
        let kinds: Vec<String> = s.events().into_iter().map(|e| e.kind).collect();
        assert_eq!(kinds, vec!["pick", "tick", "overruled"]);
        // status writes don't bump the content rev
        assert_eq!(s.load("plan").unwrap().rev, 1);
        assert!(s.decide("plan", "nope", "pick", json!(1), "select", None).is_err());
        fs::remove_dir_all(&s.home).ok();
    }

    #[test]
    fn delivered_cursors() {
        let s = temp_store();
        s.put(plan(), None).unwrap();
        s.decide("plan", "Q1", "pick", json!("mcp"), "select", None).unwrap();
        assert_eq!(s.delivered("abc"), 0);
        s.set_delivered("abc", 1);
        assert_eq!(s.delivered("abc"), 1);
        assert_eq!(s.delivered_max(), 1);
        fs::remove_dir_all(&s.home).ok();
    }

    /// Clearing sulin's mark gives the step back at the status Claude last set, not the doc's
    /// original one (one decision slot per id: a clear used to lose Claude's status).
    #[test]
    fn clear_restores_claudes_status() {
        let s = temp_store();
        s.put(plan(), None).unwrap();
        s.patch("plan", &[Op::Set { id: "t1".into(), field: "status".into(), value: json!("doing") }], None).unwrap();
        s.decide("plan", "t1", "tick", json!("next"), "key", None).unwrap();
        s.decide("plan", "t1", "tick", json!("skipped"), "key", None).unwrap();
        assert_eq!(s.state("plan").decisions["t1"].under, Some(json!("doing")), "kept under both marks");
        s.decide("plan", "t1", "untick", Value::Null, "key", None).unwrap();
        let d = &s.state("plan").decisions["t1"];
        assert_eq!((d.kind.as_str(), d.value.clone(), d.by.as_str()), ("status", json!("doing"), "claude"));
    }

    #[test]
    fn removing_a_row_drops_claudes_status_but_keeps_sulins() {
        let s = temp_store();
        s.put(plan(), None).unwrap();
        s.patch("plan", &[Op::Set { id: "t1".into(), field: "status".into(), value: json!("done") }], None).unwrap();
        s.decide("plan", "Q1", "pick", json!("mcp"), "select", None).unwrap();
        s.patch("plan", &[Op::Rm { id: "t1".into() }, Op::Rm { id: "Q1".into() }], None).unwrap();
        let st = s.state("plan");
        assert!(!st.decisions.contains_key("t1"), "Claude's status on a removed row is gone");
        assert!(st.decisions.contains_key("Q1"), "sulin's pick stays as an orphan until Claude has read it");
        // once Claude has been handed it, the next save lets it go
        let seq = st.decisions["Q1"].seq;
        s.set_delivered("sess", seq);
        s.patch("plan", &[Op::Set { id: "t2".into(), field: "text".into(), value: json!("touch") }], None).unwrap();
        assert!(!s.state("plan").decisions.contains_key("Q1"));
    }

    #[test]
    fn a_note_waits_until_claude_rewrites_its_row() {
        let s = temp_store();
        s.put(plan(), None).unwrap();
        s.note("plan", "t1", "less", "", "mouse").unwrap();
        s.note("plan", "t1", "disagree", "a quote", "mouse").unwrap();
        let d = s.load("plan").unwrap();
        assert_eq!(notes_on(&d, &s.state("plan"), "t1").len(), 2, "one per quote");
        assert!(s.note("plan", "t1", "shrug", "", "mouse").is_err());
        // taken back
        s.note("plan", "t1", "", "", "mouse").unwrap();
        assert_eq!(notes_on(&d, &s.state("plan"), "t1").len(), 1);
        // Claude rewrites the row: the note is answered and goes
        s.patch("plan", &[Op::Set { id: "t1".into(), field: "text".into(), value: json!("reworded") }], None).unwrap();
        let d = s.load("plan").unwrap();
        assert!(notes_on(&d, &s.state("plan"), "t1").is_empty());
        assert!(!s.state("plan").decisions.keys().any(|k| k.contains("#note#")), "pruned from state too");
    }

    #[test]
    fn a_strike_names_the_steps_resting_on_it() {
        let s = temp_store();
        let d: Doc = serde_json::from_value(json!({"id": "plan", "title": "p", "blocks": [
            {"type": "claim", "id": "A1", "text": "one process"},
            {"type": "todo", "id": "t1", "text": "rests on it", "rests_on": ["A1"]},
            {"type": "todo", "id": "t2", "text": "done already", "status": "done", "rests_on": ["A1"]}]})).unwrap();
        s.put(d, None).unwrap();
        let ev = s.decide("plan", "A1", "strike", json!(true), "mouse", None).unwrap();
        let line = crate::hook::describe(&s, &ev);
        assert!(line.contains("t1 “rests on it”") && line.contains("re-plan"), "{line}");
        assert!(!line.contains("t2"), "a finished step isn't news: {line}");
    }

    /// `create` makes an artifact once; a second create is refused and says how to change or
    /// remove the one there, and what is there is left exactly as it was.
    #[test]
    fn new_creates_and_refuses_a_second_time() {
        let s = temp_store();
        assert_eq!(s.create(Doc::empty("board")).unwrap(), 1);
        let d = s.load("board").unwrap();
        assert_eq!((d.title.as_str(), d.rev, d.blocks.len()), ("board", 1, 0));
        let add = |id: &str| Op::Add { parent: None, block: serde_json::from_value(json!({"type": "todo", "id": id, "text": "x"})).unwrap(), before: None, after: None };
        assert_eq!(s.patch("board", &[add("t1")], None).unwrap(), 2);
        let e = s.create(Doc { title: "Other".into(), ..Doc::empty("board") }).unwrap_err();
        assert_eq!(e, "board already exists (rev 2); change it with margin apply/add/set, or remove it with margin rm board --yes");
        let d = s.load("board").unwrap();
        assert_eq!((d.title.as_str(), d.rev, d.find("t1").is_some()), ("board", 2, true), "the refused create changed nothing");
        // a create carries its content, checked as any write is
        assert_eq!(s.create(plan()).unwrap(), 1);
        assert!(s.create(Doc { blocks: vec![serde_json::from_value(json!({"type": "nope", "id": "x"})).unwrap()], ..Doc::empty("bad") }).is_err() && !s.exists("bad"));
        fs::remove_dir_all(&s.home).ok();
    }

    /// Every update refuses an artifact that isn't there, with one message that names the
    /// command that creates it and, for a near miss, the artifact that was probably meant.
    /// Nothing is created on the way.
    #[test]
    fn updates_refuse_a_missing_artifact_and_name_the_nearest() {
        let s = temp_store();
        s.create(plan()).unwrap();
        s.create(Doc::empty("reviews")).unwrap();
        let plain = "no artifact \"scratch\" in project proj: create it with margin new scratch";
        assert_eq!(s.missing("scratch"), plain);
        assert_eq!(s.missing("paln"), "no artifact \"paln\" in project proj: create it with margin new paln (did you mean \"plan\"?)");
        assert!(s.missing("review").ends_with("(did you mean \"reviews\"?)"));
        assert!(s.missing("plans2").ends_with("(did you mean \"plan\"?)") && !s.missing("planning").contains("did you mean"), "within two edits, no further");
        let set = Op::Set { id: "t1".into(), field: "text".into(), value: json!("x") };
        assert_eq!(s.patch("scratch", &[set.clone()], None).unwrap_err(), plain);
        assert_eq!(s.replace(Doc::empty("scratch"), None).unwrap_err(), plain);
        assert_eq!(s.load("scratch").unwrap_err(), plain);
        assert_eq!(s.archive("scratch").unwrap_err(), plain);
        let w = Writer::Feed { name: "f".into(), owns: "f".into() };
        assert_eq!(s.feed_write("scratch", &[vec![set]], &w, &[]), (None, vec![plain.to_string()]));
        assert!(!s.exists("scratch") && !s.art("scratch").exists(), "a refused write leaves no trace");
        // replace is the update of a whole document
        assert_eq!(s.replace(Doc { title: "Reviews".into(), ..Doc::empty("reviews") }, None).unwrap(), 2);
        fs::remove_dir_all(&s.home).ok();
    }

    /// Two creators of one artifact: each sees no doc.json, and deciding outside the lock let
    /// the second's document replace the first's with both reporting success. Exactly one
    /// create wins; the rest are told it exists. Writers to the artifact once it exists all land.
    #[test]
    fn concurrent_new_keeps_exactly_one() {
        for round in 0..20 {
            let s = temp_store();
            let block = |i: usize| -> crate::doc::Block { serde_json::from_value(json!({"type": "todo", "id": format!("c{i}"), "text": "mine"})).unwrap() };
            let hs: Vec<_> = (0..8)
                .map(|i| {
                    let st = Store::new(s.home.clone(), s.project.clone());
                    std::thread::spawn(move || st.create(Doc { blocks: vec![block(i)], ..Doc::empty("race") }).map(|rev| (i, rev)))
                })
                .collect();
            let rs: Vec<Result<(usize, u64), String>> = hs.into_iter().map(|h| h.join().unwrap()).collect();
            let won: Vec<&(usize, u64)> = rs.iter().filter_map(|r| r.as_ref().ok()).collect();
            assert_eq!(won.len(), 1, "round {round}: exactly one create succeeds: {rs:?}");
            assert!(rs.iter().filter_map(|r| r.as_ref().err()).all(|e| e.starts_with("race already exists (rev 1)")), "round {round}: {rs:?}");
            let d = s.load("race").unwrap();
            assert_eq!((d.rev, d.ids()), (1, vec![format!("c{}", won[0].0)]), "round {round}: the winner's document, whole");
            let hs: Vec<_> = (10..18)
                .map(|i| {
                    let st = Store::new(s.home.clone(), s.project.clone());
                    std::thread::spawn(move || st.patch("race", &[Op::Add { parent: None, block: block(i), before: None, after: None }], None).unwrap())
                })
                .collect();
            let mut revs: Vec<u64> = hs.into_iter().map(|h| h.join().unwrap()).collect();
            revs.sort();
            assert_eq!(revs, (2..=9).collect::<Vec<u64>>(), "round {round}: eight updates, each its own rev");
            assert_eq!(s.load("race").unwrap().ids().len(), 9, "round {round}");
            fs::remove_dir_all(&s.home).ok();
        }
    }

    /// A feeds.json that won't parse used to read as "no feeds", so nothing was owned and Claude's
    /// write landed in a feed's ids.
    #[test]
    fn a_corrupt_registry_refuses_writes_instead_of_dropping_ownership() {
        let s = temp_store();
        s.put(plan(), None).unwrap();
        fs::write(s.dir().join("feeds.json"), r#"{"feeds": [{"name": "jj"},]}"#).unwrap();
        let set = [Op::Set { id: "t1".into(), field: "text".into(), value: json!("reworded") }];
        let e = s.patch("plan", &set, None).unwrap_err();
        assert!(e.contains("feeds.json"), "{e}");
        assert!(s.decide("plan", "t1", "tick", json!("done"), "key", None).unwrap_err().contains("feeds.json"));
        assert!(s.put(plan(), None).unwrap_err().contains("feeds.json"));
        assert_eq!(s.load("plan").unwrap().rev, 1, "nothing landed");
        // fixed (here: removed), writes go through again
        fs::remove_file(s.dir().join("feeds.json")).unwrap();
        s.patch("plan", &set, None).unwrap();
    }

    /// A state.json that won't parse used to read as empty, and the next decision wrote that
    /// emptiness back over everything sulin had decided.
    #[test]
    fn an_unparseable_state_is_an_error_not_an_empty_one() {
        let s = temp_store();
        s.put(plan(), None).unwrap();
        s.decide("plan", "Q1", "pick", json!("mcp"), "select", None).unwrap();
        let torn = r#"{"decisions": {"Q1": {"kind": "pick", "val"#;
        fs::write(s.state_path("plan"), torn).unwrap();
        let untouched = |what: &str| {
            assert_eq!(fs::read_to_string(s.state_path("plan")).unwrap(), torn, "{what} rewrote state.json");
            assert_eq!(s.events().len(), 1, "{what} logged an event for a write that failed");
        };
        let e = s.decide("plan", "t1", "tick", json!("done"), "key", None).unwrap_err();
        assert!(e.contains("state.json") && e.contains("EOF") && e.contains("nothing was written"), "{e}");
        untouched("decide");
        assert!(s.note("plan", "t1", "less", "", "mouse").is_err());
        untouched("note");
        assert!(s.decide_own("plan", "t1", true, "key").is_err());
        untouched("decide_own");
        assert!(s.resolve_proposal("plan", "t1", &json!({"verb": "skip", "at": "x"}), true, "key").is_err());
        untouched("resolve_proposal");
        // Claude's status write fails whole: the content op beside it doesn't land either
        let ops = [Op::Set { id: "t2".into(), field: "text".into(), value: json!("reworded") }, Op::Set { id: "t1".into(), field: "status".into(), value: json!("doing") }];
        assert!(s.patch("plan", &ops, None).unwrap_err().contains("nothing was written"));
        assert_eq!(s.load("plan").unwrap().rev, 1);
        untouched("patch");
        // a content write goes through and its orphan pruning leaves the file alone
        s.patch("plan", &[Op::Rm { id: "Q1".into() }], None).unwrap();
        untouched("a content patch");
        // a reader still gets something to draw
        assert!(s.state("plan").decisions.is_empty());
        fs::remove_dir_all(&s.home).ok();
    }

    #[test]
    fn a_doc_from_a_newer_format_is_refused_not_rewritten() {
        let s = temp_store();
        s.put(plan(), None).unwrap();
        let mut v: Value = serde_json::from_str(&fs::read_to_string(s.doc_path("plan")).unwrap()).unwrap();
        v["v"] = json!(2);
        let newer = serde_json::to_string(&v).unwrap();
        fs::write(s.doc_path("plan"), &newer).unwrap();
        let e = s.load("plan").unwrap_err();
        assert!(e.contains("version 2") && e.contains("doc.json"), "{e}");
        let set = Op::Set { id: "t1".into(), field: "text".into(), value: json!("x") };
        assert!(s.patch("plan", &[set.clone()], None).is_err());
        assert!(s.replace(plan(), None).is_err());
        assert!(s.put(plan(), None).is_err(), "a put over it would reset what it can't read");
        assert!(s.decide("plan", "Q1", "pick", json!("mcp"), "select", None).is_err());
        assert_eq!(fs::read_to_string(s.doc_path("plan")).unwrap(), newer);
        assert!(s.put(Doc { v: 2, ..plan() }, None).is_err());
        fs::remove_dir_all(&s.home).ok();
    }

    /// A crash mid-append leaves a last line with no newline. The next event used to be written
    /// onto it: unparseable, so it vanished and the one after reused its seq.
    #[test]
    fn a_torn_last_event_line_does_not_swallow_the_next() {
        let s = temp_store();
        s.put(plan(), None).unwrap();
        s.decide("plan", "Q1", "pick", json!("mcp"), "select", None).unwrap();
        let mut f = fs::OpenOptions::new().append(true).open(s.events_path()).unwrap();
        f.write_all(br#"{"v":1,"seq":2,"at":"2026-11-09T10:00:0"#).unwrap();
        let a = s.decide("plan", "t1", "tick", json!("done"), "key", None).unwrap();
        let b = s.decide("plan", "t2", "tick", json!("next"), "key", None).unwrap();
        assert_eq!((a.seq, b.seq), (2, 3));
        let got: Vec<(u64, String)> = s.events().into_iter().map(|e| (e.seq, e.id)).collect();
        assert_eq!(got, [(1, "Q1".to_string()), (2, "t1".to_string()), (3, "t2".to_string())]);
        // a junk line after a good one never lowers the next seq, bad UTF-8 included
        f.write_all(b"\xff\xfe not json\n{\"seq\": \"x\"}\n").unwrap();
        assert_eq!(s.last_seq(), 3);
        assert_eq!(s.decide("plan", "t2", "untick", Value::Null, "key", None).unwrap().seq, 4);
        assert_eq!(s.events().len(), 4);
        fs::remove_dir_all(&s.home).ok();
    }

    #[test]
    fn a_session_id_is_sanitized_as_a_cursor_file_name() {
        let s = temp_store();
        s.put(plan(), None).unwrap();
        s.set_delivered("../../evil/x", 3);
        assert_eq!(s.delivered("../../evil/x"), 3);
        assert!(s.dir().join("delivered").join("______evil_x").exists());
        assert!(!s.home.join("evil").exists() && !s.dir().join("evil").exists());
        fs::remove_dir_all(&s.home).ok();
    }

    #[test]
    fn taking_a_default_records_when() {
        let s = temp_store();
        s.put(plan(), None).unwrap();
        s.patch("plan", &[Op::Set { id: "Q1".into(), field: "applied".into(), value: json!(true) }], None).unwrap();
        assert!(!s.load("plan").unwrap().find("Q1").unwrap().s("applied_at").is_empty());
    }

    #[test]
    fn claude_hears_when_sulin_finishes_his_step() {
        let s = temp_store();
        let d: Doc = serde_json::from_value(json!({"id": "plan", "title": "p", "blocks": [{"type": "todo", "id": "t4", "text": "try the build", "owner": "you"}]})).unwrap();
        s.put(d, None).unwrap();
        let ev = s.decide("plan", "t4", "tick", json!("done"), "mouse", None).unwrap();
        let line = crate::hook::describe(&s, &ev);
        assert!(line.contains("finished their step") && line.contains("try the build"), "{line}");
        let ev = s.decide_own("plan", "t4", false, "key").unwrap();
        assert!(crate::hook::describe(&s, &ev).contains("handed"));
    }

    /// Setting a todo doing stamps its start once; done stamps its finish once; a repeat
    /// overwrites neither, a step set back to doing loses its finish, and none of it bumps the rev.
    #[test]
    fn step_status_stamps_start_and_finish_once() {
        let s = temp_store();
        let d: Doc = serde_json::from_value(json!({"id": "plan", "title": "p", "blocks": [
            {"type": "todo", "id": "t1", "text": "a"}, {"type": "todo", "id": "t2", "text": "b"}, {"type": "todo", "id": "t3", "text": "c"}]})).unwrap();
        s.put(d, None).unwrap();
        let set = |id: &str, v: &str| s.patch("plan", &[Op::Set { id: id.into(), field: "status".into(), value: json!(v) }], None).unwrap();
        let time = |id: &str| s.state("plan").decisions.get(&step_time_key(id)).map(|d| d.value.clone()).unwrap_or(Value::Null);
        set("t1", "doing");
        assert!(crate::util::parse_rfc3339(time("t1")["started"].as_str().unwrap()).is_some() && time("t1")["done"].is_null());
        set("t1", "done");
        assert!(crate::util::parse_rfc3339(time("t1")["done"].as_str().unwrap()).is_some());
        assert_eq!(s.load("plan").unwrap().rev, 1, "status writes don't bump the content rev");
        // stamps from an earlier hour: repeats keep them
        let mut st = s.state("plan");
        st.decisions.get_mut(&step_time_key("t1")).unwrap().value = json!({"started": "2026-01-01T00:00:00Z", "done": "2026-01-01T00:05:00Z"});
        write_atomic(&s.state_path("plan"), serde_json::to_string_pretty(&st).unwrap().as_bytes()).unwrap();
        set("t1", "done");
        assert_eq!(time("t1"), json!({"started": "2026-01-01T00:00:00Z", "done": "2026-01-01T00:05:00Z"}));
        // reopened: the start stays, the finish goes until it is done again
        set("t1", "doing");
        assert_eq!(time("t1"), json!({"started": "2026-01-01T00:00:00Z"}));
        // done without ever running: a finish, no start; other statuses stamp nothing
        set("t2", "blocked");
        assert!(time("t2").is_null());
        set("t2", "done");
        assert!(time("t2")["started"].is_null() && time("t2")["done"].is_string());
        // a removed step's times go with it
        s.patch("plan", &[Op::Rm { id: "t1".into() }], None).unwrap();
        assert!(time("t1").is_null() && !time("t2").is_null());
        fs::remove_dir_all(&s.home).ok();
    }
}
