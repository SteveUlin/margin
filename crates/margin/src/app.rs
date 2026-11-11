//! `margin view`: the live pane. Sleeps in `poll()` on stdin, SIGWINCH and an inotify fd on the
//! artifact's files; wakes for input, a file change, or the frame clock when something animates.
//! With nothing live it never wakes on its own (0 fps).

use crate::config::Config;
use crate::host::{Host, Selection};
use crate::scene::{Input, Placement, Scene};
use crate::select;
use crate::store::{Store, View};
use crate::util::{epoch_now, now_rfc3339};
use serde_json::json;
use std::ffi::CString;
use std::time::Instant;
use vellum::grid::{diff, synchronized, Grid};
use vellum::image::{kitty_delete, kitty_place, kitty_probe};
use vellum::input::{Event, Key, MouseKind, Parser};
use vellum::term::{self, Modes, Terminal};

struct Inotify {
    fd: i32,
    /// The watch on the pane's own view directory: a write there is this pane saving its view, or
    /// `margin tab` changing the tab set, and neither is a reason to reload the documents.
    view_wd: i32,
    /// The project directory's watch. Its `.lock` is opened by every command that takes the
    /// project lock, whatever it then writes: the write itself is the news, not the lock (an
    /// append to a dataset ten times a second would otherwise reload every tab each time).
    proj_wd: i32,
    /// The watches whose changes mean "reload".
    other: Vec<i32>,
    /// The watches on datasets: the project's `data/` directory (any name), and the directory of
    /// each registered external file (that file's name only).
    data: Vec<(i32, Option<String>)>,
}

/// What fired since the last look.
#[derive(Default, Debug, PartialEq)]
struct Fired {
    views: bool,
    /// A dataset's file changed: no document did.
    data: bool,
    other: bool,
}

impl Inotify {
    /// `paths`: directories to reload on. `proj`: the project directory (one of them).
    /// `data_dir` and `files`: where the datasets are (data.rs).
    fn new(paths: &[std::path::PathBuf], view_dir: &std::path::Path, proj: &std::path::Path, data_dir: &std::path::Path, files: &[std::path::PathBuf]) -> Inotify {
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        let add = |p: &std::path::Path, make: bool| -> i32 {
            if make {
                let _ = std::fs::create_dir_all(p);
            }
            match CString::new(p.to_string_lossy().as_bytes()) {
                Ok(c) => unsafe { libc::inotify_add_watch(fd, c.as_ptr(), libc::IN_CLOSE_WRITE | libc::IN_MOVED_TO | libc::IN_MODIFY | libc::IN_CREATE) },
                Err(_) => -1,
            }
        };
        let other: Vec<i32> = paths.iter().map(|p| add(p, true)).collect();
        let view_wd = add(view_dir, true);
        // the same directory watched twice is one watch: the same descriptor comes back
        let proj_wd = add(proj, true);
        let mut data = vec![(add(data_dir, true), None)];
        for f in files {
            // an external file's directory, never created here; a file replaced by rename is
            // still seen, which a watch on the file itself would lose
            if let (Some(dir), Some(name)) = (f.parent(), f.file_name()) {
                data.push((add(dir, false), Some(name.to_string_lossy().into_owned())));
            }
        }
        Inotify { fd, view_wd, proj_wd, other, data }
    }
    /// Read what fired.
    fn drain(&self) -> Fired {
        let mut out = Fired::default();
        let mut buf = [0u8; 4096];
        loop {
            let n = unsafe { libc::read(self.fd, buf.as_mut_ptr() as *mut _, buf.len()) };
            if n <= 0 {
                break;
            }
            // struct inotify_event { int wd; u32 mask; u32 cookie; u32 len; char name[len]; }
            let (mut at, n) = (0usize, n as usize);
            while at + 16 <= n {
                let wd = i32::from_ne_bytes(buf[at..at + 4].try_into().unwrap());
                let len = u32::from_ne_bytes(buf[at + 12..at + 16].try_into().unwrap()) as usize;
                let raw = &buf[(at + 16).min(n)..(at + 16 + len).min(n)];
                let name = String::from_utf8_lossy(raw.split(|b| *b == 0).next().unwrap_or(&[])).into_owned();
                let is_data = self.data.iter().any(|(d, only)| *d == wd && only.as_deref().map(|o| o == name).unwrap_or(true));
                if wd == self.view_wd {
                    out.views = true;
                } else if is_data {
                    out.data = true;
                } else if self.other.contains(&wd) && !(wd == self.proj_wd && name == ".lock") {
                    out.other = true;
                }
                at += 16 + len;
            }
        }
        out
    }
}

impl Drop for Inotify {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}

struct Live {
    st: Store,
    art: String,
    cfg: Config,
    pane: String,
    view: View,
    /// The document as the pane shows it: the stored one, with the rows of any table bound to a
    /// dataset filled in (data.rs `expand`). Never written anywhere.
    doc: crate::doc::Doc,
    /// The document as stored.
    raw: crate::doc::Doc,
    /// The datasets this board reads, as last read; the reads are cached by file length and mtime.
    data: std::rc::Rc<crate::data::Snapshot>,
    data_cache: crate::data::Cache,
    /// A dataset's file changed (or the bindings, or the width) since `data` was read. Only the
    /// tab on screen reads again: a dataset appended to costs a tab out of view nothing.
    data_dirty: bool,
    data_ready: bool,
    /// The board reads at least one dataset. One that reads none sleeps through their changes.
    binds: bool,
    /// This tab is the one on screen, and the pane's width in cells (a plot asks for as many
    /// records as it has cells).
    on_screen: bool,
    cols: u16,
    state: crate::store::State,
    delivered: u64,
    unread: usize,
    last_seq: u64,
    scene: Scene,
    mouse: bool,
    focused: bool,
    quit: bool,
    visible_since: Option<f64>,
    last_seen_poll: f64,
    fresh_look: bool,
    activity: Vec<crate::store::Activity>,
    /// A mouse drag in progress: (start, current, moved). Selection is margin's own, because
    /// with mouse reporting on Zellij hands drags to the pane instead of selecting.
    drag: Option<((u16, u16), (u16, u16), bool)>,
    /// Vim-style keys: counts, `gg`, `zc`, `]]` (vellum::keymap; the table is `keys()`).
    keys: vellum::keymap::Keymap<Act>,
    /// A `/` search being typed, and the last one run (for n / N).
    search: Option<String>,
    last_search: String,
    /// Row notes (r3): review mode on, and what a reaction would land on.
    review: bool,
    target: Option<Target>,
    /// Toasts for this artifact, and when this pane first showed each (its clock starts then).
    toasts: Vec<crate::toast::Toast>,
    toast_seen: std::collections::HashMap<String, f64>,
    /// The view as last written to this pane's view file.
    saved: View,
    /// A key or click that is the pane's to carry out, not this tab's (switch tabs).
    pane_act: Option<PaneAct>,
    /// Actions the person fired from this pane and what became of each, by block id.
    sent: std::collections::HashMap<String, Sent>,
    /// The detail panel's block while the focus settles on it: (block, since when, pane clock).
    detail_at: Option<(String, f64)>,
    /// The block `<board>.detail` was last appended for (None: the panel is closed, or the tab
    /// went off screen; the next block it shows is asked about again).
    detail_sent: Option<String>,
    /// More was asked for and a handler is registered to answer: what the panel says meanwhile.
    asked: Option<Asked>,
}

/// A `<board>.detail` some handler will be run for: the block, when (epoch seconds), and what
/// the block and the datasets it reads looked like then. A change to either is the answer.
struct Asked {
    id: String,
    at: f64,
    hash: u64,
    sets: Vec<(String, Option<u64>, usize)>,
}

/// The pane says `<board>.shown` again when it regains the terminal focus after this long away
/// (seconds): what it shows may be old, and a provider can refresh it.
pub const REFOCUS_SECS: f64 = 60.0;
/// `<board>.detail` is appended for a block once the detail panel has stayed on it this long
/// (seconds): arrowing through rows asks about the row where the focus rests, not each one passed.
pub const DETAIL_SETTLE: f64 = 0.3;
/// How long the detail panel says "asked for more" before it says "no answer yet" (seconds).
pub const ASKED_SECS: f64 = 10.0;

/// An action fired on a row, until its note on the row goes: the event it appended (or why it
/// was refused), when, and the row's content then (a row that changes has been dealt with).
struct Sent {
    seq: u64,
    kind: String,
    at: f64,
    hash: u64,
    refused: Option<String>,
}

/// How long a row says `sent` before the pane asks who took the event.
const SENT_GRACE: f64 = 1.5;
/// How long a handler that exists may leave an event unread before the row says so.
const SENT_LATE: f64 = 5.0;

/// What a tab hands up to the pane.
#[derive(Clone, Copy, Debug, PartialEq)]
enum PaneAct {
    /// `gt`, and `gT`: that many tabs on or back, wrapping.
    Next(u32),
    Prev(u32),
    /// `3gt` or a click: that tab, counted from 0.
    Go(usize),
}

/// A click on the tab bar (`mg:tab/2`, `mg:tab/prev`, `mg:tab/next`) as the pane's action.
fn tab_act(tok: &str) -> Option<PaneAct> {
    match tok.strip_prefix("mg:tab/")? {
        "prev" => Some(PaneAct::Prev(1)),
        "next" => Some(PaneAct::Next(1)),
        n => n.parse().ok().map(PaneAct::Go),
    }
}

fn store_like(st: &Store) -> Store {
    Store { home: st.home.clone(), project: st.project.clone(), author: st.author.clone() }
}

/// What a reaction lands on in review mode: a row, and the text sulin selected in it (empty for
/// the whole row). `cells` is the selection to keep tinted; `pills` the reaction buttons drawn
/// for it, as (x, y, width, reaction).
struct Target {
    row: String,
    quote: String,
    cells: Vec<(u16, u16, u16)>,
    pills: Vec<(u16, u16, u16, &'static str)>,
}

/// The pills, in order; on a todo only why and disagree (skip already says "don't do this").
fn reactions(kind: &str) -> &'static [(&'static str, &'static str)] {
    if kind == "todo" {
        &[("? why", "why"), ("✗ disagree", "disagree")]
    } else {
        &[("+ more", "more"), ("− less", "less"), ("✓ keep", "keep"), ("✗ disagree", "disagree"), ("? why", "why")]
    }
}

/// What a key does in the pane. Contextual ones (Activate, a digit's pick) depend on the focused component.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Act {
    Down,
    Up,
    Top,
    Bottom,
    HalfDown,
    HalfUp,
    PageDown,
    PageUp,
    NextSection,
    PrevSection,
    NextChange,
    PrevChange,
    FoldClose,
    FoldOpen,
    FoldToggle,
    FoldAll,
    UnfoldAll,
    Search,
    SearchNext,
    SearchPrev,
    Activate,
    Strike,
    Approve,
    Reject,
    Help,
    Mouse,
    Quit,
    Review,
    Ids,
    Stop,
    Own,
    TabNext,
    TabPrev,
    HeadFold,
    SubNext,
    SubPrev,
    Detail,
    DetailDown,
    DetailUp,
}

/// The pane's bindings, vim's names where vim has one. `1-9` on a question picks an option
/// (handled before the table, since a count would otherwise eat the digit).
fn keys() -> vellum::keymap::Keymap<Act> {
    vellum::keymap::Keymap::new(&bindings())
}

/// The characters a board's own actions may not use as their key: every character one of the
/// pane's bindings starts with (so `g`, `z`, `]` too: they begin a sequence), the digits (a
/// count), and review mode's reaction keys. Derived from the binding table, so a key added
/// there is refused in `actions` from then on (doc.rs `parse_actions` checks it at write time).
pub fn reserved_keys() -> Vec<char> {
    let mut v: Vec<char> = bindings().iter().filter(|(k, _)| !k.starts_with('<')).filter_map(|(k, _)| k.chars().next()).collect();
    v.extend('0'..='9');
    v.extend(['+', '-', '=', '!']);
    v.sort();
    v.dedup();
    v
}

fn bindings() -> Vec<(&'static str, Act)> {
    use Act::*;
    vec![
        ("j", Down), ("<Down>", Down), ("k", Up), ("<Up>", Up),
        ("gg", Top), ("<Home>", Top), ("G", Bottom), ("<End>", Bottom),
        ("<C-d>", HalfDown), ("<C-u>", HalfUp), ("<C-f>", PageDown), ("<C-b>", PageUp), ("<PageDown>", PageDown), ("<PageUp>", PageUp),
        ("]]", NextSection), ("[[", PrevSection), ("}", NextSection), ("{", PrevSection),
        // vim's diff-mode names: the next and previous change since he last looked
        ("]c", NextChange), ("[c", PrevChange),
        ("zc", FoldClose), ("h", FoldClose), ("zo", FoldOpen), ("l", FoldOpen),
        ("za", FoldToggle), ("zM", FoldAll), ("zR", UnfoldAll),
        // the pinned head: wrapped, or folded to one row a line
        ("zh", HeadFold),
        // a tabs component: the next and previous of its tabs, from its strip or anything in it
        ("]t", SubNext), ("[t", SubPrev),
        ("/", Search), ("n", SearchNext), ("N", SearchPrev),
        ("<CR>", Activate), ("<Space>", Activate), ("x", Strike), ("a", Approve), ("r", Reject),
        ("?", Help), ("m", Mouse), ("q", Quit), ("<C-c>", Quit), ("c", Review), ("i", Ids), ("S", Stop), ("o", Own),
        // vim's tab pages: gt and gT step and wrap, 3gt goes to the third
        // the arrows switch top-level tabs (h and l fold, as before)
        ("gt", TabNext), ("gT", TabPrev), ("<Right>", TabNext), ("<Left>", TabPrev),
        // the detail panel: d opens and closes it, J and K scroll it (j and k move the list)
        ("d", Detail), ("J", DetailDown), ("K", DetailUp),
    ]
}

impl Live {
    /// One artifact as this pane shows it: its document, the pane's view of it, and its scene.
    fn open(st: &Store, art: &str, cfg: &Config, pane: &str, mouse: bool, focused: bool) -> Result<Live, String> {
        let doc = st.load(art)?;
        let saved = st.view(art, pane);
        let mut view = saved.clone();
        if view.seen_at.is_empty() {
            view.seen_at = now_rfc3339();
        }
        let mut scene = Scene::new(cfg);
        // a focus the view file names inside a section of a `tabs` block that is not on show
        // (written from outside the pane) brings that section on show
        if let Some(f) = view.focus.clone() {
            crate::scene::show_in_tabs(&doc, &mut view, &f);
        }
        scene.restore(&view);
        Ok(Live {
            last_seq: st.last_seq(),
            st: store_like(st),
            art: art.to_string(),
            cfg: cfg.clone(),
            pane: pane.to_string(),
            view,
            raw: doc.clone(),
            doc,
            data: Default::default(),
            data_cache: Default::default(),
            data_dirty: true,
            data_ready: false,
            binds: false,
            on_screen: false,
            cols: 80,
            state: Default::default(),
            delivered: 0,
            unread: 0,
            scene,
            mouse,
            focused,
            quit: false,
            visible_since: None,
            last_seen_poll: -10.0,
            fresh_look: false,
            activity: Vec::new(),
            drag: None,
            keys: keys(),
            search: None,
            last_search: String::new(),
            review: false,
            target: None,
            toasts: Vec::new(),
            toast_seen: Default::default(),
            saved,
            pane_act: None,
            sent: Default::default(),
            detail_at: None,
            detail_sent: None,
            asked: None,
        })
    }

    /// Write the view file when the view, the focus or the scroll anchor changed since the last write.
    fn persist(&mut self) {
        if self.unsaved() {
            self.save_view();
        }
    }

    fn unsaved(&self) -> bool {
        self.view != self.saved || self.scene.focus != self.view.focus || self.scene.anchor.as_ref().map(|a| &a.0) != self.view.anchor.as_ref()
    }

    /// Read the datasets this board is bound to (tail-bounded and cached: data.rs) and fill the
    /// bound tables' rows. Nothing here is a change to the document: the scene reconciles the
    /// rows by id without a change bar or an unseen count. True when the content changed.
    fn refresh_data(&mut self) -> bool {
        self.data_dirty = false;
        self.data_ready = true;
        let wants = crate::data::wants(&self.raw, self.cols);
        self.binds = !wants.is_empty();
        let mut snap = crate::data::Snapshot::load(&self.st, &wants, &mut self.data_cache);
        if snap.same(&self.data) {
            return false;
        }
        snap.gen = crate::data::next_gen();
        self.data = std::rc::Rc::new(snap);
        self.doc = crate::data::expand(&self.raw, &self.data);
        true
    }

    /// Before a frame of this tab, the one on screen: read its datasets again when one changed
    /// and the pace allows, and make them the ones components draw from. A change that came too
    /// soon after the last data frame waits: the time it is due is returned, for a wake-up.
    fn data_step(&mut self, pace: &mut crate::data::Pace, t: f64, wall: f64) -> Option<f64> {
        let mut due = None;
        if self.data_dirty {
            let hold = pace.hold(t);
            if hold > 0.0 {
                due = Some(t + hold);
            } else {
                self.refresh_data();
                pace.drew(t);
            }
        }
        self.show_data(wall);
        due
    }

    /// The datasets have never been read (a tab that has not drawn yet).
    fn data_dirty_first(&self) -> bool {
        !self.data_ready
    }

    /// Make this tab's datasets the ones components draw from, and mark what is bound to an
    /// overdue one. Before every frame of this tab.
    fn show_data(&mut self, wall: f64) {
        crate::data::install(self.data.clone());
        self.scene.data_gen = self.data.gen;
        let stale = self.data.stale(wall);
        self.scene.stale_data = if stale.is_empty() {
            Default::default()
        } else {
            crate::data::bound_ids(&self.doc).into_iter().filter_map(|(id, set)| stale.get(&set).map(|why| (id, (set.clone(), why.clone())))).collect()
        };
    }

    fn reload(&mut self, now: f64) {
        // the board composed (store.rs): the layout and each owner's file, each read again only
        // when that file changed. An owner's write leaves the board's rev (the layout's) where it
        // was and moves its own, in `owned`
        let mut owner_wrote = false;
        if let Ok(d) = self.st.load(&self.art) {
            owner_wrote = d.rev == self.raw.rev && d.owned != self.raw.owned;
            self.raw = d;
        }
        // the tab on screen reads its datasets with the document (a binding added by this
        // revision draws at once); a tab out of view keeps the rows it had until it is shown.
        // The first load always reads, so opening a pane animates nothing in
        let gen = self.data.gen;
        if self.on_screen || !self.data_ready {
            self.refresh_data();
        } else {
            self.data_dirty = true;
        }
        // the scene and the table layouts take a new look at a document on a new rev or a new
        // data generation. An owner's rows changed on the same rev, so they get a generation:
        // the scene then reconciles them by id as content (change bars, the unseen count), which
        // is what tells them from a dataset's rows
        if owner_wrote && self.data.gen == gen {
            self.data = std::rc::Rc::new(crate::data::Snapshot { sets: self.data.sets.clone(), gen: crate::data::next_gen() });
        }
        self.doc = crate::data::expand(&self.raw, &self.data);
        self.scene.data_gen = self.data.gen;
        self.state = self.st.state(&self.art);
        // who is working on this board, through the seam (consumer.rs): nobody, unless a
        // consumer's session keeps it
        self.activity = self.scene.present(crate::consumer::on_board(&self.st, &self.art));
        self.scene.stopped = self.st.stopped();
        self.scene.stale = crate::owner::stale_on(&self.st, &self.art, epoch_now());
        self.toasts = crate::toast::load(&self.st, &self.art);
        self.delivered = self.st.delivered_max();
        // only what is new to Claude or to this pane: never the whole log on every reload
        let evs = self.st.events_since(self.delivered.min(self.last_seq));
        self.unread = evs.iter().filter(|e| e.artifact == self.art && crate::store::is_decision(e) && e.seq > self.delivered).count();
        // a selection that reached margin through Zellij flashes the token it came from; a fold
        // token toggles the fold here (the pane's own keys and clicks fold directly)
        let fresh: Vec<_> = evs.iter().filter(|e| e.seq > self.last_seq && e.artifact == self.art).cloned().collect();
        for e in &fresh {
            if e.via == "select" {
                self.scene.flash(&e.id, now);
            }
            if e.kind == "fold" && e.value.is_null() {
                self.toggle_fold(&e.id, None, false);
            }
        }
        self.last_seq = evs.last().map(|e| e.seq).unwrap_or(self.last_seq);
        // reconcile here, not only in the next frame: a tab out of view draws no frames, and its
        // badge counts the changes this finds
        self.scene.set_doc(&self.doc, &self.state, now, self.cfg.motion());
    }

    fn save_view(&mut self) {
        self.view.focus = self.scene.focus.clone();
        self.view.anchor = self.scene.anchor.as_ref().map(|a| a.0.clone());
        self.view.anchor_off = self.scene.anchor.as_ref().map(|a| a.1).unwrap_or(0);
        self.st.save_view(&self.art, &self.pane, &self.view);
        self.saved = self.view.clone();
    }

    fn mark_seen(&mut self, now: f64) {
        let ids = self.scene.retained.mark_seen(now);
        self.fresh_look = true;
        if !ids.is_empty() {
            self.view.seen_rev = self.doc.rev;
            let _ = self.st.log(&self.art, "", "seen", json!({"rev": self.doc.rev, "ids": ids}), "pane");
        }
    }

    fn decide(&mut self, id: &str, kind: &str, v: serde_json::Value, now: f64) {
        match self.st.decide(&self.art, id, kind, v, "key", None) {
            Ok(_) => {
                self.scene.flash(id, now);
                self.reload(now);
            }
            Err(e) => select::log_line(&format!("key decide: {e}")),
        }
    }

    /// Fire an action on a block (`id` empty: the board): append its event and note it on the
    /// row. The pane runs nothing: whichever handler is registered for the event's name does
    /// the work, and `sent_notes` says on the row what became of it.
    fn fire(&mut self, id: &str, action: &str, via: &str, now: f64) {
        let hash = self.doc.find(id).map(|b| b.hash()).unwrap_or(0);
        let r = self.st.act(&self.art, id, action, None, crate::who::HUMAN, via);
        let s = match r {
            Ok(e) => Sent { seq: e.seq, kind: e.kind, at: epoch_now(), hash, refused: None },
            Err(e) => {
                select::log_line(&format!("action {action} on {id}: {e}"));
                Sent { seq: 0, kind: format!("{}.{action}", self.art), at: epoch_now(), hash, refused: Some(e) }
            }
        };
        self.sent.insert(id.to_string(), s);
        self.scene.flash(id, now);
        self.sent_notes(epoch_now());
    }

    /// The key `c` with the focus where it is: fire the action that declares it, if one does.
    fn action_key(&mut self, c: char, now: f64) -> bool {
        let focus = self.scene.focus.clone();
        match self.doc.actions_for(focus.as_deref()).into_iter().find(|a| a.key == c) {
            Some(a) => {
                self.fire(focus.as_deref().unwrap_or(""), &a.name, "key", now);
                true
            }
            None => false,
        }
    }

    /// What each fired action's row says now (scene.sent), from the handler registry and the
    /// cursors alone:
    /// - `→ sent` at first;
    /// - nothing once the row's content changed, or once the event was taken: every handler
    ///   registered for its name has moved its cursor past it, or (no handler) some other named
    ///   cursor has, which is an outside tool following the log;
    /// - `nothing handles <name>` when no handler is registered for the name and nothing took it;
    /// - `nothing has picked this up` when a handler is registered and has not read it after
    ///   SENT_LATE seconds: the daemon is not running.
    ///
    /// True while a note is on show (the pane then comes back to look again).
    fn sent_notes(&mut self, wall: f64) -> bool {
        if self.sent.is_empty() {
            self.scene.sent.clear();
            return false;
        }
        let reg = crate::handler::Registry::load(&self.st.dir());
        let mut notes = std::collections::HashMap::new();
        let (st, doc) = (&self.st, &self.doc);
        self.sent.retain(|id, s| {
            let age = wall - s.at;
            if let Some(why) = &s.refused {
                notes.insert(id.clone(), (why.clone(), true));
                return age < 6.0;
            }
            if doc.find(id).map(|b| b.hash() != s.hash).unwrap_or(!id.is_empty()) {
                return false;
            }
            let hs: Vec<&crate::handler::Handler> = reg.for_kind(&s.kind).into_iter().filter(|h| !h.paused).collect();
            let taken = if hs.is_empty() { st.outside_cursor_passed(s.seq) } else { hs.iter().all(|h| st.cursor(&h.cursor()).map(|c| c >= s.seq).unwrap_or(false)) };
            if taken {
                return false;
            }
            let note = match (hs.is_empty(), age) {
                (_, a) if a < SENT_GRACE => ("→ sent".to_string(), false),
                (true, a) if a < 12.0 => (format!("nothing handles {} · margin doctor", s.kind), true),
                (false, a) if a < SENT_LATE => ("→ sent".to_string(), false),
                (false, a) if a < 60.0 => ("nothing has picked this up · margin daemon status".to_string(), true),
                _ => return false,
            };
            notes.insert(id.clone(), note);
            true
        });
        self.scene.sent = notes;
        !self.sent.is_empty()
    }

    /// The view event `<board>.detail`: appended once the detail panel has stayed on a block for
    /// `DETAIL_SETTLE`, with the block's id and the snapshot an action carries. The pane fetches
    /// nothing: a handler registered for the name may answer by writing the block or a dataset.
    /// Returns when to look again (pane clock) while a block waits out the settle time.
    fn detail_step(&mut self, t: f64, wall: f64) -> Option<f64> {
        let focus = self.scene.focus.clone().filter(|f| self.view.detail && self.doc.find(f).is_some());
        let Some(f) = focus else {
            (self.detail_at, self.detail_sent) = (None, None);
            return None;
        };
        if self.detail_sent.as_deref() == Some(f.as_str()) {
            self.detail_at = None;
            return None;
        }
        let since = match &self.detail_at {
            Some((id, since)) if *id == f => *since,
            _ => {
                self.detail_at = Some((f.clone(), t));
                t
            }
        };
        if t - since < DETAIL_SETTLE {
            return Some(since + DETAIL_SETTLE);
        }
        let kind = format!("{}.detail", self.art);
        let (value, hash) = self.doc.find(&f).map(|b| (crate::store::snapshot(b), b.hash())).unwrap_or_default();
        self.asked = None;
        match self.st.log(&self.art, &f, &kind, value, "pane") {
            // only a handler that will be run for it makes the panel say it asked
            Ok(_) if self.st.handled(&kind) => self.asked = Some(Asked { sets: self.bound_sets(&f), id: f.clone(), at: wall, hash }),
            Ok(_) => {}
            Err(e) => select::log_line(&format!("{kind} on {f}: {e}")),
        }
        (self.detail_at, self.detail_sent) = (None, Some(f));
        None
    }

    /// The datasets block `id` reads, as this tab last read them: name, last change, records.
    fn bound_sets(&self, id: &str) -> Vec<(String, Option<u64>, usize)> {
        crate::data::bound_ids(&self.doc)
            .into_iter()
            .filter(|(i, _)| i == id)
            .map(|(_, name)| {
                let set = self.data.sets.get(&name).and_then(|r| r.as_ref().ok());
                (set.and_then(|s| s.changed).map(f64::to_bits), set.map(|s| s.records.len()).unwrap_or(0), name)
            })
            .map(|(changed, n, name)| (name, changed, n))
            .collect()
    }

    /// The detail panel's line about what was asked (detail.rs `set_note`), before every frame
    /// of this tab: "asked for more" until the block or a dataset it reads changes, "no answer
    /// yet" from `ASKED_SECS` on, nothing once the panel closes. Never waits: it compares what
    /// the tab already holds. Returns when the wording changes (epoch seconds).
    fn asked_note(&mut self, wall: f64) -> Option<f64> {
        let open = self.asked.as_ref().map(|a| self.view.detail && self.doc.find(&a.id).map(|b| b.hash() == a.hash).unwrap_or(false) && self.bound_sets(&a.id) == a.sets).unwrap_or(false);
        if !open {
            self.asked = None;
            crate::detail::set_note(None);
            return None;
        }
        let a = self.asked.as_ref().unwrap();
        let waiting = wall - a.at < ASKED_SECS;
        crate::detail::set_note(Some((a.id.clone(), if waiting { crate::detail::ASKED } else { crate::detail::NO_ANSWER })));
        waiting.then_some(a.at + ASKED_SECS)
    }

    fn key(&mut self, k: Key, ctrl: bool, now: f64, term: &mut Terminal) {
        if self.search.is_some() {
            self.search_key(k);
            return;
        }
        // in review mode a reaction key notes the selection, or the focused row
        if self.review && !ctrl {
            let r = match k {
                Key::Char('+') => Some("more"),
                Key::Char('-') => Some("less"),
                Key::Char('=') => Some("keep"),
                Key::Char('!') => Some("disagree"),
                Key::Char('?') => Some("why"),
                Key::Esc => {
                    if self.target.take().is_none() {
                        self.set_review(false);
                    }
                    return;
                }
                _ => None,
            };
            if let Some(r) = r {
                let (row, quote) = match &self.target {
                    Some(t) => (t.row.clone(), t.quote.clone()),
                    None => match self.scene.focus.clone() {
                        Some(f) => (f, String::new()),
                        None => return,
                    },
                };
                self.react(&row, &quote, r, now);
                return;
            }
        }
        let focus = self.scene.focus.clone();
        let fb = focus.as_ref().and_then(|f| self.doc.find(f)).cloned();
        let kind = fb.as_ref().map(|b| b.kind.as_str()).unwrap_or("").to_string();
        // a digit on a question picks an option, unless a count or sequence is under way
        if let (Key::Char(c @ '1'..='9'), false, true, true) = (k, ctrl, kind == "question", self.keys.pending().is_empty()) {
            let b = fb.unwrap();
            if let Some(o) = b.strs("options").get(c as usize - '1' as usize) {
                self.decide(&b.id, "pick", json!(o), now);
            }
            return;
        }
        // a key one of the focused block's actions declares (never one of the pane's own: those
        // are refused when the board is written)
        if let (Key::Char(c), false, true) = (k, ctrl, self.keys.pending().is_empty()) {
            if self.action_key(c, now) {
                return;
            }
        }
        // a count typed before this key: `3gt` is the third tab, `gt` the next one
        let counted = self.keys.pending().starts_with(|c: char| c.is_ascii_digit());
        let fed = self.keys.feed(vellum::keymap::Press { key: k, ctrl }, now);
        self.scene.pending_keys = self.keys.pending();
        let vellum::keymap::Fed::Act(act, n) = fed else { return };
        let n = n as i32;
        let half = (self.scene.view_h as i32 / 2).max(1);
        match act {
            Act::Down => self.scene.move_focus(n),
            Act::Up => self.scene.move_focus(-n),
            Act::Top => self.scene.move_focus(-10_000),
            Act::Bottom => self.scene.move_focus(10_000),
            Act::HalfDown => self.scene.scroll_by(half * n),
            Act::HalfUp => self.scene.scroll_by(-half * n),
            Act::PageDown => self.scene.scroll_by((self.scene.view_h as i32 - 2).max(1) * n),
            Act::PageUp => self.scene.scroll_by(-(self.scene.view_h as i32 - 2).max(1) * n),
            Act::NextSection => self.scene.section_step(&self.doc, n),
            Act::PrevSection => self.scene.section_step(&self.doc, -n),
            Act::NextChange | Act::PrevChange => {
                for _ in 0..n {
                    if let Some(id) = self.scene.change_step(act == Act::NextChange) {
                        self.scene.flash(&id, now);
                    }
                }
            }
            Act::FoldClose | Act::FoldOpen | Act::FoldToggle if kind == "section" => {
                let want = match act {
                    Act::FoldClose => Some(true),
                    Act::FoldOpen => Some(false),
                    _ => None,
                };
                self.toggle_fold(&focus.clone().unwrap(), want, true);
            }
            Act::FoldAll | Act::UnfoldAll => {
                let mut ids = Vec::new();
                crate::doc::walk(&self.doc.blocks, &mut |b, _, _| {
                    if b.kind == "section" {
                        ids.push(b.id.clone());
                    }
                });
                for id in ids {
                    self.view.folds.insert(id, act == Act::FoldAll);
                }
            }
            Act::Search => {
                self.search = Some(String::new());
                self.scene.prompt = Some(String::new());
            }
            Act::SearchNext | Act::SearchPrev => {
                for _ in 0..n {
                    if let Some(id) = self.scene.find(&self.doc, &self.last_search, act == Act::SearchNext) {
                        self.scene.reveal(&id);
                        self.scene.flash(&id, now);
                    }
                }
            }
            Act::Activate => match kind.as_str() {
                "section" => self.toggle_fold(&focus.clone().unwrap(), None, true),
                // open is an action like any other: the event goes out, a handler opens it
                "link" | "status" => self.fire(&focus.clone().unwrap(), "open", "key", now),
                "todo" => {
                    // the circle's cycle: next → skipped → Claude's again
                    let b = fb.unwrap();
                    let st = crate::components::status_of(&self.state, &b);
                    let _ = &st;
                    match crate::components::cycle_for(&self.state, &b) {
                        Some("clear") => self.decide(&b.id, "untick", serde_json::Value::Null, now),
                        Some(v) => self.decide(&b.id, "tick", json!(v), now),
                        None => {}
                    }
                }
                // any other block with a target opens it (a row, a table's row): what the
                // detail panel's hint promises
                _ if fb.as_ref().map(|b| b.opt("target").is_some()).unwrap_or(false) => self.fire(&focus.clone().unwrap(), "open", "key", now),
                _ => {}
            },
            Act::Strike if kind == "claim" => {
                let id = focus.unwrap();
                let struck = self.state.decisions.get(&id).map(|d| d.kind == "strike").unwrap_or(false);
                self.decide(&id, if struck { "unstrike" } else { "strike" }, json!(!struck), now);
            }
            Act::Approve | Act::Reject if kind == "diff" => {
                self.decide(&focus.unwrap(), "verdict", json!(if act == Act::Approve { "approved" } else { "rejected" }), now);
            }
            Act::Help => self.scene.help_open = !self.scene.help_open,
            Act::Mouse => {
                self.mouse = !self.mouse;
                let _ = term.set_mouse(self.mouse);
            }
            Act::Quit => self.quit = true,
            Act::Review => self.set_review(!self.review),
            Act::Ids => self.scene.show_ids = !self.scene.show_ids,
            Act::Stop => self.stop(now),
            Act::TabNext if counted => self.pane_act = Some(PaneAct::Go(n as usize - 1)),
            Act::TabNext => self.pane_act = Some(PaneAct::Next(1)),
            Act::TabPrev => self.pane_act = Some(PaneAct::Prev(n as u32)),
            Act::Detail => self.view.detail = !self.view.detail,
            Act::HeadFold => self.view.head_folded = !self.view.head_folded,
            Act::SubNext => self.subtab_step(n),
            Act::SubPrev => self.subtab_step(-n),
            Act::DetailDown => self.scene.detail_scroll_by(n),
            Act::DetailUp => self.scene.detail_scroll_by(-n),
            // take the focused step as yours, or hand it back
            Act::Own if kind == "todo" => {
                let b = fb.unwrap();
                let now_yours = crate::components::yours(&self.state, &b);
                let _ = self.st.decide_own(&self.art, &b.id, !now_yours, "key");
                self.scene.flash(&b.id, now);
                self.reload(now);
            }
            _ => {}
        }
    }

    /// Show section `sec` of the tabs component `tabs`. A focus left inside the section that
    /// went out of view moves to the strip.
    fn subtab_show(&mut self, tabs: &str, sec: &str) {
        let Some(b) = self.doc.find(tabs).filter(|b| b.kind == "tabs" && b.children.iter().any(|c| c.id == sec)) else { return };
        let was = crate::scene::subtab_active(b, &self.view).map(|c| c.id.clone());
        if was.as_deref() == Some(sec) {
            return;
        }
        // the first is the default: naming it would only pin the view to an id
        if b.children.first().map(|c| c.id.as_str()) == Some(sec) {
            self.view.subtabs.remove(tabs);
        } else {
            self.view.subtabs.insert(tabs.to_string(), sec.to_string());
        }
        if self.scene.focus.as_deref() != Some(tabs) && self.tabs_above(self.scene.focus.clone().as_deref()).as_deref() == Some(tabs) {
            self.scene.focus = Some(tabs.to_string());
            self.scene.scroll_to_focus = true;
        }
    }

    /// The nearest tabs component at or above block `id`.
    fn tabs_above(&self, id: Option<&str>) -> Option<String> {
        let mut at = self.doc.find(id?);
        while let Some(b) = at {
            if b.kind == "tabs" {
                return Some(b.id.clone());
            }
            at = self.doc.parent_of(&b.id);
        }
        None
    }

    /// `]t` / `[t`: `n` tabs on (or back) in the tabs component the focus is in, wrapping.
    fn subtab_step(&mut self, n: i32) {
        let Some(tabs) = self.tabs_above(self.scene.focus.clone().as_deref()) else { return };
        let Some(b) = self.doc.find(&tabs) else { return };
        let len = b.children.len() as i32;
        if len == 0 {
            return;
        }
        let cur = crate::scene::subtab_active(b, &self.view).and_then(|c| b.children.iter().position(|x| x.id == c.id)).unwrap_or(0) as i32;
        let next = b.children[(cur + n).rem_euclid(len) as usize].id.clone();
        self.subtab_show(&tabs, &next);
    }

    /// The stop signal, for the session that keeps this board: its next tool call is refused
    /// until sulin sends a message. On a board no session keeps there is nobody to stop, and the
    /// key does nothing.
    fn stop(&mut self, now: f64) {
        if self.scene.linked && self.st.stop(&self.art, "pane").is_ok() {
            self.reload(now);
        }
    }

    fn set_review(&mut self, on: bool) {
        self.review = on;
        // told to the board's consumer too, which may be holding something back for it
        crate::consumer::review(&self.st, &self.pane, &self.art, on);
        self.scene.review = on;
        if !on {
            self.target = None;
        }
    }

    /// Note a reaction on a row (and quote). The same reaction again takes the note back; a todo
    /// takes only why and disagree.
    fn react(&mut self, row: &str, quote: &str, r: &str, now: f64) {
        let kind = self.doc.find(row).map(|b| b.kind.clone()).unwrap_or_default();
        if !reactions(&kind).iter().any(|(_, x)| *x == r) {
            return;
        }
        let key = crate::store::note_key(row, quote);
        let same = self.state.decisions.get(&key).map(|d| d.value["r"] == r).unwrap_or(false);
        if self.st.note(&self.art, row, if same { "" } else { r }, quote, "pane").is_ok() {
            self.scene.flash(row, now);
            self.target = None;
            self.reload(now);
        }
    }

    /// A release in review mode: a pill notes the target; a drag makes its selection the target;
    /// a plain click targets the whole row under it.
    fn review_release(&mut self, g: &Grid, a: (u16, u16), b: (u16, u16), moved: bool, w: u16, now: f64) {
        if !moved {
            if let Some(r) = self.target.as_ref().and_then(|t| t.pills.iter().find(|p| b.1 == p.1 && b.0 >= p.0 && b.0 < p.0 + p.2)).map(|p| p.3) {
                let (row, quote) = self.target.as_ref().map(|t| (t.row.clone(), t.quote.clone())).unwrap();
                self.react(&row, &quote, r, now);
                return;
            }
            // a toast is on top in review mode too: a click dismisses it, never notes the row under it
            if let Some(id) = self.scene.overlay_at(b.0, b.1).and_then(|t| t.strip_prefix("mg:toast/")).map(String::from) {
                crate::toast::dismiss(&self.st, &self.art, &id);
                self.toasts.retain(|t| t.id != id);
                return;
            }
        }
        // the row the selection starts in, reading top to bottom: a drag upward ends there
        let top = if moved { a.1.min(b.1) } else { b.1 };
        // the row under the pointer where the selection starts; none in the detail panel
        let Some(row) = self.scene.row_at(if moved && a.1 <= b.1 { a.0 } else { b.0 }, top).map(String::from) else {
            self.target = None;
            return;
        };
        let (cells, quote) = if moved {
            let c = sel_rows(a, b, w);
            (c.clone(), clean_quote(&sel_text(g, &c)))
        } else {
            (vec![(b.1, 1, w.saturating_sub(2))], String::new())
        };
        self.scene.focus = Some(row.clone());
        self.target = Some(Target { row, quote, cells, pills: Vec::new() });
    }

    /// Typing a `/` search: it jumps as you type (incremental, like vim's incsearch); Enter keeps
    /// it for n / N, Esc drops it.
    fn search_key(&mut self, k: Key) {
        let Some(q) = self.search.as_mut() else { return };
        match k {
            Key::Esc => {
                self.search = None;
                self.scene.prompt = None;
                return;
            }
            Key::Enter => {
                self.last_search = q.clone();
                self.search = None;
                self.scene.prompt = None;
                return;
            }
            Key::Backspace => {
                q.pop();
            }
            Key::Char(c) => q.push(c),
            _ => return,
        }
        let q = q.clone();
        self.scene.prompt = Some(q.clone());
        if let Some(id) = self.scene.find(&self.doc, &q, true) {
            self.scene.reveal(&id);
        }
    }

    /// Fold (`Some(true)`), unfold (`Some(false)`) or toggle (`None`) a section in this pane's
    /// view. `log` records it as a passive event; a fold that arrived as an event isn't re-logged.
    fn toggle_fold(&mut self, id: &str, want: Option<bool>, log: bool) {
        let Some(b) = self.doc.find(id).filter(|b| b.kind == "section") else { return };
        let cur = self.view.folds.get(id).copied().unwrap_or_else(|| crate::components::default_folded(&self.state, b));
        let next = want.unwrap_or(!cur);
        if next != cur {
            self.view.folds.insert(id.to_string(), next);
            if log {
                let _ = self.st.log(&self.art, id, if next { "fold" } else { "unfold" }, json!(next), "key");
            }
        }
    }

    /// A click (mouse mode on): a token under the pointer acts as if selected; anywhere else on a
    /// section header folds that section, so a group closes with one click.
    fn click(&mut self, grid: &Grid, x: u16, y: u16, now: f64) {
        let button = self.scene.hit_at(x, y).map(String::from);
        if let Some(a) = button.as_deref().and_then(tab_act) {
            self.pane_act = Some(a);
            return;
        }
        if let Some((tabs, sec)) = button.as_deref().and_then(|b| b.strip_prefix("mg:subtab/")).and_then(|r| r.split_once('/')).map(|(a, b)| (a.to_string(), b.to_string())) {
            self.subtab_show(&tabs, &sec);
            self.scene.focus = Some(tabs);
            return;
        }
        if button.as_deref() == Some("mg:headfold") {
            self.view.head_folded = !self.view.head_folded;
            return;
        }
        if let Some(id) = button.as_deref().and_then(|b| b.strip_prefix("mg:head/")).map(String::from) {
            // a line of the pinned head: open the folds its block sits in, then go to it
            let mut up = Vec::new();
            let mut at = id.clone();
            let mut shows = Vec::new();
            while let Some(p) = self.doc.parent_of(&at) {
                // inside a tabs component: the section that holds it comes on show
                if p.kind == "tabs" {
                    shows.push((p.id.clone(), at.clone()));
                }
                at = p.id.clone();
                up.push(at.clone());
            }
            for (tabs, sec) in shows {
                self.subtab_show(&tabs, &sec);
            }
            for sec in up {
                self.toggle_fold(&sec, Some(false), true);
            }
            self.scene.goto(&id);
            self.scene.flash(&id, now);
            return;
        }
        if button.as_deref() == Some("mg:agents") {
            let running = self.scene.agents.iter().any(|g| g.done() < g.agents.len());
            self.scene.agents_folded = Some(!self.scene.agents_folded.unwrap_or(!running));
            return;
        }
        if let Some(id) = button.as_deref().and_then(|b| b.strip_prefix("mg:toast/")) {
            crate::toast::dismiss(&self.st, &self.art, id);
            self.toasts.retain(|t| t.id != id);
            return;
        }
        if let Some(id) = button.as_deref().and_then(|b| b.strip_prefix("mg:agent/")) {
            // a tracker's header: open or collapse a finished subagent's steps
            if !self.scene.agents_open.remove(id) {
                self.scene.agents_open.insert(id.to_string());
            }
            return;
        }
        if let Some(id) = button.as_deref().and_then(|b| b.strip_prefix("mg:stopagent/")) {
            if self.st.stop_agent(&self.art, id, "pane").is_ok() {
                self.reload(now);
            }
            return;
        }
        // an action's button: in the hint row, or its line in the detail panel
        if let Some((id, name)) = button.as_deref().and_then(|b| b.strip_prefix("mg:act/")).and_then(|r| r.rsplit_once('/')).map(|(a, b)| (a.to_string(), b.to_string())) {
            self.fire(&id, &name, "mouse", now);
            return;
        }
        if let Some(t) = button.or_else(|| token_at(grid, x, y)).as_deref().and_then(select::parse_token) {
            // a click that opens a block's target is the open action: the same event as enter
            if t.verb == "open" && t.artifact.as_deref().map(|a| a == self.art).unwrap_or(true) && self.doc.find(&t.id).is_some() {
                self.fire(&t.id, "open", "mouse", now);
                return;
            }
            // the pane's own project and artifact, not whatever `resolve` finds first
            let art = t.artifact.clone().unwrap_or_else(|| self.art.clone());
            if select::apply_in(&self.st, &art, &t, "mouse", &self.cfg).is_ok() {
                self.scene.flash(&t.id, now);
                self.reload(now);
            }
            return;
        }
        let Some(id) = self.scene.row_at(x, y).map(String::from) else { return };
        match self.doc.find(&id).map(|b| b.kind.as_str()) {
            Some("section") => {
                self.toggle_fold(&id, None, true);
                self.scene.focus = Some(id);
            }
            // a click on a todo's text selects it, which shows its buttons (no hover needed)
            Some(_) => self.scene.focus = Some(id),
            None => {}
        }
    }
}

/// A selection as a quote: whitespace runs (indent, wrapped lines, right-aligned padding) become
/// one space, and the ellipsis a truncated row ends in goes.
fn clean_quote(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").trim_end_matches('…').trim().to_string()
}

/// The cells a drag from `a` to `b` covers, in reading order, clipped to the content columns (the
/// gutter and the right margin aren't text): (row, first col, last col) per row.
fn sel_rows(a: (u16, u16), b: (u16, u16), w: u16) -> Vec<(u16, u16, u16)> {
    let ((x0, y0), (x1, y1)) = if (a.1, a.0) <= (b.1, b.0) { (a, b) } else { (b, a) };
    let (left, right) = (1u16, w.saturating_sub(2));
    (y0..=y1)
        .map(|y| {
            let xs = if y == y0 { x0.max(left) } else { left };
            let xe = if y == y1 { x1.min(right) } else { right };
            (y, xs, xe)
        })
        .filter(|(_, xs, xe)| xs <= xe)
        .collect()
}

/// The text a selection covers, trailing blanks trimmed per line.
fn sel_text(g: &Grid, rows: &[(u16, u16, u16)]) -> String {
    let mut buf = [0u8; 4];
    rows.iter()
        .map(|&(y, xs, xe)| {
            (xs..=xe).filter_map(|x| g.get(x, y).filter(|c| c.width > 0).map(|c| c.sym.as_str(&mut buf).to_string())).collect::<String>().trim_end().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim_matches('\n')
        .to_string()
}

/// The whitespace-delimited word under (x, y) if it's an `mg:` token, the same rule Zellij's
/// double-click uses.
pub fn token_at(g: &Grid, x: u16, y: u16) -> Option<String> {
    let mut buf = [0u8; 4];
    let row: Vec<String> = (0..g.w).map(|i| g.get(i, y).map(|c| c.sym.as_str(&mut buf).to_string()).unwrap_or_default()).collect();
    if row.get(x as usize).map(|s| s.trim().is_empty()).unwrap_or(true) {
        return None;
    }
    let mut a = x as usize;
    while a > 0 && !row[a - 1].trim().is_empty() {
        a -= 1;
    }
    let mut b = x as usize;
    while b + 1 < row.len() && !row[b + 1].trim().is_empty() {
        b += 1;
    }
    let word: String = row[a..=b].concat();
    word.starts_with("mg:").then_some(word)
}

fn detect_kitty(term: &mut Terminal, parser: &mut Parser, clock: &Instant) -> (bool, (u16, u16)) {
    let _ = term.write(&format!("\x1b[16t{}", kitty_probe()));
    let start = Instant::now();
    let mut kitty = false;
    let mut cell = (0u16, 0u16);
    while start.elapsed().as_millis() < 700 {
        let Ok((bytes, _, _)) = term.poll(&[], 100) else { break };
        let mut done = false;
        for ev in parser.feed(&bytes, clock.elapsed().as_secs_f64()) {
            match ev {
                Event::KittyReply(s) if s.contains("OK") => kitty = true,
                Event::PixelSize { kind: 6, h, w } => cell = (w, h),
                Event::DeviceAttrs(_) => done = true,
                _ => {}
            }
        }
        if done {
            break;
        }
    }
    (kitty, cell)
}

/// The pane's own binary and when it last changed, for reloading in place after a rebuild.
fn exe_mtime(exe: &std::path::Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(exe).ok()?.modified().ok()
}

/// Run the pane. Returns true when the binary was rebuilt and the caller should exec the new one
/// in this same pane: a rebuild then reaches every open pane without anyone closing, reopening or
/// restacking panes in sulin's Zellij layout.
/// How long an image placement must hold still before it is sent.
const IMG_SETTLE: f64 = 0.15;

/// Keeps kitty placements in step with the frames without re-sending a bitmap per frame. A
/// placement that moves (slide-in, scroll, reflow) is taken down at once, a cheap delete, and sent
/// again only once it has held still for IMG_SETTLE. Re-sending on every frame of a motion froze
/// the pane: each send is the whole bitmap, and Zellij decodes and re-encodes it for the host.
#[derive(Default)]
struct ImgSync {
    /// What the terminal is showing now.
    shown: Vec<Placement>,
    /// What the frames want, and since when they've wanted exactly that.
    want: Option<(Vec<Placement>, f64)>,
}

#[derive(Default, Debug)]
struct ImgPlan {
    delete: Vec<Placement>,
    send: Vec<Placement>,
    /// Slots waiting for their image: draw a placeholder there.
    pending: Vec<Placement>,
    wake: Option<f64>,
}

impl ImgSync {
    /// `repaint`: the cells were just redrawn from scratch (resize, end of a drag), which can
    /// wipe placements, so what's shown is sent again at once.
    fn step(&mut self, frame: &[Placement], t: f64, repaint: bool) -> ImgPlan {
        let mut plan = ImgPlan::default();
        if repaint && !self.shown.is_empty() && frame == self.shown.as_slice() {
            self.want = Some((frame.to_vec(), t - IMG_SETTLE));
            plan.delete = std::mem::take(&mut self.shown);
        }
        if frame == self.shown.as_slice() {
            // includes an image scrolled out of view while it waited: both empty, nothing pending
            self.want = None;
            return plan;
        }
        plan.delete.append(&mut self.shown);
        if self.want.as_ref().map(|(v, _)| v.as_slice()) != Some(frame) {
            self.want = Some((frame.to_vec(), t));
        }
        let since = self.want.as_ref().map_or(t, |w| w.1);
        if t - since >= IMG_SETTLE {
            plan.send = frame.to_vec();
            self.shown = frame.to_vec();
            self.want = None;
        } else {
            plan.pending = frame.to_vec();
            plan.wake = Some(since + IMG_SETTLE);
        }
        plan
    }
}

/// The pane: one `Live` per tab and the tab on screen. The first tab is the pane's own artifact
/// (what `margin view` was given first, what the Zellij pane is titled after); its view file
/// keeps the tab set and the tab on screen (tabs.rs). Only the tab on screen is drawn, takes
/// input, and is ever marked seen; the others reload when their files change so their badges
/// stay right, and ask for no frames.
struct Pane {
    tabs: Vec<Live>,
    active: usize,
    /// The board `<board>.shown` was last appended for: the one on screen as far as the log
    /// knows, and when (epoch seconds).
    shown: Option<(String, f64)>,
    /// When each board this pane showed went off screen (epoch seconds).
    hidden_at: std::collections::HashMap<String, f64>,
    /// When the pane lost the terminal focus (epoch seconds), until it has it back.
    blurred: Option<f64>,
}

impl Pane {
    /// `more`: the other artifacts named on the command line (none: the set the view file
    /// remembers). One named there that doesn't exist is an error; a remembered one that has
    /// gone is dropped.
    fn open(st: &Store, primary: &str, more: &[String], cfg: &Config, pane: &str, mouse: bool, focused: bool) -> Result<Pane, String> {
        let first = Live::open(st, primary, cfg, pane, mouse, focused)?;
        let (set, active) = crate::tabs::resolve(primary, more, &first.view);
        let active_art = set[active].clone();
        let mut tabs = vec![first];
        for a in &set[1..] {
            match Live::open(st, a, cfg, pane, mouse, focused) {
                Ok(l) => tabs.push(l),
                Err(e) if more.contains(a) => return Err(e),
                Err(e) => select::log_line(&format!("tab {a} dropped: {e}")),
            }
        }
        let active = tabs.iter().position(|l| l.art == active_art).unwrap_or(0);
        let mut pn = Pane { tabs, active, shown: None, hidden_at: Default::default(), blurred: None };
        pn.store_set();
        Ok(pn)
    }

    /// The view events `<board>.hidden` and `<board>.shown`: appended when the board on screen
    /// is not the one the log was last told, so once per tab switch however it came about (a
    /// key, a click, `margin tab`) and once at the pane's start. `shown` carries how long the
    /// board was off screen in this pane (`since`, seconds; null the first time).
    fn view_step(&mut self, wall: f64) {
        let art = self.tabs[self.active].art.clone();
        if self.shown.as_ref().map(|(a, _)| *a == art).unwrap_or(false) {
            return;
        }
        self.hide(wall);
        let since = self.hidden_at.get(&art).map(|h| (wall - h).max(0.0).round() as u64);
        let _ = self.tabs[self.active].st.log(&art, "", &format!("{art}.shown"), json!({"since": since}), "pane");
        self.shown = Some((art, wall));
    }

    /// The board on screen stops being it (another tab, or the pane is leaving): `<board>.hidden`,
    /// with how long it was on screen. Its detail panel asks again when it is next shown.
    fn hide(&mut self, wall: f64) {
        let Some((art, at)) = self.shown.take() else { return };
        let _ = self.tabs[0].st.log(&art, "", &format!("{art}.hidden"), json!({"for": (wall - at).max(0.0).round() as u64}), "pane");
        if let Some(l) = self.tabs.iter_mut().find(|l| l.art == art) {
            (l.detail_at, l.detail_sent) = (None, None);
        }
        self.hidden_at.insert(art, wall);
    }

    /// The terminal focus left the pane or came back. Back after `REFOCUS_SECS` or more, the
    /// board on screen is `shown` again (`since`: the seconds away). Losing the focus alone
    /// hides nothing: the pane may be in plain sight beside the chat.
    fn focus_changed(&mut self, focused: bool, wall: f64) {
        if !focused {
            self.blurred.get_or_insert(wall);
            return;
        }
        let Some(away) = self.blurred.take().map(|b| wall - b) else { return };
        if let (true, Some((art, _))) = (away >= REFOCUS_SECS, self.shown.as_ref()) {
            let _ = self.tabs[0].st.log(art, "", &format!("{art}.shown"), json!({"since": away.round() as u64}), "pane");
        }
    }

    fn set(&self) -> Vec<String> {
        self.tabs.iter().map(|l| l.art.clone()).collect()
    }

    /// Put the tab set and the tab on screen into the pane's own view (written by `persist`).
    fn store_set(&mut self) {
        let set = self.set();
        crate::tabs::store(&mut self.tabs[0].view, &set, self.active);
    }

    fn lv(&mut self) -> &mut Live {
        &mut self.tabs[self.active]
    }

    fn reload(&mut self, now: f64) {
        for l in &mut self.tabs {
            l.reload(now);
        }
    }

    fn persist(&mut self) {
        for l in &mut self.tabs {
            l.persist();
        }
    }

    /// About to save the pane's own view: does the file hold a tab set this pane hasn't read
    /// (`margin tab`, or `margin open --tabs` racing a pane that just started)? Saving would
    /// undo it. Reads the file only when there is something to save.
    fn edited_outside(&self) -> bool {
        let l = &self.tabs[0];
        if !l.unsaved() {
            return false;
        }
        let disk = l.st.view(&l.art, &l.pane);
        (&disk.tabs, &disk.tab) != (&l.saved.tabs, &l.saved.tab)
    }

    /// Bring tab `i` on screen. The tab left behind keeps everything (scroll, focus, folds, a
    /// search), and its changes not yet seen stay unseen: they become its badge.
    fn switch(&mut self, i: usize) -> bool {
        if i >= self.tabs.len() || i == self.active {
            return false;
        }
        let old = self.lv();
        old.drag = None;
        old.keys.reset();
        old.scene.pending_keys.clear();
        old.scene.hover = None;
        self.active = i;
        let new = self.lv();
        // its time on screen starts now
        new.visible_since = None;
        new.last_seen_poll = -10.0;
        // and its datasets are read again: they were not while it was out of view
        new.data_dirty = true;
        self.store_set();
        true
    }

    /// Carry out what a tab handed up. True when the tab on screen changed.
    fn act(&mut self, a: PaneAct) -> bool {
        let n = self.tabs.len() as i64;
        let to = match a {
            PaneAct::Go(i) => (i as i64).min(n - 1),
            PaneAct::Next(k) => (self.active as i64 + k as i64).rem_euclid(n),
            PaneAct::Prev(k) => (self.active as i64 - k as i64).rem_euclid(n),
        };
        self.switch(to as usize)
    }

    /// What the tab bar says about each tab (tabs.rs): its title, what changed in it since it
    /// was last on screen, whether it waits on the person, whether something in it failed.
    fn infos(&self) -> Vec<crate::tabs::TabInfo> {
        let titles = crate::tabs::short_titles(&self.tabs.iter().map(|l| if l.doc.title.is_empty() { l.art.clone() } else { l.doc.title.clone() }).collect::<Vec<_>>());
        self.tabs
            .iter()
            .zip(titles)
            .enumerate()
            .map(|(i, (l, title))| {
                let (needs, failed) = crate::tabs::attention(&l.doc, &l.state);
                // Claude blocked on a permission prompt in a session that keeps this artifact
                let waiting = l.activity.iter().find(|a| a.agent.is_empty()).map(|a| a.state == "waiting").unwrap_or(false);
                crate::tabs::TabInfo {
                    art: l.art.clone(),
                    title,
                    unseen: if i == self.active { 0 } else { l.scene.unseen_count() },
                    needs: needs || waiting,
                    // an overdue writer of the board is a failure of the board
                    failed: failed || !l.scene.stale.is_empty() || !l.data.stale(epoch_now()).is_empty() || l.toasts.iter().any(|t| t.level == "error"),
                }
            })
            .collect()
    }

    /// The tab on screen has been looked at: its change bars start to fade and a `seen` event is
    /// logged for its artifact. No other tab's: nobody has seen those.
    fn mark_seen(&mut self, now: f64) {
        self.lv().mark_seen(now);
    }

    /// Follow the view file: `margin tab` changed the tab set or the tab on screen. Tabs that
    /// stay keep their scenes. True when anything changed.
    fn sync(&mut self, cfg: &Config, now: f64) -> bool {
        let (st, pane, primary) = (store_like(&self.tabs[0].st), self.tabs[0].pane.clone(), self.tabs[0].art.clone());
        let disk = st.view(&primary, &pane);
        let mine = &self.tabs[0].view;
        if (&disk.tabs, &disk.tab) == (&mine.tabs, &mine.tab) {
            return false;
        }
        let (set, active) = crate::tabs::resolve(&primary, &[], &disk);
        let active_art = set[active].clone();
        let (mouse, focused) = (self.tabs[0].mouse, self.tabs[0].focused);
        let mut old: Vec<Live> = std::mem::take(&mut self.tabs);
        for a in &set {
            match old.iter().position(|l| l.art == *a) {
                Some(i) => self.tabs.push(old.remove(i)),
                None => match Live::open(&st, a, cfg, &pane, mouse, focused) {
                    Ok(mut l) => {
                        l.reload(now);
                        let _ = st.log(a, "", "view", json!({"pane": pane, "tab_of": primary}), "pane");
                        self.tabs.push(l);
                    }
                    Err(e) => select::log_line(&format!("tab {a}: {e}")),
                },
            }
        }
        self.active = self.tabs.iter().position(|l| l.art == active_art).unwrap_or(0);
        let lv = self.lv();
        lv.visible_since = None;
        lv.last_seen_poll = -10.0;
        // what the file says is now what this pane holds: nothing to write back, unless a tab
        // it named could not be opened
        self.tabs[0].saved.tabs = disk.tabs;
        self.tabs[0].saved.tab = disk.tab;
        self.store_set();
        true
    }

    /// What Claude sessions are doing across every tab's artifact, newest first: the pane marks
    /// its Zellij tab and wakes an idle Claude for any of them, whichever tab is on screen.
    fn activity(&self) -> Vec<crate::store::Activity> {
        if self.tabs.len() == 1 {
            return self.tabs[0].activity.clone();
        }
        let mut v: Vec<crate::store::Activity> = Vec::new();
        for a in self.tabs.iter().flat_map(|l| l.activity.iter()) {
            if !v.iter().any(|x| x.session == a.session && x.agent == a.agent) {
                v.push(a.clone());
            }
        }
        v.sort_by(|a, b| b.since.cmp(&a.since));
        v
    }

    /// The directories whose changes mean "reload": each tab's artifact, and the
    /// project's shared ones.
    fn watch(&self) -> Vec<std::path::PathBuf> {
        let st = &self.tabs[0].st;
        let mut w = vec![st.dir(), st.dir().join("delivered"), st.dir().join("cursors"), st.activity_dir(), st.dir().join("toasts")];
        for l in &self.tabs {
            w.push(st.art(&l.art));
            // where its owners' files are: an owner's write changes nothing else of the board
            w.push(st.owned_dir(&l.art));
        }
        w
    }

    fn view_dir(&self) -> std::path::PathBuf {
        self.tabs[0].st.art(&self.tabs[0].art).join("view")
    }

    /// The registered external dataset files (`margin data add --file`): each is watched where
    /// it is.
    fn data_files(&self) -> Vec<std::path::PathBuf> {
        crate::data::Registry::load(&self.tabs[0].st.dir()).datasets.values().filter_map(|d| d.file.as_ref().map(std::path::PathBuf::from)).collect()
    }

    /// A dataset's file changed: every tab's datasets are out of date. Nothing is read here.
    fn data_changed(&mut self) {
        for l in &mut self.tabs {
            l.data_dirty = true;
        }
    }

    /// Say which tab is on screen, and how wide the pane is.
    fn on_screen(&mut self, cols: u16) {
        let active = self.active;
        for (i, l) in self.tabs.iter_mut().enumerate() {
            l.on_screen = i == active;
            if l.on_screen && l.cols != cols {
                l.cols = cols;
                l.data_dirty = true;
            }
        }
    }
}

/// What a pane does on its way out, however it goes: every tab's view is saved, and the marks
/// that say "a pane with this pid is in review mode" are removed (a mark left behind held back
/// Claude's wake-up until something noticed the pid was dead).
fn leave(pn: &mut Pane) {
    pn.hide(epoch_now());
    for l in &mut pn.tabs {
        l.save_view();
        if l.review {
            crate::consumer::review(&l.st, &l.pane, &l.art, false);
        }
    }
}

/// The kitty deletes for the placements on screen (the tab is about to change under them).
fn take_down(imgs: &ImgSync, lv: &Live, fitted: &std::collections::HashMap<(u32, u16, u16), vellum::image::Image>) -> String {
    let mut out = String::new();
    for p in &imgs.shown {
        if let Some(Some(img)) = lv.scene.images.get(&p.key) {
            out.push_str(&kitty_delete(fitted.get(&(img.id, p.cols, p.rows)).map(|f| f.id).unwrap_or(img.id)));
        }
    }
    out
}

/// `art` is the pane's own artifact; `more` the other artifacts to show as tabs beside it (none:
/// the tabs its view file remembers).
pub fn run(st: Store, art: String, more: Vec<String>, cfg: Config, host: Box<dyn Host>) -> Result<bool, String> {
    let exe = std::env::current_exe().ok();
    let exe_t0 = exe.as_deref().and_then(exe_mtime);
    let mut rebuilt: Option<(std::time::SystemTime, f64)> = None;
    let pane = std::env::var("ZELLIJ_PANE_ID").map(|p| format!("zj-{p}")).unwrap_or_else(|_| "tty".into());
    // without a copy_command route (plain host), a click on a token is the only pointer input
    let mouse0 = cfg.mouse || host.selection() == Selection::InPane;
    let mut pn = Pane::open(&st, &art, &more, &cfg, &pane, mouse0, host.is_visible().is_none())?;
    crate::consumer::pane_started(&st);
    let modes = Modes { mouse: mouse0, kitty_keys: true, focus: true, paste: true };
    let mut term = Terminal::enter(modes).map_err(|e| e.to_string())?;
    let mut parser = Parser::new();
    // one clock for the parser's deadlines, here and in the loop
    let clock = Instant::now();
    let (probe_kitty, probe_cell) = match cfg.images.as_str() {
        "blocks" => (false, (0, 0)),
        "kitty" => (true, (0, 0)),
        _ => detect_kitty(&mut term, &mut parser, &clock),
    };
    let sz = term::size();
    let cell = if probe_cell.0 > 0 {
        probe_cell
    } else if sz.px_w > 0 {
        (sz.px_w / sz.cols.max(1), sz.px_h / sz.rows.max(1))
    } else {
        (10, 20)
    };
    // the binary's directory: a build replacing it is how the pane learns to reload
    let exe_dir = exe.as_deref().and_then(|e| e.parent()).map(|d| d.to_path_buf());
    let watch = |pn: &Pane, files: &[std::path::PathBuf]| {
        let mut w = pn.watch();
        w.extend(exe_dir.clone());
        let st = &pn.tabs[0].st;
        Inotify::new(&w, &pn.view_dir(), &st.dir(), &st.data_dir(), files)
    };
    let mut data_files = pn.data_files();
    let mut ino = watch(&pn, &data_files);
    // frames drawn because a dataset changed: 10 a second at most (data.rs `Pace`)
    let mut pace = crate::data::Pace::new();
    let now = || clock.elapsed().as_secs_f64();
    pn.on_screen(term::size().cols);
    pn.reload(0.0);
    for l in &pn.tabs {
        let _ = l.st.log(&l.art, "", "view", json!({"pane": l.pane, "kitty": probe_kitty}), "pane");
    }
    let mut prev: Option<Grid> = None;
    let mut imgs = ImgSync::default();
    // toasts: is the pane on screen (their clocks start then), checked at most once a second
    let (mut toast_vis, mut toast_vis_t) = (false, -10.0f64);
    // the seam to a consumer of the person's events (consumer.rs): what an adapter keeps for
    // this pane, if one is installed. Marking the terminal tab and waking an idle session are
    // the adapter's manners, not the pane's: the loop only hands it each frame
    let mut consumer = crate::consumer::pane();
    // whether the last frame had a session's activity to hand over (the frame after gets one
    // more call, so the adapter can take down what it put up)
    let mut consumer_on = false;
    // keyed by the source image's content id and the slot, so an edited PNG gets a fresh fit
    let mut fitted: std::collections::HashMap<(u32, u16, u16), vellum::image::Image> = Default::default();
    let mut size = term::size();
    // take up what `margin tab` wrote to the view file: the tab set, or the tab on screen
    macro_rules! follow {
        () => {
            let down = take_down(&imgs, pn.lv(), &fitted);
            if pn.sync(&cfg, now()) {
                if !down.is_empty() {
                    term.write(&down).map_err(|e| e.to_string())?;
                }
                imgs = ImgSync::default();
                ino = watch(&pn, &data_files);
                prev = None;
            }
        };
    }
    loop {
        let t = now();
        let wall = epoch_now();
        // the tab bar's badges, from every tab; then this iteration is the tab on screen's
        let infos = pn.infos();
        pn.on_screen(size.cols);
        // the view events: which board is on screen, and the block the detail panel rests on
        pn.view_step(wall);
        let active = pn.active;
        let lv = &mut pn.tabs[active];
        lv.scene.tabs = infos;
        lv.scene.tab = active;
        let data_due = lv.data_step(&mut pace, t, wall);
        let detail_due = lv.detail_step(t, wall);
        let asked_due = lv.asked_note(wall);
        // an owner goes stale by the clock alone: looked at on every frame, with a wake-up
        // (below) for the moment the next one would
        lv.scene.stale = crate::owner::stale_on(&lv.st, &lv.art, wall);
        let sent_wait = lv.sent_notes(wall);
        let next_stale = [crate::owner::next_stale(&lv.st, &lv.art, wall), lv.data.next_stale(wall)].into_iter().flatten().min_by(|a, b| a.total_cmp(b));
        // toasts: start each one's clock when the pane is first on screen, drop the ones run out
        if !lv.toasts.is_empty() {
            if lv.toasts.iter().any(|x| !lv.toast_seen.contains_key(&x.id)) && t - toast_vis_t >= 1.0 {
                toast_vis_t = t;
                toast_vis = lv.focused || host.is_visible().unwrap_or(true);
            }
            let mut shown = Vec::new();
            let mut gone = Vec::new();
            for x in &lv.toasts {
                if toast_vis || lv.focused {
                    lv.toast_seen.entry(x.id.clone()).or_insert(wall);
                }
                match crate::toast::left(x, lv.toast_seen.get(&x.id).copied(), wall) {
                    Some(f) => shown.push((x.clone(), f)),
                    None => gone.push(x.id.clone()),
                }
            }
            for id in gone {
                crate::toast::dismiss(&lv.st, &lv.art, &id);
                lv.toasts.retain(|x| x.id != id);
            }
            lv.scene.toasts = shown;
        } else {
            lv.scene.toasts.clear();
        }
        let mut frame = lv.scene.frame(&Input {
            doc: &lv.doc,
            state: &lv.state,
            view: &lv.view,
            delivered: lv.delivered,
            unread: lv.unread,
            now: t,
            wall,
            cfg: &lv.cfg,
            kitty: probe_kitty,
            cell,
            focused_pane: lv.focused,
            mouse: lv.mouse,
            project: &lv.st.project,
            w: size.cols,
            h: size.rows,
            activity: &lv.activity,
        });
        // the selection highlight, Everforest's visual-mode ground
        if let Some((a, b, true)) = lv.drag {
            for (y, xs, xe) in sel_rows(a, b, size.cols) {
                frame.grid.tint(vellum::layout::Rect::new(xs, y, xe - xs + 1, 1), vellum::color::Rgb::hex("#543a48"));
            }
        }
        // review mode: the target stays tinted, and its reaction pills sit under it (above it
        // when it's at the bottom of the view)
        if let Some(tg) = lv.target.as_mut() {
            let p = &vellum::color::EVERFOREST;
            for &(y, xs, xe) in &tg.cells {
                frame.grid.tint(vellum::layout::Rect::new(xs, y, xe.saturating_sub(xs) + 1, 1), p.purple.mix(p.bg0, 0.75));
            }
            let last = tg.cells.iter().map(|c| c.0).max().unwrap_or(0);
            let first = tg.cells.iter().map(|c| c.0).min().unwrap_or(0);
            let py = if last + 1 < size.rows.saturating_sub(3) { last + 1 } else { first.saturating_sub(1) };
            let kind = lv.doc.find(&tg.row).map(|b| b.kind.clone()).unwrap_or_default();
            let area = frame.grid.area();
            frame.grid.fill(vellum::layout::Rect::new(1, py, size.cols.saturating_sub(2), 1), vellum::style::Style::default().on(p.bg0));
            let mut x = 2u16;
            tg.pills.clear();
            for (label, r) in reactions(&kind) {
                let x0 = x;
                x = vellum::parts::tag(&mut frame.grid, x, py, &format!(" {label} "), p.purple, p.bg0, area) + 1;
                tg.pills.push((x0, py, x - 1 - x0, r));
            }
        }
        let mut out = String::new();
        // images: see ImgSync for when a placement is taken down, held back and sent
        let mut img_wake = None;
        let mut img_out = String::new();
        let plan = imgs.step(&frame.images, t, prev.is_none());
        for p in &plan.delete {
            if let Some(Some(img)) = lv.scene.images.get(&p.key) {
                img_out.push_str(&kitty_delete(fitted.get(&(img.id, p.cols, p.rows)).map(|f| f.id).unwrap_or(img.id)));
            }
        }
        for p in &plan.send {
            if let Some(Some(img)) = lv.scene.images.get(&p.key) {
                // sent at the slot's pixel size, cached per slot
                let f = fitted
                    .entry((img.id, p.cols, p.rows))
                    .or_insert_with(|| img.fit(p.cols as u32 * cell.0 as u32, p.rows as u32 * cell.1 as u32));
                img_out.push_str(&format!("\x1b[{};{}H", p.y + 1, p.x + 1));
                img_out.push_str(&kitty_place(f, p.cols, p.rows));
            }
        }
        if let Some(w) = plan.wake {
            img_wake = Some(w);
        }
        // while an image waits to be sent, its slot shows a placeholder: a raised ground and a
        // spinner. Kitty images draw above the cells, and once sent the next frame repaints the
        // slot bare, so a transparent PNG never shows the placeholder through
        if !plan.pending.is_empty() {
            let p = &vellum::color::EVERFOREST;
            let m = lv.cfg.motion();
            for pl in &plan.pending {
                let r = vellum::layout::Rect::new(pl.x, pl.y, pl.cols.min(size.cols.saturating_sub(pl.x)), pl.rows);
                frame.grid.tint(r, p.bg1);
                let label = format!("{} loading image", vellum::parts::spinner(t, m));
                let lw = vellum::text::width(&label) as u16;
                if r.w > lw + 2 && pl.rows > 0 {
                    let st = vellum::style::Style::fg(p.grey1).on(p.bg1);
                    frame.grid.put(pl.x + (r.w - lw) / 2, pl.y + pl.rows / 2, &label, st);
                }
            }
            if m.on {
                img_wake = Some(img_wake.map_or(t + 0.1, |w: f64| w.min(t + 0.1)));
            }
        }
        diff(prev.as_ref(), &frame.grid, &mut out);
        out.push_str(&img_out);
        if out.len() > 8 {
            term.write(&synchronized(&out)).map_err(|e| e.to_string())?;
        }
        prev = Some(frame.grid);

        // once the change bars have faded, this look becomes the new "since you looked"
        if lv.fresh_look && !lv.scene.retained.nodes.values().any(|n| n.change.is_some()) {
            lv.view.seen_at = now_rfc3339();
            lv.fresh_look = false;
        }
        // `margin tab` may have written the view file since this pane last read it: follow it
        // before saving over it
        if pn.edited_outside() {
            follow!();
        }
        pn.persist();

        // seen: an unseen change waits until the pane is actually on screen for 1.5 s
        let mut wake = frame.wake;
        if let Some(at) = next_stale {
            wake.at(t + (at - wall).max(0.0) + 0.5);
        }
        if let Some(at) = data_due.into_iter().chain(detail_due).reduce(f64::min) {
            wake.at(at);
        }
        // "asked for more" becomes "no answer yet"
        if let Some(at) = asked_due {
            wake.at(t + (at - wall).max(0.0) + 0.05);
        }
        // a row saying `sent` is looked at again until its note goes
        if sent_wait {
            wake.at(t + 0.5);
        }
        // the board's consumer, for every tab's sessions: a pane none of whose boards a session
        // keeps has no activity and takes none of this path
        let acts = pn.activity();
        if let Some(c) = consumer.as_mut().filter(|_| !acts.is_empty() || consumer_on) {
            consumer_on = !acts.is_empty();
            let boards: Vec<(&crate::doc::Doc, &crate::store::State)> = pn.tabs.iter().map(|l| (&l.doc, &l.state)).collect();
            let tick = crate::consumer::Tick {
                activity: &acts,
                t,
                wall: epoch_now(),
                cfg: &cfg,
                store: &pn.tabs[0].st,
                last_seq: pn.tabs.iter().map(|l| l.last_seq).max().unwrap_or(0),
                review: pn.tabs.iter().any(|l| l.review),
                boards: &boards,
                visible: &|| host.is_visible(),
            };
            if let Some(at) = c.step(&tick) {
                wake.at(at);
            }
        }
        let lv = pn.lv();
        // a half-typed `z` or count clears itself after the keymap's timeout
        if !lv.scene.pending_keys.is_empty() {
            if lv.keys.expire(t) {
                lv.scene.pending_keys.clear();
                prev = None;
            } else {
                wake.at(t + lv.keys.timeout);
            }
        }
        if let Some(at) = img_wake {
            wake.at(at);
        }
        // only the tab on screen: a tab out of view keeps its changes unseen (they are its badge)
        if lv.scene.retained.any_unseen() {
            // the host knows whether this pane is on screen (Zellij: is its tab the active one)
            let visible = if t - lv.last_seen_poll >= 1.0 {
                lv.last_seen_poll = t;
                host.is_visible().unwrap_or(true)
            } else {
                lv.visible_since.is_some()
            };
            if visible || lv.focused {
                let since = *lv.visible_since.get_or_insert(t);
                if lv.focused || t - since >= 1.5 {
                    lv.visible_since = None;
                    pn.mark_seen(t);
                    continue;
                }
            } else {
                lv.visible_since = None;
            }
            wake.at(t + 0.5);
        }
        if pn.tabs.iter().any(|l| l.quit) || term::terminated() {
            break;
        }
        // reload once the new binary has stopped changing for a second (the linker writes it in
        // pieces); a file mid-write would fail to exec
        if let (Some(e), Some(t0)) = (exe.as_deref(), exe_t0) {
            if let Some(m) = exe_mtime(e).filter(|m| *m != t0) {
                match rebuilt {
                    Some((pm, since)) if pm == m && t - since >= 1.0 => {
                        // the new binary's pane says `shown` when it starts
                        pn.hide(epoch_now());
                        for l in &mut pn.tabs {
                            l.save_view();
                        }
                        return Ok(true);
                    }
                    Some((pm, _)) if pm == m => {}
                    _ => rebuilt = Some((m, t)),
                }
            }
        }
        let mut timeout = wake.timeout_ms(now());
        if rebuilt.is_some() {
            timeout = if timeout < 0 { 250 } else { timeout.min(250) };
        }
        // wake when the parser's quiet period is up; it decides whether that has really passed
        if let Some(d) = parser.deadline() {
            let ms = ((d - now()) * 1000.0).ceil().max(0.0) as i32;
            timeout = if timeout < 0 { ms } else { timeout.min(ms) };
        }
        // sleep until something happens. A dataset changing is held to the pace: while the
        // last data frame is less than `Pace::GAP` old and nothing else happened, the pane goes
        // back to sleep until the gap is up, so a burst of appends draws a bounded number of frames
        let deadline = (timeout >= 0).then(|| now() + timeout as f64 / 1000.0);
        let (bytes, resized) = loop {
            let (bytes, resized, ready) = term.poll(&[ino.fd], timeout).map_err(|e| e.to_string())?;
            let t = now();
            let mut only_data = false;
            if !ready.is_empty() {
                let fired = ino.drain();
                if fired.other {
                    pn.reload(t);
                    // a dataset registered on a file of its own is watched where that file is
                    let files = pn.data_files();
                    if files != data_files {
                        data_files = files;
                        ino = watch(&pn, &data_files);
                    }
                }
                // the view directory: this pane's own saves, or `margin tab` changing the set
                if fired.views {
                    follow!();
                }
                if fired.data {
                    pn.data_changed();
                }
                only_data = fired == Fired { data: true, ..Default::default() };
            }
            // a board that reads no dataset has nothing to draw for one; a board that does
            // waits out the pace
            let (hold, idle) = (pace.hold(t), !pn.lv().binds && !pn.lv().data_dirty_first());
            if only_data && bytes.is_empty() && !resized && (idle || hold > 0.0) {
                let rest = deadline.map(|d| ((d - t) * 1000.0).ceil().max(0.0) as i32);
                timeout = match (idle, rest) {
                    (true, r) => r.unwrap_or(-1),
                    (false, Some(r)) => r.min((hold * 1000.0).ceil() as i32),
                    (false, None) => (hold * 1000.0).ceil() as i32,
                };
                continue;
            }
            break (bytes, resized);
        };
        let t = now();
        if resized {
            size = term::size();
            prev = None;
        }
        let mut evs = parser.feed(&bytes, t);
        evs.extend(parser.flush(t));
        for ev in evs {
            let lv = pn.lv();
            match ev {
                Event::Key(k, mods) => lv.key(k, mods.ctrl, t, &mut term),
                Event::Focus(f) => {
                    lv.focused = f;
                    if f && lv.scene.retained.any_unseen() {
                        pn.mark_seen(t);
                    }
                    pn.focus_changed(f, epoch_now());
                }
                Event::Mouse { kind: MouseKind::Move, x, y, .. } => lv.scene.hover = lv.scene.row_at(x, y).map(String::from),
                // the wheel scrolls what it is over: the detail panel, or the list
                Event::Mouse { kind: MouseKind::WheelDown, x, y, .. } if lv.scene.in_detail(x, y) => lv.scene.detail_scroll_by(3),
                Event::Mouse { kind: MouseKind::WheelUp, x, y, .. } if lv.scene.in_detail(x, y) => lv.scene.detail_scroll_by(-3),
                Event::Mouse { kind: MouseKind::WheelDown, .. } => lv.scene.scroll_by(3),
                Event::Mouse { kind: MouseKind::WheelUp, .. } => lv.scene.scroll_by(-3),
                // press starts a drag; a release without movement is the click, so a selection
                // that starts on a button doesn't press it
                Event::Mouse { kind: MouseKind::Press(0), x, y, .. } => lv.drag = Some(((x, y), (x, y), false)),
                Event::Mouse { kind: MouseKind::Drag(0), x, y, .. } => {
                    if let Some(d) = &mut lv.drag {
                        d.1 = (x, y);
                        d.2 |= d.1 != d.0;
                    }
                }
                Event::Mouse { kind: MouseKind::Release(_), x, y, .. } => {
                    // a click on the tab bar is the pane's, in review mode too
                    let tab = lv.scene.hit_at(x, y).and_then(tab_act).filter(|_| matches!(lv.drag, Some((_, _, false))));
                    if tab.is_some() {
                        lv.drag = None;
                        lv.pane_act = tab;
                    } else if let (true, Some((a, _, moved)), Some(g)) = (lv.review, lv.drag, prev.clone()) {
                        lv.drag = None;
                        lv.review_release(&g, a, (x, y), moved, size.cols, t);
                        prev = None;
                    } else if let (Some((a, b, moved)), Some(g)) = (lv.drag.take(), prev.clone()) {
                        if moved {
                            let text = sel_text(&g, &sel_rows(a, b, size.cols));
                            if !text.is_empty() {
                                let how = select::copy(&text, &lv.cfg);
                                select::log_line(&format!("pane copy {} chars via {how}", text.chars().count()));
                            }
                            prev = None; // repaint without the highlight
                        } else {
                            lv.click(&g, x, y, t);
                        }
                    }
                }
                // kitty graphics replies Zellij writes into the pane: never keystrokes
                Event::KittyReply(_) => {}
                _ => {}
            }
            // the keyboard focus and mouse reporting are the pane's: every tab holds the same
            let lv = pn.lv();
            let (mouse, focused, act) = (lv.mouse, lv.focused, lv.pane_act.take());
            for l in &mut pn.tabs {
                (l.mouse, l.focused) = (mouse, focused);
            }
            if let Some(a) = act {
                // the images on screen belong to the tab being left
                let down = take_down(&imgs, pn.lv(), &fitted);
                if pn.act(a) {
                    if !down.is_empty() {
                        term.write(&down).map_err(|e| e.to_string())?;
                    }
                    imgs = ImgSync::default();
                }
            }
        }
    }
    // leaving (q, SIGTERM, SIGHUP: the tab or terminal closed): the view is kept, this pane's
    // own marks go, and its images come down. The terminal itself (raw mode, mouse reporting,
    // the alternate screen, the cursor) is restored when `term` drops, on a panic too (term.rs)
    let down = take_down(&imgs, pn.lv(), &fitted);
    if !down.is_empty() {
        let _ = term.write(&down);
    }
    leave(&mut pn);
    Ok(false)
}

#[cfg(test)]
mod pane_tests {
    use super::*;
    use crate::doc::Op;

    fn pane(arts: &[&str]) -> (Store, Pane) {
        let st = crate::store::tests::temp_store();
        for a in arts {
            let doc: crate::doc::Doc = serde_json::from_value(json!({"id": a, "title": format!("proj · {a}"), "blocks": [
                {"type": "text", "id": "x", "text": "one"}, {"type": "text", "id": "y", "text": "two"}]}))
            .unwrap();
            st.put(doc, None).unwrap();
        }
        let more: Vec<String> = arts[1..].iter().map(|s| s.to_string()).collect();
        let mut pn = Pane::open(&st, arts[0], &more, &Config::default(), "tty", false, true).unwrap();
        pn.reload(0.0);
        (st, pn)
    }

    fn seen(st: &Store) -> Vec<String> {
        st.events().into_iter().filter(|e| e.kind == "seen").map(|e| e.artifact).collect()
    }

    /// A change in a tab out of view becomes its badge and stays unseen; only the tab on screen
    /// is ever logged as seen, and the other one is when it comes on screen.
    #[test]
    fn only_the_tab_on_screen_is_seen() {
        let (st, mut pn) = pane(&["a", "b"]);
        assert_eq!(pn.infos().iter().map(|i| i.title.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        for a in ["a", "b"] {
            st.patch(a, &[Op::Set { id: "x".into(), field: "text".into(), value: json!("uno") }], None).unwrap();
        }
        pn.reload(1.0);
        let infos = pn.infos();
        assert_eq!((infos[0].unseen, infos[1].unseen), (0, 1), "the tab on screen has no badge; the other counts its change");
        pn.mark_seen(2.0);
        assert_eq!(seen(&st), ["a"]);
        assert_eq!(pn.infos()[1].unseen, 1, "b is still unseen");
        assert!(pn.act(PaneAct::Next(1)));
        assert_eq!(pn.active, 1);
        pn.mark_seen(3.0);
        assert_eq!(seen(&st), ["a", "b"]);
        // a, now out of view, has nothing new
        assert_eq!(pn.infos()[0].unseen, 0);
    }

    fn views(st: &Store) -> Vec<(String, serde_json::Value)> {
        st.events().into_iter().filter(|e| crate::store::is_view(&e.kind)).map(|e| (e.kind, e.value)).collect()
    }

    /// The board on screen is said once: `shown` at the start, `hidden` then `shown` on a tab
    /// switch (with how long the board coming back was away), `shown` again after a long time
    /// without the terminal focus, `hidden` on the way out. None of it is a decision or an
    /// action: the hooks hand a Claude session nothing.
    #[test]
    fn a_tab_switch_says_hidden_then_shown_once_each() {
        let (st, mut pn) = pane(&["a", "b"]);
        pn.view_step(100.0);
        pn.view_step(101.0);
        assert_eq!(views(&st), [("a.shown".to_string(), json!({"since": null}))], "once, however many frames");
        assert!(pn.act(PaneAct::Next(1)));
        pn.view_step(130.0);
        pn.view_step(131.0);
        assert_eq!(views(&st)[1..], [("a.hidden".to_string(), json!({"for": 30})), ("b.shown".to_string(), json!({"since": null}))]);
        assert!(pn.act(PaneAct::Prev(1)));
        pn.view_step(190.0);
        assert_eq!(views(&st)[3..], [("b.hidden".to_string(), json!({"for": 60})), ("a.shown".to_string(), json!({"since": 60}))]);
        // the focus: a short time away says nothing, a long one says shown again; never hidden
        pn.focus_changed(false, 200.0);
        pn.focus_changed(true, 200.0 + REFOCUS_SECS - 1.0);
        assert_eq!(views(&st).len(), 5);
        pn.focus_changed(false, 300.0);
        pn.focus_changed(true, 300.0 + REFOCUS_SECS + 30.0);
        pn.focus_changed(true, 500.0);
        assert_eq!(views(&st)[5..], [("a.shown".to_string(), json!({"since": 90}))]);
        pn.hide(500.0);
        assert_eq!(views(&st)[6..], [("a.hidden".to_string(), json!({"for": 310}))]);
        let evs = st.events();
        let e = evs.iter().find(|e| e.kind == "a.shown").unwrap();
        assert_eq!((e.artifact.as_str(), e.id.as_str(), e.by.as_str(), e.via.as_str()), ("a", "", "you", "pane"));
        // passive: no decision, no action, nothing for a Claude session's hooks
        assert!(evs.iter().all(|e| !crate::store::is_decision(e) && !crate::store::is_action(e)));
        assert!(crate::claude::hook::unread(&st, "sess").is_empty());
        assert!(crate::claude::hook::actions_for_session(&st, "a", 0).is_empty() && crate::claude::hook::actions_for_session(&st, "b", 0).is_empty());
        std::fs::remove_dir_all(&st.home).ok();
    }

    /// The tab set and the tab on screen are written to the pane's own view file and come back
    /// on the next start; each tab keeps its own focus.
    #[test]
    fn tabs_persist_in_the_view_file() {
        let (st, mut pn) = pane(&["a", "b", "c"]);
        pn.tabs[0].scene.focus = Some("x".into());
        assert!(pn.act(PaneAct::Go(2)));
        pn.tabs[2].scene.focus = Some("y".into());
        pn.persist();
        let v = st.view("a", "tty");
        assert_eq!((v.tabs.clone(), v.tab.as_str(), v.focus.as_deref()), (vec!["a".to_string(), "b".into(), "c".into()], "c", Some("x")));
        assert_eq!(st.view("c", "tty").focus.as_deref(), Some("y"));
        assert!(st.view("b", "tty").tabs.is_empty(), "only the pane's own artifact holds the set");
        // a start with no tabs named restores the set and the tab on screen
        let again = Pane::open(&st, "a", &[], &Config::default(), "tty", false, true).unwrap();
        assert_eq!((again.set(), again.active), (vec!["a".to_string(), "b".into(), "c".into()], 2));
        // an artifact named on the command line that doesn't exist is an error
        assert!(Pane::open(&st, "a", &["nope".into()], &Config::default(), "tty", false, true).is_err());
    }

    /// `]t` / `[t` step the tabs component the focus is in, wrapping; the choice is the view's
    /// (per pane, by the component's id) and the default writes nothing; a focus inside the
    /// section that went moves to the strip.
    #[test]
    fn sub_tab_keys_step_and_persist() {
        let st = crate::store::tests::temp_store();
        let doc: crate::doc::Doc = serde_json::from_value(json!({"id": "a", "title": "a", "blocks": [
            {"type": "tabs", "id": "views", "children": [
                {"type": "section", "id": "mine", "title": "Mine", "children": [{"type": "row", "id": "m1", "text": "mine"}]},
                {"type": "section", "id": "team", "title": "Team", "children": [{"type": "row", "id": "t1", "text": "team"}]}]},
            {"type": "row", "id": "out", "text": "outside"}]}))
        .unwrap();
        st.put(doc, None).unwrap();
        let mut pn = Pane::open(&st, "a", &[], &Config::default(), "tty", false, true).unwrap();
        pn.reload(0.0);
        let lv = pn.lv();
        lv.scene.focus = Some("m1".into());
        lv.subtab_step(1);
        assert_eq!(lv.view.subtabs.get("views").map(String::as_str), Some("team"));
        assert_eq!(lv.scene.focus.as_deref(), Some("views"), "the focus left with its section: it goes to the strip");
        pn.persist();
        assert_eq!(st.view("a", "tty").subtabs.get("views").map(String::as_str), Some("team"));
        let lv = pn.lv();
        lv.subtab_step(1);
        assert!(lv.view.subtabs.is_empty(), "wrapped to the first, which is the default");
        lv.subtab_step(-1);
        assert_eq!(lv.view.subtabs.get("views").map(String::as_str), Some("team"));
        // outside any tabs component the keys do nothing
        lv.scene.focus = Some("out".into());
        lv.subtab_step(1);
        assert_eq!(lv.view.subtabs.get("views").map(String::as_str), Some("team"));
        // a click on a tab is the same switch
        lv.subtab_show("views", "mine");
        assert!(lv.view.subtabs.is_empty());
        lv.subtab_show("views", "nope");
        assert!(lv.view.subtabs.is_empty(), "a section that isn't there changes nothing");
        // the keymap
        let mut k = keys();
        let mut feed = |s: &str| s.chars().map(|c| k.feed(vellum::keymap::Press::ch(c), 0.0)).last().unwrap();
        assert_eq!(feed("]t"), vellum::keymap::Fed::Act(Act::SubNext, 1));
        assert_eq!(feed("[t"), vellum::keymap::Fed::Act(Act::SubPrev, 1));
        assert_eq!(feed("]]"), vellum::keymap::Fed::Act(Act::NextSection, 1));
    }

    /// gt and gT wrap; a count goes to that tab; a click on the bar is the same action.
    #[test]
    fn tab_keys_and_clicks() {
        let (_st, mut pn) = pane(&["a", "b", "c"]);
        assert!(pn.act(PaneAct::Prev(1)));
        assert_eq!(pn.active, 2, "gT from the first wraps to the last");
        assert!(pn.act(PaneAct::Next(1)));
        assert_eq!(pn.active, 0);
        assert!(pn.act(PaneAct::Go(1)));
        assert!(!pn.act(PaneAct::Go(1)), "already there");
        assert!(pn.act(PaneAct::Go(9)));
        assert_eq!(pn.active, 2, "a count past the end is the last tab");
        assert_eq!(tab_act("mg:tab/1"), Some(PaneAct::Go(1)));
        assert_eq!(tab_act("mg:tab/prev"), Some(PaneAct::Prev(1)));
        assert_eq!(tab_act("mg:todo/x"), None);
        // the keymap: gt, 2gt, gT
        let mut k = keys();
        let mut feed = |s: &str| s.chars().map(|c| k.feed(vellum::keymap::Press::ch(c), 0.0)).last().unwrap();
        assert_eq!(feed("gt"), vellum::keymap::Fed::Act(Act::TabNext, 1));
        assert_eq!(feed("2gt"), vellum::keymap::Fed::Act(Act::TabNext, 2));
        assert_eq!(feed("gT"), vellum::keymap::Fed::Act(Act::TabPrev, 1));
    }

    /// `margin tab` writes the view file; the pane follows it, keeping the scenes of the tabs
    /// that stay.
    #[test]
    fn pane_follows_the_view_file() {
        let (st, mut pn) = pane(&["a", "b"]);
        pn.persist();
        pn.tabs[1].scene.focus = Some("y".into());
        let doc: crate::doc::Doc = serde_json::from_value(json!({"id": "c", "title": "c", "blocks": []})).unwrap();
        st.put(doc, None).unwrap();
        let mut v = st.view("a", "tty");
        crate::tabs::edit(&mut v, "a", "add", "c").unwrap();
        crate::tabs::edit(&mut v, "a", "show", "c").unwrap();
        st.save_view("a", "tty", &v);
        assert!(pn.sync(&Config::default(), 1.0));
        assert_eq!((pn.set(), pn.active), (vec!["a".to_string(), "b".into(), "c".into()], 2));
        assert_eq!(pn.tabs[1].scene.focus.as_deref(), Some("y"), "b kept its scene");
        assert!(!pn.sync(&Config::default(), 2.0), "nothing new in the file");
        // the tab on screen is removed: its left neighbour comes forward
        crate::tabs::edit(&mut v, "a", "rm", "c").unwrap();
        st.save_view("a", "tty", &v);
        assert!(pn.sync(&Config::default(), 3.0));
        assert_eq!((pn.set(), pn.active), (vec!["a".to_string(), "b".into()], 1));
    }
}

#[cfg(test)]
mod action_tests {
    use super::*;
    use crate::doc::Op;

    /// A board with a feed-owned table whose rows offer `e retry`, one row with its own action,
    /// and a board-wide `u refresh`.
    fn board() -> (Store, Pane) {
        let st = crate::store::tests::temp_store();
        let doc: crate::doc::Doc = serde_json::from_value(json!({"id": "ci", "title": "CI", "actions": [{"key": "u", "name": "refresh", "label": "refresh all"}], "blocks": [
            {"type": "table", "id": "job", "columns": [{"key": "name"}], "actions": [{"key": "e", "name": "retry", "label": "retry the job"}], "children": [
                {"type": "tr", "id": "job-1", "target": "https://example.com/job/1", "detail": "exit 2", "cells": {"name": "build"}},
                {"type": "tr", "id": "job-2", "cells": {"name": "test"}, "actions": [{"key": "X", "name": "cancel"}]}]},
            {"type": "text", "id": "note", "text": "a note"}]}))
        .unwrap();
        st.put(doc, None).unwrap();
        // the table is a feed's: its rows are read-only content, and still take the person's actions
        std::fs::write(st.dir().join("owners.json"), r#"{"owners": [{"name": "ci", "board": "ci", "owns": "job"}]}"#).unwrap();
        let mut pn = Pane::open(&st, "ci", &[], &Config::default(), "tty", false, true).unwrap();
        pn.reload(0.0);
        (st, pn)
    }

    fn handler(st: &Store, name: &str, on: &str) {
        let h = crate::handler::Handler { name: name.into(), cmd: vec!["true".into()], on: vec![on.into()], board: String::new(), owns: String::new(), paused: false, cwd: "/".into(), path: String::new(), timeout: 60.0, retries: 3, added: String::new() };
        crate::handler::edit_registry(st, |r| r.add(h)).unwrap();
        st.cursor_or_create(&format!("handler:{name}"));
    }

    fn frame(lv: &mut Live, w: u16) -> (String, Grid) {
        let g = lv
            .scene
            .frame(&Input { doc: &lv.doc, state: &lv.state, view: &lv.view, delivered: 0, unread: 0, now: 10.0, wall: epoch_now(), cfg: &lv.cfg, kitty: false, cell: (10, 20), focused_pane: true, mouse: true, project: "proj", w, h: 30, activity: &[] })
            .grid;
        (g.to_text(), g)
    }

    /// A board bound to two datasets, in a pane: a plot, a line of text and a table.
    fn data_board() -> (Store, Pane) {
        let st = crate::store::tests::temp_store();
        crate::data::tests::held(&st, "requests");
        crate::data::tests::held(&st, "prs");
        st.data_put("requests", None, &(0..5).map(|i| json!({"rps": 70 + i})).collect::<Vec<_>>()).unwrap();
        st.data_put("prs", None, &[json!({"id": 12, "title": "Cache the layout"}), json!({"id": 14, "title": "Fix the flaky test", "target": "https://example.com/14"})]).unwrap();
        let doc: crate::doc::Doc = serde_json::from_value(json!({"id": "dash", "title": "Dash", "blocks": [
            {"type": "plot", "id": "load", "kind": "spark", "data": "requests", "y": ["rps"], "last": 5},
            {"type": "text", "id": "note", "text": "now ~[@requests.rps:3]"},
            {"type": "table", "id": "pr", "rows": "prs", "key": "id", "actions": [{"key": "e", "name": "retry"}], "columns": [{"key": "id", "title": "PR"}, {"key": "title", "title": "Title"}]}]}))
        .unwrap();
        st.put(doc, None).unwrap();
        st.put(serde_json::from_value(json!({"id": "other", "title": "Other", "blocks": [{"type": "text", "id": "x", "text": "nothing bound"}]})).unwrap(), None).unwrap();
        let mut pn = Pane::open(&st, "dash", &["other".to_string()], &Config::default(), "tty", false, true).unwrap();
        pn.on_screen(80);
        pn.reload(0.0);
        (st, pn)
    }

    /// One frame of the tab on screen at `now`, the way the loop draws it: the datasets first.
    fn data_frame(pn: &mut Pane, pace: &mut crate::data::Pace, now: f64) -> String {
        pn.on_screen(80);
        let lv = pn.lv();
        lv.data_step(pace, now, epoch_now());
        lv.scene.frame(&Input { doc: &lv.doc, state: &lv.state, view: &lv.view, delivered: 0, unread: 0, now, wall: epoch_now(), cfg: &lv.cfg, kitty: false, cell: (10, 20), focused_pane: false, mouse: false, project: "proj", w: 80, h: 24, activity: &[] }).grid.to_text()
    }

    /// A dataset changing is not the document changing: the plot and the inline strip draw the
    /// new numbers, a table's rows keep their ids (the focus stays on a record, a record that
    /// goes collapses away, a new one opens), and through all of it the board has the revision
    /// it had, no change bar, no unseen count and no "new" in its title bar.
    #[test]
    fn a_dataset_change_redraws_without_a_revision_a_change_bar_or_an_unseen_count() {
        let (st, mut pn) = data_board();
        let mut pace = crate::data::Pace::new();
        let t = data_frame(&mut pn, &mut pace, 1.0);
        assert!(t.contains("12 Cache the layout") && t.contains("14 Fix the flaky test") && t.contains("now ▁▅█"), "{t}");
        let ids = |pn: &mut Pane| pn.lv().doc.find("pr").unwrap().children.iter().map(|r| r.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&mut pn), ["pr-12", "pr-14"], "a row's id is the table's id and the record's key");
        assert!(pn.lv().scene.focusable.contains(&"pr-14".to_string()));
        pn.lv().scene.focus = Some("pr-14".into());
        let rev = st.load("dash").unwrap().rev;
        // new numbers, a record gone and a record new
        st.data_append("requests", None, &[json!({"rps": 10}), json!({"rps": 200})]).unwrap();
        st.data_put("prs", None, &[json!({"id": 14, "title": "Fix the flaky test"}), json!({"id": 15, "title": "A third"})]).unwrap();
        pn.data_changed();
        let t = data_frame(&mut pn, &mut pace, 10.0);
        assert!(t.contains("now ▃▁█"), "{t}");
        assert_eq!(ids(&mut pn), ["pr-14", "pr-15"]);
        let lv = pn.lv();
        assert_eq!((lv.scene.focus.as_deref(), lv.doc.rev, lv.raw.rev, st.load("dash").unwrap().rev), (Some("pr-14"), rev, rev, rev));
        assert!(lv.raw.find("pr").unwrap().children.is_empty(), "the rows live in the pane's copy only");
        // the row that went is leaving (it collapses where it was), the new one is opening
        assert!(lv.scene.retained.get("pr-12").map(|n| n.leaving).unwrap_or(false), "pr-12 animates out");
        assert!(lv.scene.retained.get("pr-15").is_some());
        // and nothing is marked as changed: no bar, no unseen count, no tour stop, no title tag
        assert!(lv.scene.retained.nodes.values().all(|n| n.change.is_none() && !n.unseen), "a change bar or an unseen mark");
        assert_eq!((lv.scene.unseen_count(), lv.scene.change_step(true)), (0, None));
        let later = data_frame(&mut pn, &mut pace, 30.0);
        assert!(!later.lines().next().unwrap().contains(" new ") && !later.contains("Cache the layout") && later.contains("15 A third"), "{later}");
        assert_eq!(pn.infos()[0].unseen, 0);
        // the action's event carries the record
        pn.lv().fire("pr-15", "retry", "key", 31.0);
        let ev = st.events().into_iter().find(|e| e.kind == "dash.retry").unwrap();
        assert_eq!((ev.id.as_str(), ev.value["cells"]["title"].as_str(), ev.rev), ("pr-15", Some("A third"), rev));
        // a real change to the document is still one
        st.patch("dash", &[crate::doc::Op::Set { id: "note".into(), field: "text".into(), value: json!("edited ~[@requests.rps:3]") }], None).unwrap();
        pn.reload(40.0);
        data_frame(&mut pn, &mut pace, 40.0);
        assert!(pn.lv().scene.retained.get("note").unwrap().unseen && pn.lv().doc.rev == rev + 1);
        assert_eq!(ids(&mut pn), ["pr-14", "pr-15"], "and the rows are still there after a reload");
        std::fs::remove_dir_all(&st.home).ok();
    }

    /// An owner's rows are content, kept in the owner's own file: its write leaves the board's
    /// rev (the layout's) where it was, and the pane still reconciles the rows by id and flags
    /// them as it always did: a change bar and an unseen mark on what changed or appeared, the
    /// row that went collapsing where it was, the count in the title bar and on the tab. The
    /// reload reads the one file that changed and nothing else.
    #[test]
    fn an_owners_write_marks_its_changed_rows_and_the_title_counts_them() {
        let st = crate::store::tests::temp_store();
        let doc: crate::doc::Doc = serde_json::from_value(json!({"id": "ci", "title": "CI", "blocks": [
            {"type": "table", "id": "jobs", "columns": [{"key": "name", "title": "Job"}]},
            {"type": "text", "id": "note", "text": "a note of the layout"}]}))
        .unwrap();
        st.put(doc, None).unwrap();
        st.add_owner(crate::owner::Owner { name: "queue".into(), board: "ci".into(), owns: "run".into(), expect_every: None, added: now_rfc3339(), last_write: String::new() }).unwrap();
        let row = |id: &str, name: &str| crate::doc::parse_line(&json!({"type": "tr", "id": id, "parent": "jobs", "cells": {"name": name}}).to_string()).unwrap();
        st.apply_as("ci", "queue", &[row("run-1", "build"), row("run-2", "test")].concat()).unwrap();
        let mut pn = Pane::open(&st, "ci", &[], &Config::default(), "tty", false, true).unwrap();
        pn.on_screen(80);
        pn.reload(0.0);
        let mut pace = crate::data::Pace::new();
        let t = data_frame(&mut pn, &mut pace, 1.0);
        assert!(t.contains("build") && t.contains("test") && !t.lines().next().unwrap().contains(" new "), "{t}");
        let rev = pn.lv().doc.rev;
        // nothing changed: a reload reads no file again
        let r0 = crate::store::file_reads();
        pn.reload(2.0);
        assert_eq!(crate::store::file_reads(), r0, "an unchanged board is not read again");
        // the owner writes: one row changed, one new, one gone
        let mut ops = [row("run-2", "test (retried)"), row("run-3", "deploy")].concat();
        ops.push(crate::doc::Op::Rm { id: "run-1".into() });
        assert_eq!(st.apply_as("ci", "queue", &ops).unwrap(), rev, "the board's rev did not move");
        let r1 = crate::store::file_reads();
        pn.reload(10.0);
        let (reads, bytes) = crate::store::file_reads();
        let file = std::fs::metadata(st.art("ci").join("owned/owner.queue.json")).unwrap().len();
        assert_eq!((reads - r1.0, bytes - r1.1), (1, file), "the reload reads the owner's file, and only it");
        data_frame(&mut pn, &mut pace, 10.0);
        // a moment later: the new row has opened, the one that went has closed
        let t = data_frame(&mut pn, &mut pace, 12.0);
        let lv = pn.lv();
        assert_eq!((lv.doc.rev, lv.raw.owned["owner:queue"].rev), (rev, 2));
        assert!(t.contains("test (retried)") && t.contains("deploy"), "{t}");
        for id in ["run-2", "run-3"] {
            let n = lv.scene.retained.get(id).unwrap();
            assert!(n.unseen && n.change.is_some(), "{id} is flagged as changed");
        }
        assert!(lv.scene.retained.get("run-1").map(|n| n.leaving).unwrap_or(true) && !t.contains("build"), "run-1 animated out: {t}");
        assert!(!lv.scene.retained.get("note").unwrap().unseen && !lv.scene.retained.get("jobs").unwrap().unseen, "the layout did not change");
        assert_eq!(lv.scene.unseen_count(), 2);
        // unseen for a while (nobody looked): the title bar says how many
        let t = data_frame(&mut pn, &mut pace, 30.0);
        assert!(t.lines().next().unwrap().contains(" 2 new "), "the title bar counts them: {t}");
        assert_eq!(pn.infos()[0].unseen, 0, "the tab on screen has no badge");
        let lv = pn.lv();
        // the same state again is no change: nothing new is flagged
        st.apply_as("ci", "queue", &row("run-3", "deploy")).unwrap();
        lv.reload(40.0);
        assert_eq!(lv.scene.unseen_count(), 2);
        std::fs::remove_dir_all(&st.home).ok();
    }

    /// The pace and the tab on screen bound what a busy dataset costs: a burst of appends is
    /// read and drawn at most once per `Pace::GAP`, an unchanged file is not read at all, and a
    /// tab out of view reads nothing until it is shown.
    #[test]
    fn a_burst_of_appends_is_read_at_the_pace_and_only_by_the_tab_on_screen() {
        let (st, mut pn) = data_board();
        let mut pace = crate::data::Pace::new();
        data_frame(&mut pn, &mut pace, 1.0);
        let r0 = crate::data::reads();
        // 50 appends 10 ms apart: half a second
        let mut refreshes = 0;
        for i in 0..50 {
            let t = 2.0 + i as f64 * 0.01;
            st.data_append("requests", None, &[json!({"rps": i})]).unwrap();
            pn.data_changed();
            let before = crate::data::reads();
            let due = pn.lv().data_step(&mut pace, t, epoch_now());
            let read = crate::data::reads() > before;
            refreshes += read as usize;
            assert!(read || due.is_some(), "a change that is not drawn is owed a wake-up");
        }
        assert!((4..=6).contains(&refreshes), "{refreshes} reads for 50 appends in half a second");
        // the wake-up comes: the last appends are drawn
        let t = data_frame(&mut pn, &mut pace, 3.0);
        assert!(!pn.lv().data_dirty && t.contains("now ▁▅█"), "{t}");
        // nothing changed: a frame reads nothing (the cache is by length and mtime)
        let r1 = crate::data::reads();
        pn.data_changed();
        data_frame(&mut pn, &mut pace, 4.0);
        assert_eq!(crate::data::reads(), r1);
        assert!(r1 - r0 < 20);
        // the other tab on screen: appends are not read by the board out of view
        pn.switch(1);
        data_frame(&mut pn, &mut pace, 5.0);
        let r2 = crate::data::reads();
        for i in 0..20 {
            st.data_append("requests", None, &[json!({"rps": 500 + i})]).unwrap();
            pn.data_changed();
            data_frame(&mut pn, &mut pace, 6.0 + i as f64);
        }
        assert_eq!(crate::data::reads(), r2, "a tab out of view costs nothing");
        assert!(!pn.lv().binds && pn.tabs[0].data_dirty);
        pn.switch(0);
        let t = data_frame(&mut pn, &mut pace, 60.0);
        assert!(crate::data::reads() > r2 && t.contains("now ▁▅█"), "shown again, it reads: {t}");
        std::fs::remove_dir_all(&st.home).ok();
    }

    /// An overdue dataset marks what is bound to it the way an overdue owner marks its rows, and
    /// puts the failure mark on the board's tab.
    #[test]
    fn an_overdue_dataset_marks_what_is_bound_to_it() {
        let (st, mut pn) = data_board();
        st.data_set("prs", |d| d.expect_every = Some(600.0)).unwrap();
        pn.reload(0.0);
        let mut pace = crate::data::Pace::new();
        let t = data_frame(&mut pn, &mut pace, 1.0);
        assert!(!t.contains("stale") && !pn.infos()[0].failed, "{t}");
        let next = pn.lv().data.next_stale(epoch_now()).unwrap();
        assert!((next - epoch_now() - 600.0).abs() < 5.0, "the pane wakes when it would go stale");
        // an hour on
        let lv = pn.lv();
        lv.show_data(epoch_now() + 3600.0);
        assert_eq!(lv.scene.stale_data.keys().cloned().collect::<std::collections::BTreeSet<_>>(), ["pr", "pr-12", "pr-14"].iter().map(|s| s.to_string()).collect());
        let t = lv.scene.frame(&Input { doc: &lv.doc, state: &lv.state, view: &lv.view, delivered: 0, unread: 0, now: 2.0, wall: epoch_now() + 3600.0, cfg: &lv.cfg, kitty: false, cell: (10, 20), focused_pane: false, mouse: false, project: "proj", w: 90, h: 24, activity: &[] }).grid.to_text();
        assert_eq!(t.matches("◷ stale · last updated 1h ago, expected every 10m").count(), 1, "{t}");
        assert_eq!(t.matches("◷ stale").count(), 3, "the table and its two rows: {t}");
        std::fs::remove_dir_all(&st.home).ok();
    }

    /// What the pane's watches report: a dataset's file is `data` and never a reload; the project
    /// lock every command opens is nothing; an external file counts by its own name only.
    #[test]
    fn the_watch_tells_a_dataset_from_a_document_and_ignores_the_lock() {
        let st = crate::store::tests::temp_store();
        let ext = st.home.join("elsewhere");
        std::fs::create_dir_all(&ext).unwrap();
        crate::data::tests::held(&st, "requests");
        st.data_add("log", crate::data::Dataset { file: Some(ext.join("app.jsonl").display().to_string()), ..Default::default() }).unwrap();
        let ino = Inotify::new(&[st.dir()], &st.dir().join("view"), &st.dir(), &st.data_dir(), &[ext.join("app.jsonl")]);
        // making the watched directories is itself news: read past it
        ino.drain();
        assert_eq!(ino.drain(), Fired::default());
        st.data_append("requests", None, &[json!(1)]).unwrap();
        assert_eq!(ino.drain(), Fired { data: true, ..Default::default() }, "an append: the lock is taken, the file grows, no document changed");
        std::fs::write(ext.join("unrelated.txt"), "x").unwrap();
        assert_eq!(ino.drain(), Fired::default(), "another file beside an external dataset");
        std::fs::write(ext.join("app.jsonl"), "{\"a\": 1}\n").unwrap();
        assert_eq!(ino.drain(), Fired { data: true, ..Default::default() });
        st.emit("x.y", "", "", json!(1), "you", "emit").unwrap();
        assert_eq!(ino.drain(), Fired { other: true, ..Default::default() }, "an event is appended to the log in the project directory");
        st.data_set("requests", |d| d.cap = Some(9)).unwrap();
        assert!(ino.drain().other, "the registry changed: reload, and look at the watches again");
        std::fs::remove_dir_all(&st.home).ok();
    }

    /// An action's key on a feed-owned row appends `<board>.<name>` with the row's id and a
    /// snapshot of it, by the person, via the key. The nearest declaration wins; a key nothing
    /// declares is not an action; the pane's own keys can't be declared at all.
    #[test]
    fn an_action_key_appends_the_event_with_the_row_snapshot() {
        let (st, mut pn) = board();
        let lv = pn.lv();
        lv.scene.focus = Some("job-1".into());
        assert!(lv.action_key('e', 1.0));
        let e = st.events().into_iter().find(|e| e.kind == "ci.retry").expect("the event");
        assert_eq!((e.artifact.as_str(), e.id.as_str(), e.by.as_str(), e.via.as_str(), e.rev), ("ci", "job-1", "you", "key", 1));
        assert_eq!(e.value, json!({"cells": {"name": "build"}, "target": "https://example.com/job/1", "detail": "exit 2"}));
        assert_eq!(st.load("ci").unwrap().rev, 1, "an action changes no content");
        // the row's own action, the table's, and the board's, each from the same row
        lv.scene.focus = Some("job-2".into());
        assert!(lv.action_key('X', 1.0) && lv.action_key('e', 1.0) && lv.action_key('u', 1.0));
        assert!(!lv.action_key('y', 1.0), "nothing declares y");
        // off the table only the board's action is on offer
        lv.scene.focus = Some("note".into());
        assert!(!lv.action_key('e', 1.0) && lv.action_key('u', 1.0));
        let kinds: Vec<(String, String)> = st.events().into_iter().filter(|e| crate::store::is_action(e)).map(|e| (e.kind, e.id)).collect();
        assert_eq!(kinds, [("ci.retry", "job-1"), ("ci.cancel", "job-2"), ("ci.retry", "job-2"), ("ci.refresh", "job-2"), ("ci.refresh", "note")].map(|(k, i)| (k.to_string(), i.to_string())));
        // enter on a row with a target is the open action: an event, and nothing is run here
        lv.fire("job-1", "open", "key", 2.0);
        let e = st.events().into_iter().find(|e| e.kind == "ci.open").unwrap();
        assert_eq!((e.id.as_str(), e.value["target"].as_str()), ("job-1", Some("https://example.com/job/1")));
        // a row with no target has nothing to open: refused, said on the row, nothing logged
        let n = st.events().len();
        lv.fire("job-2", "open", "key", 2.0);
        assert_eq!(st.events().len(), n);
        assert!(lv.scene.sent["job-2"].0.contains("has no target to open") && lv.scene.sent["job-2"].1);
        std::fs::remove_dir_all(&st.home).ok();
    }

    /// The keys an action may not take are the pane's own, read from its binding table; a
    /// write that declares one is refused, as is a repeated key or name and a second `open`.
    #[test]
    fn a_reserved_or_repeated_action_key_is_refused_at_write() {
        let reserved = reserved_keys();
        for c in ['j', 'k', 'd', 'g', 'z', ']', '/', '?', 'q', 'o', 'x', 'r', 'a', '3', '+'] {
            assert!(reserved.contains(&c), "{c} is the pane's");
        }
        for (k, _) in bindings() {
            if !k.starts_with('<') {
                assert!(reserved.contains(&k.chars().next().unwrap()), "{k} is bound and not reserved");
            }
        }
        assert!(!reserved.contains(&'u') && !reserved.contains(&'R') && !reserved.contains(&'e'));
        let st = crate::store::tests::temp_store();
        st.create(crate::doc::Doc::empty("b")).unwrap();
        let add = |actions: serde_json::Value| st.patch("b", &[Op::Replace { block: serde_json::from_value(json!({"type": "row", "id": "r1", "text": "t", "actions": actions})).unwrap(), parent: None }], None);
        let e = add(json!([{"key": "j", "name": "jump"}])).unwrap_err();
        assert!(e.contains("r1: action \"jump\" can't use the key 'j': the pane uses it"), "{e}");
        assert!(add(json!([{"key": "e", "name": "a"}, {"key": "e", "name": "b"}])).unwrap_err().contains("share the key 'e'"));
        assert!(add(json!([{"key": "e", "name": "a"}, {"key": "f", "name": "a"}])).unwrap_err().contains("two actions are named \"a\""));
        assert!(add(json!([{"key": "ee", "name": "a"}])).unwrap_err().contains("a key of one character"));
        assert!(add(json!([{"key": "e"}])).unwrap_err().contains("needs a name"));
        assert!(add(json!([{"key": "e", "name": "open"}])).unwrap_err().contains("open is the action every block with a target has"));
        assert!(add(json!([{"key": "e", "name": "a", "lable": "x"}])).unwrap_err().contains("did you mean label?"));
        assert!(add(json!("e")).unwrap_err().contains("must be a list"));
        assert_eq!(st.load("b").unwrap().rev, 1, "nothing was written");
        add(json!([{"key": "e", "name": "edit"}, {"key": "R", "name": "rerun", "label": "run again"}])).unwrap();
        let acts = st.load("b").unwrap().actions_for(Some("r1"));
        assert_eq!(acts.iter().map(|a| (a.key, a.name.as_str(), a.label.as_str())).collect::<Vec<_>>(), [('e', "edit", "edit"), ('R', "rerun", "run again")]);
        // a todo takes no actions of its own; the document does, under the same rule
        let todo = Op::Add { parent: None, block: serde_json::from_value(json!({"type": "todo", "id": "t1", "text": "t", "actions": []})).unwrap(), before: None, after: None };
        assert!(st.patch("b", &[todo], None).unwrap_err().contains("unknown field \"actions\" on todo"));
        let mut d = st.load("b").unwrap();
        d.actions = json!([{"key": "q", "name": "quit"}]);
        assert!(st.replace(d, None).unwrap_err().contains("the document: action \"quit\" can't use the key 'q'"));
        std::fs::remove_dir_all(&st.home).ok();
    }

    /// The row says what became of the action, from the registry and the cursors: `sent`, then
    /// `nothing handles <name>` when no handler is registered for it; with a handler, the note
    /// goes when its cursor passes the event, and says so when it never does.
    #[test]
    fn the_row_says_sent_then_who_took_it_or_that_nobody_did() {
        let (st, mut pn) = board();
        let lv = pn.lv();
        lv.scene.focus = Some("job-1".into());
        assert!(lv.action_key('e', 1.0));
        let t0 = epoch_now();
        assert_eq!(lv.scene.sent["job-1"], ("→ sent".to_string(), false));
        assert!(frame(lv, 100).0.contains("→ sent"), "{}", frame(lv, 100).0);
        // nobody is registered for ci.retry, and nothing took it
        assert!(lv.sent_notes(t0 + SENT_GRACE + 0.5));
        assert_eq!(lv.scene.sent["job-1"], ("nothing handles ci.retry · margin doctor".to_string(), true));
        let text = frame(lv, 100).0;
        assert!(text.lines().any(|l| l.contains("build") && l.contains("nothing handles ci.retry")), "on its row:
{text}");
        assert!(!lv.sent_notes(t0 + 30.0) && lv.scene.sent.is_empty(), "the notice goes after a while");
        // an outside tool following the log with its own cursor took it: no notice at all
        assert!(lv.action_key('e', 1.0));
        let seq = st.last_seq();
        st.set_cursor("mytool", seq);
        assert!(!lv.sent_notes(epoch_now() + SENT_GRACE + 0.5) && lv.scene.sent.is_empty());
        // a handler is registered: sent until its cursor passes the event
        handler(&st, "retry", "ci.retry");
        assert!(lv.action_key('e', 1.0));
        let (seq, t0) = (st.last_seq(), epoch_now());
        assert!(lv.sent_notes(t0 + 3.0));
        assert_eq!(lv.scene.sent["job-1"].0, "→ sent", "a handler exists: it is given time");
        assert!(lv.sent_notes(t0 + SENT_LATE + 1.0));
        assert_eq!(lv.scene.sent["job-1"], ("nothing has picked this up · margin daemon status".to_string(), true));
        st.set_cursor("handler:retry", seq);
        assert!(!lv.sent_notes(t0 + SENT_LATE + 2.0) && lv.scene.sent.is_empty(), "its handler finished with it");
        // a paused handler handles nothing
        crate::handler::edit_registry(&st, |r| r.set_paused("retry", true)).unwrap();
        assert!(lv.action_key('e', 1.0));
        lv.sent_notes(epoch_now() + SENT_GRACE + 0.5);
        assert!(lv.scene.sent["job-1"].0.starts_with("nothing handles ci.retry"));
        std::fs::remove_dir_all(&st.home).ok();
    }

    /// The sent mark goes when the row's content changes: whoever handled it wrote the row.
    #[test]
    fn the_sent_mark_clears_when_the_row_changes() {
        let (st, mut pn) = board();
        let lv = pn.lv();
        lv.scene.focus = Some("job-1".into());
        assert!(lv.action_key('e', 1.0));
        assert!(lv.scene.sent.contains_key("job-1"));
        // another row changing is not this row changing
        let w = crate::store::Writer::Named { by: "handler:ci".into(), owns: "job".into() };
        st.handler_commit("ci", &[vec![Op::Set { id: "job-2".into(), field: "detail".into(), value: json!("x") }]], &w, &[], None);
        lv.reload(2.0);
        assert!(lv.sent_notes(epoch_now()) && lv.scene.sent.contains_key("job-1"));
        st.handler_commit("ci", &[vec![Op::Set { id: "job-1".into(), field: "detail".into(), value: json!("retrying") }]], &w, &[], None);
        lv.reload(3.0);
        assert!(!lv.sent_notes(epoch_now()) && lv.scene.sent.is_empty());
        assert!(!frame(lv, 100).0.contains("→ sent"));
        // a row that is removed takes its note with it
        assert!(lv.action_key('e', 1.0));
        st.handler_commit("ci", &[vec![Op::Rm { id: "job-1".into() }]], &w, &[], None);
        lv.reload(4.0);
        assert!(!lv.sent_notes(epoch_now()));
        std::fs::remove_dir_all(&st.home).ok();
    }

    /// The hint row leads with the focused block's actions, the detail panel lists them, and a
    /// click on either fires the action; a block with a target says `⏎ open`.
    #[test]
    fn actions_show_in_the_hint_row_and_the_detail_panel_and_take_clicks() {
        let (st, mut pn) = board();
        let lv = pn.lv();
        lv.scene.focus = Some("job-1".into());
        let (text, g) = frame(lv, 100);
        let help = text.lines().find(|l| l.contains("j/k move")).expect("the hint row");
        assert!(help.trim_start().starts_with("e retry the job • u refresh all • j/k move"), "{help}");
        assert!(help.contains("⏎ open"), "{help}");
        let (x, y, _, tok) = lv.scene.hits.iter().find(|h| h.3 == "mg:act/job-1/retry").cloned().expect("the hint is a button");
        assert_eq!(tok, "mg:act/job-1/retry");
        lv.click(&g, x + 1, y, 5.0);
        assert_eq!(st.events().into_iter().filter(|e| e.kind == "ci.retry").map(|e| (e.id, e.via)).collect::<Vec<_>>(), [("job-1".to_string(), "mouse".to_string())]);
        // another row: its own action too
        lv.scene.focus = Some("job-2".into());
        let text = frame(lv, 100).0;
        assert!(text.lines().any(|l| l.trim_start().starts_with("X cancel • e retry the job • u refresh all")), "{text}");
        // the detail panel lists them under the fields, each a button
        lv.view.detail = true;
        lv.scene.focus = Some("job-1".into());
        let (text, g) = frame(lv, 120);
        assert!(text.contains("⏎ open the target") && text.lines().any(|l| l.trim_end().ends_with("e retry the job")) && text.lines().any(|l| l.trim_end().ends_with("u refresh all")), "{text}");
        let (x, y) = lv.scene.hits.iter().filter(|h| h.3 == "mg:act/job-1/refresh").map(|h| (h.0, h.1)).max_by_key(|h| h.0).expect("the panel line is a button");
        lv.click(&g, x, y, 6.0);
        assert!(st.events().iter().any(|e| e.kind == "ci.refresh" && e.id == "job-1" && e.via == "mouse"));
        // a click on the line that opens the target is the open action, not an opener
        let (x, y) = lv.scene.hits.iter().find(|h| h.3 == "mg:open/job-1").map(|h| (h.0, h.1)).unwrap();
        lv.click(&g, x, y, 7.0);
        assert!(st.events().iter().any(|e| e.kind == "ci.open" && e.id == "job-1" && e.via == "mouse"));
        std::fs::remove_dir_all(&st.home).ok();
    }

    fn details(st: &Store) -> Vec<String> {
        st.events().into_iter().filter(|e| e.kind == "ci.detail").map(|e| e.id).collect()
    }

    /// The detail panel asks about the block where the focus rests: a quick run of `j` appends
    /// one `<board>.detail`, for the last block, carrying the snapshot an action carries. A
    /// block already asked about is not asked again until the panel was closed.
    #[test]
    fn a_quick_run_of_j_asks_about_the_block_where_the_focus_rests() {
        let (st, mut pn) = board();
        let lv = pn.lv();
        frame(lv, 100);
        lv.scene.focus = Some("job".into());
        assert_eq!(lv.detail_step(1.0, 1.0), None, "the panel is closed: nothing is asked");
        lv.view.detail = true;
        assert_eq!(lv.detail_step(1.0, 1.0), Some(1.0 + DETAIL_SETTLE));
        // j, j, 100 ms apart: each press starts the wait again
        lv.scene.move_focus(1);
        assert_eq!(lv.scene.focus.as_deref(), Some("job-1"));
        assert_eq!(lv.detail_step(1.1, 1.1), Some(1.1 + DETAIL_SETTLE));
        lv.scene.move_focus(1);
        assert_eq!(lv.scene.focus.as_deref(), Some("job-2"));
        assert_eq!(lv.detail_step(1.2, 1.2), Some(1.2 + DETAIL_SETTLE));
        assert_eq!(lv.detail_step(1.35, 1.35), Some(1.2 + DETAIL_SETTLE));
        assert!(details(&st).is_empty(), "nothing while the focus moves");
        assert_eq!(lv.detail_step(1.25 + DETAIL_SETTLE, 1.5), None);
        assert_eq!(lv.detail_step(5.0, 5.0), None);
        assert_eq!(details(&st), ["job-2"], "one event, for the block where the focus rests");
        let e = st.events().into_iter().find(|e| e.kind == "ci.detail").unwrap();
        assert_eq!((e.artifact.as_str(), e.by.as_str(), e.via.as_str(), &e.value), ("ci", "you", "pane", &json!({"cells": {"name": "test"}})));
        assert!(!crate::store::is_decision(&e) && !crate::store::is_action(&e));
        // back to a block: asked about once it rests there
        lv.scene.move_focus(-1);
        lv.detail_step(6.0, 6.0);
        lv.detail_step(6.05 + DETAIL_SETTLE, 6.3);
        assert_eq!(details(&st), ["job-2", "job-1"]);
        assert_eq!(st.events().into_iter().find(|e| e.id == "job-1").unwrap().value, json!({"cells": {"name": "build"}, "target": "https://example.com/job/1", "detail": "exit 2"}));
        // closed and opened again on the same block: asked again
        lv.view.detail = false;
        lv.detail_step(7.0, 7.0);
        lv.view.detail = true;
        lv.detail_step(8.0, 8.0);
        lv.detail_step(8.05 + DETAIL_SETTLE, 8.3);
        assert_eq!(details(&st), ["job-2", "job-1", "job-1"]);
        // nothing registered for the name: the panel says nothing extra, and no daemon is wanted
        lv.asked_note(9.0);
        assert!(lv.asked.is_none() && !frame(lv, 120).0.contains(crate::detail::ASKED));
        assert!(!st.wants_daemon("ci.detail") && !st.wants_daemon("ci.shown"));
        assert!(crate::claude::hook::unread(&st, "sess").is_empty() && crate::claude::hook::actions_for_session(&st, "ci", 0).is_empty());
        std::fs::remove_dir_all(&st.home).ok();
    }

    /// A handler registered for `<board>.detail` is given the event and one registered for an
    /// action is not; while the first exists the panel says "asked for more", then "no answer
    /// yet", and nothing once the block changed.
    #[test]
    fn a_handler_for_detail_gets_it_and_the_panel_says_it_asked() {
        let (st, mut pn) = board();
        handler(&st, "more", "ci.detail");
        handler(&st, "approve", "ci.approve");
        handler(&st, "all", "*");
        let reg = crate::handler::Registry::load(&st.dir());
        let lv = pn.lv();
        frame(lv, 120);
        lv.view.detail = true;
        lv.scene.focus = Some("job-1".into());
        let wall = epoch_now();
        lv.detail_step(1.0, wall);
        lv.detail_step(1.05 + DETAIL_SETTLE, wall);
        let waiting = |name: &str| crate::handler::pending(&st, reg.get(name).unwrap()).into_iter().map(|e| (e.kind, e.id)).collect::<Vec<_>>();
        assert_eq!(waiting("more"), [("ci.detail".to_string(), "job-1".to_string())]);
        assert!(waiting("approve").is_empty() && waiting("all").is_empty(), "a view event goes only to a handler that names it");
        assert!(st.wants_daemon("ci.detail") && !st.wants_daemon("ci.shown") && !st.wants_daemon("seen") && st.wants_daemon("ci.retry"));
        // the panel: one quiet line under the block's content, above the hint that opens it
        assert_eq!(lv.asked_note(wall + 1.0), Some(wall + ASKED_SECS));
        let text = frame(lv, 120).0;
        let at = |t: &str, what: &str| t.lines().position(|l| l.contains(what));
        assert!(at(&text, "exit 2") < at(&text, "asked for more") && at(&text, "asked for more") < at(&text, "open the target"), "{text}");
        // another block in the panel: the line is the block's, not the panel's
        lv.scene.focus = Some("note".into());
        lv.asked_note(wall + 2.0);
        assert!(!frame(lv, 120).0.contains("asked for more"));
        lv.scene.focus = Some("job-1".into());
        assert_eq!(lv.asked_note(wall + ASKED_SECS + 1.0), None);
        let text = frame(lv, 120).0;
        assert!(text.contains("no answer yet") && !text.contains("asked for more"), "{text}");
        // the answer: whoever handled it wrote the block
        let w = crate::store::Writer::Named { by: "handler:ci".into(), owns: "job".into() };
        st.handler_commit("ci", &[vec![Op::Set { id: "job-1".into(), field: "detail".into(), value: json!("exit 2: the linker ran out of memory") }]], &w, &[], None);
        lv.reload(3.0);
        lv.asked_note(wall + ASKED_SECS + 2.0);
        let text = frame(lv, 120).0;
        assert!(lv.asked.is_none() && text.contains("ran out of memory") && !text.contains("no answer yet") && !text.contains("asked for more"), "{text}");
        // a paused handler will not be run: nothing is said
        crate::handler::edit_registry(&st, |r| r.set_paused("more", true)).unwrap();
        lv.scene.focus = Some("job-2".into());
        lv.detail_step(4.0, wall);
        lv.detail_step(4.05 + DETAIL_SETTLE, wall);
        assert!(lv.asked.is_none() && !st.wants_daemon("ci.detail"));
        crate::detail::set_note(None);
        std::fs::remove_dir_all(&st.home).ok();
    }

    /// A board no Claude session keeps says nothing about Claude, whatever another session in
    /// the project is doing; one that is linked does.
    #[test]
    fn claude_is_named_only_on_a_board_a_session_keeps() {
        crate::claude::install();
        let (st, mut pn) = board();
        st.set_activity(&crate::store::Activity { session: "elsewhere".into(), state: "running".into(), text: "Build the thing".into(), since: crate::util::now_rfc3339(), ..Default::default() });
        let lv = pn.lv();
        lv.reload(0.5);
        lv.scene.help_open = true;
        let text = frame(lv, 140).0;
        assert!(!text.contains("Claude") && !text.contains("Build the thing") && lv.activity.is_empty(), "no session is linked:
{text}");
        crate::claude::link::set(&st, "sess-1", "ci", "explicit");
        lv.reload(1.0);
        let text = frame(lv, 200).0;
        assert!(text.contains("Claude is current"), "{:?} {text}", crate::claude::link::all(&st).len());
        assert!(text.contains("stop Claude"), "{text}");
        std::fs::remove_dir_all(&st.home).ok();
    }
}

#[cfg(test)]
mod sel_tests {
    use super::*;
    #[test]
    fn selection_reads_in_order_without_borders() {
        let mut g = Grid::new(12, 3);
        // column 0 is the gutter (change bars): text starts at 1, the last column is margin
        g.put(0, 0, ".ab cd      ", vellum::style::Style::default());
        g.put(0, 1, ".ef         ", vellum::style::Style::default());
        let rows = sel_rows((3, 1), (2, 0), 12);
        assert_eq!(rows, vec![(0, 2, 10), (1, 1, 3)]);
        assert_eq!(sel_text(&g, &rows), "b cd\nef");
    }
}

#[test]
fn quotes_lose_padding_and_ellipses() {
    assert_eq!(clean_quote("  Reviews, triage and the not-doing list (archive)        …"), "Reviews, triage and the not-doing list (archive)");
    assert_eq!(clean_quote("first line\n   second"), "first line second");
}

#[cfg(test)]
mod img_tests {
    use super::*;
    fn at(y: u16) -> Vec<Placement> {
        vec![Placement { key: "im".into(), x: 2, y, cols: 20, rows: 6 }]
    }

    #[test]
    fn moving_image_waits_then_sends_once() {
        let mut s = ImgSync::default();
        let p = s.step(&at(10), 0.0, true);
        assert!(p.send.is_empty() && p.pending == at(10) && p.wake == Some(IMG_SETTLE));
        let p = s.step(&at(10), IMG_SETTLE, false);
        assert_eq!(p.send, at(10));
        assert!(p.pending.is_empty());
        // held still: nothing more goes out
        let p = s.step(&at(10), 1.0, false);
        assert!(p.send.is_empty() && p.delete.is_empty() && p.pending.is_empty());
        // scrolling: taken down at once, pending while it moves
        let p = s.step(&at(9), 1.1, false);
        assert_eq!(p.delete, at(10));
        assert_eq!(p.pending, at(9));
    }

    #[test]
    fn scrolled_away_while_pending_leaves_no_placeholder() {
        let mut s = ImgSync::default();
        s.step(&at(10), 0.0, false);
        // gone from view before it was ever sent: no placement, no placeholder, no wake
        let p = s.step(&[], 0.05, false);
        assert!(p.pending.is_empty() && p.send.is_empty() && p.wake.is_none(), "{p:?}");
        let p = s.step(&[], 1.0, false);
        assert!(p.pending.is_empty(), "{p:?}");
    }

    #[test]
    fn repaint_in_place_resends_now() {
        let mut s = ImgSync::default();
        s.step(&at(4), 0.0, false);
        s.step(&at(4), 0.2, false);
        let p = s.step(&at(4), 5.0, true);
        assert_eq!((p.delete.clone(), p.send.clone()), (at(4), at(4)));
        assert!(p.pending.is_empty());
    }
}
