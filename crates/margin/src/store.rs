//! The store: one directory per project, one per artifact, three owners kept in three files.
//!
//! ```text
//! $MARGIN_HOME (~/.local/state/margin)/<project>/
//!   events.jsonl          append-only, every artifact; past EVENTS_CAP it rotates to
//!                         events.1.jsonl, events.2.jsonl (seq goes on counting across them)
//!   events.seq            the last seq handed out and the log's length then (the append's fast path)
//!   .lock                 flock: every write takes it
//!   cursors/<name>        a named position in the log: the last seq a consumer has taken
//!                         (`margin events --cursor`, a job's `job.<name>`)
//!   delivered/<session>   the same thing for a Claude session: the last seq its hooks handed it
//!   handlers.json  owners.json  data.json   the registries      daemon.lock|.pid
//!   <artifact>/doc.json   the LAYOUT: structure and authored text (layout writers only)
//!              owned/owner.<name>.json  owned/handler.<name>.json
//!                                the blocks under one owner's or handler's prefix (only it writes)
//!              state.json        decisions (sulin)
//!              view/<pane>.json  folds, focus, scroll (the pane)
//! ```
//!
//! One writer per file. A board is read COMPOSED (`Store::load`): the layout with each owner's
//! blocks put in under their parents. An owner's write touches its own file and nothing else, so
//! the board's `rev` (the layout's) moves only when the layout changes.
//!
//! Writes go to a temp file and `rename` into place, so the renderer never reads half a file.

use crate::doc::{Block, Doc, Op, OwnedRev};
use crate::util::{now_rfc3339, slug};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;
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

/// The view events the pane appends about a board (`<board>.shown`, `<board>.hidden`,
/// `<board>.detail`): what is on screen, so a provider can load what is looked at and stop when
/// it is not. The last part of the name is one of these.
pub const VIEW_EVENTS: &[&str] = &["shown", "hidden", "detail"];

/// Is `kind` a view event's name: `<board>.shown`, `<board>.hidden` or `<board>.detail`? Decided
/// by the name alone, so the three endings are reserved: an event someone emits under such a
/// name is passive like the pane's own.
pub fn is_view(kind: &str) -> bool {
    kind.rsplit_once('.').map(|(board, name)| !board.is_empty() && VIEW_EVENTS.contains(&name)).unwrap_or(false)
}

/// Passive kinds are logged but never pushed into the prompt (design §2), never counted as one
/// of the person's decisions, and given to a handler only when its `--on` names them exactly.
pub fn is_passive(kind: &str) -> bool {
    matches!(kind, "fold" | "unfold" | "seen" | "open" | "close" | "focus" | "view" | "feed" | "toasttest") || is_view(kind)
}

/// Is this one of the person's decisions in a pane (a tick, a pick, a strike…): what the hooks
/// hand a Claude session as binding? Not a passive kind, and not a named event someone appended
/// with `margin emit`: that is for whoever listens for its name, never an instruction to Claude.
pub fn is_decision(e: &Event) -> bool {
    crate::who::is_human(&e.by) && !is_passive(&e.kind) && e.via != "emit" && e.via != "handler" && !is_action(e)
}

/// Is this an action on a board (`<board>.<name>`: a declared action's key, or `open`)? An
/// action is for the handler registered for its name. The hooks hand one to a Claude session
/// only when the board is that session's own pane (hook.rs `actions_for_session`). A view event
/// has the same shape of name and is not one: nobody did anything.
pub fn is_action(e: &Event) -> bool {
    !e.artifact.is_empty() && !is_view(&e.kind) && e.kind.strip_prefix(e.artifact.as_str()).map(|r| r.starts_with('.')).unwrap_or(false)
}

/// The log rotates once events.jsonl is this long: the append and a read of what is new stay
/// cheap however long the project lives. `$MARGIN_EVENTS_CAP` (bytes) overrides it.
pub const EVENTS_CAP: u64 = 4 * 1024 * 1024;
/// Rotated generations kept beside the live file (events.1.jsonl is the newest of them).
pub const EVENTS_KEEP: usize = 2;

fn events_cap() -> u64 {
    std::env::var("MARGIN_EVENTS_CAP").ok().and_then(|s| s.parse().ok()).filter(|n| *n > 0).unwrap_or(EVENTS_CAP)
}

/// A named event's name (`margin emit`, a job's emit line, an action): letters, digits and
/// `. _ - :`, starting with a letter or digit, so it is one shell word and one glob target.
pub fn valid_event_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 128 && s.starts_with(|c: char| c.is_ascii_alphanumeric()) && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':'))
}

/// What an action's event carries about the block it was pressed on: a table row's `cells`,
/// `target` and `detail`; any other block's fields. Never its children or its `actions`.
pub fn snapshot(b: &crate::doc::Block) -> Value {
    let keep = |k: &str| if b.kind == "tr" { matches!(k, "cells" | "target" | "detail") } else { k != "actions" };
    Value::Object(b.f.iter().filter(|(k, _)| keep(k)).map(|(k, v)| (k.clone(), v.clone())).collect())
}

fn parse_events(bytes: &[u8]) -> Vec<Event> {
    // lossy: a torn line of bad UTF-8 must not read as an empty log
    String::from_utf8_lossy(bytes).lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

fn read_cursor(p: &Path) -> Option<u64> {
    fs::read_to_string(p).ok().and_then(|s| s.trim().parse().ok())
}

/// Write a cursor down for the first time, at `at`. `create_new`, not a rename: it must never
/// replace a cursor a concurrent consumer just moved (an empty file left by a crash in between
/// reads as "no cursor", i.e. the same starting point).
fn create_cursor(p: &Path, at: u64) {
    if p.exists() || p.parent().map(|d| fs::create_dir_all(d).is_err()).unwrap_or(true) {
        return;
    }
    if let Ok(mut f) = fs::OpenOptions::new().write(true).create_new(true).open(p) {
        let _ = f.write_all(at.to_string().as_bytes());
    }
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
    /// A registered owner of a prefix: a handler (`by` is `handler:<name>`) or an outside
    /// program (`owner:<name>`). It writes only inside its prefix (`*` = the whole board).
    Named { by: String, owns: String },
}

impl Writer {
    pub fn by(&self) -> String {
        match self {
            Writer::Claude => "claude".into(),
            Writer::Named { by, .. } => by.clone(),
        }
    }
}

/// A claim on part of an artifact: every id starting with `owns` belongs to the feed, handler
/// or owner named `feed` (the field keeps the name it had when only feeds made claims).
#[derive(Clone, Debug, PartialEq)]
pub struct Claim {
    pub feed: String,
    pub owns: String,
    /// `feed`, `handler` or `owner`: for the message a refused write gets.
    pub kind: &'static str,
}

impl Claim {
    /// Who the claim's writes are logged as, and its key in a board's `owned` map:
    /// `owner:<name>` or `handler:<name>`.
    pub fn by(&self) -> String {
        format!("{}:{}", self.kind, self.feed)
    }
}

/// One owner's or handler's blocks on one board: the file `<board>/owned/<kind>.<name>.json`.
/// Only that owner writes it (`margin apply --as`, a handler's commit); a layout write never does.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Owned {
    #[serde(default = "crate::util::v1")]
    pub v: u64,
    /// Who writes this file: `owner:<name>` or `handler:<name>`.
    #[serde(default)]
    pub by: String,
    /// The id prefix it held when it last wrote.
    #[serde(default)]
    pub owns: String,
    /// This file's own revision: up by one each time its blocks change.
    #[serde(default)]
    pub rev: u64,
    /// When its blocks last changed (RFC 3339). The file's mtime is the owner's last write,
    /// changed or not: the staleness clock (owner.rs).
    #[serde(default)]
    pub updated: String,
    /// The board's title as a whole-board (`*`) owner set it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default)]
    pub blocks: Vec<Entry>,
}

/// One owned block (with its children) and where it sits in the layout.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    /// The layout block it sits under; absent: the top level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Its place among the parent's layout children: right after the one with this id, or
    /// first when empty. Absent (or naming a sibling that is gone): after all of them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    pub block: Block,
}

impl Owned {
    fn new(c: &Claim) -> Owned {
        Owned { v: 1, by: c.by(), owns: c.owns.clone(), rev: 0, updated: String::new(), title: None, blocks: Vec::new() }
    }
}

/// What `margin doctor` says about one board's owned files (`Store::inspect`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Inspect {
    /// Owned blocks whose parent is not in the layout: kept in their file, not drawn.
    /// (who, the missing parent, the blocks' ids)
    pub hidden: Vec<(String, String, Vec<String>)>,
    /// Owned files nobody registered claims: (kind, name, blocks). Not drawn.
    pub unclaimed: Vec<(String, String, usize)>,
    /// Blocks under a claimed prefix still in doc.json (a board from before owners had files,
    /// or an interrupted move): (the claim, how many). The next write to the board moves them.
    pub legacy: Vec<(Claim, usize)>,
    /// Owned files that do not parse: not drawn, and their owner's writes are refused.
    pub corrupt: Vec<String>,
}

// ------------------------------------------------------------------ reading files once

/// A file as last read: (length, mtime, inode). A write replaces the file by rename, so any
/// write changes the inode even inside one tick of the clock.
type Stamp = (u64, Option<std::time::SystemTime>, u64);

thread_local! {
    /// Parsed store files by path, kept while their stamp holds: a board that has not changed is
    /// not read again, and a change to one owned file reads that file only.
    static FILES: RefCell<HashMap<PathBuf, (Stamp, Rc<dyn std::any::Any>)>> = RefCell::new(HashMap::new());
    /// Reads of store files that went to the disk on this thread, and their bytes (tests and
    /// the measurements in the docs count them; a cache hit is neither).
    static FILE_READS: Cell<u64> = const { Cell::new(0) };
    static FILE_BYTES: Cell<u64> = const { Cell::new(0) };
}

/// (reads, bytes) of store files (doc.json, owned files, the registries) on this thread so far.
#[cfg(test)]
pub fn file_reads() -> (u64, u64) {
    (FILE_READS.with(|c| c.get()), FILE_BYTES.with(|c| c.get()))
}

fn stamp(p: &Path) -> Option<Stamp> {
    use std::os::unix::fs::MetadataExt;
    let m = fs::metadata(p).ok()?;
    Some((m.len(), m.modified().ok(), m.ino()))
}

/// The JSON file at `p`, parsed: `Ok(None)` when it is not there. `fresh`: read it now whatever
/// was kept (every writer, under the lock: a read-modify-write never starts from a kept copy).
fn cached<T: for<'a> Deserialize<'a> + 'static>(p: &Path, fresh: bool) -> Result<Option<Rc<T>>, String> {
    let Some(st) = stamp(p) else {
        FILES.with(|f| f.borrow_mut().remove(p));
        return Ok(None);
    };
    if !fresh {
        let hit = FILES.with(|f| f.borrow().get(p).filter(|(s, _)| *s == st).map(|(_, v)| v.clone()));
        if let Some(Ok(v)) = hit.map(|v| v.downcast::<Result<Rc<T>, String>>()) {
            return (*v).clone().map(Some);
        }
    }
    let r: Result<Rc<T>, String> = match fs::read(p) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => Err(e.to_string()),
        Ok(b) => {
            FILE_READS.with(|c| c.set(c.get() + 1));
            FILE_BYTES.with(|c| c.set(c.get() + b.len() as u64));
            serde_json::from_slice::<T>(&b).map(Rc::new).map_err(|e| e.to_string())
        }
    };
    FILES.with(|f| f.borrow_mut().insert(p.to_path_buf(), (st, Rc::new(r.clone()) as Rc<dyn std::any::Any>)));
    r.map(Some)
}

// ------------------------------------------------------------------ layout + owned = the board

fn find_block_mut<'a>(bs: &'a mut Vec<Block>, id: &str) -> Option<&'a mut Block> {
    for b in bs.iter_mut() {
        if b.id == id {
            return Some(b);
        }
        if let Some(x) = find_block_mut(&mut b.children, id) {
            return Some(x);
        }
    }
    None
}

/// Take out of `blocks` every subtree whose root `which` gives to a writer (by index into
/// `out`), recording where it sat: its parent, and the nearest sibling before it that stays
/// (`""`: none, it was first). In a list where nothing stays the place is left out: "after the
/// layout's own children", which is where a provider's rows go in a container it fills.
fn extract(blocks: &mut Vec<Block>, parent: Option<&str>, which: &dyn Fn(&str) -> Option<usize>, out: &mut [Vec<Entry>]) {
    let old = std::mem::take(blocks);
    let stays = old.iter().any(|b| which(&b.id).is_none());
    let mut prev = String::new();
    for mut b in old {
        match which(&b.id) {
            Some(i) => out[i].push(Entry { parent: parent.map(String::from), after: stays.then(|| prev.clone()), block: b }),
            None => {
                let id = b.id.clone();
                extract(&mut b.children, Some(&id), which, out);
                prev = id;
                blocks.push(b);
            }
        }
    }
}

/// Put each owner's blocks into `d` under their parents: the parent's own children keep their
/// order, an owned block goes right after the sibling it names (or first, or after them all),
/// owners in the order given (registration), each owner's blocks in its file's order. Returns
/// the entries whose parent is not in `d`, as (owner, entry) indexes: they are left out.
fn insert_owned(d: &mut Doc, owners: &[(&Claim, &Owned)]) -> Vec<(usize, usize)> {
    let mut groups: Vec<(Option<&str>, Vec<(usize, usize)>)> = Vec::new();
    for (oi, (_, o)) in owners.iter().enumerate() {
        for (ei, e) in o.blocks.iter().enumerate() {
            match groups.iter_mut().find(|(p, _)| *p == e.parent.as_deref()) {
                Some(g) => g.1.push((oi, ei)),
                None => groups.push((e.parent.as_deref(), vec![(oi, ei)])),
            }
        }
    }
    let mut hidden = Vec::new();
    for (parent, members) in groups {
        let list = match parent {
            None => Some(&mut d.blocks),
            Some(p) => find_block_mut(&mut d.blocks, p).map(|b| &mut b.children),
        };
        let Some(list) = list else {
            hidden.extend(members);
            continue;
        };
        let own = std::mem::take(list);
        let (mut first, mut last): (Vec<Block>, Vec<Block>) = Default::default();
        let mut mid: HashMap<&str, Vec<Block>> = HashMap::new();
        for (oi, ei) in members {
            let e = &owners[oi].1.blocks[ei];
            match e.after.as_deref() {
                Some("") => first.push(e.block.clone()),
                Some(x) if own.iter().any(|b| b.id == x) => mid.entry(x).or_default().push(e.block.clone()),
                _ => last.push(e.block.clone()),
            }
        }
        list.append(&mut first);
        for b in own {
            let follow = mid.remove(b.id.as_str());
            list.push(b);
            list.extend(follow.into_iter().flatten());
        }
        list.append(&mut last);
    }
    hidden
}

/// The board as every reader gets it: `layout` with each owner's blocks in place, and each
/// owner's revision in `owned`. A block the layout still holds under a prefix whose owner has a
/// file is left out: the file is the truth (a move from doc.json into the file that was
/// interrupted leaves the block in both). Returns the entries that are not drawn, too.
fn compose(layout: &Doc, owners: &[(&Claim, &Owned)]) -> (Doc, Vec<(usize, usize)>) {
    let mut d = layout.clone();
    d.owned.clear();
    if owners.is_empty() {
        return (d, Vec::new());
    }
    let filed = |id: &str| owners.iter().any(|(c, _)| owns(&c.owns, id));
    let mut any = false;
    crate::doc::walk(&layout.blocks, &mut |b, _, _| any |= filed(&b.id));
    if any {
        extract(&mut d.blocks, None, &|id| filed(id).then_some(0), &mut [Vec::new()]);
    }
    for (c, o) in owners {
        d.owned.insert(c.by(), OwnedRev { rev: o.rev, updated: o.updated.clone() });
        if let Some(t) = &o.title {
            d.title = t.clone();
        }
        // `updated` and `by` are the board's as a reader knows them: its last change, whoever
        // made it
        if !o.updated.is_empty() && o.updated >= d.updated {
            d.updated = o.updated.clone();
            d.by = c.by();
        }
    }
    let hidden = insert_owned(&mut d, owners);
    (d, hidden)
}

fn count_blocks(bs: &[Block]) -> usize {
    let mut n = 0;
    crate::doc::walk(bs, &mut |_, _, _| n += 1);
    n
}

/// A board's files as a writer holds them under the lock: read fresh, and already settled
/// (`Store::settle_locked`), so the layout holds no block under a claimed prefix.
struct Settled {
    layout: Doc,
    claims: Vec<Claim>,
    /// Each claim's file, by the claim's index; None: it has not written yet.
    files: Vec<Option<Owned>>,
}

impl Settled {
    /// The owned block `id` when it is the root of an entry: (who owns it, the entry).
    fn entry(&self, id: &str) -> Option<(&Claim, &Entry)> {
        self.claims.iter().zip(&self.files).find_map(|(c, f)| f.as_ref()?.blocks.iter().find(|e| e.block.id == id).map(|e| (c, e)))
    }

    /// A layout op that names an owner's block as the sibling to land beside. The layout does
    /// not hold that block, so `after` it becomes the same place in the layout's own terms (the
    /// owned block keeps the place its file records, and the new block follows it). `before`
    /// it cannot be said in the layout alone, and is refused.
    fn place(&self, op: &Op) -> Result<Op, String> {
        let (before, after) = match op {
            Op::Add { before, after, .. } | Op::Move { before, after, .. } => (before, after),
            _ => return Ok(op.clone()),
        };
        if let Some((c, _)) = before.as_deref().and_then(|x| self.entry(x)) {
            let x = before.as_deref().unwrap_or("");
            return Err(format!("{x:?} belongs to {} `{}`, and its place is kept in that owner's file: a layout block cannot be put before it. Use --after {x}, or name a block of the layout", c.kind, c.feed));
        }
        let Some((_, e)) = after.as_deref().and_then(|x| self.entry(x)) else { return Ok(op.clone()) };
        let empty = Vec::new();
        let sibs = match &e.parent {
            None => &self.layout.blocks,
            Some(p) => self.layout.find(p).map(|b| &b.children).unwrap_or(&empty),
        };
        let (b, a) = match e.after.as_deref() {
            Some("") => (sibs.first().map(|b| b.id.clone()), None),
            Some(x) if sibs.iter().any(|b| b.id == x) => (None, Some(x.to_string())),
            _ => (None, sibs.last().map(|b| b.id.clone())),
        };
        Ok(match op {
            Op::Add { block, .. } => Op::Add { parent: e.parent.clone(), block: block.clone(), before: b, after: a },
            Op::Move { id, .. } => Op::Move { id: id.clone(), parent: if b.is_none() && a.is_none() { e.parent.clone() } else { None }, before: b, after: a },
            _ => unreachable!(),
        })
    }
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
    // a writer with a prefix: who it is, in words, and the prefix
    let prefixed = match w {
        Writer::Claude => None,
        Writer::Named { by, owns } => Some((by.replacen(':', " ", 1), owns)),
    };
    match prefixed {
        Some((who, p)) => {
            if matches!(op, Op::Title { .. }) && p != "*" {
                return Err(format!("{who} owns the ids {p} and {p}-…, not the board's title"));
            }
            if let Some(id) = ids.iter().find(|id| !owns(p, id)) {
                return Err(format!("{who} may only write the ids {p} and {p}-…; {id:?} is outside its prefix"));
            }
            // outside its prefix a parent is a container of the layout, which the owner fills
            // and does not write. It must be there: the layout is somebody else's file
            if let Some(par) = parent.filter(|par| !owns(p, par)) {
                if doc.find(&par).is_none() {
                    return Err(format!("{who} writes under {par:?}, and the layout of {} has no such block (removed, or not created yet): add that container to the layout first, or write under another parent", doc.id));
                }
            }
        }
        None => {
            let mine = |id: &str| claims.iter().find(|c| owns(&c.owns, id));
            if matches!(op, Op::Title { .. }) {
                if let Some(c) = claims.iter().find(|c| c.owns == "*") {
                    return Err(feed_owned_msg("the title", c, &doc.id));
                }
            }
            for id in ids.iter().chain(parent.iter()) {
                if let Some(c) = mine(id) {
                    return Err(feed_owned_msg(&format!("{id:?}"), c, &doc.id));
                }
            }
        }
    }
    Ok(())
}

fn feed_owned_msg(what: &str, c: &Claim, board: &str) -> String {
    let span = if c.owns == "*" { "this whole board".to_string() } else { format!("the ids {0} and {0}-… here", c.owns) };
    let n = &c.feed;
    match c.kind {
        "handler" => format!("{what} belongs to handler `{n}`, which owns {span} and writes it when its events arrive; write outside that prefix, or remove the handler first (`margin handler rm {n}`)"),
        "owner" => format!("{what} belongs to owner `{n}`, which owns {span}: an outside program writes it (`margin apply {board} --as {n}`); write outside that prefix, or `margin owner rm {n}` first"),
        _ => format!("{what} belongs to feed `{n}`, which owns {span} and rewrites it on its own; write outside that prefix, or stop the feed first (`margin feed pause {n}` / `margin feed rm {n}`)"),
    }
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

/// `write_atomic`, and then the directory is synced too, so the rename itself survives a power
/// loss. For the files whose loss would lose one of the person's decisions or move a cursor
/// back: state.json, the cursors, the registries. The rest (view files, status files, activity,
/// the seq sidecar, toasts) are rebuilt or rewritten in the normal course of things, and
/// events.jsonl is appended without a sync: a power loss may cost the last events, which the
/// torn-line rule then tolerates, and syncing every append would put a disk flush on every key.
pub fn write_durable(path: &Path, data: &[u8]) -> std::io::Result<()> {
    write_atomic(path, data)?;
    if let Some(d) = path.parent() {
        if let Ok(f) = fs::File::open(d) {
            let _ = f.sync_all();
        }
    }
    Ok(())
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

/// A session id or cursor name as a file name (the same mapping link.rs, wake.rs and recap.rs
/// use), with `.` kept for the runner's own cursors (`job.<name>`).
fn cursor_key(name: &str) -> String {
    // `handler:open` is the file `handler.open`: no name a person can give a cursor has a dot
    let k: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else if c == ':' { '.' } else { '_' }).collect();
    // never `.`, `..` or a hidden file
    if k.starts_with('.') || k.is_empty() {
        format!("_{k}")
    } else {
        k
    }
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
    /// A blocking wait for the log to change (watch.rs): an append, or a rotation. Build it
    /// before the first read of the log; whatever is appended after that read is then already
    /// queued, and the next `wait` returns at once. It watches the project directory for the
    /// live file's name, so it outlives the file being rotated away and created again.
    pub fn watch_events(&self) -> crate::watch::Watch {
        crate::watch::Watch::new().dir(&self.dir(), &["events.jsonl"])
    }
    /// Generation `n` of the log: 0 is the live file, 1 the newest rotated one.
    fn events_gen(&self, n: usize) -> PathBuf {
        if n == 0 {
            self.events_path()
        } else {
            self.dir().join(format!("events.{n}.jsonl"))
        }
    }
    fn seq_path(&self) -> PathBuf {
        self.dir().join("events.seq")
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
        format!("no board {a:?} in project {}: create it with margin new {a}{hint}", self.project)
    }

    /// The ids of this project's artifacts, from the directory alone (no document is parsed).
    fn names(&self) -> Vec<String> {
        let Ok(rd) = fs::read_dir(self.dir()) else { return vec![] };
        let mut v: Vec<String> = rd.flatten().filter(|e| e.path().join("doc.json").exists()).filter_map(|e| e.file_name().into_string().ok()).filter(|n| !n.starts_with('.')).collect();
        v.sort();
        v
    }

    /// Where the owners' and handlers' files of board `a` are.
    pub fn owned_dir(&self, a: &str) -> PathBuf {
        self.art(a).join("owned")
    }
    /// The file holding the blocks of claim `c` on board `a`.
    pub fn owned_path(&self, a: &str, c: &Claim) -> PathBuf {
        self.owned_dir(a).join(format!("{}.{}.json", c.kind, c.feed))
    }

    /// The owned files of board `a` as (kind, name), from the directory alone.
    fn owned_files(&self, a: &str) -> Vec<(String, String)> {
        let Ok(rd) = fs::read_dir(self.owned_dir(a)) else { return vec![] };
        let mut v: Vec<(String, String)> = rd
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter_map(|n| n.strip_suffix(".json").and_then(|n| n.split_once('.')).map(|(k, n)| (k.to_string(), n.to_string())))
            .filter(|(k, n)| (k == "owner" || k == "handler") && crate::util::valid_id(n))
            .collect();
        v.sort();
        v
    }

    /// The layout of board `a`: doc.json and nothing else.
    fn layout(&self, a: &str, fresh: bool) -> Result<Rc<Doc>, String> {
        let d: Rc<Doc> = cached(&self.doc_path(a), fresh).map_err(|e| format!("{a}/doc.json: {e}"))?.ok_or_else(|| self.missing(a))?;
        // design §3 "Versions": a newer format is refused, not read as v 1 and rewritten
        if d.v != 1 {
            return Err(format!("{a}/doc.json: version {}: this margin reads v 1", d.v));
        }
        Ok(d)
    }

    /// The claims on board `a` for a reader, from the registries as last read (re-read only when
    /// one changed). A registry that will not parse claims nothing here: the board still draws.
    fn claims_cached(&self, a: &str) -> Vec<Claim> {
        let d = self.dir();
        let mut v = cached::<crate::handler::Registry>(&crate::handler::Registry::path(&d), false).ok().flatten().map(|r| r.claims(a)).unwrap_or_default();
        v.extend(cached::<crate::owner::Registry>(&crate::owner::Registry::path(&d), false).ok().flatten().map(|r| r.claims(a)).unwrap_or_default());
        v
    }

    /// The claims on `a` that have a file, in registration order, with the file as last read.
    /// An owned file that will not parse is skipped (`margin doctor` names it).
    fn owners_of(&self, a: &str) -> Vec<(Claim, Rc<Owned>)> {
        // no owned file at all: nothing to compose, and the registries need not be looked at
        if self.owned_files(a).is_empty() {
            return Vec::new();
        }
        self.claims_cached(a).into_iter().filter_map(|c| cached::<Owned>(&self.owned_path(a, &c), false).ok().flatten().filter(|o| o.v == 1).map(|o| (c, o))).collect()
    }

    /// The board as every reader gets it (`margin show`, `render`, the pane, an action's
    /// snapshot, a handler): the layout with each owner's blocks in place (`compose`). Its `rev`
    /// is the layout's; each owner's is in `owned`. Files are read once and kept by (length,
    /// mtime, inode): a board that has not changed costs a few `stat` calls.
    pub fn load(&self, a: &str) -> Result<Doc, String> {
        let layout = self.layout(a, false)?;
        let owners = self.owners_of(a);
        let refs: Vec<(&Claim, &Owned)> = owners.iter().map(|(c, o)| (c, &**o)).collect();
        Ok(compose(&layout, &refs).0)
    }

    /// What is not as it should be in board `a`'s files, for `margin doctor`.
    pub fn inspect(&self, a: &str) -> Inspect {
        let mut out = Inspect::default();
        let Ok(layout) = self.layout(a, true) else { return out };
        let claims = self.claims(a);
        let mut owners: Vec<(Claim, Owned)> = Vec::new();
        for (kind, name) in self.owned_files(a) {
            let p = self.owned_dir(a).join(format!("{kind}.{name}.json"));
            let rel = format!("{a}/owned/{kind}.{name}.json");
            match cached::<Owned>(&p, true) {
                Ok(Some(o)) if o.v == 1 => match claims.iter().find(|c| c.kind == kind && c.feed == name) {
                    Some(c) => owners.push((c.clone(), (*o).clone())),
                    None => out.unclaimed.push((kind, name, o.blocks.iter().map(|e| count_blocks(std::slice::from_ref(&e.block))).sum())),
                },
                Ok(Some(o)) => out.corrupt.push(format!("{rel}: version {}: this margin reads v 1", o.v)),
                Ok(None) => {}
                Err(e) => out.corrupt.push(format!("{rel}: {e}")),
            }
        }
        for c in &claims {
            let mut n = 0;
            crate::doc::walk(&layout.blocks, &mut |b, _, _| n += owns(&c.owns, &b.id) as usize);
            if n > 0 {
                out.legacy.push((c.clone(), n));
            }
        }
        // in registration order, as a reader composes them
        owners.sort_by_key(|(c, _)| claims.iter().position(|x| x == c));
        let refs: Vec<(&Claim, &Owned)> = owners.iter().map(|(c, o)| (c, o)).collect();
        for (oi, ei) in compose(&layout, &refs).1 {
            let e = &refs[oi].1.blocks[ei];
            let (by, parent) = (refs[oi].0.by(), e.parent.clone().unwrap_or_default());
            match out.hidden.iter_mut().find(|(b, p, _)| *b == by && *p == parent) {
                Some(h) => h.2.push(e.block.id.clone()),
                None => out.hidden.push((by, parent, vec![e.block.id.clone()])),
            }
        }
        out
    }

    /// An owned file for a writer: read now, and one that will not parse is an error (a write
    /// over it would make the loss permanent).
    fn owned_strict(&self, a: &str, c: &Claim) -> Result<Option<Owned>, String> {
        let p = self.owned_path(a, c);
        let what = || format!("{a}/owned/{}.{}.json", c.kind, c.feed);
        match cached::<Owned>(&p, true).map_err(|e| format!("{}: {e}; nothing was written (fix the file or move it aside)", what()))? {
            Some(o) if o.v != 1 => Err(format!("{}: version {}: this margin reads v 1", what(), o.v)),
            o => Ok(o.map(|o| (*o).clone())),
        }
    }

    /// Write an owner's file: temp file, fsync, rename. `durable` syncs the directory too: for
    /// the write that must be on disk before doc.json gives up the same rows.
    fn write_owned(&self, a: &str, c: &Claim, o: &Owned, durable: bool) -> Result<(), String> {
        let p = self.owned_path(a, c);
        let data = serde_json::to_string_pretty(o).unwrap();
        if durable { write_durable(&p, data.as_bytes()) } else { write_atomic(&p, data.as_bytes()) }.map_err(|e| format!("{}: {e}", p.display()))
    }

    /// Write the layout as it is, keeping its revision: for a step that moves blocks between
    /// doc.json and an owner's file, which changes nothing a reader sees.
    fn write_layout(&self, d: &Doc) -> Result<(), String> {
        let mut d = d.clone();
        d.owned.clear();
        write_durable(&self.doc_path(&d.id), serde_json::to_string_pretty(&d).unwrap().as_bytes()).map_err(|e| e.to_string())
    }

    /// Under the lock, before any write to board `a`: its layout and its owners' files, read
    /// now, with every block under a claimed prefix in its owner's file and none in doc.json.
    ///
    /// This is the migration of a board written before owners had files (and of rows a layout
    /// writer put under a prefix before it was claimed): those blocks move out of doc.json into
    /// the owner's file, here, in the first write anyone makes to the board. The owner's file is
    /// written and synced first and doc.json rewritten after, so a crash between the two leaves
    /// the blocks in both, never in neither; and where a block is in both, the file wins (for a
    /// reader in `compose`, and here, where the copy in doc.json is dropped). The layout keeps
    /// its revision: nothing a reader sees has changed.
    fn settle_locked(&self, a: &str) -> Result<Settled, String> {
        let mut layout = (*self.layout(a, true)?).clone();
        layout.owned.clear();
        let claims = self.claims_strict(a)?;
        let mut files: Vec<Option<Owned>> = claims.iter().map(|c| self.owned_strict(a, c)).collect::<Result<_, _>>()?;
        let mut theirs = false;
        crate::doc::walk(&layout.blocks, &mut |b, _, _| theirs |= claims.iter().any(|c| owns(&c.owns, &b.id)));
        if theirs {
            let mut out = vec![Vec::new(); claims.len()];
            extract(&mut layout.blocks, None, &|id| claims.iter().position(|c| owns(&c.owns, id)), &mut out);
            for (i, entries) in out.into_iter().enumerate() {
                // a file is already there: what doc.json held is the leftover of a move that was
                // interrupted after the file was written, and the file is the truth
                if entries.is_empty() || files[i].is_some() {
                    continue;
                }
                // when they last changed is known only if their owner was the board's last writer
                let updated = if layout.by == claims[i].by() { layout.updated.clone() } else { String::new() };
                let o = Owned { rev: 1, updated, blocks: entries, ..Owned::new(&claims[i]) };
                self.write_owned(a, &claims[i], &o, true)?;
                // the file's mtime is the owner's last write: moving its rows is not one
                if claims[i].kind == "owner" {
                    let wrote = crate::owner::Registry::recorded_write(&self.dir(), &claims[i].feed);
                    if let (Some(t), Ok(f)) = (wrote, fs::File::options().write(true).open(self.owned_path(a, &claims[i]))) {
                        let _ = f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs_f64(t.max(0.0)));
                    }
                }
                files[i] = Some(o);
            }
            // only now: every row doc.json gives up is safe in its owner's file
            self.write_layout(&layout)?;
        }
        Ok(Settled { layout, claims, files })
    }

    /// `margin doctor --fix`: do now what the next write to `a` would (`settle_locked`).
    pub fn settle(&self, a: &str) -> Result<(), String> {
        let _l = self.lock().map_err(|e| e.to_string())?;
        self.settle_locked(a).map(|_| ())
    }

    /// Give the blocks of the owned file `<kind>.<name>` on board `a` back to the layout, where
    /// they become ordinary content: what unregistering an owner or a handler does, so that
    /// nothing it wrote disappears. doc.json is written (and synced) with the blocks first and
    /// the file removed after: a crash between the two leaves them in both, and the file wins
    /// while its claim stands. A block that cannot go back (its parent is not in the layout, or
    /// the layout now has its id) stays in the file, which then moves to `.archive/`. Returns
    /// how many entries went into the layout.
    pub(crate) fn release_locked(&self, a: &str, kind: &str, name: &str) -> Result<usize, String> {
        if !self.exists(a) {
            return Ok(0);
        }
        let p = self.owned_dir(a).join(format!("{kind}.{name}.json"));
        if !p.exists() {
            return Ok(0);
        }
        let st = self.settle_locked(a)?;
        let c = Claim { feed: name.to_string(), owns: String::new(), kind: if kind == "handler" { "handler" } else { "owner" } };
        let Some(mut o) = self.owned_strict(a, &c)? else { return Ok(0) };
        let mut d = st.layout.clone();
        // an id the layout holds already stays behind (a file nobody claimed, whose ids were
        // free to write since)
        let taken: HashSet<String> = d.ids().into_iter().collect();
        let clash = |e: &Entry| {
            let mut hit = false;
            crate::doc::walk(std::slice::from_ref(&e.block), &mut |b, _, _| hit |= taken.contains(&b.id));
            hit
        };
        let (keep, go): (Vec<Entry>, Vec<Entry>) = o.blocks.drain(..).partition(clash);
        o.blocks = go;
        let hidden = insert_owned(&mut d, &[(&c, &o)]);
        let moved = o.blocks.len() - hidden.len();
        let mut left = keep;
        left.extend(hidden.iter().map(|(_, ei)| o.blocks[*ei].clone()));
        if let Some(t) = &o.title {
            d.title = t.clone();
        }
        d.normalize()?;
        if moved > 0 || o.title.is_some() {
            d.rev = st.layout.rev + 1;
            d.updated = now_rfc3339();
            d.by = self.author.clone();
            self.write_layout(&d)?;
        }
        if left.is_empty() {
            fs::remove_file(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        } else {
            let dir = self.dir().join(".archive");
            fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let base = format!("{a}-owned-{kind}.{name}-{}", &now_rfc3339()[..10]);
            let dest = (1..).map(|n| dir.join(if n == 1 { format!("{base}.json") } else { format!("{base}-{n}.json") })).find(|p| !p.exists()).unwrap();
            o.blocks = left;
            write_durable(&dest, serde_json::to_string_pretty(&o).unwrap().as_bytes()).map_err(|e| e.to_string())?;
            fs::remove_file(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        }
        Ok(moved)
    }

    /// `margin doctor --fix` for an owned file nobody claims: its blocks go into the layout.
    pub fn release(&self, a: &str, kind: &str, name: &str) -> Result<usize, String> {
        let _l = self.lock().map_err(|e| e.to_string())?;
        self.release_locked(a, kind, name)
    }

    /// A registry is about to change, under the lock: `before` and `after` are every (board,
    /// claim) it holds. A claim that goes (removed, or moved to another board or prefix) gives
    /// its blocks back to the layout first, so the registry never stops naming a file that
    /// still holds rows.
    pub(crate) fn claims_going_locked(&self, before: &[(String, Claim)], after: &[(String, Claim)]) -> Result<(), String> {
        for (board, c) in before.iter().filter(|x| !after.contains(x)) {
            self.release_locked(board, c.kind, &c.feed)?;
        }
        Ok(())
    }

    /// The registry changed: a new claim takes the blocks already under its prefix into its
    /// file now, not at the board's next write. A failure here is left for that write.
    pub(crate) fn claims_came_locked(&self, before: &[(String, Claim)], after: &[(String, Claim)]) {
        for (board, _) in after.iter().filter(|x| !before.contains(x)) {
            if self.exists(board) {
                let _ = self.settle_locked(board);
            }
        }
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

    /// Write the layout as its next revision. `d` is the layout alone: no owner's block.
    fn save_doc_locked(&self, d: &mut Doc, old: Option<&Doc>, by: &str) -> Result<u64, String> {
        // no copy of the old revision is kept: nothing ever read history.jsonl, and it grew
        // without bound (a board made before 2026-11 may still have one; `margin doctor` lists it)
        d.rev = old.map(|o| o.rev).unwrap_or(0) + 1;
        d.updated = now_rfc3339();
        d.by = by.to_string();
        d.owned.clear();
        if let (true, Ok(s)) = (by == "claude", std::env::var("CLAUDE_CODE_SESSION_ID")) {
            if !d.sessions.contains(&s) {
                d.sessions.push(s);
            }
        }
        write_atomic(&self.doc_path(&d.id), serde_json::to_string_pretty(d).unwrap().as_bytes()).map_err(|e| e.to_string())?;
        self.prune_orphans(&d.id);
        Ok(d.rev)
    }

    /// Drop decisions Claude (or a feed) made on components that no longer exist: a status on a
    /// row Claude removed is Claude's own bookkeeping, not something sulin decided, so it would
    /// only pile up as orphans. sulin's own decisions stay until Claude has been handed them.
    /// After a write to board `a`, the layout's or an owner's. The board is read composed: a
    /// decision on an owner's row is on a row that exists.
    fn prune_orphans(&self, a: &str) {
        // an unparseable state.json is left exactly as it is
        let Ok(mut st) = self.state_rw(a) else { return };
        // nothing decided (most dashboards): nothing to prune, and the board need not be read
        if st.decisions.is_empty() {
            return;
        }
        let Ok(d) = self.load(a) else { return };
        let d = &d;
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
            let _ = write_durable(&self.state_path(&d.id), serde_json::to_string_pretty(&st).unwrap().as_bytes());
        }
    }

    /// The store's whole-document write: create or replace, whichever applies. The CLI never
    /// calls it for a document someone named: `margin new` goes through `create` and
    /// a whole-document `margin apply` through `replace`, which each allow one
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

    /// A whole-document `margin apply`: replace the document of an artifact
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
            return Err(format!("bad board id {:?}", d.id));
        }
        if d.v != 1 {
            return Err(format!("document version {}: this margin reads v 1", d.v));
        }
        d.normalize()?;
        d.blocks.iter().try_for_each(crate::doc::check_block)?;
        crate::doc::parse_actions(&d.actions, "the document", &crate::app::reserved_keys())?;
        // a doc that exists but won't parse is not a missing doc: replacing it would reset the rev
        // and skip archiving what was there
        if !self.doc_path(&d.id).exists() {
            check_base(None, base_rev)?;
            // the registries must read even here: a claim on this name may be waiting
            self.claims_strict(&d.id)?;
            let rev = self.save_doc_locked(&mut d, None, &self.author)?;
            // a new board that comes with rows under a prefix already claimed: into that
            // owner's file at once
            self.settle_locked(&d.id)?;
            return Ok(rev);
        }
        let st = self.settle_locked(&d.id)?;
        check_base(Some(&st.layout), base_rev)?;
        // a whole document is a layout write: what it carries under an owner's prefix (it was
        // read composed, and sent back) is not the layout's, and is left out. The owners' files
        // stay exactly as their owners left them
        if !st.claims.is_empty() {
            extract(&mut d.blocks, None, &|id| st.claims.iter().any(|c| owns(&c.owns, id)).then_some(0), &mut [Vec::new()]);
            if let Some((c, f)) = st.claims.iter().zip(&st.files).find(|(c, _)| c.owns == "*") {
                let shown = f.as_ref().and_then(|o| o.title.clone()).unwrap_or_else(|| st.layout.title.clone());
                if shown != d.title {
                    return Err(feed_owned_msg("the title", c, &d.id));
                }
                d.title = st.layout.title.clone();
            }
        }
        let rev = self.save_doc_locked(&mut d, Some(&st.layout), &self.author)?;
        Ok(rev)
    }

    /// Who owns part of artifact `a`: one table for every kind of claim. Handlers with a board
    /// (handlers.json; paused ones still own theirs) and outside owners (owners.json).
    pub fn claims(&self, a: &str) -> Vec<Claim> {
        let d = self.dir();
        let mut v = crate::handler::Registry::load(&d).claims(a);
        v.extend(crate::owner::Registry::load(&d).claims(a));
        v
    }

    /// Like `claims`, for Claude's and sulin's writes: a registry that won't parse is an error,
    /// not "nothing is owned" (a write into an owned id would land, and its owner would fight it).
    pub fn claims_strict(&self, a: &str) -> Result<Vec<Claim>, String> {
        let d = self.dir();
        let mut v = crate::handler::Registry::load_strict(&d)?.claims(a);
        v.extend(crate::owner::Registry::load_strict(&d)?.claims(a));
        Ok(v)
    }

    /// One id has one owner: the claim that `owns` on `board` would collide with, if any
    /// (`except`: the claim being changed, by kind and name).
    pub fn claim_overlap(&self, board: &str, owns: &str, except: (&str, &str)) -> Result<(), String> {
        match self.claims_strict(board)?.into_iter().find(|c| (c.kind, c.feed.as_str()) != except && crate::owner::overlaps(&c.owns, owns)) {
            Some(c) => Err(format!("prefix {owns:?} overlaps {} {} (owns {:?} in {board}); one id has one owner", c.kind, c.feed, c.owns)),
            None => Ok(()),
        }
    }

    /// `margin owner add`: register an outside program as the writer of a prefix on a board
    /// that exists. Under the lock, so the overlap check and the registration are one step.
    pub fn add_owner(&self, o: crate::owner::Owner) -> Result<(), String> {
        let _l = self.lock().map_err(|e| e.to_string())?;
        let mut r = crate::owner::Registry::load_strict(&self.dir())?;
        // its own check first: a second `add` says "exists", not "overlaps itself"
        r.add(o.clone())?;
        if !self.exists(&o.board) {
            return Err(self.missing(&o.board));
        }
        self.claim_overlap(&o.board, &o.owns, ("owner", ""))?;
        r.save(&self.dir())?;
        // the blocks already under its prefix are its own from here on: into its file
        let _ = self.settle_locked(&o.board);
        Ok(())
    }

    /// `margin owner set`: change a registered owner (its last write stays).
    pub fn set_owner(&self, o: crate::owner::Owner) -> Result<(), String> {
        let _l = self.lock().map_err(|e| e.to_string())?;
        let mut r = crate::owner::Registry::load_strict(&self.dir())?;
        let before = r.all_claims();
        r.set(o.clone())?;
        if !self.exists(&o.board) {
            return Err(self.missing(&o.board));
        }
        self.claim_overlap(&o.board, &o.owns, ("owner", &o.name))?;
        // another board or prefix: what it wrote under the old one goes back to that layout
        self.claims_going_locked(&before, &r.all_claims())?;
        r.save(&self.dir())?;
        self.claims_came_locked(&before, &r.all_claims());
        Ok(())
    }

    pub fn remove_owner(&self, name: &str) -> Result<crate::owner::Owner, String> {
        crate::owner::edit_registry(self, |r| r.remove(name))
    }

    /// `margin apply <board> --as <owner>`: an outside program's write. All of `ops` or none,
    /// each checked against the owner's prefix. It writes the owner's file and nothing else; the
    /// file's mtime (the owner's last write) moves whether or not the content changed. Returns
    /// the board's rev, which is the layout's: this write does not move it.
    pub fn apply_as(&self, board: &str, name: &str, ops: &[Op]) -> Result<u64, String> {
        let _l = self.lock().map_err(|e| e.to_string())?;
        let reg = crate::owner::Registry::load_strict(&self.dir())?;
        let o = reg.get(name).cloned().ok_or_else(|| crate::owner::missing(name))?;
        if o.board != board {
            return Err(format!("owner {name} is registered on {}, not {board}", o.board));
        }
        let c = Claim { feed: o.name.clone(), owns: o.owns.clone(), kind: "owner" };
        let (changed, mut errs, rev) = self.owned_write_locked(board, &c, &[ops.to_vec()])?;
        if !errs.is_empty() {
            return Err(errs.remove(0));
        }
        if changed.is_none() {
            self.touch_owned(board, &c)?;
        }
        Ok(rev)
    }

    /// An owner wrote and nothing changed: that is still a write. Move its file's mtime (an
    /// owner that has no file yet gets an empty one).
    fn touch_owned(&self, a: &str, c: &Claim) -> Result<(), String> {
        let p = self.owned_path(a, c);
        match fs::File::options().write(true).open(&p) {
            Ok(f) => f.set_modified(std::time::SystemTime::now()).map_err(|e| format!("{}: {e}", p.display())),
            Err(_) => self.write_owned(a, c, &Owned::new(c), false),
        }
    }

    /// What a handler's run commits on exit 0, under one lock: its ops (each line all or
    /// nothing, as a feed's), its emitted events (`by` the handler, via `handler`, about its
    /// board), and its cursor moved to the last event it was given. One step, so a crash never
    /// leaves the board updated with the cursor behind it, or the reverse. Returns the new rev
    /// of the handler's own file, if its blocks changed, and what was rejected.
    pub fn handler_commit(&self, a: &str, groups: &[Vec<Op>], w: &Writer, emits: &[(String, Value)], cursor: Option<(&str, u64)>) -> (Option<u64>, Vec<String>) {
        let _l = match self.lock() {
            Ok(l) => l,
            Err(e) => return (None, vec![e.to_string()]),
        };
        let (rev, mut errs) = match (groups.is_empty(), a.is_empty()) {
            (true, _) => (None, Vec::new()),
            (false, true) => (None, vec![format!("{} line(s) write a board, and this handler has none: register it with --board B --owns PREFIX", groups.len())]),
            (false, false) => match w {
                Writer::Named { owns: prefix, .. } => {
                    // the prefix says whose file: the one claim that holds exactly it. A writer
                    // whose prefix nobody holds (a handler removed while it ran) owns nothing:
                    // those ids are ordinary layout, and that is where its lines go
                    let r = match self.claims_strict(a) {
                        Ok(claims) => match claims.iter().find(|c| c.owns == *prefix) {
                            Some(c) => self.owned_write_locked(a, c, groups).map(|(rev, errs, _)| (rev, errs)),
                            None => self.unclaimed_write_locked(a, groups, w),
                        },
                        Err(e) => Err(e),
                    };
                    r.unwrap_or_else(|e| (None, vec![e]))
                }
                Writer::Claude => (None, vec!["a handler writes as itself, inside the prefix it owns".to_string()]),
            },
        };
        let at = if a.is_empty() { 0 } else { self.layout(a, false).map(|d| d.rev).unwrap_or(0) };
        for (name, value) in emits {
            // `<board>.<name>` is how a board's own actions are named: a handler writing board
            // A can't raise an event that reads as board B's
            let other = name.split_once('.').map(|(b, _)| b).filter(|b| *b != a && self.exists(b));
            let r = match other {
                Some(b) => Err(format!("emit {name:?}: names starting with {b}. belong to the board {b}")),
                None => self.append_event_locked(a, "", name, value.clone(), &w.by(), "handler", at).map(|_| ()),
            };
            if let Err(e) = r {
                errs.push(e);
            }
        }
        if let Some((name, seq)) = cursor {
            if self.cursor(name).map(|c| seq > c).unwrap_or(true) {
                self.set_cursor(name, seq);
            }
        }
        (rev, errs)
    }

    /// The runner's own event: a handler gave up on a batch (`handler.failed`, `by` the
    /// handler). Its cursor moves past the events it gave up on in the same step.
    pub fn handler_failed(&self, a: &str, by: &str, value: Value, skip: (&str, u64)) {
        let Ok(_l) = self.lock() else { return };
        let a = if !a.is_empty() && self.exists(a) { a } else { "" };
        let rev = if a.is_empty() { 0 } else { self.load(a).map(|d| d.rev).unwrap_or(0) };
        let _ = self.append_event_locked(a, "", "handler.failed", value, by, "handler", rev);
        if self.cursor(skip.0).map(|c| skip.1 > c).unwrap_or(true) {
            self.set_cursor(skip.0, skip.1);
        }
    }

    /// The write of a named writer that holds no claim on `a`: into the layout, inside its
    /// prefix and outside everyone's claims, each group all-or-nothing. Returns the layout's
    /// new rev when it changed.
    fn unclaimed_write_locked(&self, a: &str, groups: &[Vec<Op>], w: &Writer) -> Result<(Option<u64>, Vec<String>), String> {
        let st = self.settle_locked(a)?;
        let mut errs = Vec::new();
        let mut d = st.layout.clone();
        for g in groups {
            let mut t = d.clone();
            let r: Result<(), String> = g.iter().try_for_each(|op| {
                check_owner(op, &t, w, &st.claims)?;
                check_owner(op, &t, &Writer::Claude, &st.claims)?;
                t.apply(op)
            });
            match r {
                Ok(()) => d = t,
                Err(e) => errs.push(e),
            }
        }
        if d.blocks == st.layout.blocks && d.title == st.layout.title {
            return Ok((None, errs));
        }
        let rev = self.save_doc_locked(&mut d, Some(&st.layout), &w.by())?;
        Ok((Some(rev), errs))
    }

    /// An owner's or a handler's write: groups of ops (one group per stdout line), each applied
    /// all-or-nothing, so one bad line is rejected and logged without losing its neighbours. It
    /// reads the layout and the writer's own file, and writes that file only: never doc.json,
    /// never another owner's file. A script that re-prints the same state costs no revision. It
    /// updates its board and never creates it: a missing one fails the write with the message
    /// that names `margin new`. Returns (the file's new rev when its blocks changed, what was
    /// rejected, the layout's rev).
    fn owned_write_locked(&self, a: &str, c: &Claim, groups: &[Vec<Op>]) -> Result<(Option<u64>, Vec<String>, u64), String> {
        let st = self.settle_locked(a)?;
        let i = st.claims.iter().position(|x| x == c).ok_or_else(|| format!("{} {} is not registered on {a} with the prefix {:?}: nothing was written", c.kind, c.feed, c.owns))?;
        let own = st.files[i].clone().unwrap_or_else(|| Owned::new(c));
        // the layout and this writer's blocks: all it may read or touch. Another owner's rows
        // are not here, so nothing it does can depend on them or move them
        let (mut d, hidden) = compose(&st.layout, &[(c, &own)]);
        let w = Writer::Named { by: c.by(), owns: c.owns.clone() };
        let mut errs = Vec::new();
        for g in groups {
            let mut t = d.clone();
            let r: Result<(), String> = g.iter().try_for_each(|op| {
                check_owner(op, &t, &w, &st.claims)?;
                t.apply(op)
            });
            match r {
                Ok(()) => d = t,
                Err(e) => errs.push(e),
            }
        }
        let title = (d.title != st.layout.title).then(|| d.title.clone());
        let mut out = [Vec::new()];
        extract(&mut d.blocks, None, &|id| owns(&c.owns, id).then_some(0), &mut out);
        let [mut entries] = out;
        if d.blocks != st.layout.blocks {
            return Err(format!("{} {} would have changed the layout of {a}: nothing was written", c.kind, c.feed));
        }
        // what is not drawn (its parent left the layout) was not in front of the writer: it
        // stays in the file unless this write gave the same id a place
        if !hidden.is_empty() {
            let mut ids: HashSet<String> = HashSet::new();
            for e in &entries {
                crate::doc::walk(std::slice::from_ref(&e.block), &mut |b, _, _| {
                    ids.insert(b.id.clone());
                });
            }
            for (_, ei) in hidden {
                if !ids.contains(&own.blocks[ei].block.id) {
                    entries.push(own.blocks[ei].clone());
                }
            }
        }
        if entries == own.blocks && title == own.title {
            return Ok((None, errs, st.layout.rev));
        }
        let o = Owned { rev: own.rev + 1, updated: now_rfc3339(), title, blocks: entries, ..Owned::new(c) };
        self.write_owned(a, c, &o, false)?;
        self.prune_orphans(a);
        Ok((Some(o.rev), errs, st.layout.rev))
    }

    /// Apply ops to an artifact that exists, atomically: all land or none do (a missing artifact
    /// is `missing`'s error, never a create). A todo's `status` is a shared field (design §2):
    /// it goes to state.json with `by: claude`, and overruling sulin's own tick is logged.
    pub fn patch(&self, a: &str, ops: &[Op], base_rev: Option<u64>) -> Result<u64, String> {
        let _l = self.lock().map_err(|e| e.to_string())?;
        self.patch_locked(a, ops, base_rev)
    }

    fn patch_locked(&self, a: &str, ops: &[Op], base_rev: Option<u64>) -> Result<u64, String> {
        // the layout alone: an owner's rows are not this writer's to read, move or remove, and
        // a container may go from under them (they stay in their file; `margin doctor` says so)
        let st = self.settle_locked(a)?;
        let old = st.layout.clone();
        check_base(Some(&old), base_rev)?;
        let claims = &st.claims;
        let mut d = old.clone();
        let mut status_writes = Vec::new();
        for op in ops {
            let op = &st.place(op)?;
            check_owner(op, &d, &Writer::Claude, claims)?;
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
            write_durable(&self.state_path(a), serde_json::to_string_pretty(&st).unwrap().as_bytes()).map_err(|e| e.to_string())?;
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
        write_durable(&self.state_path(a), serde_json::to_string_pretty(&st).unwrap().as_bytes()).map_err(|e| e.to_string())?;
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
            return Err(format!("{id:?} is kept current by {} `{}`; its components are read-only", c.kind, c.feed));
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
        write_durable(&self.state_path(a), serde_json::to_string_pretty(&st).unwrap().as_bytes()).map_err(|e| e.to_string())?;
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
        write_durable(&self.state_path(a), serde_json::to_string_pretty(&st).unwrap().as_bytes()).map_err(|e| e.to_string())?;
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
        write_durable(&self.state_path(a), serde_json::to_string_pretty(&st).unwrap().as_bytes()).map_err(|e| e.to_string())?;
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

    /// `margin emit`: append a named event from outside the pane. `board` and `id` say what it
    /// is about (both may be empty); a board that is named must exist.
    pub fn emit(&self, name: &str, board: &str, id: &str, value: Value, by: &str, via: &str) -> Result<Event, String> {
        if !valid_event_name(name) {
            return Err(format!("bad event name {name:?}: letters, digits and . _ - : (start with a letter or digit)"));
        }
        let _l = self.lock().map_err(|e| e.to_string())?;
        let rev = if board.is_empty() { 0 } else { self.load(board)?.rev };
        self.append_event_locked(board, id, name, value, by, via, rev)
    }

    /// An action on a block (`margin act`, an action's key in the pane): append the event
    /// `<board>.<action>` carrying the block's id and a snapshot of it. It changes no content, so
    /// feed ownership does not stop it: a feed keeps a row current, the person acts on it.
    /// `id` empty or `-`: the board itself. `arg`: the CLI's optional VALUE, as `value.arg`.
    pub fn act(&self, board: &str, id: &str, action: &str, arg: Option<Value>, by: &str, via: &str) -> Result<Event, String> {
        if !crate::util::valid_id(action) {
            return Err(format!("bad action name {action:?}: letters, digits, - and _"));
        }
        let _l = self.lock().map_err(|e| e.to_string())?;
        // a row that comes from a dataset is not in the stored document: read it in, so the
        // event carries the record as the pane shows it
        let d = crate::data::with_rows(self, self.load(board)?);
        let id = if id == "-" { "" } else { id };
        let block = match id {
            "" => None,
            _ => Some(d.find(id).ok_or_else(|| format!("no component {id:?} in {board}; margin show {board}"))?),
        };
        // only what is on offer there: `open` wherever there is a target, else a declared action
        let offered = d.actions_for(Some(id));
        let has_target = block.map(|b| b.opt("target").is_some()).unwrap_or(false);
        if !(action == "open" && has_target) && !offered.iter().any(|a| a.name == action) {
            let what = if id.is_empty() { format!("the board {board}") } else { format!("{id:?} in {board}") };
            if action == "open" {
                return Err(format!("{what} has no target to open"));
            }
            let mut names: Vec<&str> = offered.iter().map(|a| a.name.as_str()).collect();
            if has_target {
                names.insert(0, "open");
            }
            return Err(format!("{what} offers no action {action:?}; {}", if names.is_empty() { "it declares none (an `actions` list on the block, its table or the document)".to_string() } else { format!("its actions: {}", names.join(" ")) }));
        }
        let mut value = block.map(snapshot).unwrap_or_else(|| json!({}));
        if let Some(a) = arg {
            value["arg"] = a;
        }
        self.append_event_locked(board, id, &format!("{board}.{action}"), value, by, via, d.rev)
    }

    /// The sidecar: (last seq handed out, the live file's length right after that append).
    fn seq_file(&self) -> Option<(u64, u64)> {
        let s = fs::read_to_string(self.seq_path()).ok()?;
        let mut it = s.split_whitespace().map(|x| x.parse::<u64>().ok());
        Some((it.next()??, it.next()??))
    }

    /// The highest seq on any parseable line of any generation: the slow path, for a log the
    /// sidecar does not describe (none yet, a crash mid-append, a hand edit). A torn or edited
    /// line is skipped and never lowers what is handed out next.
    fn scan_seq(&self) -> u64 {
        (0..=EVENTS_KEEP).flat_map(|n| parse_events(&fs::read(self.events_gen(n)).unwrap_or_default())).map(|e| e.seq).max().unwrap_or(0)
    }

    fn append_event_locked(&self, a: &str, id: &str, kind: &str, value: Value, by: &str, via: &str, rev: u64) -> Result<Event, String> {
        let path = self.events_path();
        let mut len = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let side = self.seq_file();
        // the sidecar is trusted only while the log still ends where it says: then nothing was
        // appended, torn or edited since, and the whole file need not be read
        let trusted = side.map(|(_, l)| l == len).unwrap_or(false);
        let last = match side {
            Some((s, _)) if trusted => s,
            _ => self.scan_seq().max(side.map(|(s, _)| s).unwrap_or(0)),
        };
        let ev = Event { v: 1, seq: last + 1, at: now_rfc3339(), artifact: a.into(), id: id.into(), kind: kind.into(), value, by: by.into(), via: via.into(), rev };
        let mut line = serde_json::to_string(&ev).unwrap() + "\n";
        if len >= events_cap() {
            self.rotate_events_locked()?;
            len = 0;
        } else if !trusted && len > 0 && !ends_with_newline(&path) {
            // a crash mid-append leaves a torn last line: end it first, or this event would join
            // it, read as garbage too, and have its seq handed out again
            line.insert(0, '\n');
        }
        let mut f = fs::OpenOptions::new().create(true).append(true).open(&path).map_err(|e| e.to_string())?;
        // one write, so a line is whole or absent short of a crash inside the call
        f.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
        // after the line: a crash between the two leaves a sidecar that no longer matches the
        // length, which the next append takes as "scan"
        let _ = fs::write(self.seq_path().with_extension("tmp"), format!("{} {}", ev.seq, len + line.len() as u64)).and_then(|_| fs::rename(self.seq_path().with_extension("tmp"), self.seq_path()));
        // a registered handler may want this: make sure something is there to dispatch it. Not
        // in unit tests, where the "binary" a daemon would be started from is the test harness
        if !cfg!(test) && self.wants_daemon(kind) {
            crate::daemon::wake(self);
        }
        Ok(ev)
    }

    /// Should appending an event named `kind` make sure the daemon runs? Any event that is not
    /// passive: yes (`daemon::wake` then starts one only if a handler is registered at all). A
    /// passive one (a view event, `seen`, a fold) only when a handler that is not paused asked
    /// for that name: a pane being looked at starts nothing when nobody listens.
    pub fn wants_daemon(&self, kind: &str) -> bool {
        if !is_passive(kind) {
            return true;
        }
        self.handled(kind)
    }

    /// Would a registered handler that is not paused be run for an event named `kind`? What the
    /// pane asks before it says "asked for more" (it runs nothing itself).
    pub fn handled(&self, kind: &str) -> bool {
        crate::handler::Registry::path(&self.dir()).exists() && crate::handler::Registry::load(&self.dir()).for_kind(kind).iter().any(|h| !h.paused)
    }

    /// events.jsonl becomes events.1.jsonl, the older generations move up one, and the one past
    /// EVENTS_KEEP goes. Seq goes on counting (the sidecar keeps it), so a cursor is a position
    /// in the whole log and stays valid; a cursor older than every kept generation has lost the
    /// events in between.
    fn rotate_events_locked(&self) -> Result<(), String> {
        let _ = fs::remove_file(self.events_gen(EVENTS_KEEP));
        for n in (0..EVENTS_KEEP).rev() {
            if self.events_gen(n).exists() {
                fs::rename(self.events_gen(n), self.events_gen(n + 1)).map_err(|e| format!("rotate events: {e}"))?;
            }
        }
        Ok(())
    }

    /// Every event still kept, oldest first (the rotated generations, then the live file).
    pub fn events(&self) -> Vec<Event> {
        self.events_since(0)
    }

    /// The events after `since`, oldest first. Reads the live file, and an older generation only
    /// when the live one starts after `since` (a cursor from before a rotation).
    pub fn events_since(&self, since: u64) -> Vec<Event> {
        let mut gens: Vec<Vec<Event>> = Vec::new();
        for n in 0..=EVENTS_KEEP {
            // a missing generation is not the end: the live file is absent right after a rotation
            let Ok(bytes) = fs::read(self.events_gen(n)) else { continue };
            let evs = parse_events(&bytes);
            let covered = evs.first().map(|e| e.seq <= since + 1).unwrap_or(false);
            gens.push(evs);
            if covered {
                break;
            }
        }
        let mut out: Vec<Event> = gens.into_iter().rev().flatten().filter(|e| e.seq > since).collect();
        // a rotation between two of the reads above shows one generation twice
        let mut seen = std::collections::HashSet::new();
        out.retain(|e| seen.insert(e.seq));
        out
    }

    /// The last seq handed out. Cheap: the sidecar, while the log still ends where it says.
    pub fn last_seq(&self) -> u64 {
        let len = fs::metadata(self.events_path()).map(|m| m.len()).unwrap_or(0);
        match self.seq_file() {
            Some((s, l)) if l == len => s,
            side => self.scan_seq().max(side.map(|(s, _)| s).unwrap_or(0)),
        }
    }

    /// A named cursor's position: the last seq its consumer has taken. None: never used.
    pub fn cursor(&self, name: &str) -> Option<u64> {
        read_cursor(&self.cursor_path(name))
    }

    pub fn cursor_path(&self, name: &str) -> PathBuf {
        self.dir().join("cursors").join(cursor_key(name))
    }

    /// A cursor's position, creating it at the end of the log on first use: a new consumer
    /// hears what happens from now on, not the project's history (`--since 0` replays).
    pub fn cursor_or_create(&self, name: &str) -> u64 {
        let p = self.cursor_path(name);
        if let Some(n) = read_cursor(&p) {
            return n;
        }
        let at = self.last_seq();
        create_cursor(&p, at);
        read_cursor(&p).unwrap_or(at)
    }

    /// Has any cursor that is not a handler's moved to `seq` or past it? That is an outside
    /// tool following the log (`margin events --follow --cursor`): it took the event.
    pub fn outside_cursor_passed(&self, seq: u64) -> bool {
        fs::read_dir(self.dir().join("cursors"))
            .map(|rd| rd.flatten().filter(|e| !e.file_name().to_string_lossy().starts_with("handler.")).any(|e| read_cursor(&e.path()).map(|c| c >= seq).unwrap_or(false)))
            .unwrap_or(false)
    }

    pub fn set_cursor(&self, name: &str, seq: u64) {
        let _ = write_durable(&self.cursor_path(name), seq.to_string().as_bytes());
    }

    /// The last event seq handed to `session`: a cursor like any other, kept under `delivered/`
    /// so `delivered_max` ("has Claude read it") counts sessions only. A session margin hasn't
    /// seen starts where the furthest-along session is: actions no session has read yet still
    /// reach it, while ones already handed to another session aren't replayed as binding (a
    /// fresh session got four old picks, two of them for one question, 2026-11-06).
    pub fn delivered(&self, session: &str) -> u64 {
        read_cursor(&self.delivered_path(session)).unwrap_or_else(|| self.delivered_max())
    }

    fn delivered_path(&self, session: &str) -> PathBuf {
        self.dir().join("delivered").join(session_key(session))
    }

    /// A hook's first sight of `session`: write its cursor down where `delivered` puts a new
    /// session. Without the file that fallback is taken afresh on every read, so it slides past
    /// whatever another session reads in the meantime and this one never hears it.
    pub fn first_seen(&self, session: &str) {
        let p = self.delivered_path(session);
        if !p.exists() {
            create_cursor(&p, self.delivered_max());
        }
    }

    pub fn set_delivered(&self, session: &str, seq: u64) {
        let _ = write_durable(&self.delivered_path(session), seq.to_string().as_bytes());
    }

    /// Session cursors (`delivered/<session>`) not touched for `days`: a session that long gone
    /// will not be back for its unread decisions. Named cursors (`cursors/`: a tool's, a
    /// handler's) are never listed: nothing says their owner is gone. For `margin gc` and
    /// `margin doctor`.
    pub fn stale_session_cursors(&self, days: u32) -> Vec<PathBuf> {
        if days == 0 {
            return vec![];
        }
        let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(days as u64 * 86400);
        fs::read_dir(self.dir().join("delivered"))
            .map(|rd| rd.flatten().filter(|e| e.metadata().and_then(|m| m.modified()).map(|t| t < cutoff).unwrap_or(false)).map(|e| e.path()).collect())
            .unwrap_or_default()
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
                    .filter_map(|e| read_cursor(&e.path()))
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
    ///
    /// Removing a board by name (`margin rm <board> --yes`) also unregisters the handlers and
    /// owners that claimed a prefix on it: with the board gone they could only fail. What was
    /// unregistered is written beside the archived board as `unregistered.json`.
    pub fn archive(&self, a: &str) -> Result<PathBuf, String> {
        let _l = self.lock().map_err(|e| e.to_string())?;
        let dest = self.archive_locked(a)?;
        // after the move: a crash in between leaves a claim on a board that is gone, which
        // every command already tolerates; the other order would leave rows nobody claims
        let dir = self.dir();
        let mut gone = json!({});
        if let Ok(mut r) = crate::handler::Registry::load_strict(&dir) {
            let (out, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut r.handlers).into_iter().partition(|h| h.board == a);
            r.handlers = keep;
            if !out.is_empty() && r.save(&dir).is_ok() {
                gone["handlers"] = json!(out);
            }
        }
        if let Ok(mut r) = crate::owner::Registry::load_strict(&dir) {
            let (out, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut r.owners).into_iter().partition(|o| o.board == a);
            r.owners = keep;
            if !out.is_empty() && r.save(&dir).is_ok() {
                gone["owners"] = json!(out);
            }
        }
        if gone.as_object().map(|m| !m.is_empty()).unwrap_or(false) {
            let _ = write_atomic(&dest.join("unregistered.json"), serde_json::to_string_pretty(&gone).unwrap().as_bytes());
        }
        Ok(dest)
    }

    /// `margin gc`: the move alone. A board that went quiet keeps its registrations, so moving
    /// its directory back restores it whole.
    fn archive_quiet(&self, a: &str) -> Result<PathBuf, String> {
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
                // an owner's write is a touch of the board too, though doc.json does not move
                .chain(fs::read_dir(self.owned_dir(&d.id)).into_iter().flatten().filter_map(|e| e.ok()?.metadata().ok()?.modified().ok()))
                .max();
            if newest.map(|t| t < cutoff).unwrap_or(false) && self.archive_quiet(&d.id).is_ok() {
                moved.push(d.id.clone());
            }
        }
        moved
    }
}

fn ends_with_newline(p: &Path) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let mut b = [0u8; 1];
    fs::File::open(p).and_then(|mut f| f.seek(SeekFrom::End(-1)).and_then(|_| f.read_exact(&mut b))).map(|_| b[0] == b'\n').unwrap_or(true)
}

fn check_base(old: Option<&Doc>, base: Option<u64>) -> Result<(), String> {
    if let Some(b) = base {
        let cur = old.map(|d| d.rev).unwrap_or(0);
        if cur != b {
            return Err(format!(
                "stale base: you edited rev {b} but the board's layout is at rev {cur}; run `margin show` and redo the change on rev {cur}"
            ));
        }
    }
    Ok(())
}

/// Every project directory under the store home (for resolving tokens without a known project).
pub fn projects(home: &Path) -> Vec<String> {
    fs::read_dir(home)
        .map(|rd| rd.filter_map(|e| e.ok()).filter(|e| e.path().is_dir()).map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n != crate::pin::DIR).collect())
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
        assert!(!s.art("plan").join("history.jsonl").exists(), "old revisions are not kept");
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
        let plain = "no board \"scratch\" in project proj: create it with margin new scratch";
        assert_eq!(s.missing("scratch"), plain);
        assert_eq!(s.missing("paln"), "no board \"paln\" in project proj: create it with margin new paln (did you mean \"plan\"?)");
        assert!(s.missing("review").ends_with("(did you mean \"reviews\"?)"));
        assert!(s.missing("plans2").ends_with("(did you mean \"plan\"?)") && !s.missing("planning").contains("did you mean"), "within two edits, no further");
        let set = Op::Set { id: "t1".into(), field: "text".into(), value: json!("x") };
        assert_eq!(s.patch("scratch", &[set.clone()], None).unwrap_err(), plain);
        assert_eq!(s.replace(Doc::empty("scratch"), None).unwrap_err(), plain);
        assert_eq!(s.load("scratch").unwrap_err(), plain);
        assert_eq!(s.archive("scratch").unwrap_err(), plain);
        let w = Writer::Named { by: "handler:f".into(), owns: "f".into() };
        assert_eq!(s.handler_commit("scratch", &[vec![set]], &w, &[], None), (None, vec![plain.to_string()]));
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

    /// A registry that won't parse used to read as "nothing registered", so nothing was owned
    /// and Claude's write landed in an owner's ids.
    #[test]
    fn a_corrupt_registry_refuses_writes_instead_of_dropping_ownership() {
        let s = temp_store();
        s.put(plan(), None).unwrap();
        fs::write(s.dir().join("handlers.json"), r#"{"handlers": [{"name": "jj"},]}"#).unwrap();
        let set = [Op::Set { id: "t1".into(), field: "text".into(), value: json!("reworded") }];
        let e = s.patch("plan", &set, None).unwrap_err();
        assert!(e.contains("handlers.json"), "{e}");
        assert!(s.decide("plan", "t1", "tick", json!("done"), "key", None).unwrap_err().contains("handlers.json"));
        assert!(s.put(plan(), None).unwrap_err().contains("handlers.json"));
        assert_eq!(s.load("plan").unwrap().rev, 1, "nothing landed");
        // fixed (here: removed), writes go through again
        fs::remove_file(s.dir().join("handlers.json")).unwrap();
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

    /// `emit` appends a named event with the existing fields: the name is the kind, `by`/`via`
    /// say who and how. A bad name or a board that isn't there is refused and logs nothing.
    #[test]
    fn emit_appends_a_named_event() {
        let s = temp_store();
        s.put(plan(), None).unwrap();
        let e = s.emit("deploy.finished", "", "", json!({"ok": true}), "you", "emit").unwrap();
        assert_eq!((e.seq, e.kind.as_str(), e.by.as_str(), e.via.as_str(), e.artifact.as_str()), (1, "deploy.finished", "you", "emit", ""));
        let e = s.emit("ci.red", "plan", "t1", Value::Null, "cron", "emit").unwrap();
        assert_eq!((e.seq, e.artifact.as_str(), e.id.as_str(), e.rev), (2, "plan", "t1", 1));
        for bad in ["", "two words", "-x", "a/b", "a=b"] {
            assert!(s.emit(bad, "", "", Value::Null, "you", "emit").unwrap_err().contains("bad event name"), "{bad}");
        }
        assert!(s.emit("x", "nope", "", Value::Null, "you", "emit").unwrap_err().contains("no board"));
        assert_eq!(s.events().len(), 2);
        // an emitted event is nobody's decision: the hooks never hand it to a Claude session
        assert!(s.events().iter().all(|e| !is_decision(e)));
        fs::remove_dir_all(&s.home).ok();
    }

    /// Concurrent appends from separate handles (as separate processes would): every seq is
    /// handed out once, with no gap.
    #[test]
    fn concurrent_emits_get_distinct_consecutive_seqs() {
        let s = temp_store();
        let hs: Vec<_> = (0..16)
            .map(|i| {
                let st = Store::new(s.home.clone(), s.project.clone());
                std::thread::spawn(move || st.emit("tick", "", "", json!(i), "you", "emit").unwrap().seq)
            })
            .collect();
        let mut seqs: Vec<u64> = hs.into_iter().map(|h| h.join().unwrap()).collect();
        seqs.sort();
        assert_eq!(seqs, (1..=16).collect::<Vec<u64>>());
        assert_eq!(s.last_seq(), 16);
        assert_eq!(s.events().iter().map(|e| e.seq).collect::<Vec<_>>(), (1..=16).collect::<Vec<u64>>());
        fs::remove_dir_all(&s.home).ok();
    }

    /// The sidecar spares the append a read of the log, and is believed only while the log
    /// still ends where it says: a log written behind its back is scanned again.
    #[test]
    fn the_seq_sidecar_is_checked_against_the_log() {
        let s = temp_store();
        for _ in 0..3 {
            s.emit("a", "", "", Value::Null, "you", "emit").unwrap();
        }
        let len = fs::metadata(s.events_path()).unwrap().len();
        assert_eq!(fs::read_to_string(s.seq_path()).unwrap(), format!("3 {len}"));
        // a sidecar that lies about the seq but matches the length is the fast path: believed
        fs::write(s.seq_path(), format!("40 {len}")).unwrap();
        assert_eq!(s.emit("a", "", "", Value::Null, "you", "emit").unwrap().seq, 41);
        // a line appended behind its back (an older binary, a hand edit): the log is scanned,
        // and the seq never goes below what was handed out
        let mut f = fs::OpenOptions::new().append(true).open(s.events_path()).unwrap();
        f.write_all(b"{\"v\":1,\"seq\":90,\"at\":\"2026-11-10T10:00:00Z\",\"artifact\":\"\",\"id\":\"\",\"kind\":\"x\",\"by\":\"you\",\"via\":\"emit\"}\n").unwrap();
        assert_eq!(s.last_seq(), 90);
        assert_eq!(s.emit("a", "", "", Value::Null, "you", "emit").unwrap().seq, 91);
        // no sidecar at all (a store from before it existed): scanned once, then written
        fs::remove_file(s.seq_path()).unwrap();
        assert_eq!(s.emit("a", "", "", Value::Null, "you", "emit").unwrap().seq, 92);
        assert!(fs::read_to_string(s.seq_path()).unwrap().starts_with("92 "));
        fs::remove_dir_all(&s.home).ok();
    }

    /// Rotation: the live file moves to events.1.jsonl, seq goes on counting, and a cursor on
    /// either side of the rotation reads exactly what came after it.
    #[test]
    fn rotation_keeps_seq_and_cursors_valid() {
        let s = temp_store();
        for i in 0..5 {
            s.emit("old", "", "", json!(i), "you", "emit").unwrap();
        }
        s.set_cursor("before", 3);
        {
            let _l = s.lock().unwrap();
            s.rotate_events_locked().unwrap();
        }
        assert!(s.events_gen(1).exists() && !s.events_path().exists());
        assert_eq!(s.last_seq(), 5, "the sidecar still knows, with no live file");
        for i in 0..4 {
            assert_eq!(s.emit("new", "", "", json!(i), "you", "emit").unwrap().seq, 6 + i);
        }
        s.set_cursor("after", 7);
        let seqs = |c: &str| s.events_since(s.cursor(c).unwrap()).iter().map(|e| e.seq).collect::<Vec<_>>();
        assert_eq!(seqs("before"), [4, 5, 6, 7, 8, 9], "a cursor from before the rotation reads across it");
        assert_eq!(seqs("after"), [8, 9]);
        assert_eq!(s.events().len(), 9);
        // a second and third rotation: two generations are kept, the oldest goes, seq never restarts
        for _ in 0..2 {
            let _l = s.lock().unwrap();
            s.rotate_events_locked().unwrap();
        }
        assert_eq!(s.emit("newest", "", "", Value::Null, "you", "emit").unwrap().seq, 10);
        assert_eq!(s.events().iter().map(|e| e.seq).collect::<Vec<_>>(), [6, 7, 8, 9, 10], "events 1-5 went with the third generation");
        assert_eq!(seqs("after"), [8, 9, 10]);
        // a torn last line is still ended before the next event, rotation or not
        let mut f = fs::OpenOptions::new().append(true).open(s.events_path()).unwrap();
        f.write_all(br#"{"v":1,"seq":11,"at":"2026"#).unwrap();
        assert_eq!(s.emit("after-torn", "", "", Value::Null, "you", "emit").unwrap().seq, 11);
        assert_eq!(s.events().last().unwrap().kind, "after-torn");
        fs::remove_dir_all(&s.home).ok();
    }

    /// The size cap rotates on the append itself.
    #[test]
    fn the_log_rotates_at_its_cap() {
        let s = temp_store();
        let big = "x".repeat(600);
        // the cap is read per append; this test owns the variable (no other test sets it)
        std::env::set_var("MARGIN_EVENTS_CAP", "2000");
        for _ in 0..12 {
            s.emit("big", "", "", json!(big), "you", "emit").unwrap();
        }
        std::env::remove_var("MARGIN_EVENTS_CAP");
        assert!(s.events_gen(1).exists() && s.events_gen(2).exists() && !s.events_gen(3).exists());
        assert!(fs::metadata(s.events_path()).unwrap().len() < 3000);
        let seqs: Vec<u64> = s.events().iter().map(|e| e.seq).collect();
        assert_eq!(*seqs.last().unwrap(), 12);
        assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1), "{seqs:?}");
        assert_eq!(s.events_since(10).len(), 2);
        fs::remove_dir_all(&s.home).ok();
    }

    /// A named cursor starts at the end of the log, moves only when set, and its name can't
    /// leave the cursors directory.
    #[test]
    fn named_cursors_start_at_the_end() {
        let s = temp_store();
        s.emit("a", "", "", Value::Null, "you", "emit").unwrap();
        s.emit("a", "", "", Value::Null, "you", "emit").unwrap();
        assert_eq!(s.cursor("mine"), None);
        assert_eq!(s.cursor_or_create("mine"), 2);
        s.emit("a", "", "", Value::Null, "you", "emit").unwrap();
        assert_eq!(s.cursor_or_create("mine"), 2, "created once: it does not slide");
        assert_eq!(s.events_since(s.cursor("mine").unwrap()).len(), 1);
        s.set_cursor("mine", 3);
        assert_eq!(s.cursor("mine"), Some(3));
        assert!(s.dir().join("cursors/mine").exists());
        s.set_cursor("../../evil", 1);
        assert!(!s.home.join("evil").exists() && !s.dir().join("evil").exists());
        s.set_cursor("..", 1);
        assert_eq!(s.cursor(".."), Some(1));
        // a session's cursor is the same file format, in its own directory
        s.set_delivered("sess", 2);
        assert_eq!(read_cursor(&s.dir().join("delivered/sess")), Some(2));
        assert_eq!(s.delivered_max(), 2, "named cursors don't count as a session having read");
        fs::remove_dir_all(&s.home).ok();
    }

    /// The view events are passive by their name: not decisions, not actions (though named
    /// like one), and an emit under such a name is passive too. Nothing else with a dot is.
    #[test]
    fn view_events_are_passive_by_name() {
        let ev = |kind: &str, art: &str, by: &str, via: &str| Event { v: 1, seq: 1, at: String::new(), artifact: art.into(), id: String::new(), kind: kind.into(), value: Value::Null, by: by.into(), via: via.into(), rev: 0 };
        for k in ["reviews.shown", "reviews.hidden", "reviews.detail", "a.b.detail"] {
            assert!(is_view(k) && is_passive(k), "{k}");
            let e = ev(k, k.rsplit_once('.').unwrap().0, "you", "pane");
            assert!(!is_decision(&e) && !is_action(&e), "{k}");
        }
        for k in ["reviews.open", "reviews.approve", "reviews.focus", "detail", "shown", ".detail", "reviews.details", "deploy.finished"] {
            assert!(!is_view(k), "{k}");
        }
        assert!(is_action(&ev("reviews.open", "reviews", "you", "key")) && is_passive("seen") && !is_passive("reviews.approve"));
        // nobody registered: a passive event wants no daemon; anything else leaves it to wake
        let s = temp_store();
        assert!(!s.wants_daemon("reviews.detail") && !s.wants_daemon("seen") && s.wants_daemon("reviews.approve") && s.wants_daemon("tick"));
        assert!(!s.handled("reviews.detail") && !s.handled("reviews.approve"));
        fs::remove_dir_all(&s.home).ok();
    }

    /// An action appends `<board>.<action>` with the block's id and snapshot, on a feed-owned
    /// row too (ownership guards content, not the person's actions); `decide` still refuses there.
    #[test]
    fn act_appends_the_event_with_a_snapshot_even_on_a_feed_owned_row() {
        let s = temp_store();
        let d: Doc = serde_json::from_value(json!({"id": "reviews", "title": "r", "blocks": [
            {"type": "table", "id": "pr", "columns": [{"key": "title"}], "children": [
                {"type": "tr", "id": "pr-1", "target": "https://example.com/1", "detail": "d", "cells": {"title": "One"}}]},
            {"type": "row", "id": "note", "text": "hello", "tag": "x", "actions": [{"key": "e", "name": "ping"}]}],
            "actions": [{"key": "u", "name": "refresh"}]})).unwrap();
        s.put(d, None).unwrap();
        fs::write(s.dir().join("owners.json"), r#"{"owners": [{"name": "reviews", "board": "reviews", "owns": "pr"}]}"#).unwrap();
        assert!(s.decide("reviews", "pr-1", "pin", json!(true), "key", None).unwrap_err().contains("read-only"));
        let e = s.act("reviews", "pr-1", "open", None, "you", "act").unwrap();
        assert_eq!((e.kind.as_str(), e.id.as_str(), e.by.as_str(), e.via.as_str(), e.artifact.as_str()), ("reviews.open", "pr-1", "you", "act", "reviews"));
        assert_eq!(e.value, json!({"cells": {"title": "One"}, "target": "https://example.com/1", "detail": "d"}));
        let e = s.act("reviews", "note", "ping", Some(json!("now")), "you", "act").unwrap();
        assert_eq!(e.value, json!({"text": "hello", "tag": "x", "arg": "now"}));
        assert_eq!(s.act("reviews", "-", "refresh", None, "you", "act").unwrap().id, "");
        assert!(s.act("reviews", "nope", "open", None, "you", "act").unwrap_err().contains("no component \"nope\""));
        assert!(s.act("review", "pr-1", "open", None, "you", "act").unwrap_err().contains("did you mean \"reviews\""));
        assert!(s.act("reviews", "pr-1", "bad name", None, "you", "act").is_err());
        // only what the block offers: open (it has a target) and the board's refresh
        assert_eq!(s.act("reviews", "pr-1", "ping", None, "you", "act").unwrap_err(), "\"pr-1\" in reviews offers no action \"ping\"; its actions: open refresh");
        assert_eq!(s.act("reviews", "note", "open", None, "you", "act").unwrap_err(), "\"note\" in reviews has no target to open");
        assert_eq!(s.events().len(), 3, "a refusal logs nothing");
        assert_eq!(s.load("reviews").unwrap().rev, 1, "and an action changes no content");
        fs::remove_dir_all(&s.home).ok();
    }

    // ------------------------------------------------------------ one writer per file

    fn owner(name: &str, board: &str, owns: &str) -> crate::owner::Owner {
        crate::owner::Owner { name: name.into(), board: board.into(), owns: owns.into(), expect_every: None, added: "2026-11-10T10:00:00Z".into(), last_write: String::new() }
    }
    fn handler(name: &str, board: &str, owns: &str) -> crate::handler::Handler {
        crate::handler::Handler { name: name.into(), cmd: vec!["true".into()], on: vec![format!("{name}.go")], board: board.into(), owns: owns.into(), paused: false, cwd: "/".into(), path: String::new(), timeout: 60.0, retries: 3, added: String::new() }
    }
    fn line(v: Value) -> Vec<Op> {
        crate::doc::parse_line(&v.to_string()).unwrap()
    }
    fn top(d: &Doc) -> Vec<&str> {
        d.blocks.iter().map(|b| b.id.as_str()).collect()
    }
    fn kids<'a>(d: &'a Doc, id: &str) -> Vec<&'a str> {
        d.find(id).unwrap().children.iter().map(|b| b.id.as_str()).collect()
    }
    /// A layout with two containers and a note between them, and two owners and a handler.
    fn dash() -> Store {
        let s = temp_store();
        let d: Doc = serde_json::from_value(json!({"id": "dash", "title": "Dash", "blocks": [
            {"type": "section", "id": "top", "title": "Top", "children": [{"type": "text", "id": "intro", "text": "the layout's own"}]},
            {"type": "text", "id": "note", "text": "between"},
            {"type": "table", "id": "jobs", "columns": [{"key": "name"}]}]}))
        .unwrap();
        s.create(d).unwrap();
        crate::handler::edit_registry(&s, |r| r.add(handler("fetch", "dash", "f"))).unwrap();
        s.add_owner(owner("queue", "dash", "q")).unwrap();
        s.add_owner(owner("ci", "dash", "ci")).unwrap();
        s
    }

    /// An owner's write lands in the owner's file and nowhere else: doc.json keeps its bytes,
    /// the board keeps its rev, and a layout write made against that rev goes through after any
    /// number of owner writes. Only a layout write makes a base stale.
    #[test]
    fn an_owner_write_leaves_the_layout_and_its_rev_alone() {
        let s = dash();
        let layout = fs::read(s.doc_path("dash")).unwrap();
        for i in 0..25 {
            assert_eq!(s.apply_as("dash", "queue", &line(json!({"type": "tr", "id": format!("q-{}", i % 5), "parent": "jobs", "cells": {"name": format!("job {i}")}}))).unwrap(), 1);
        }
        let w = Writer::Named { by: "handler:fetch".into(), owns: "f".into() };
        assert_eq!(s.handler_commit("dash", &[line(json!({"type": "row", "id": "f-1", "text": "fetched", "parent": "top"}))], &w, &[], None), (Some(1), vec![]));
        assert_eq!(fs::read(s.doc_path("dash")).unwrap(), layout, "doc.json is byte for byte what the layout writer left");
        let d = s.load("dash").unwrap();
        assert_eq!((d.rev, d.owned["owner:queue"].rev, d.owned["handler:fetch"].rev, d.owned.len()), (1, 25, 1, 2));
        // `by` and `updated` are the board's last change, an owner's included (to the second)
        assert!(d.by != s.author && d.updated >= d.owned["owner:queue"].updated, "{}", d.by);
        assert_eq!((kids(&d, "jobs").len(), kids(&d, "top")), (5, vec!["intro", "f-1"]));
        // the assistant read rev 1 before all of that: its edit still applies
        let set = Op::Set { id: "note".into(), field: "text".into(), value: json!("edited") };
        assert_eq!(s.patch("dash", &[set.clone()], Some(1)).unwrap(), 2);
        assert!(s.patch("dash", &[set], Some(1)).unwrap_err().contains("stale base"), "another layout write is what makes it stale");
        // each owner's file holds its own blocks and nobody else's
        let q: Owned = serde_json::from_str(&fs::read_to_string(s.art("dash").join("owned/owner.queue.json")).unwrap()).unwrap();
        assert_eq!((q.by.as_str(), q.owns.as_str(), q.rev, q.blocks.len()), ("owner:queue", "q", 25, 5));
        assert!(q.blocks.iter().all(|e| e.parent.as_deref() == Some("jobs") && e.after.is_none() && e.block.id.starts_with("q-")));
        assert!(!s.art("dash").join("owned/owner.ci.json").exists(), "an owner that has not written has no file");
        fs::remove_dir_all(&s.home).ok();
    }

    /// The composed order: a parent's layout children in their order, then the owners' blocks,
    /// owners in registration order (handlers, then owners), each owner's blocks in its own
    /// order. An owned block written beside a layout sibling keeps that place when the layout
    /// grows.
    #[test]
    fn composition_puts_layout_children_first_then_each_owners_blocks_in_order() {
        let s = dash();
        let tr = |id: &str| line(json!({"type": "tr", "id": id, "parent": "jobs", "cells": {"name": id}}));
        // written in the opposite order to their registration
        s.apply_as("dash", "ci", &[tr("ci-2"), tr("ci-1")].concat()).unwrap();
        s.apply_as("dash", "queue", &tr("q-1")).unwrap();
        let w = Writer::Named { by: "handler:fetch".into(), owns: "f".into() };
        s.handler_commit("dash", &[tr("f-1")], &w, &[], None);
        s.apply_as("dash", "queue", &tr("q-2")).unwrap();
        assert_eq!(kids(&s.load("dash").unwrap(), "jobs"), ["f-1", "q-1", "q-2", "ci-2", "ci-1"]);
        // under a parent with layout children, and at the top level: after them
        s.apply_as("dash", "queue", &line(json!({"type": "row", "id": "q-in-top", "text": "x", "parent": "top"}))).unwrap();
        s.apply_as("dash", "ci", &line(json!({"type": "row", "id": "ci-top", "text": "y"}))).unwrap();
        let d = s.load("dash").unwrap();
        assert_eq!((kids(&d, "top"), top(&d)), (vec!["intro", "q-in-top"], vec!["top", "note", "jobs", "ci-top"]));
        // the layout grows: its new blocks land where the layout writer put them, and an owned
        // block stays after the layout sibling it followed
        let add = |id: &str, parent: Option<&str>| Op::Add { parent: parent.map(String::from), block: serde_json::from_value(json!({"type": "text", "id": id, "text": id})).unwrap(), before: None, after: None };
        s.patch("dash", &[add("outro", Some("top")), add("foot", None)], None).unwrap();
        let d = s.load("dash").unwrap();
        assert_eq!((kids(&d, "top"), top(&d)), (vec!["intro", "q-in-top", "outro"], vec!["top", "note", "jobs", "ci-top", "foot"]));
        // an owner places a block of its own before a layout sibling, and it stays there
        s.apply_as("dash", "ci", &[Op::Add { parent: None, block: serde_json::from_value(json!({"type": "row", "id": "ci-first", "text": "z"})).unwrap(), before: Some("top".into()), after: None }]).unwrap();
        assert_eq!(top(&s.load("dash").unwrap()), ["ci-first", "top", "note", "jobs", "ci-top", "foot"]);
        // a layout block asked to go after an owner's block goes right after it
        s.patch("dash", &[Op::Add { parent: None, block: serde_json::from_value(json!({"type": "text", "id": "under", "text": "u"})).unwrap(), before: None, after: Some("ci-top".into()) }], None).unwrap();
        assert_eq!(top(&s.load("dash").unwrap()), ["ci-first", "top", "note", "jobs", "ci-top", "under", "foot"]);
        let e = s.patch("dash", &[Op::Add { parent: None, block: serde_json::from_value(json!({"type": "text", "id": "over", "text": "o"})).unwrap(), before: Some("ci-top".into()), after: None }], None).unwrap_err();
        assert!(e.contains("belongs to owner `ci`") && e.contains("--after ci-top"), "{e}");
        // composing is deterministic: the same files, the same board
        assert_eq!(s.load("dash").unwrap(), s.load("dash").unwrap());
        fs::remove_dir_all(&s.home).ok();
    }

    /// An owner may write under a container of the layout, and only one that is there: a
    /// missing parent is refused by name. The layout writer may remove the container: the rows
    /// stay in the owner's file, are not drawn, are reported, survive the owner's next write,
    /// and draw again when the container is back.
    #[test]
    fn an_owned_block_whose_parent_left_the_layout_is_kept_and_reported() {
        let s = dash();
        let tr = |id: &str, parent: &str| line(json!({"type": "tr", "id": id, "parent": parent, "cells": {"name": id}}));
        let e = s.apply_as("dash", "queue", &tr("q-1", "nowhere")).unwrap_err();
        assert!(e.contains("owner queue writes under \"nowhere\"") && e.contains("the layout of dash has no such block"), "{e}");
        s.apply_as("dash", "queue", &[tr("q-1", "jobs"), tr("q-2", "jobs")].concat()).unwrap();
        // inside the prefix the layout writer is still refused; the container itself is its own
        assert!(s.patch("dash", &[Op::Rm { id: "q-1".into() }], None).unwrap_err().contains("belongs to owner `queue`"));
        assert!(s.patch("dash", &[Op::Add { parent: Some("q-1".into()), block: serde_json::from_value(json!({"type": "text", "id": "x", "text": "x"})).unwrap(), before: None, after: None }], None).is_err());
        assert_eq!(s.patch("dash", &[Op::Rm { id: "jobs".into() }], None).unwrap(), 2);
        let d = s.load("dash").unwrap();
        assert!(d.find("jobs").is_none() && d.find("q-1").is_none(), "not drawn");
        assert_eq!(s.inspect("dash").hidden, vec![("owner:queue".to_string(), "jobs".to_string(), vec!["q-1".to_string(), "q-2".to_string()])]);
        // the owner goes on writing elsewhere: what is hidden is not lost
        s.apply_as("dash", "queue", &line(json!({"type": "row", "id": "q-9", "text": "elsewhere", "parent": "top"}))).unwrap();
        let q: Owned = serde_json::from_str(&fs::read_to_string(s.art("dash").join("owned/owner.queue.json")).unwrap()).unwrap();
        assert_eq!(q.blocks.iter().map(|e| e.block.id.as_str()).collect::<Vec<_>>(), ["q-9", "q-1", "q-2"]);
        // and writing under the missing container is refused, naming it
        assert!(s.apply_as("dash", "queue", &tr("q-3", "jobs")).unwrap_err().contains("\"jobs\""));
        // the container comes back: so do the rows
        s.patch("dash", &[Op::Add { parent: None, block: serde_json::from_value(json!({"type": "table", "id": "jobs", "columns": [{"key": "name"}]})).unwrap(), before: None, after: None }], None).unwrap();
        assert_eq!(kids(&s.load("dash").unwrap(), "jobs"), ["q-1", "q-2"]);
        assert!(s.inspect("dash").hidden.is_empty());
        fs::remove_dir_all(&s.home).ok();
    }

    /// A board from before owners had files: every block in doc.json, the owner's among them.
    fn old_board(s: &Store) -> String {
        s.create(Doc::empty("old")).unwrap();
        let stored = json!({"v": 1, "id": "old", "title": "Old", "kind": "", "rev": 41, "updated": "2026-11-01T10:00:00Z", "by": "owner:queue", "blocks": [
            {"type": "text", "id": "head", "text": "the layout"},
            {"type": "table", "id": "pr", "columns": [{"key": "title"}], "children": [
                {"type": "tr", "id": "pr-1", "target": "https://example.com/1", "cells": {"title": "One"}},
                {"type": "tr", "id": "pr-2", "cells": {"title": "Two"}}]},
            {"type": "text", "id": "foot", "text": "more layout"},
            {"type": "section", "id": "jj", "title": "left by a runner that is gone", "children": [{"type": "row", "id": "jj-1", "text": "a change"}]}]});
        let text = serde_json::to_string_pretty(&stored).unwrap();
        fs::write(s.doc_path("old"), &text).unwrap();
        fs::write(s.dir().join("owners.json"), r#"{"owners": [{"name": "queue", "board": "old", "owns": "pr", "expect_every": 600.0, "added": "2026-10-01T10:00:00Z", "last_write": "2026-11-01T10:00:00Z"}]}"#).unwrap();
        text
    }

    /// The migration: a board whose doc.json holds an owner's blocks reads as it always did,
    /// and the first write to it (here the owner's) moves those blocks into the owner's file in
    /// one locked step: nothing is lost, nothing moves on screen, the layout keeps its rev, and
    /// the owner's last write is still when it last wrote. Blocks under a prefix nobody claims
    /// are layout and stay.
    #[test]
    fn an_old_board_moves_its_owners_blocks_out_on_the_first_write() {
        let s = temp_store();
        let before = old_board(&s);
        let d = s.load("old").unwrap();
        assert_eq!((d.rev, top(&d), kids(&d, "pr")), (41, vec!["head", "pr", "foot", "jj"], vec!["pr-1", "pr-2"]));
        assert_eq!(fs::read_to_string(s.doc_path("old")).unwrap(), before, "reading moves nothing");
        assert_eq!(s.inspect("old").legacy.iter().map(|(c, n)| (c.feed.as_str(), *n)).collect::<Vec<_>>(), [("queue", 3)]);
        let ev = s.act("old", "pr-1", "open", None, "you", "act").unwrap();
        assert_eq!(ev.value, json!({"cells": {"title": "One"}, "target": "https://example.com/1"}));
        // the owner's next write, as it always wrote: one more row
        s.apply_as("old", "queue", &line(json!({"type": "tr", "id": "pr-3", "parent": "pr", "cells": {"title": "Three"}}))).unwrap();
        let layout: Doc = serde_json::from_str(&fs::read_to_string(s.doc_path("old")).unwrap()).unwrap();
        assert_eq!((layout.rev, layout.ids()), (41, vec!["head".to_string(), "foot".into(), "jj".into(), "jj-1".into()]), "the layout: what nobody claims, at the rev it had");
        let q: Owned = serde_json::from_str(&fs::read_to_string(s.art("old").join("owned/owner.queue.json")).unwrap()).unwrap();
        assert_eq!((q.rev, q.blocks.len(), q.blocks[0].parent.as_deref(), q.blocks[0].after.as_deref(), q.blocks[0].block.children.len()), (2, 1, None, Some("head"), 3));
        let d = s.load("old").unwrap();
        assert_eq!((d.rev, top(&d), kids(&d, "pr")), (41, vec!["head", "pr", "foot", "jj"], vec!["pr-1", "pr-2", "pr-3"]), "on screen: the same board, one row more");
        assert!(s.inspect("old") == Inspect::default());
        // an action on the owned row still carries its snapshot
        assert_eq!(s.act("old", "pr-3", "open", None, "you", "act").unwrap_err(), "\"pr-3\" in old has no target to open");
        assert_eq!(s.act("old", "pr-1", "open", None, "you", "act").unwrap().value["cells"]["title"], "One");
        // the unclaimed rows are ordinary layout: anyone writes them
        assert_eq!(s.patch("old", &[Op::Set { id: "jj-1".into(), field: "text".into(), value: json!("mine") }], Some(41)).unwrap(), 42);
        fs::remove_dir_all(&s.home).ok();
    }

    /// A layout write migrates too (any first write does), and the owner's clock does not
    /// start again because its rows moved.
    #[test]
    fn a_layout_write_migrates_and_keeps_the_owners_last_write() {
        let s = temp_store();
        old_board(&s);
        assert_eq!(s.patch("old", &[Op::Set { id: "head".into(), field: "text".into(), value: json!("edited") }], Some(41)).unwrap(), 42);
        assert!(s.art("old").join("owned/owner.queue.json").exists());
        let d = s.load("old").unwrap();
        assert_eq!((top(&d), kids(&d, "pr"), d.owned["owner:queue"].rev), (vec!["head", "pr", "foot", "jj"], vec!["pr-1", "pr-2"], 1));
        let o = crate::owner::Registry::load(&s.dir()).get("queue").unwrap().clone();
        assert_eq!(o.last_write, "2026-11-01T10:00:00Z");
        assert!(o.stale(crate::util::epoch_now()).is_some(), "it was overdue before its rows moved, and still is");
        s.apply_as("old", "queue", &line(json!({"op": "set", "id": "pr-1", "fields": {"detail": "now"}}))).unwrap();
        assert!(crate::owner::stale_on(&s, "old", crate::util::epoch_now()).is_empty(), "its own write is what makes it fresh");
        fs::remove_dir_all(&s.home).ok();
    }

    /// The move is two writes: the owner's file, then doc.json. A crash between them leaves the
    /// blocks in both. A reader then takes the owner's file (no row twice, none missing), and
    /// the next write drops the copy in doc.json.
    #[test]
    fn a_block_in_both_files_reads_once_from_the_owners_file() {
        let s = temp_store();
        let before = old_board(&s);
        s.settle("old").unwrap();
        let owned = fs::read_to_string(s.art("old").join("owned/owner.queue.json")).unwrap();
        s.apply_as("old", "queue", &line(json!({"op": "set", "id": "pr-1", "fields": {"cells": {"title": "One, newer"}}}))).unwrap();
        // the crash: doc.json as it was before the move, the owner's file already written
        fs::write(s.doc_path("old"), &before).unwrap();
        let d = s.load("old").unwrap();
        assert_eq!((top(&d), kids(&d, "pr")), (vec!["head", "pr", "foot", "jj"], vec!["pr-1", "pr-2"]));
        assert_eq!(d.find("pr-1").unwrap().f["cells"]["title"], "One, newer", "the owner's file is the truth");
        assert_eq!(d.ids().len(), 7, "nothing twice");
        assert_eq!(s.inspect("old").legacy.len(), 1, "and the doctor sees the leftover");
        // the other half of the same crash: the file as the move wrote it, doc.json still whole
        fs::write(s.art("old").join("owned/owner.queue.json"), &owned).unwrap();
        assert_eq!(kids(&s.load("old").unwrap(), "pr"), ["pr-1", "pr-2"]);
        // any write finishes the move
        s.settle("old").unwrap();
        let layout: Doc = serde_json::from_str(&fs::read_to_string(s.doc_path("old")).unwrap()).unwrap();
        assert!(layout.find("pr").is_none() && layout.rev == 41 && s.inspect("old") == Inspect::default());
        assert_eq!(kids(&s.load("old").unwrap(), "pr"), ["pr-1", "pr-2"]);
        fs::remove_dir_all(&s.home).ok();
    }

    /// Unregistering gives the blocks back to the layout, in place, as ordinary content; so does
    /// moving a handler to another prefix. Registering takes the blocks already under the prefix.
    #[test]
    fn unregistering_returns_the_blocks_to_the_layout() {
        let s = dash();
        let w = Writer::Named { by: "handler:fetch".into(), owns: "f".into() };
        s.handler_commit("dash", &[line(json!({"type": "row", "id": "f-1", "text": "fetched", "parent": "top"}))], &w, &[], None);
        s.apply_as("dash", "queue", &line(json!({"type": "tr", "id": "q-1", "parent": "jobs", "cells": {"name": "a"}}))).unwrap();
        s.apply_as("dash", "ci", &[Op::Add { parent: None, block: serde_json::from_value(json!({"type": "row", "id": "ci-1", "text": "z"})).unwrap(), before: Some("note".into()), after: None }]).unwrap();
        let shown = s.load("dash").unwrap();
        assert_eq!((shown.rev, top(&shown)), (1, vec!["top", "ci-1", "note", "jobs"]));
        s.remove_owner("ci").unwrap();
        crate::handler::edit_registry(&s, |r| r.remove("fetch").map(|_| ())).unwrap();
        let d = s.load("dash").unwrap();
        assert_eq!((top(&d), kids(&d, "top"), kids(&d, "jobs")), (vec!["top", "ci-1", "note", "jobs"], vec!["intro", "f-1"], vec!["q-1"]), "where they were");
        assert_eq!(d.owned.keys().collect::<Vec<_>>(), ["owner:queue"]);
        let layout: Doc = serde_json::from_str(&fs::read_to_string(s.doc_path("dash")).unwrap()).unwrap();
        assert!(layout.find("ci-1").is_some() && layout.find("f-1").is_some() && layout.find("q-1").is_none() && layout.rev == 3);
        assert!(!s.art("dash").join("owned/owner.ci.json").exists() && !s.art("dash").join("owned/handler.fetch.json").exists());
        // ordinary content: anyone writes it
        s.patch("dash", &[Op::Set { id: "ci-1".into(), field: "text".into(), value: json!("mine") }, Op::Rm { id: "f-1".into() }], None).unwrap();
        // claimed again: the blocks under the prefix are the owner's from the registration on
        s.add_owner(owner("ci", "dash", "ci")).unwrap();
        assert!(s.art("dash").join("owned/owner.ci.json").exists());
        assert!(s.patch("dash", &[Op::Set { id: "ci-1".into(), field: "text".into(), value: json!("no") }], None).unwrap_err().contains("belongs to owner `ci`"));
        assert_eq!(top(&s.load("dash").unwrap()), ["top", "ci-1", "note", "jobs"]);
        // an owner set to another prefix lets go of what it wrote under the old one
        s.set_owner(owner("queue", "dash", "qq")).unwrap();
        let layout: Doc = serde_json::from_str(&fs::read_to_string(s.doc_path("dash")).unwrap()).unwrap();
        assert!(layout.find("q-1").is_some() && s.load("dash").unwrap().owned.keys().collect::<Vec<_>>() == ["owner:ci"]);
        fs::remove_dir_all(&s.home).ok();
    }

    /// A whole-document write is a layout write: it replaces doc.json and leaves the owners'
    /// files alone, whether it carries the owners' blocks (it was read composed) or not.
    #[test]
    fn a_whole_document_replace_leaves_owned_files_alone() {
        let s = dash();
        s.apply_as("dash", "queue", &line(json!({"type": "tr", "id": "q-1", "parent": "jobs", "cells": {"name": "a"}}))).unwrap();
        let file = fs::read(s.art("dash").join("owned/owner.queue.json")).unwrap();
        // read composed, change the layout, send it all back: with a stale copy of the row
        let mut d = s.load("dash").unwrap();
        d.purpose = "the queue".into();
        d.blocks[2].children[0].f.insert("detail".into(), json!("an edit that is not the layout's to make"));
        assert_eq!(s.replace(d, Some(1)).unwrap(), 2);
        // a layout that names none of them
        let bare: Doc = serde_json::from_value(json!({"id": "dash", "title": "Dash", "blocks": [{"type": "table", "id": "jobs", "columns": [{"key": "name"}]}]})).unwrap();
        assert_eq!(s.replace(bare, Some(2)).unwrap(), 3);
        assert_eq!(fs::read(s.art("dash").join("owned/owner.queue.json")).unwrap(), file);
        let d = s.load("dash").unwrap();
        assert_eq!((top(&d), kids(&d, "jobs"), d.find("q-1").unwrap().opt("detail")), (vec!["jobs"], vec!["q-1"], None));
        let layout: Doc = serde_json::from_str(&fs::read_to_string(s.doc_path("dash")).unwrap()).unwrap();
        assert!(layout.find("q-1").is_none() && layout.owned.is_empty());
        fs::remove_dir_all(&s.home).ok();
    }

    /// Reading a board twice reads its files once; an owner's write is one file read again, a
    /// layout write is doc.json read again, and the other files stay as they were kept.
    #[test]
    fn the_composition_cache_reads_only_the_file_that_changed() {
        let s = dash();
        let tr = |id: &str, n: usize| line(json!({"type": "tr", "id": id, "parent": "jobs", "cells": {"name": "x".repeat(n)}}));
        s.apply_as("dash", "queue", &tr("q-1", 2000)).unwrap();
        s.apply_as("dash", "ci", &tr("ci-1", 3000)).unwrap();
        let size = |f: &str| fs::metadata(s.art("dash").join(f)).unwrap().len();
        let reader = Store::new(s.home.clone(), s.project.clone());
        let cost = |f: &dyn Fn()| {
            let a = file_reads();
            f();
            let b = file_reads();
            (b.0 - a.0, b.1 - a.1)
        };
        reader.load("dash").unwrap();
        assert_eq!(cost(&|| drop(reader.load("dash").unwrap())), (0, 0), "an unchanged board is not read again");
        // writes come from another process: here another thread, so its reads are not counted
        let other = |f: Box<dyn FnOnce(Store) + Send>| {
            let st = Store::new(s.home.clone(), s.project.clone());
            std::thread::spawn(move || f(st)).join().unwrap()
        };
        let ops = tr("q-1", 2500);
        other(Box::new(move |st| drop(st.apply_as("dash", "queue", &ops).unwrap())));
        assert_eq!(cost(&|| assert_eq!(reader.load("dash").unwrap().owned["owner:queue"].rev, 2)), (1, size("owned/owner.queue.json")), "one owner wrote: its file, alone");
        other(Box::new(|st| drop(st.patch("dash", &[Op::Set { id: "note".into(), field: "text".into(), value: json!("edited") }], None).unwrap())));
        assert_eq!(cost(&|| assert_eq!(reader.load("dash").unwrap().rev, 2)), (1, size("doc.json")), "the layout changed: doc.json, alone");
        assert_eq!(cost(&|| drop(reader.load("dash").unwrap())), (0, 0));
        // a board nobody owns a part of costs its doc.json and no registry
        reader.create(Doc::empty("plain")).unwrap();
        let fresh = Store::new(s.home.clone(), s.project.clone());
        let plain = fs::metadata(s.doc_path("plain")).unwrap().len();
        let got = std::thread::spawn(move || {
            let a = file_reads();
            fresh.load("plain").unwrap();
            let b = file_reads();
            (b.0 - a.0, b.1 - a.1)
        })
        .join()
        .unwrap();
        assert_eq!(got, (1, plain));
        fs::remove_dir_all(&s.home).ok();
    }

    /// Two owners, a handler and a layout writer at once, many times over: every write lands,
    /// each in its own file, and the layout's rev counts the layout's writes alone.
    #[test]
    fn concurrent_owners_and_a_layout_writer_lose_nothing() {
        let s = dash();
        const N: usize = 30;
        let spawn = |f: Box<dyn Fn(&Store, usize) + Send>| {
            let st = Store::new(s.home.clone(), s.project.clone());
            std::thread::spawn(move || (0..N).for_each(|i| f(&st, i)))
        };
        let hs = vec![
            spawn(Box::new(|st, i| drop(st.apply_as("dash", "queue", &line(json!({"type": "tr", "id": format!("q-{i}"), "parent": "jobs", "cells": {"name": "q"}}))).unwrap()))),
            spawn(Box::new(|st, i| drop(st.apply_as("dash", "ci", &line(json!({"type": "row", "id": format!("ci-{i}"), "text": "c", "parent": "top"}))).unwrap()))),
            spawn(Box::new(|st, i| {
                let w = Writer::Named { by: "handler:fetch".into(), owns: "f".into() };
                assert_eq!(st.handler_commit("dash", &[line(json!({"type": "row", "id": format!("f-{i}"), "text": "f"}))], &w, &[], None).1, Vec::<String>::new());
            })),
            spawn(Box::new(|st, i| {
                // read, then write against what was read: never stale, whatever the owners did
                let rev = st.load("dash").unwrap().rev;
                let add = Op::Add { parent: None, block: serde_json::from_value(json!({"type": "text", "id": format!("l-{i}"), "text": "l"})).unwrap(), before: None, after: None };
                assert_eq!(st.patch("dash", &[add], Some(rev)).unwrap(), rev + 1);
            })),
        ];
        hs.into_iter().for_each(|h| h.join().unwrap());
        let d = s.load("dash").unwrap();
        for pre in ["q", "ci", "f", "l"] {
            assert_eq!(d.ids().iter().filter(|id| id.starts_with(&format!("{pre}-"))).count(), N, "{pre}");
        }
        assert_eq!((d.rev as usize, d.owned["owner:queue"].rev as usize, d.owned["owner:ci"].rev as usize, d.owned["handler:fetch"].rev as usize), (1 + N, N, N, N));
        fs::remove_dir_all(&s.home).ok();
    }

    /// `margin rm <board> --yes` archives the directory, owned files and all, and unregisters
    /// what claimed a prefix on it; `margin gc` moves a quiet board and leaves its registrations.
    #[test]
    fn removing_a_board_unregisters_its_owners_and_handlers() {
        let s = dash();
        s.create(Doc::empty("other")).unwrap();
        s.add_owner(owner("elsewhere", "other", "x")).unwrap();
        s.apply_as("dash", "queue", &line(json!({"type": "tr", "id": "q-1", "parent": "jobs", "cells": {"name": "a"}}))).unwrap();
        let dest = s.archive("dash").unwrap();
        assert!(dest.join("owned/owner.queue.json").exists() && dest.join("doc.json").exists());
        assert!(crate::handler::Registry::load(&s.dir()).handlers.is_empty());
        assert_eq!(crate::owner::Registry::load(&s.dir()).owners.iter().map(|o| o.name.as_str()).collect::<Vec<_>>(), ["elsewhere"]);
        let gone: Value = serde_json::from_str(&fs::read_to_string(dest.join("unregistered.json")).unwrap()).unwrap();
        assert_eq!((gone["handlers"][0]["name"].as_str(), gone["owners"].as_array().unwrap().len()), (Some("fetch"), 2));
        // the name is free: a new board of that name starts with nothing owned
        s.create(Doc::empty("dash")).unwrap();
        assert!(s.claims("dash").is_empty() && s.load("dash").unwrap().owned.is_empty());
        fs::remove_dir_all(&s.home).ok();
    }

    /// A whole-board owner (`*`) writes the title too, in its own file.
    #[test]
    fn a_whole_board_owner_writes_every_block_and_the_title_in_its_file() {
        let s = temp_store();
        s.create(Doc::empty("live")).unwrap();
        s.add_owner(owner("all", "live", "*")).unwrap();
        let layout = fs::read(s.doc_path("live")).unwrap();
        s.apply_as("live", "all", &[vec![Op::Title { value: "Live now".into() }], line(json!({"type": "row", "id": "anything", "text": "x"}))].concat()).unwrap();
        assert_eq!(fs::read(s.doc_path("live")).unwrap(), layout);
        let d = s.load("live").unwrap();
        assert_eq!((d.title.as_str(), top(&d), d.rev), ("Live now", vec!["anything"], 1));
        assert!(s.patch("live", &[Op::Title { value: "mine".into() }], None).unwrap_err().contains("belongs to owner `all`"));
        s.remove_owner("all").unwrap();
        let d = s.load("live").unwrap();
        assert_eq!((d.title.as_str(), top(&d), d.owned.len()), ("Live now", vec!["anything"], 0));
        fs::remove_dir_all(&s.home).ok();
    }
}
