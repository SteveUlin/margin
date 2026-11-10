//! `margin view`: the live pane. Sleeps in `poll()` on stdin, SIGWINCH and an inotify fd on the
//! artifact's files; wakes for input, a file change, or the frame clock when something animates.
//! With nothing live it never wakes on its own (0 fps).

use crate::config::Config;
use crate::host::{Host, Selection};
use crate::scene::{Input, Placement, Scene};
use crate::select;
use crate::store::{is_passive, Store, View};
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
}

impl Inotify {
    fn new(paths: &[std::path::PathBuf], view_dir: &std::path::Path) -> Inotify {
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        let add = |p: &std::path::Path| -> i32 {
            let _ = std::fs::create_dir_all(p);
            match CString::new(p.to_string_lossy().as_bytes()) {
                Ok(c) => unsafe { libc::inotify_add_watch(fd, c.as_ptr(), libc::IN_CLOSE_WRITE | libc::IN_MOVED_TO | libc::IN_MODIFY | libc::IN_CREATE) },
                Err(_) => -1,
            }
        };
        for p in paths {
            add(p);
        }
        let view_wd = add(view_dir);
        Inotify { fd, view_wd }
    }
    /// Read what fired: (the view directory did, anything else did).
    fn drain(&self) -> (bool, bool) {
        let (mut views, mut other) = (false, false);
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
                if wd == self.view_wd {
                    views = true;
                } else {
                    other = true;
                }
                at += 16 + len;
            }
        }
        (views, other)
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
    doc: crate::doc::Doc,
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
}

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
    use Act::*;
    vellum::keymap::Keymap::new(&[
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
    ])
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
        let _ = std::fs::create_dir_all(crate::tail::dir(st, art));
        Ok(Live {
            last_seq: st.last_seq(),
            st: store_like(st),
            art: art.to_string(),
            cfg: cfg.clone(),
            pane: pane.to_string(),
            view,
            doc,
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

    fn reload(&mut self, now: f64) {
        if let Ok(d) = self.st.load(&self.art) {
            self.doc = d;
        }
        self.state = self.st.state(&self.art);
        self.activity = crate::link::activity_for(&self.st, &self.art);
        self.scene.recap = crate::recap::for_pane(&self.st, &self.art);
        self.scene.stopped = self.st.stopped();
        self.scene.agents = crate::agents::for_pane(&self.st, &self.art);
        self.toasts = crate::toast::load(&self.st, &self.art);
        // each tail component's run: its state and its last 30 lines (tail.rs)
        self.scene.tails = crate::tail::for_doc(&self.st, &self.art, &self.doc);
        self.delivered = self.st.delivered_max();
        let evs = self.st.events();
        self.unread = evs
            .iter()
            .filter(|e| e.artifact == self.art && crate::who::is_human(&e.by) && !is_passive(&e.kind) && e.seq > self.delivered)
            .count();
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
                "link" | "status" => {
                    let t = select::Token { verb: "open".into(), artifact: Some(self.art.clone()), id: focus.clone().unwrap(), value: None };
                    let _ = select::apply_in(&self.st, &self.art, &t, "key", &self.cfg);
                    self.scene.flash(&t.id, now);
                }
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
                _ if fb.as_ref().map(|b| b.opt("target").is_some()).unwrap_or(false) => {
                    let t = select::Token { verb: "open".into(), artifact: Some(self.art.clone()), id: focus.clone().unwrap(), value: None };
                    let _ = select::apply_in(&self.st, &self.art, &t, "key", &self.cfg);
                    self.scene.flash(&t.id, now);
                }
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

    /// The stop signal: Claude's next tool call is refused until sulin sends a message.
    fn stop(&mut self, now: f64) {
        if self.st.stop(&self.art, "pane").is_ok() {
            self.reload(now);
        }
    }

    fn set_review(&mut self, on: bool) {
        self.review = on;
        // said in the store too, where the rewake watcher reads it (wake.rs)
        crate::wake::review_mark(&self.st, &self.pane, &self.art, on);
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
        if let Some(id) = button.as_deref().and_then(|b| b.strip_prefix("mg:tail/")) {
            // a tail's header: open it to 30 lines, or close it
            if !self.scene.tails_open.remove(id) {
                self.scene.tails_open.insert(id.to_string());
            }
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
        if let Some(t) = button.or_else(|| token_at(grid, x, y)).as_deref().and_then(select::parse_token) {
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
        let mut pn = Pane { tabs, active };
        pn.store_set();
        Ok(pn)
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
                    failed: failed || l.toasts.iter().any(|t| t.level == "error"),
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

    /// The directories whose changes mean "reload": each tab's artifact and tails, and the
    /// project's shared ones.
    fn watch(&self) -> Vec<std::path::PathBuf> {
        let st = &self.tabs[0].st;
        let mut w = vec![st.dir(), st.dir().join("delivered"), st.activity_dir(), st.dir().join("toasts")];
        for l in &self.tabs {
            w.push(st.art(&l.art));
            w.push(crate::tail::dir(st, &l.art));
        }
        w
    }

    fn view_dir(&self) -> std::path::PathBuf {
        self.tabs[0].st.art(&self.tabs[0].art).join("view")
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
    crate::wake::review_reset(&st);
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
    let watch = |pn: &Pane| {
        let mut w = pn.watch();
        w.extend(exe_dir.clone());
        Inotify::new(&w, &pn.view_dir())
    };
    let mut ino = watch(&pn);
    let now = || clock.elapsed().as_secs_f64();
    pn.reload(0.0);
    for l in &pn.tabs {
        let _ = l.st.log(&l.art, "", "view", json!({"pane": l.pane, "kitty": probe_kitty}), "pane");
    }
    let mut prev: Option<Grid> = None;
    let mut imgs = ImgSync::default();
    // toasts: is the pane on screen (their clocks start then), checked at most once a second
    let (mut toast_vis, mut toast_vis_t) = (false, -10.0f64);
    // the ● on this pane's tab while Claude waits on sulin out of view; Drop takes it down
    let mut attn = crate::attention::Attention::default();
    // r5: when the pane last checked whether a wake-up is due (what one covered is in wake::woke)
    let mut wake_check = -10.0f64;
    // what that check last covered, and whether actions were still waiting out their quiet period
    let (mut wake_look, mut wake_owed) = ((0u64, String::new()), false);
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
                ino = watch(&pn);
                prev = None;
            }
        };
    }
    loop {
        let t = now();
        let wall = epoch_now();
        // the tab bar's badges, from every tab; then this iteration is the tab on screen's
        let infos = pn.infos();
        let active = pn.active;
        let lv = &mut pn.tabs[active];
        lv.scene.tabs = infos;
        lv.scene.tab = active;
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
        // the tab mark and the phone push: a wake only for a look that is owed (attention.rs);
        // an idle Claude with nothing else pending asks for none. Across every tab's sessions
        let acts = pn.activity();
        if let Some(at) = attn.step(&acts, t, &cfg, || host.is_visible()) {
            wake.at(at);
        }
        // wake an idle Claude once sulin is done in the pane; at most one look a second
        let idle = crate::attention::need(&acts).filter(|a| cfg.wake != "off" && a.state == "idle").map(|a| a.session.clone());
        let last_seq = pn.tabs.iter().map(|l| l.last_seq).max().unwrap_or(0);
        let look = (last_seq, idle.clone().unwrap_or_default());
        if t - wake_check < 1.0 {
            // an action (or the turn's end) landed inside that second, or actions are waiting
            // out their quiet period: come back for the look. Nothing new and nothing waiting
            // asks for nothing, so an idle pane sleeps
            if idle.is_some() && (wake_owed || look != wake_look) {
                wake.at(wake_check + 1.0);
            }
        } else {
            wake_check = t;
            wake_look = look;
            wake_owed = false;
            if let Some(sess) = idle {
                let wall_now = epoch_now();
                let st = &pn.tabs[0].st;
                let all = st.events();
                let (net, _) = crate::hook::pending(&all, st.delivered(&sess), wall_now, 0.0);
                // a wake-up covers the batch's newest action. Not the cursor: that moves past
                // actions that net to nothing, and a tick taken back sent a second wake-up
                let top = crate::wake::top(&net);
                // a store that started over hands out low seqs again: an old mark covers nothing
                let mut woke = crate::wake::woke(&sess);
                if woke > all.last().map(|e| e.seq).unwrap_or(0) {
                    crate::wake::forget(&sess);
                    woke = 0;
                }
                let mut owed = top > woke;
                // review mode or a half-answered question set in any tab holds the wake-up back
                let review = pn.tabs.iter().any(|l| l.review);
                let open = pn.tabs.iter().any(|l| crate::wake::open_sets(&l.doc, &l.state, &net));
                if owed && crate::wake::done(&net, wall_now, review, open) {
                    let send = || crate::wake::send(&sess, &crate::wake::message(&net));
                    match crate::wake::pane_wake(&sess, top, &cfg.wake, wall_now, send) {
                        // the rewake hook's watcher will wake Claude; keep looking in case it dies
                        crate::wake::Pane::Watcher => {}
                        crate::wake::Pane::Sent(Err(e)) => {
                            select::log_line(&format!("wake {sess}: {e}"));
                            owed = false; // claimed: no retry every second
                        }
                        _ => owed = false,
                    }
                }
                if owed {
                    wake_owed = true;
                    wake.at(t + 1.0);
                }
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
        let (bytes, resized, ready) = term.poll(&[ino.fd], timeout).map_err(|e| e.to_string())?;
        let t = now();
        if !ready.is_empty() {
            let (views, other) = ino.drain();
            if other {
                pn.reload(t);
            }
            // the view directory: this pane's own saves, or `margin tab` changing the set
            if views {
                follow!();
            }
        }
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
    for l in &mut pn.tabs {
        l.save_view();
    }
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
