//! The scene: turns (doc, state, view) into a frame, and keeps everything that animates between
//! frames. Shared by the live pane (`margin view`) and the headless renderer (`margin render`).
//!
//! Each frame: flatten the tree into items (sections folded per the pane's view), draw each item
//! into its own grid at natural height, animate that height (inserts ease open, removals and folds
//! collapse, a card that resolves shrinks on a spring), stack the items, offset any that moved by
//! their FLIP spring, scroll by the anchor id, and composite into the pane with the frame, gutter
//! change bars, help line and status bar. Then ask the clock for the next frame only if something
//! still moves.

use crate::components::{self, icon, Ctx, ItalicIf};
use crate::config::Config;
use crate::doc::{walk, Block, Doc};
use crate::store::{State, View};
use crate::util::{ago, parse_rfc3339};
use std::collections::{HashMap, HashSet};
use vellum::anim::{Easing, Motion, Spring, SpringCfg, Tween, Wake};
use vellum::color::{Rgb, EVERFOREST, LIPGLOSS_STATUS};
use vellum::grid::Grid;
use vellum::image::Image;
use vellum::layout::Rect;
use vellum::parts::{self, Ring, StatusBar};
use vellum::retained::{Retained, Timings};
use vellum::style::Style;
use vellum::text::{truncate, width};

pub struct Input<'a> {
    pub doc: &'a Doc,
    pub state: &'a State,
    pub view: &'a View,
    pub delivered: u64,
    /// sulin's events Claude hasn't been handed yet, for this artifact.
    pub unread: usize,
    pub now: f64,
    pub wall: f64,
    pub cfg: &'a Config,
    pub kitty: bool,
    pub cell: (u16, u16),
    pub focused_pane: bool,
    pub mouse: bool,
    pub project: &'a str,
    pub w: u16,
    pub h: u16,
    /// What Claude sessions in this project are doing, newest first (the tool hooks write it).
    pub activity: &'a [crate::store::Activity],
}

#[derive(Clone, Debug, PartialEq)]
pub struct Placement {
    pub key: String,
    pub x: u16,
    pub y: u16,
    pub cols: u16,
    pub rows: u16,
}

pub struct Frame {
    pub grid: Grid,
    pub images: Vec<Placement>,
    pub wake: Wake,
}

struct Ghost {
    block: Block,
    depth: usize,
    after: Option<String>,
}

#[derive(Clone)]
enum Kind {
    /// `level` counts bullet ancestors (sub-bullets indent by it); `gap` puts a blank row above a
    /// top-level bullet that has sub-bullets, so groups read apart.
    Block { depth: usize, folded: bool, level: usize, gap: bool },
    Ghost(usize),
    OrphanHead(usize),
    /// Claude Code's latest session recap, the pane's first row (recap.rs).
    Recap,
    /// The work overview's subagents (agents.rs): a header, a fan-out's progress line, a row each.
    AgentsHead,
    AgentGroup(usize),
    AgentRow(usize, usize),
    Orphan(String),
}

#[derive(Clone)]
struct Item {
    id: String,
    kind: Kind,
    focusable: bool,
}

/// The live strip's end caps: Powerline's lower-right and lower-left triangles, so the band's
/// bottom corners slant in.
pub(crate) const STRIP_CAP_L: &str = "\u{e0ba}";
pub(crate) const STRIP_CAP_R: &str = "\u{e0b8}";

/// Seconds a change must go unseen before the header counts it: longer than the pane takes to
/// mark a change seen while it is on screen (1.5 s, plus a 1 s visibility poll).
const UNSEEN_TAG: f64 = 3.0;

/// The child of a `tabs` component on show: the one the view names, else the first.
pub fn subtab_active<'a>(b: &'a Block, view: &View) -> Option<&'a Block> {
    view.subtabs.get(&b.id).and_then(|id| b.children.iter().find(|c| c.id == *id)).or(b.children.first())
}

/// Put block `id` on show: every `tabs` component above it switches to the section that holds
/// it (the first section is the default, so it is never named). True when a tab changed. For a
/// focus that arrives from outside the visible list: `margin render --focus`, a view file, a
/// click on a line of the pinned head.
pub fn show_in_tabs(doc: &Doc, view: &mut View, id: &str) -> bool {
    let mut changed = false;
    let mut at = id.to_string();
    while let Some(p) = doc.parent_of(&at) {
        if p.kind == "tabs" && subtab_active(p, view).map(|c| c.id != at).unwrap_or(false) {
            if p.children.first().map(|c| c.id == at).unwrap_or(false) {
                view.subtabs.remove(&p.id);
            } else {
                view.subtabs.insert(p.id.clone(), at.clone());
            }
            changed = true;
        }
        at = p.id.clone();
    }
    changed
}

/// The status bar's second field: the project the artifact belongs to, as `--project` takes it.
/// A long one (a directory's slug) keeps its end. It used to be drawn as a path, `~/<last
/// word>`, which for `--project jobs` named a directory `~/jobs` that doesn't exist.
fn project_label(project: &str) -> String {
    const MAX: usize = 24;
    let n = project.chars().count();
    if n <= MAX {
        return project.to_string();
    }
    format!("…{}", project.chars().skip(n - (MAX - 1)).collect::<String>())
}

/// What a tab of a `tabs` component is called: its section's title.
fn subtab_label(c: &Block) -> &str {
    [c.s("title"), c.s("text")].into_iter().find(|s| !s.trim().is_empty()).unwrap_or(&c.id)
}

/// Rows the artifact's reminders (`Doc::hints`) may take above the key hints.
const HINT_ROWS: usize = 4;

/// What a run of the reminders row is.
#[derive(Clone, Copy, PartialEq, Debug)]
enum HintRun {
    Key,
    Text,
    Sep,
}

/// The reminders as rows of runs `w` columns wide. Each reminder is `key  what it does` (two
/// spaces: the key draws as a key) or plain text. They follow each other on a row with a dot
/// between, a reminder that doesn't fit the rest of a row starts the next, and one longer than a
/// row wraps: authored text is never cut.
fn hint_flow(hints: &[String], w: usize) -> Vec<Vec<(String, HintRun)>> {
    let w = w.max(8);
    let mut rows: Vec<Vec<(String, HintRun)>> = Vec::new();
    let mut cur = 0usize;
    for h in hints {
        let h = h.trim();
        if h.is_empty() {
            continue;
        }
        let (key, text) = match h.find("  ") {
            Some(i) => (&h[..i], h[i..].trim_start()),
            None => ("", h),
        };
        let kw = if key.is_empty() { 0 } else { width(key) + 1 };
        let need = kw + width(text);
        if !rows.is_empty() && cur > 0 && cur + 3 + need <= w {
            rows.last_mut().unwrap().push((" • ".into(), HintRun::Sep));
            cur += 3;
        } else {
            rows.push(Vec::new());
            cur = 0;
        }
        if !key.is_empty() {
            rows.last_mut().unwrap().push((format!("{key} "), HintRun::Key));
        }
        if need <= w {
            rows.last_mut().unwrap().push((text.to_string(), HintRun::Text));
            cur += need;
            continue;
        }
        // longer than a row: wrapped, the rows after the first under its text
        for (i, line) in vellum::text::wrap(text, w.saturating_sub(kw).max(1)).into_iter().enumerate() {
            if i > 0 {
                rows.push(vec![(" ".repeat(kw), HintRun::Text)]);
            }
            cur = kw + width(line.trim_start());
            rows.last_mut().unwrap().push((line.trim_start().to_string(), HintRun::Text));
        }
    }
    rows
}

/// Rows one line of the pinned head may wrap to before it is cut.
const HEAD_WRAP: usize = 3;

/// Below this width the pane is a strip: the title bar and the head only, wrapped (glance mode).
const GLANCE_W: u16 = 40;

#[derive(Clone, Copy, PartialEq)]
enum HeadKind {
    Purpose,
    Running,
    Needs,
}

/// One line of the pinned head: where the work stands, said without scrolling.
struct HeadLine {
    kind: HeadKind,
    text: String,
    /// Dim, at the right edge: the running step's "+2" and elapsed time.
    right: String,
    /// The block a click on the line goes to (empty: the line is the document's own).
    target: String,
    /// Needs: the ids listed after `text`, each its own click target.
    ids: Vec<String>,
}

/// The head's lines: what the pane is for (the document's `purpose`; a pane written before
/// there was one falls back to the first text in its section `goal` or `overview`), then two
/// derived from what the doc already holds so Claude maintains nothing new: the step that is
/// `doing`, and what waits on sulin (open questions, diffs without a verdict, his own open
/// steps). A line with nothing to say is left out.
fn head_lines(inp: &Input) -> Vec<HeadLine> {
    let plain = |t: &str| components::markup(t).into_iter().map(|(x, _)| x).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = Vec::new();
    if !inp.doc.purpose.trim().is_empty() {
        out.push(HeadLine { kind: HeadKind::Purpose, text: plain(&inp.doc.purpose), right: String::new(), target: String::new(), ids: Vec::new() });
    } else {
        let mut goal: Option<&Block> = None;
        walk(&inp.doc.blocks, &mut |b, _, _| {
            if goal.is_none() && b.kind == "section" && matches!(b.id.as_str(), "goal" | "overview") {
                walk(&b.children, &mut |c, _, _| {
                    if goal.is_none() && !c.s("text").trim().is_empty() {
                        goal = Some(c);
                    }
                });
            }
        });
        if let Some(b) = goal {
            out.push(HeadLine { kind: HeadKind::Purpose, text: plain(b.s("text")), right: String::new(), target: b.id.clone(), ids: Vec::new() });
        }
    }
    let mut doing: Vec<&Block> = Vec::new();
    let mut needs: Vec<String> = Vec::new();
    walk(&inp.doc.blocks, &mut |b, _, _| match b.kind.as_str() {
        "todo" => {
            let st = components::status_of(inp.state, b);
            if components::yours(inp.state, b) {
                if !matches!(st.as_str(), "done" | "skipped") {
                    needs.push(b.id.clone());
                }
            } else if st == "doing" {
                doing.push(b);
            }
        }
        "question" if !components_resolved(inp, b) && !inp.state.decisions.contains_key(&b.id) => needs.push(b.id.clone()),
        "diff" if !inp.state.decisions.get(&b.id).map(|d| d.kind == "verdict").unwrap_or(false) => needs.push(b.id.clone()),
        _ => {}
    });
    if let Some(b) = doing.first() {
        // elapsed: known when the status came as a decision (it carries its time), not when the
        // block was written already `doing`
        let since = inp.state.decisions.get(&b.id).filter(|d| d.kind == "status" && d.value == "doing").and_then(|d| parse_rfc3339(&d.at));
        let mut right = Vec::new();
        if doing.len() > 1 {
            right.push(format!("+{}", doing.len() - 1));
        }
        if let Some(t) = since {
            right.push(components::age_short(inp.wall - t));
        }
        out.push(HeadLine { kind: HeadKind::Running, text: plain(b.s("text")), right: right.join("  "), target: b.id.clone(), ids: Vec::new() });
    }
    if let Some(first) = needs.first().cloned() {
        let text = format!("{} need{} you: ", needs.len(), if needs.len() == 1 { "s" } else { "" });
        out.push(HeadLine { kind: HeadKind::Needs, text, right: String::new(), target: first, ids: needs });
    }
    out
}

pub struct Scene {
    pub retained: Retained,
    /// Blocks still carrying a change bar since sulin last looked (the header's "N new").
    unseen_n: usize,
    /// When the current run of unseen changes began, and whether the tag has latched on.
    unseen_t: Option<f64>,
    tag_on: bool,
    heights: HashMap<String, Spring>,
    /// Item width the height springs were last aimed at: a reflow from a resize snaps rather than
    /// animates, since only a change of content means something moved.
    heights_w: u16,
    progress: HashMap<String, Spring>,
    vis: HashMap<String, Tween<f32>>,
    visible_last: HashSet<String>,
    ghosts: Vec<Ghost>,
    prev: Option<Doc>,
    moved: HashSet<String>,
    rev: Option<u64>,
    pub pulse: HashMap<String, f64>,
    pub images: HashMap<String, Option<Image>>,
    image_src: HashMap<String, String>,
    /// Scroll anchor: (item id, rows into it). Keeps his place when content lands above.
    pub anchor: Option<(String, i32)>,
    pub scroll: i32,
    pub focus: Option<String>,
    pub focusable: Vec<String>,
    /// Every item in display order (search and section jumps walk it).
    pub order: Vec<String>,
    /// What the help row shows instead of keys: a search being typed (`/query`).
    pub prompt: Option<String>,
    /// A count or sequence half-typed (`5`, `z`), shown at the right of the help row.
    pub pending_keys: String,
    /// Review mode (row notes): the header says so, and a drag picks a quote instead of copying.
    pub review: bool,
    /// `i`: every row shows its id, dim at its right edge, so any row can be named.
    pub show_ids: bool,
    /// Overdue writers of this board (owner.rs): (the id prefix they own, why). Their rows get a
    /// quiet mark at the right edge, the first one with the reason.
    pub stale: Vec<(String, String)>,
    /// What an action on a row came to, by block id: (the note, whether it is a warning). The
    /// app sets it after an action's key and clears it (app.rs `Sent`).
    pub sent: HashMap<String, (String, bool)>,
    /// Claude Code's latest recap for this pane's sessions: (text, local time it was written).
    pub recap: Option<(String, String)>,
    /// The stop flag is up (sulin pressed stop; Claude's tool calls are refused).
    pub stopped: bool,
    /// This pane's sessions' subagents, grouped per fan-out (agents.rs), and whether sulin folded
    /// the section (None: open while any runs).
    pub agents: Vec<crate::agents::Group>,
    pub agents_folded: Option<bool>,
    /// Finished subagents sulin opened again (a finished tracker collapses to its result).
    pub agents_open: HashSet<String>,
    /// Toasts to draw, each with how much of its time is left (toast.rs; the app runs the clock).
    pub toasts: Vec<(crate::toast::Toast, f32)>,
    pub item_pos: HashMap<String, (i32, u16)>,
    /// Buttons on screen this frame: (x, y, width, token).
    pub hits: Vec<(u16, u16, u16, String)>,
    /// Boxes drawn over the list this frame (toasts), each with its token. A click inside one is
    /// its own, whatever button lies underneath.
    overlays: Vec<(Rect, String)>,
    /// The block under the pointer, when mouse reporting is on.
    pub hover: Option<String>,
    /// Screen row where the scrolled list starts (below the title bar), for mapping clicks.
    pub view_top: u16,
    pub view_h: u16,
    pub total_h: i32,
    pub help_open: bool,
    pub scroll_to_focus: bool,
    /// The kind of the focused item, for the help line and key handling.
    pub focus_kind: String,
    loaded_at: f64,
    /// Rows the pinned head takes this frame, its separator included (0: no head).
    pub head_h: u16,
    /// A frame has been drawn: from then on a view at the very top stays at the top.
    framed: bool,
    /// A block to bring to the top of the view on the next frame (a click on a head line): by
    /// then a fold the click opened has put it in the list.
    goto: Option<String>,
    /// The blocks changed since sulin last looked, kept after their bars fade so the changes
    /// tour (`]c`) still has them; the next batch of changes replaces them.
    changed: Vec<String>,
    /// Where the changes tour last stopped.
    tour_at: Option<String>,
    /// The pane's tabs (tabs.rs) and which one this scene is; fewer than two draws no bar. The
    /// app sets both before each frame.
    pub tabs: Vec<crate::tabs::TabInfo>,
    pub tab: usize,
    /// Columns the list has: the pane's, less the detail panel when it sits beside the list.
    list_w: u16,
    /// The `tabs` components' sections on show last frame: a switch swaps the content at once
    /// instead of folding one section shut while the other opens.
    subtabs_last: std::collections::BTreeMap<String, String>,
    /// The detail panel (detail.rs), when it is open: where it is, the block it shows, how far
    /// it is scrolled, and how far it can be.
    /// The row drawn in full last frame (the focused one, components::in_full). When it changes,
    /// the row that takes the focus keeps its place on screen while heights change around it.
    in_full_last: Option<String>,
    detail_rect: Option<Rect>,
    detail_of: Option<String>,
    pub detail_scroll: usize,
    detail_max: usize,
}

impl Scene {
    pub fn new(cfg: &Config) -> Scene {
        let _ = cfg;
        Scene {
            retained: Retained::new(Timings::default()),
            unseen_n: 0,
            unseen_t: None,
            tag_on: false,
            heights: HashMap::new(),
            heights_w: 0,
            progress: HashMap::new(),
            vis: HashMap::new(),
            visible_last: HashSet::new(),
            ghosts: Vec::new(),
            prev: None,
            moved: HashSet::new(),
            rev: None,
            pulse: HashMap::new(),
            images: HashMap::new(),
            image_src: HashMap::new(),
            anchor: None,
            scroll: 0,
            focus: None,
            focusable: Vec::new(),
            order: Vec::new(),
            prompt: None,
            pending_keys: String::new(),
            review: false,
            show_ids: false,
            stale: Vec::new(),
            sent: HashMap::new(),
            recap: None,
            stopped: false,
            agents: Vec::new(),
            agents_folded: None,
            agents_open: HashSet::new(),
            toasts: Vec::new(),
            item_pos: HashMap::new(),
            view_top: 0,
            hits: Vec::new(),
            overlays: Vec::new(),
            hover: None,
            view_h: 0,
            total_h: 0,
            help_open: false,
            scroll_to_focus: false,
            focus_kind: String::new(),
            loaded_at: 0.0,
            head_h: 0,
            framed: false,
            goto: None,
            changed: Vec::new(),
            tour_at: None,
            tabs: Vec::new(),
            tab: 0,
            list_w: 0,
            subtabs_last: Default::default(),
            in_full_last: None,
            detail_rect: None,
            detail_of: None,
            detail_scroll: 0,
            detail_max: 0,
        }
    }

    /// Take a new revision of the document: reconcile by id and start the animations it implies.
    pub fn set_doc(&mut self, doc: &Doc, state: &State, now: f64, m: Motion) {
        if self.rev == Some(doc.rev) && self.prev.as_ref().map(|p| p.id == doc.id).unwrap_or(false) {
            return;
        }
        let first = self.rev.is_none();
        let mut list = Vec::new();
        walk(&doc.blocks, &mut |b, _, _| list.push((b.id.clone(), b.hash())));
        // a batch that lands after the last one was seen and faded starts the tour's list anew
        if !self.retained.nodes.values().any(|n| n.change.is_some()) {
            self.changed.clear();
        }
        let r = self.retained.update(list, now, m, first);
        if !first {
            for id in r.inserted.iter().chain(r.changed.iter()) {
                if !self.changed.contains(id) {
                    self.changed.push(id.clone());
                }
            }
            self.changed.retain(|id| !r.removed.contains(id));
        }
        // removed blocks keep drawing, collapsing, where they were
        if let Some(prev) = &self.prev {
            let mut order: Vec<(String, usize)> = Vec::new();
            walk(&prev.blocks, &mut |b, _, d| order.push((b.id.clone(), d)));
            for id in &r.removed {
                // only what was on screen collapses: a row hidden in a folded section has nothing
                // to animate, and its ghost had no place to draw but the top of the pane
                if !self.item_pos.get(id).map(|&(_, h)| h > 0).unwrap_or(false) {
                    continue;
                }
                let i = order.iter().position(|(x, _)| x == id).unwrap_or(0);
                // the nearest survivor above it that is in the list (not one inside a fold)
                let after = order[..i].iter().rev().map(|(x, _)| x.clone()).find(|x| doc.find(x).is_some() && self.item_pos.contains_key(x));
                if let Some(b) = prev.find(id) {
                    let mut b = b.clone();
                    b.children.clear();
                    self.ghosts.push(Ghost { block: b, depth: order[i].1, after });
                }
            }
        }
        // a removed id that came back is a block again, not a ghost
        self.ghosts.retain(|g| doc.find(&g.block.id).is_none());
        self.moved.extend(r.moved.iter().cloned());
        // a card that needs sulin breathes when it appears (and on first open)
        for id in r.inserted.iter().chain(if first { r.kept.iter() } else { [].iter() }) {
            if let Some(b) = doc.find(id) {
                if b.kind == "question" && !state.decisions.contains_key(id) {
                    self.pulse.insert(id.clone(), now);
                }
            }
        }
        // load or reload images whose source changed
        walk(&doc.blocks, &mut |b, _, _| {
            if b.kind == "image" {
                let src = b.s("path").to_string();
                if self.image_src.get(&b.id) != Some(&src) {
                    let img = Image::load_png(std::path::Path::new(&src)).ok();
                    self.images.insert(b.id.clone(), img);
                    self.image_src.insert(b.id.clone(), src);
                }
            }
        });
        if first {
            self.loaded_at = now;
        }
        self.rev = Some(doc.rev);
        self.prev = Some(doc.clone());
    }

    /// Restore the pane's own view (folds come from View directly; focus and anchor here).
    pub fn restore(&mut self, v: &View) {
        self.focus = v.focus.clone();
        self.anchor = v.anchor.clone().map(|a| (a, v.anchor_off));
    }

    fn flatten(&mut self, doc: &Doc, view: &View, state: &State, now: f64, m: Motion, first_frame: bool, delivered: u64) -> Vec<Item> {
        let t = self.retained.timings;
        // which blocks are logically visible (not inside a folded section)
        let mut logical: HashSet<String> = HashSet::new();
        fn vis_walk(bs: &[Block], view: &View, state: &State, hidden: bool, out: &mut HashSet<String>) {
            for b in bs {
                if !hidden {
                    out.insert(b.id.clone());
                }
                // a tabs component: its strip stands for its sections' headers, and only the
                // section on show has its content in the list
                if b.kind == "tabs" {
                    if let Some(on) = subtab_active(b, view) {
                        vis_walk(&on.children, view, state, hidden, out);
                    }
                    continue;
                }
                let folded = view.folds.get(&b.id).copied().unwrap_or_else(|| b.kind == "section" && components::default_folded(state, b));
                vis_walk(&b.children, view, state, hidden || folded, out);
            }
        }
        vis_walk(&doc.blocks, view, state, false, &mut logical);
        // a tab of a tabs component was switched: the content swaps, nothing eases
        let switched = self.subtabs_last != view.subtabs;
        if switched {
            self.subtabs_last = view.subtabs.clone();
            self.vis.clear();
        }
        if !first_frame && !switched {
            for id in logical.difference(&self.visible_last) {
                // shown by an unfold (inserted blocks already reveal through the retained tree)
                if self.retained.get(id).map(|n| n.reveal.done(now, m)).unwrap_or(false) {
                    self.vis.insert(id.clone(), Tween::new(0.0, 1.0, now, t.open, Easing::EaseOut));
                }
            }
            for id in self.visible_last.difference(&logical) {
                if doc.find(id).is_some() {
                    let cur = self.vis.get(id).map(|v| v.value(now, m)).unwrap_or(1.0);
                    self.vis.insert(id.clone(), Tween::new(cur, 0.0, now, t.close, Easing::EaseIn));
                }
            }
        }
        self.visible_last = logical.clone();
        let mut items = Vec::new();
        let vis = &self.vis;
        #[allow(clippy::too_many_arguments)]
        fn go(bs: &[Block], depth: usize, level: usize, view: &View, state: &State, logical: &HashSet<String>, vis: &HashMap<String, Tween<f32>>, now: f64, m: Motion, out: &mut Vec<Item>) {
            for (i, b) in bs.iter().enumerate() {
                let shown = logical.contains(&b.id) || vis.get(&b.id).map(|v| !v.done(now, m)).unwrap_or(false);
                if !shown {
                    continue;
                }
                if b.kind == "tabs" {
                    // the strip is the item; the section on show contributes its children only
                    out.push(Item { id: b.id.clone(), kind: Kind::Block { depth, folded: false, level: 0, gap: false }, focusable: true });
                    if let Some(on) = subtab_active(b, view) {
                        go(&on.children, depth + 1, 0, view, state, logical, vis, now, m, out);
                    }
                    continue;
                }
                let folded = view.folds.get(&b.id).copied().unwrap_or_else(|| b.kind == "section" && components::default_folded(state, b));
                let bullet = b.kind == "bullet";
                out.push(Item {
                    id: b.id.clone(),
                    kind: Kind::Block { depth, folded, level, gap: bullet && level == 0 && i > 0 && !b.children.is_empty() },
                    // (a table's own item is its header: its rows, `tr`, take the focus)
                    focusable: !matches!(b.kind.as_str(), "text" | "progress" | "timer" | "bullet" | "code" | "rule" | "table" | "runs"),
                });
                go(&b.children, depth + 1, if bullet { level + 1 } else { 0 }, view, state, logical, vis, now, m, out);
            }
        }
        go(&doc.blocks, 0, 0, view, state, &logical, vis, now, m, &mut items);
        // ghosts of removed blocks, after their old predecessor
        for (gi, gh) in self.ghosts.iter().enumerate() {
            // no predecessor: it was the first block. A predecessor that isn't listed (folded away
            // since) leaves the ghost nowhere to be, so it isn't drawn
            let mut at = match &gh.after {
                None => 0,
                Some(a) => match items.iter().position(|i| &i.id == a) {
                    Some(p) => p + 1,
                    None => continue,
                },
            };
            // ghosts that share a predecessor (a section and its children) keep their old order
            while at < items.len() && matches!(items[at].kind, Kind::Ghost(j) if j < gi) {
                at += 1;
            }
            items.insert(at, Item { id: gh.block.id.clone(), kind: Kind::Ghost(gi), focusable: false });
        }
        // how many blocks still carry a change bar since sulin last looked, shown as a count in
        // the header (a divider row made the content jump). Latched: it appears only once a
        // change has gone unseen for UNSEEN_TAG seconds, so a change that lands while he is
        // watching (a feed every 5 s) never blinks it; then it stays until the bars have faded
        let bars = if view.seen_at.is_empty() {
            0
        } else {
            items.iter().filter(|it| matches!(it.kind, Kind::Block { .. }) && self.retained.get(&it.id).map(|n| n.change.is_some()).unwrap_or(false)).count()
        };
        if self.retained.any_unseen() {
            let t0 = *self.unseen_t.get_or_insert(now);
            if now - t0 >= UNSEEN_TAG {
                self.tag_on = true;
            }
        } else {
            self.unseen_t = None;
        }
        if bars == 0 {
            self.tag_on = false;
        }
        self.unseen_n = if self.tag_on { bars } else { 0 };
        // decisions whose component Claude removed stay visible until acknowledged
        let ids: HashSet<String> = doc.ids().into_iter().collect();
        // only sulin's: Claude's own status on a row it removed is nothing he decided
        let orphans: Vec<String> =
            state.decisions.iter().filter(|(k, d)| !ids.contains(*k) && !k.is_empty() && !k.contains('#') && crate::who::is_human(&d.by) && d.seq > delivered).map(|(k, _)| k.clone()).collect();
        // the work overview's subagents, after the recap: a header, and unless folded a row each
        if !self.agents.is_empty() {
            let mut rows = vec![Item { id: "~agents".into(), kind: Kind::AgentsHead, focusable: false }];
            let running = self.agents.iter().any(|g| g.done() < g.agents.len());
            if !self.agents_folded.unwrap_or(!running) {
                for (gi, g) in self.agents.iter().enumerate() {
                    if g.agents.len() > 1 {
                        rows.push(Item { id: format!("~agent-g{gi}"), kind: Kind::AgentGroup(gi), focusable: false });
                    }
                    for (ai, a) in g.agents.iter().enumerate() {
                        rows.push(Item { id: format!("~agent-{}", a.id), kind: Kind::AgentRow(gi, ai), focusable: false });
                    }
                }
            }
            for (i, r) in rows.into_iter().enumerate() {
                items.insert(i, r);
            }
        }
        if self.recap.is_some() {
            items.insert(0, Item { id: "~recap".into(), kind: Kind::Recap, focusable: false });
        }
        if !orphans.is_empty() {
            items.push(Item { id: "~orphans".into(), kind: Kind::OrphanHead(orphans.len()), focusable: false });
            for o in orphans {
                items.push(Item { id: format!("~orphan-{o}"), kind: Kind::Orphan(o), focusable: false });
            }
        }
        items
    }

    pub fn frame(&mut self, inp: &Input) -> Frame {
        let m = inp.cfg.motion();
        let now = inp.now;
        let p = &EVERFOREST;
        let (w, h) = (inp.w.max(20), inp.h.max(8));
        let mut g = Grid::new(w, h);
        g.fill(g.area(), Style::default().on(p.bg0));
        let mut wake = Wake::default();
        let first_frame = self.visible_last.is_empty() && self.item_pos.is_empty();
        self.set_doc(inp.doc, inp.state, now, m);
        let items = self.flatten(inp.doc, inp.view, inp.state, now, m, first_frame, inp.delivered);

        // geometry
        let status_y = h - 1;
        let boxr = Rect::new(0, 0, w, h - 1);
        // no side or bottom border: a top rule carries the title, and the columns go to content
        let gx = 0u16; // gutter (change bars)
        let ix = 1u16; // item grid origin (caret column)
        let help_y = status_y.saturating_sub(1);
        // glance mode: a pane too narrow to read rows in is a strip of title bar and head
        let glance = inp.w < GLANCE_W;
        let mut head = head_lines(inp);
        // the artifact's reminders, above the key hints: wrapped to the width, but never more
        // than HINT_ROWS rows nor so many that the list is squeezed out. Glance mode and a short
        // pane have no room for them
        let mut hint_rows = if glance || h < 14 { Vec::new() } else { hint_flow(&inp.doc.hints, w.saturating_sub(4) as usize) };
        let hint_cap = HINT_ROWS.min((h as usize).saturating_sub(10) / 2).max(1);
        if hint_rows.len() > hint_cap {
            hint_rows.truncate(hint_cap);
            hint_rows.last_mut().unwrap().push((" …".into(), HintRun::Sep));
        }
        let hints_h = hint_rows.len() as u16;
        // the view without a head; the head's height is known only once the items are stacked
        // (it shows when they overflow), so until then last frame's stands in
        let vbot_full = if h >= 12 { help_y.saturating_sub(1 + hints_h) } else { boxr.bottom().saturating_sub(1) };
        // the first content row: under the title line and its spacer. A pane's tabs sit on the
        // title line itself, so a pane with tabs gives up no rows for them
        let top: u16 = 2;
        // the detail panel takes its place from the list: columns on the right of a wide pane,
        // rows at the bottom of a narrow one. The title bar, hints, keys and status bar stay whole
        let panel = if inp.view.detail && !glance { crate::detail::place(w, h, top, vbot_full) } else { None };
        let panel_side = panel.map(|r| r.x > 0).unwrap_or(false);
        let lw = match panel {
            Some(r) if panel_side => r.x,
            _ => w,
        };
        let vbot_full = match panel {
            Some(r) if !panel_side => r.y,
            _ => vbot_full,
        };
        self.list_w = lw;
        let iw = lw.saturating_sub(2); // item grid width; content runs ix+1 .. lw-2
        if iw != self.heights_w {
            self.heights.clear();
            self.heights_w = iw;
        }
        self.view_top = top + self.head_h;
        self.view_h = vbot_full.saturating_sub(self.view_top);
        // what changed, for the title bar: how many blocks changed since he looked (latched in
        // flatten) and how long ago the doc last changed. The rev means nothing to him
        let age = parse_rfc3339(&inp.doc.updated).map(|u| inp.wall - u).unwrap_or(0.0);
        wake.every(now, age_pace(age));
        let (meta_new, meta_age) = match self.unseen_n {
            0 => (String::new(), ago(age)),
            n => (format!("{n} new"), components::age_short(age)),
        };
        let meta = if meta_new.is_empty() { format!(" {meta_age} ") } else { format!(" {meta_new} · {meta_age} ") };
        // where it doesn't fit (glance mode then says it under the head) the title takes its room
        let mut meta_fits = w > width(&meta) as u16 + 20;
        // tabs share the line with it: when they fit only without it, it goes before they
        // collapse
        let tabbed = self.tabs.len() > 1;
        if tabbed && meta_fits {
            let all = w.saturating_sub(4);
            if glance || !crate::tabs::fits(&self.tabs, all.saturating_sub(width(&meta) as u16 + 2)) {
                // collapsed, the chooser is followed by the tabs that have something to say:
                // they matter more than this tab's age, which goes whenever one does
                let says = self.tabs.iter().enumerate().any(|(i, t)| i != self.tab && (t.unseen > 0 || t.needs || t.failed));
                if (!glance && crate::tabs::fits(&self.tabs, all)) || says {
                    meta_fits = false;
                }
            }
        }

        // per-frame animated values handed to components
        let mut flash = HashMap::new();
        for (id, n) in &self.retained.nodes {
            if let Some(f) = &n.flash {
                flash.insert(id.clone(), f.value(now, m));
            }
        }
        let mut prog = HashMap::new();
        {
            let tmp_ctx_progress: Vec<(String, f32)> = {
                let empty = HashMap::new();
                let cx = self.ctx(inp, p, &empty, &empty);
                let mut v = Vec::new();
                walk(&inp.doc.blocks, &mut |b, _, _| {
                    if b.kind == "progress" {
                        let (d, t) = components::progress_counts(inp.doc, &cx, b);
                        let target = if t > 0 { d as f32 / t as f32 } else { b.num("value").unwrap_or(0.0) as f32 };
                        v.push((b.id.clone(), target));
                    }
                });
                v
            };
            for (id, target) in tmp_ctx_progress {
                let s = self.progress.entry(id.clone()).or_insert_with(|| Spring::new(target, SpringCfg::BOUNCY));
                s.set_target(target, now, m);
                if !s.settled(now, m) {
                    wake.fps(now, inp.cfg.fps);
                }
                prog.insert(id, s.value(now, m));
            }
        }

        // focus bookkeeping
        self.focusable = items.iter().filter(|i| i.focusable).map(|i| i.id.clone()).collect();
        self.order = items.iter().filter(|i| !i.id.starts_with('~')).map(|i| i.id.clone()).collect();
        if self.focus.as_ref().map(|f| !self.focusable.contains(f)).unwrap_or(true) {
            // the focused component disappeared: move to its nearest surviving neighbour
            let old = self.focus.take();
            let near = old.as_ref().and_then(|o| self.item_pos.get(o)).map(|&(y, _)| y);
            self.focus = match near {
                Some(y) => self
                    .focusable
                    .iter()
                    .min_by_key(|id| self.item_pos.get(*id).map(|&(iy, _)| (iy - y).abs()).unwrap_or(i32::MAX))
                    .cloned(),
                None => self.focusable.first().cloned(),
            };
        }

        // the focused row draws in full (nothing on it is cut), so a move of the focus changes
        // two heights at once: the row left shrinks back to a line, the row entered grows. Both
        // snap, and the row entered keeps the screen row it had (`pin`), so the view doesn't
        // jump under the eye or the pointer
        let in_full = if inp.focused_pane { self.focus.clone() } else { None };
        let full_moved = in_full != self.in_full_last;
        let pin: Option<(String, i32)> = if full_moved { in_full.as_ref().and_then(|f| self.item_pos.get(f).map(|&(y, _)| (f.clone(), y - self.scroll))) } else { None };
        let full_was = std::mem::replace(&mut self.in_full_last, in_full.clone());
        // draw and stack the items
        let mut drawn: Vec<(Item, components::Drawn, i32, u16)> = Vec::new();
        let mut y = 0i32;
        let mut new_pos = HashMap::new();
        let mut first_top = true;
        // dependency lanes, per section: its todos in order with their `after` ids (graph.rs)
        let mut graphs: HashMap<String, crate::graph::Row> = HashMap::new();
        walk(&inp.doc.blocks, &mut |b, _, _| {
            if b.kind == "section" {
                let nodes: Vec<(String, Vec<String>)> = b.children.iter().filter(|c| c.kind == "todo").map(|c| (c.id.clone(), c.strs("after"))).collect();
                graphs.extend(crate::graph::layout(&nodes));
            }
        });
        for it in &items {
            let cx = self.ctx(inp, p, &flash, &prog);
            let d = match &it.kind {
                Kind::Block { depth, .. } if inp.doc.find(&it.id).map(|b| b.kind == "tabs").unwrap_or(false) => {
                    let d = self.draw_subtabs(inp, inp.doc.find(&it.id).unwrap(), iw, *depth, first_top);
                    if *depth == 0 {
                        first_top = false;
                    }
                    d
                }
                Kind::Block { depth, folded, level, gap } => {
                    let b = inp.doc.find(&it.id).unwrap();
                    let mut bb;
                    let b = if b.kind == "section" {
                        bb = b.clone();
                        bb.children.truncate(b.children.len());
                        bb.f.insert("_folded".into(), (*folded).into());
                        &bb
                    } else if b.kind == "bullet" {
                        bb = b.clone();
                        bb.f.insert("_level".into(), (*level).into());
                        bb.f.insert("_gap".into(), (*gap).into());
                        &bb
                    } else if let Some(r) = graphs.get(&b.id) {
                        // a todo in a section with `after` edges: its lane cells and node links
                        bb = b.clone();
                        let cells: String = r.cells.iter().collect();
                        // each line cell's colour: its edge's two steps, one colour when they
                        // agree, else an OKLCH blend by how far along the edge the cell sits
                        // a stale step (resting on a struck claim) colours its lines red, like blocked
                        // sulin's own open step colours its lines pink (his colour), like next
                        let st_of = |id: &str| {
                            inp.doc
                                .find(id)
                                .map(|x| {
                                    let st = components::status_of(inp.state, x);
                                    if components::stale(inp.state, x) {
                                        "blocked".to_string()
                                    } else if components::yours(inp.state, x) && !matches!(st.as_str(), "done" | "skipped") {
                                        "next".to_string()
                                    } else {
                                        st
                                    }
                                })
                                .unwrap_or_default()
                        };
                        let colors: Vec<serde_json::Value> = r
                            .edges
                            .iter()
                            .map(|e| match e {
                                Some((from, to, t)) => {
                                    let (sf, st) = (st_of(from), st_of(to));
                                    let (a, z) = (components::status_color(p, &sf), components::status_color(p, &st));
                                    // grey (open) is a colour like any other: one colour when
                                    // the ends agree, else a blend held near each node and
                                    // turning through the middle (smoothstep)
                                    let c = if a == z { a } else { a.mix_lch(z, t * t * (3.0 - 2.0 * t)) };
                                    serde_json::json!(c.to_hex())
                                }
                                None => serde_json::Value::Null,
                            })
                            .collect();
                        bb.f.insert("_graph".into(), serde_json::json!({"cells": cells, "colors": colors, "node": r.node, "u": r.up, "d": r.down, "l": r.left, "r": r.right}));
                        &bb
                    } else {
                        b
                    };
                    let d = components::draw(&cx, b, iw, *depth, first_top);
                    if *depth == 0 {
                        first_top = false;
                    }
                    d
                }
                Kind::Ghost(i) => {
                    let gh = &self.ghosts[*i];
                    components::draw(&cx, &gh.block, iw, gh.depth, false)
                }
                Kind::Recap => {
                    // the recap Claude Code wrote when he came back: grey, wrapped, with its time
                    let (text, when) = self.recap.clone().unwrap_or_default();
                    let lines = vellum::text::wrap(&text, iw.saturating_sub(4) as usize);
                    let mut gg = Grid::new(iw, lines.len() as u16 + 1);
                    gg.fill(gg.area(), Style::default().on(p.bg0));
                    gg.put(1, 0, "↺", Style::fg(p.blue).on(p.bg0).bold());
                    for (i, l) in lines.iter().enumerate() {
                        gg.put(3, i as u16, l, Style::fg(p.grey2).on(p.bg0).italic());
                    }
                    if !when.is_empty() {
                        let ww = width(&when) as u16;
                        let last = lines.last().map(|l| width(l) as u16 + 3).unwrap_or(3);
                        let y = if last + ww + 2 < iw { lines.len().saturating_sub(1) as u16 } else { lines.len() as u16 };
                        gg.put(iw.saturating_sub(ww + 1), y, &when, Style::fg(p.grey0).on(p.bg0));
                    }
                    components::Drawn { grid: Some(gg), ..Default::default() }
                }
                Kind::AgentsHead => {
                    // how many are at work, how many wait, how many are done: the three states
                    // the rows below tell apart
                    let (mut run, mut wait, mut done) = (0usize, 0usize, 0usize);
                    for ag in self.agents.iter().flat_map(|g| &g.agents) {
                        match ag.phase(inp.wall) {
                            crate::agents::Phase::Done => done += 1,
                            crate::agents::Phase::Waiting => wait += 1,
                            _ => run += 1,
                        }
                    }
                    let mut gg = Grid::new(iw, 2);
                    gg.fill(gg.area(), Style::default().on(p.bg0));
                    gg.put(1, 1, icon::ROBOT, Style::fg(p.blue).on(p.bg0).bold());
                    let x = gg.put(1 + components::TX, 1, "Agents", Style::fg(p.green).on(p.bg0).bold());
                    let counts: Vec<String> = [(run, "running"), (wait, "waiting"), (done, "done")].into_iter().filter(|(n, _)| *n > 0).map(|(n, l)| format!("{n} {l}")).collect();
                    gg.put(x + 2, 1, &counts.join(" · "), Style::fg(p.grey1).on(p.bg0));
                    components::Drawn { grid: Some(gg), buttons: vec![(0, 1, iw, "mg:agents".into())], ..Default::default() }
                }
                Kind::AgentGroup(gi) => {
                    let g = &self.agents[*gi];
                    let mut gg = Grid::new(iw, 1);
                    gg.fill(gg.area(), Style::default().on(p.bg0));
                    let label = format!("fan-out · {}/{} done", g.done(), g.agents.len());
                    let x = gg.put(3, 0, &label, Style::fg(p.grey2).on(p.bg0));
                    let a = gg.area();
                    let bw = iw.saturating_sub(x + 4).min(20);
                    if bw > 4 {
                        parts::progress_bar(&mut gg, x + 2, 0, bw, g.done() as f32 / g.agents.len().max(1) as f32, p.green, p.aqua, p.bg2, a);
                    }
                    components::Drawn { grid: Some(gg), ..Default::default() }
                }
                Kind::AgentRow(gi, ai) => {
                    // one subagent in one line of aligned columns: glyph, who, phase, plan
                    // progress, elapsed. What it said along the way and its plan's step in hand
                    // show only when the row is opened (a click): the line says how far along it
                    // is, not what it is running
                    use crate::agents::Phase;
                    let g = &self.agents[*gi];
                    let ag = &g.agents[*ai];
                    let ind: u16 = if g.agents.len() > 1 { 5 } else { 3 };
                    let cols = agent_cols(&self.agents, iw, inp.mouse);
                    // the phase is read from what the transcript already gave (no file is touched):
                    // waiting begins with time passing, which no reload announces
                    let phase = ag.phase(inp.wall);
                    let wait = ag.waiting_for(inp.wall);
                    let open = self.agents_open.contains(&ag.id);
                    let tx = ind + components::TX;
                    let text_w = iw.saturating_sub(tx + 1).max(1) as usize;
                    let who = ag.who();
                    let who_room = cols.phase_x.saturating_sub(tx + 2) as usize;
                    // opened: the lines under the row, each (node glyph, its color, text, text color)
                    let mut more: Vec<(Option<char>, Rgb, String, Rgb)> = Vec::new();
                    if open {
                        const SHOW: usize = 5;
                        /// Rows one of its notes may wrap to.
                        const NOTE_ROWS: usize = 3;
                        let capped = |t: &str| {
                            let mut rows = vellum::text::wrap(t, text_w);
                            if rows.len() > NOTE_ROWS {
                                rows.truncate(NOTE_ROWS);
                                let last = rows.pop().unwrap_or_default();
                                rows.push(format!("{}…", truncate(&last, text_w.saturating_sub(1)).trim_end_matches('…')));
                            }
                            rows
                        };
                        // what it was sent to do, when the row's name is its plan's or was cut
                        if who != ag.description || width(&who) > who_room {
                            more.extend(vellum::text::wrap(&ag.description, text_w).into_iter().map(|l| (None, p.bg0, l, p.grey2)));
                        }
                        // the step of its plan in hand
                        if let Some(pr) = ag.progress.as_ref().filter(|pr| !pr.current.is_empty() && !ag.finished) {
                            for (i, l) in vellum::text::wrap(&pr.current, text_w.saturating_sub(5).max(1)).into_iter().enumerate() {
                                more.push((None, p.bg0, format!("{}{l}", if i == 0 { "step " } else { "     " }), p.fg));
                            }
                        }
                        // its own words along the way (the high level), or one line of what kind
                        // of work it did; a finished agent's last note is its result
                        let mut chain: Vec<String> = if ag.notes.is_empty() { vec![ag.summary()].into_iter().filter(|x| !x.is_empty()).collect() } else { ag.notes.clone() };
                        let result = if ag.finished && !ag.notes.is_empty() { chain.pop().unwrap_or_default() } else { String::new() };
                        let hidden = chain.len().saturating_sub(SHOW);
                        if hidden > 0 {
                            more.push((Some(crate::graph::V), p.grey1, format!("· {hidden} earlier"), p.grey1));
                        }
                        let n = chain.len() - hidden;
                        for (i, st) in chain[hidden..].iter().enumerate() {
                            let last = i + 1 == n;
                            let current = last && !ag.finished;
                            let r = crate::graph::Row { up: true, down: !last || !result.is_empty(), ..Default::default() };
                            for (k, l) in capped(st).into_iter().enumerate() {
                                let node = if k == 0 { Some(crate::graph::node_glyph(&r, !current)) } else { r.down.then_some(crate::graph::V) };
                                more.push((node, if current { p.yellow } else { p.blue }, l, if current { p.fg } else { p.grey2 }));
                            }
                        }
                        for (k, l) in capped(&result).into_iter().enumerate() {
                            let node = (k == 0 && n > 0).then(|| crate::graph::node_glyph(&crate::graph::Row { up: true, ..Default::default() }, true));
                            more.push((node, p.green, l, p.fg));
                        }
                    }
                    let mut gg = Grid::new(iw, 1 + more.len() as u16);
                    gg.fill(gg.area(), Style::default().on(p.bg0));
                    // the glyph: a check once finished, a still pause while it waits, else the
                    // spinner, whose frames go through `tick` so the motion and on-screen filter
                    // below applies to it like any other row
                    let (gl, gc, spin) = match phase {
                        Phase::Done => (icon::DONE.to_string(), p.green, None),
                        Phase::Waiting => (icon::PAUSE.to_string(), p.grey1, None),
                        _ => (parts::spinner(now, m).to_string(), p.yellow, Some(0.1)),
                    };
                    gg.put(ind, 0, &gl, Style::fg(gc).on(p.bg0).bold());
                    // the clocks on the row (elapsed, how long it has waited) at the pace an age
                    // changes; a running agent is looked at every half minute at least, so the
                    // turn to waiting shows without a reload
                    if !ag.finished {
                        let ran = parse_rfc3339(&ag.started).map(|s| inp.wall - s).unwrap_or(0.0);
                        wake.every(now, age_pace(wait.unwrap_or(ran).min(ran)).min(if wait.is_some() { 60.0 } else { 30.0 }));
                    }
                    let right = ag.took(inp.wall);
                    let mut buttons = vec![(0u16, 0u16, iw, format!("mg:agent/{}", ag.id))];
                    // the time is right-aligned in a slot of fixed width, so the stop buttons of
                    // rows with "45s" and "1h04" sit in one column
                    const TIME_SLOT: u16 = 5;
                    let tw = width(&right) as u16;
                    gg.put(iw.saturating_sub(tw + 1), 0, &right, Style::fg(p.grey1).on(p.bg0));
                    let mut edge = iw.saturating_sub(tw.max(TIME_SLOT) + 1);
                    if !ag.finished && inp.mouse {
                        let a = gg.area();
                        let bx = edge.saturating_sub(8);
                        let e = parts::tag(&mut gg, bx, 0, " stop ", p.red, p.bg0, a);
                        buttons.insert(0, (bx, 0, e - bx, format!("mg:stopagent/{}", ag.id)));
                        edge = bx;
                    }
                    // who: a label, cut to its column (opened, the row says it in full)
                    gg.put(tx, 0, &truncate(&who, who_room.min(edge.saturating_sub(tx + 1) as usize)), Style::fg(p.fg).on(p.bg0).bold());
                    // the phase word, in its phase's color: the kinds of work read apart down the
                    // column. Waiting is dim (nothing is happening) and says for how long
                    let (word, pc) = match phase {
                        Phase::Planning => (phase.label().to_string(), p.blue),
                        Phase::Coding => (phase.label().to_string(), p.yellow),
                        Phase::Testing => (phase.label().to_string(), p.aqua),
                        Phase::Reviewing => (phase.label().to_string(), p.orange),
                        Phase::Waiting => (format!("waiting {}", components::age_short(wait.unwrap_or(0.0))), p.grey1),
                        Phase::Done => (phase.label().to_string(), p.green),
                    };
                    if cols.phase_x + width(&word) as u16 + 1 <= edge {
                        gg.put(cols.phase_x, 0, &word, Style::fg(pc).on(p.bg0));
                    }
                    // the plan: a bar and done/total; an agent that keeps no plan shows neither
                    if let Some(pr) = ag.progress.as_ref().filter(|_| cols.count_x + cols.count_w + 1 <= edge) {
                        let bar = pr.bar(AGENT_BAR_W as usize);
                        let full = bar.chars().filter(|c| *c == '▰').count();
                        let x = gg.put(cols.bar_x, 0, &"▰".repeat(full), Style::fg(if pr.done >= pr.total { p.green } else { p.grey2 }).on(p.bg0));
                        gg.put(x, 0, &"▱".repeat(AGENT_BAR_W as usize - full), Style::fg(p.bg4).on(p.bg0));
                        gg.put(cols.count_x, 0, &format!("{}/{}", pr.done, pr.total), Style::fg(p.grey2).on(p.bg0));
                    }
                    for (i, (node, nc, text, tc)) in more.iter().enumerate() {
                        let y = 1 + i as u16;
                        if let Some(ch) = node {
                            let mut buf = [0u8; 4];
                            gg.put(ind, y, crate::graph::glyph(*ch, inp.cfg.graph_glyphs == "flog").encode_utf8(&mut buf), Style::fg(*nc).on(p.bg0).bold());
                        }
                        gg.put(tx, y, text, Style::fg(*tc).on(p.bg0));
                    }
                    components::Drawn { grid: Some(gg), buttons, tick: spin, ..Default::default() }
                }
                Kind::OrphanHead(n) => {
                    let mut gg = Grid::new(iw, 2);
                    gg.fill(gg.area(), Style::default().on(p.bg0));
                    let x = gg.put(1, 1, icon::WARN, Style::fg(p.yellow).on(p.bg0));
                    gg.put(x + 1, 1, &format!("Orphaned decisions ({n}): Claude removed what you decided on"), Style::fg(p.grey2).on(p.bg0).bold());
                    components::Drawn { grid: Some(gg), ..Default::default() }
                }
                Kind::Orphan(id) => {
                    let mut gg = Grid::new(iw, 1);
                    gg.fill(gg.area(), Style::default().on(p.bg0));
                    let d = &inp.state.decisions[id];
                    gg.put(3, 0, &format!("{id}  {} {}", d.kind, d.value), Style::fg(p.grey1).on(p.bg0));
                    components::Drawn { grid: Some(gg), ..Default::default() }
                }
            };
            let nat = d.grid.as_ref().map(|g| g.h).unwrap_or(0);
            // animated height: a spring toward the natural height (a resolving card shrinks on it)
            let hs = self.heights.entry(it.id.clone()).or_insert_with(|| Spring::new(nat as f32, SpringCfg::GENTLE));
            if full_moved && (Some(&it.id) == in_full.as_ref() || Some(&it.id) == full_was.as_ref()) {
                *hs = Spring::new(nat as f32, SpringCfg::GENTLE);
            }
            hs.set_target(nat as f32, now, m);
            if !hs.settled(now, m) {
                wake.fps(now, inp.cfg.fps);
            }
            // a ghost draws only through its leaving node: once that is gone it has no height
            let gone = if matches!(it.kind, Kind::Ghost(_)) { 0.0 } else { 1.0 };
            let reveal = self.retained.get(&it.id).map(|n| n.reveal.value(now, m)).unwrap_or(gone)
                * self.vis.get(&it.id).map(|v| v.value(now, m)).unwrap_or(1.0);
            if self.vis.get(&it.id).map(|v| !v.done(now, m)).unwrap_or(false) {
                wake.fps(now, inp.cfg.fps);
            }
            let rows = ((hs.value(now, m).max(0.0) * reveal.clamp(0.0, 1.0)).round() as u16).min(nat.max(hs.value(now, m).ceil() as u16));
            let moved = self.moved.contains(&it.id);
            // a ghost's node is leaving; placing it would re-create it as present and the removed
            // block would never finish collapsing
            if !matches!(it.kind, Kind::Ghost(_)) {
                self.retained.place(&it.id, y, moved, now, m);
            }
            new_pos.insert(it.id.clone(), (y, rows));
            // sub-second ticks are spinner frames: none with motion off (the pane woke 10x/s for
            // a still glyph), and none for a block scrolled out of view. Clock ticks (>= 1 s) stay
            let on_screen = y + rows as i32 > self.scroll && y < self.scroll + self.view_h as i32;
            if let Some(t) = d.tick.filter(|&t| t >= 1.0 || (m.on && on_screen)) {
                wake.every(now, t);
            }
            if let Some(r) = &d.ring {
                r.wake(now, m, &mut wake);
            }
            drawn.push((it.clone(), d, y, rows));
            y += rows as i32;
        }
        self.moved.clear();
        self.vis.retain(|_, v| !v.done(now, m) || v.to > 0.5);
        self.item_pos = new_pos;
        self.total_h = y;

        // the pinned head: its running and needs lines only when the content overflows the view
        // (when everything is on screen they would repeat it); the purpose line whenever the
        // document has one, since nothing in the list says it; and all of it in glance mode,
        // where the head is the content
        self.hits.clear();
        self.overlays.clear();
        let full_h = vbot_full.saturating_sub(top);
        // (a purpose line and its rule take two rows from the list: content that fits only
        // without them overflows with them)
        let own_purpose = head.first().map(|l| l.kind == HeadKind::Purpose && l.target.is_empty()).unwrap_or(false);
        if !glance && self.total_h <= full_h as i32 - if own_purpose { 2 } else { 0 } {
            head.retain(|l| l.kind == HeadKind::Purpose && l.target.is_empty());
        }
        // folded (`zh`): the head is its first line, cut to one row, over the rule. What the
        // hidden lines held stays out of sight except that something waits on him: the rule
        // says how many
        let mut folded_needs = 0usize;
        if inp.view.head_folded && !glance && head.len() > 1 {
            folded_needs = head.iter().skip(1).filter(|l| l.kind == HeadKind::Needs).map(|l| l.ids.len()).sum();
            head.truncate(1);
        }
        let show_head = !head.is_empty() && full_h >= head.len() as u16 + 5;
        // the head's lines wrap (HEAD_WRAP rows each), but the list keeps at least 4 rows
        let head_max = (head.len() * if inp.view.head_folded { 1 } else { HEAD_WRAP }).min(full_h.saturating_sub(5) as usize) as u16;
        self.head_h = if glance {
            let mut rows = self.draw_head(&mut g, &head, inp, top, vbot_full.saturating_sub(top), true, &mut wake);
            // the title bar has no room for what changed at this width: it closes the strip
            let y = top + rows;
            if y < vbot_full && !meta_fits {
                let x = g.put(2, y, icon::CLOCK, Style::fg(p.grey1).on(p.bg0));
                let mut x = x + 2;
                if !meta_new.is_empty() {
                    x = g.put(x, y, &meta_new, Style::fg(p.orange).on(p.bg0)) + 1;
                    x = g.put(x, y, "·", Style::fg(p.grey1).on(p.bg0)) + 1;
                }
                g.put_clipped(x, y, &meta_age, Style::fg(p.grey1).on(p.bg0), Rect::new(0, y, w.saturating_sub(1), 1));
                rows += 1;
            }
            rows
        } else if show_head {
            let rows = self.draw_head(&mut g, &head, inp, top, head_max, false, &mut wake);
            // a thin rule under it: the head is the pane's, the rows below scroll
            for x in 1..lw.saturating_sub(1) {
                g.put(x, top + rows, "─", Style::fg(p.bg2).on(p.bg0));
            }
            // the rule folds the head (`zh`): a chevron at its left, down while open and up
            // while folded, and the whole rule answers a click
            g.put(2, top + rows, if inp.view.head_folded { icon::CHEVRON_UP } else { icon::DOWN }, Style::fg(p.grey0).on(p.bg0));
            g.put(3, top + rows, " ", Style::default().on(p.bg0));
            if folded_needs > 0 {
                // purple is his color: the one thing a folded head must not hide
                let t = format!(" {folded_needs} need{} you ", if folded_needs == 1 { "s" } else { "" });
                let x = lw.saturating_sub(width(&t) as u16 + 2);
                if x > 6 {
                    g.put(x, top + rows, &t, Style::fg(p.purple).on(p.bg0));
                }
            }
            self.hits.push((1, top + rows, lw.saturating_sub(2), "mg:headfold".into()));
            rows + 1
        } else {
            0
        };
        let vtop = top + self.head_h;
        let vbot = if glance { vtop.min(vbot_full) } else { vbot_full };
        self.view_top = vtop;
        self.view_h = vbot.saturating_sub(vtop);
        let clip = Rect::new(ix, vtop, iw, self.view_h);

        // scroll: hold the anchor's place, then keep the focus in view when keys moved it
        let max_scroll = (self.total_h - self.view_h as i32).max(0);
        // a view at the very top stays at the top: content inserted above the first row is the
        // news, and holding the old first row in place put it off screen. (Before the first
        // frame the scroll is 0 only because the restored anchor hasn't been applied yet.)
        let stick_top = self.framed && self.scroll == 0;
        if let Some((a, off)) = self.anchor.as_ref().filter(|_| !stick_top) {
            if let Some(&(ay, _)) = self.item_pos.get(a) {
                self.scroll = ay + off;
            }
        }
        // the focus moved: the row it moved to stays on the screen row it had
        if let Some((id, off)) = &pin {
            if let Some(&(py, _)) = self.item_pos.get(id) {
                self.scroll = py - off;
            }
        }
        // a head line was clicked: its block goes to the top of the view, focused if it can be
        if let Some(id) = self.goto.take() {
            if let Some(&(gy, _)) = self.item_pos.get(&id) {
                self.scroll = gy;
                if self.focusable.contains(&id) {
                    self.focus = Some(id);
                }
            }
        }
        self.framed = true;
        if self.scroll_to_focus {
            if let Some(&(fy, fh)) = self.focus.as_ref().and_then(|f| self.item_pos.get(f)) {
                if fy < self.scroll {
                    self.scroll = fy;
                } else if fy + fh as i32 > self.scroll + self.view_h as i32 {
                    self.scroll = fy + fh as i32 - self.view_h as i32;
                }
            }
            self.scroll_to_focus = false;
        }
        self.scroll = self.scroll.clamp(0, max_scroll);
        self.set_anchor_from_scroll();

        // composite
        let focus_on = inp.focused_pane;
        let mut images = Vec::new();
        let mut stale_said: HashSet<String> = HashSet::new();
        for (it, d, iy, rows) in &drawn {
            let Some(src) = &d.grid else { continue };
            let off = self.retained.get(&it.id).map(|n| n.offset.value(now, m)).unwrap_or(0.0).round() as i32;
            let sy = vtop as i32 + iy + off - self.scroll;
            g.blit(src, 0, *rows, ix, sy, clip);
            for (bx, by, bw, tok) in &d.buttons {
                let yy = sy + *by as i32;
                if *by < *rows && yy >= vtop as i32 && yy < vbot as i32 {
                    self.hits.push((ix + bx, yy as u16, *bw, tok.clone()));
                }
            }
            let is_focus = focus_on && self.focus.as_deref() == Some(it.id.as_str());
            let is_card = inp.doc.find(&it.id).map(|b| b.kind == "question" && !components_resolved(inp, b)).unwrap_or(false);
            if is_focus && !is_card {
                let fr = if matches!(inp.doc.find(&it.id).map(|b| b.kind.as_str()), Some("diff")) { 1 } else { *rows };
                for r in d.lead.min(fr)..fr {
                    let yy = sy + r as i32;
                    if yy >= vtop as i32 && yy < vbot as i32 {
                        g.tint(Rect::new(ix, yy as u16, iw, 1), p.bg1);
                        g.put(ix, yy as u16, "▌", Style::fg(p.purple).on(p.bg1));
                    }
                }
            }
            // change bar in the gutter: full orange while unseen, fading once he's looked
            if let Some(c) = self.retained.get(&it.id).and_then(|n| n.change.as_ref()) {
                let k = c.value(now, m);
                if k > 0.02 {
                    let col = p.bg0.mix(p.orange, k);
                    for r in 0..*rows {
                        let yy = sy + r as i32;
                        if yy >= vtop as i32 && yy < vbot as i32 {
                            g.put(gx, yy as u16, "▎", Style::fg(col).on(p.bg0));
                        }
                    }
                }
            }
            // a note at the row's right edge, over whatever sits there: what the person's action
            // on it came to, else that its writer is overdue
            if sy >= vtop as i32 && sy < vbot as i32 && !it.id.starts_with('~') {
                let note = match self.sent.get(&it.id) {
                    Some((t, warn)) => Some((t.clone(), if *warn { p.yellow } else { p.aqua })),
                    None => self.stale.iter().find(|(pre, _)| crate::store::owns(pre, &it.id)).map(|(pre, why)| {
                        let first = !stale_said.contains(pre);
                        stale_said.insert(pre.clone());
                        (if first { format!("◷ stale · {why}") } else { "◷ stale".to_string() }, p.grey1)
                    }),
                };
                if let Some((t, col)) = note {
                    let t = truncate(&t, iw.saturating_sub(4) as usize).into_owned();
                    let x = (ix + iw).saturating_sub(width(&t) as u16 + 1);
                    g.put(x.saturating_sub(1), sy as u16, " ", Style::default().on(p.bg0));
                    g.put(x, sy as u16, &t, Style::fg(col).on(p.bg0).italic());
                }
            }
            // `i`: the row's id at its right edge, over whatever sits there
            if self.show_ids && sy >= vtop as i32 && sy < vbot as i32 && !it.id.starts_with('~') {
                let iw_ = width(&it.id) as u16;
                let x = (ix + iw).saturating_sub(iw_ + 1);
                g.put(x.saturating_sub(1), sy as u16, " ", Style::default().on(p.bg0));
                g.put(x, sy as u16, &it.id, Style::fg(p.grey1).on(p.bg0).italic());
            }
            // sulin's open notes on this row: the reaction's glyph in the gutter, over the bar
            if let Some((_, nd)) = crate::store::notes_on(inp.doc, inp.state, &it.id).first() {
                let yy = sy;
                if yy >= vtop as i32 && yy < vbot as i32 {
                    let gl = note_glyph(nd.value["r"].as_str().unwrap_or(""));
                    g.put(gx, yy as u16, gl, Style::fg(p.purple).on(p.bg0).bold());
                }
            }
            for (key, x, y0, cols, rws) in &d.images {
                let scr_y = sy + *y0 as i32;
                if *y0 + *rws <= *rows && scr_y >= vtop as i32 && scr_y + *rws as i32 <= vbot as i32 {
                    images.push(Placement { key: key.clone(), x: ix + x, y: scr_y as u16, cols: *cols, rows: *rws });
                }
            }
        }
        self.retained.tick(now, m, &mut wake, inp.cfg.fps);
        // after the tick, which drops the nodes that finished collapsing: pruned before it, a
        // ghost outlived its node by a frame and drew again at full height
        self.ghosts.retain(|gh| self.retained.get(&gh.block.id).is_some());
        self.focus_kind = self.focus.as_ref().and_then(|f| inp.doc.find(f)).map(|b| b.kind.clone()).unwrap_or_default();

        // the detail panel: the focused block, drawn straight (it follows the focus without
        // easing: a panel that slid on every j would never be still)
        self.detail_rect = panel;
        if let Some(r) = panel {
            let body = crate::detail::body(r, panel_side);
            let (lines, open) = match self.focus.as_ref().and_then(|f| inp.doc.find(f)) {
                Some(b) => crate::detail::lines(b, inp.state, inp.wall, body.w as usize),
                None => (vec![vec![("no row has the focus (j / k)".to_string(), crate::detail::Run::Dim)]], None),
            };
            if self.detail_of != self.focus {
                self.detail_of = self.focus.clone();
                self.detail_scroll = 0;
            }
            self.detail_max = lines.len().saturating_sub(body.h as usize);
            self.detail_scroll = self.detail_scroll.min(self.detail_max);
            if let Some((x, y, bw)) = crate::detail::draw(&mut g, r, panel_side, &lines, self.detail_scroll, open, p) {
                if let Some(f) = &self.focus {
                    self.hits.push((x, y, bw, format!("mg:open/{f}")));
                }
            }
        }

        // scrollbar in the last column (the right margin, free now that there's no border): a
        // faint track the height of the view and a thumb sized to the share of the doc in view,
        // so it reads as a scrollbar rather than a mark on whatever row it sits beside
        if self.total_h > self.view_h as i32 && self.view_h > 1 {
            let vh = self.view_h as f32;
            let thumb = ((vh * vh / self.total_h as f32).round() as u16).clamp(1, self.view_h);
            let frac = self.scroll as f32 / max_scroll.max(1) as f32;
            let ty = vtop + ((self.view_h - thumb) as f32 * frac).round() as u16;
            let sx = lw.saturating_sub(1);
            for y in vtop..vtop + self.view_h {
                let on = y >= ty && y < ty + thumb;
                g.put(sx, y, if on { "┃" } else { "│" }, Style::fg(if on { p.grey0 } else { p.bg2 }).on(p.bg0));
            }
        }

        // frame: a gradient ring that flows around the panel while it has focus, resting otherwise
        let stops = vec![p.purple, p.aqua, p.green, p.aqua];
        let ring = match (inp.cfg.ring.as_str(), focus_on) {
            ("plain", _) => Ring::Plain(if focus_on { p.purple } else { p.bg3 }),
            (_, true) => Ring::Flow { stops, period: inp.cfg.ring_period },
            (_, false) => Ring::Flow { stops: stops.iter().map(|c| c.mix(p.bg3, 0.55)).collect(), period: inp.cfg.ring_period },
        };
        let ring_m = if focus_on && inp.cfg.ring == "flow" { m } else { Motion::off() };
        let a = g.area();
        parts::rule(&mut g, boxr.x, boxr.y, boxr.w, &ring, p.bg0, now, ring_m, a);
        ring.wake(now, ring_m, &mut wake);
        // title pill and meta set into the top edge
        let ic = inp.doc.icon.clone().unwrap_or_else(|| icon::PLAN.to_string());
        let mw = if meta_fits { width(&meta) as u16 } else { 0 };
        let tmax = if glance && !meta_fits { w.saturating_sub(4) } else { w.saturating_sub(mw + 10) } as usize;
        let title = format!(" {ic} {} ", truncate(&inp.doc.title, tmax.saturating_sub(4)));
        let (pb, pf) = if focus_on { (p.purple, p.bg0) } else { (p.bg3, p.fg) };
        let tx = if tabbed {
            // the tabs stand where the title would: the tab on screen's label is the title, and
            // the others say where else to look. A strip (glance mode) gets the chooser
            let tw = w.saturating_sub(4 + if mw > 0 { mw + 2 } else { 0 });
            let (hits, end) = crate::tabs::draw(&mut g, 2, 0, &self.tabs, self.tab, tw, focus_on, glance, p);
            self.hits.extend(hits.into_iter().map(|(x, bw, tok)| (x, 0, bw, tok)));
            end
        } else {
            parts::pill(&mut g, 2, 0, &title, pb, pf, p.bg0, a)
        };
        // since-you-looked badge beside the title: how many blocks carry a live change bar, readable
        // from across the tab. The count comes latched from flatten (unseen_n), so a feed writing
        // while sulin watches never blinks it
        // Claude blocked on sulin (a permission prompt): said in the top edge, where it can't be
        // missed and moves nothing
        let mut tx = tx;
        if inp.activity.iter().find(|x| x.agent.is_empty()).map(|x| x.state == "waiting").unwrap_or(false) {
            let pill = format!(" {} waiting on you ", icon::HOURGLASS);
            if tx + width(&pill) as u16 + 2 < w.saturating_sub(mw + 2) {
                tx = parts::pill(&mut g, tx + 1, 0, &pill, p.purple, p.bg0, p.bg0, a);
            }
        }
        // the stop flag, while it is up: a filled red pill. Stopping itself is a key (`S`), the
        // CLI, or esc in the chat; the bar holds no button for it (the line is the tabs')
        let room0 = w.saturating_sub(mw + 2);
        if self.stopped {
            let pill = " ⏹ STOPPED ";
            if tx + width(pill) as u16 + 2 < room0 {
                tx = parts::pill(&mut g, tx + 1, 0, pill, p.red, p.bg0, p.bg0, a);
            }
        }
        // row notes: review mode says it is on (`c` turns it on and off: a mode with nothing on
        // screen to say so is a trap), and how many notes wait to go (his draft)
        let draft = inp.state.decisions.iter().filter(|(k, v)| v.kind == "note" && v.seq > inp.delivered && crate::store::note_open(inp.doc, k, v)).count();
        let room = w.saturating_sub(mw + 2);
        if self.review {
            // the short form where the long one has no room: the marker itself must not go
            for pill in [" ✎ REVIEW ", " ✎ "] {
                if tx + width(pill) as u16 + 2 < room {
                    tx = parts::pill(&mut g, tx + 1, 0, pill, p.purple, p.bg0, p.bg0, a);
                    break;
                }
            }
        }
        if draft > 0 {
            let t = format!(" {draft} note{} ", if draft == 1 { "" } else { "s" });
            if tx + width(&t) as u16 + 2 < room {
                parts::tag(&mut g, tx + 1, 0, &t, p.purple, p.bg0, a);
            }
        }
        if meta_fits {
            // "3 new" in the change bars' orange, the age dim beside it
            let x = w.saturating_sub(mw + 2);
            g.put(x, 0, &meta, Style::fg(p.grey1).on(p.bg0));
            if !meta_new.is_empty() {
                g.put(x + 1, 0, &meta_new, Style::fg(p.orange).on(p.bg0));
            }
        }

        // the live strip: what Claude is doing right now, from the tool hooks. It takes the help
        // line's row; the keys come back while sulin has the pane focused
        let live = live_activity(inp.activity, inp.wall);
        if h >= 12 && (!focus_on || self.help_open || glance) {
            if let Some((act, agents)) = &live {
                // a cell of padding inside each cap
                let r = Rect::new(3, help_y, w.saturating_sub(6), 1);
                // its own band: a faint blue (Claude's colour) across the row, so what's happening
                // now reads apart from the content above (sulin, 2026-11-08)
                let band = p.blue.mix(p.bg0, 0.84);
                g.fill(Rect::new(1, help_y, w.saturating_sub(2), 1), Style::default().on(band));
                // its ends: Powerline's slanted-bottom caps in the band's colour (sulin picked them
                // from a row of options, 2026-11-08)
                g.put(1, help_y, STRIP_CAP_L, Style::fg(band).on(p.bg0));
                g.put(w.saturating_sub(2), help_y, STRIP_CAP_R, Style::fg(band).on(p.bg0));
                let age = (inp.wall - crate::util::parse_rfc3339(&act.since).unwrap_or(inp.wall)).max(0.0);
                let (gl, gc, text, tc) = match act.state.as_str() {
                    "running" => (parts::spinner(now, m).to_string(), p.yellow, act.text.clone(), p.fg),
                    "idle" => (icon::BELL.to_string(), p.purple, "your turn".to_string(), p.purple),
                    "waiting" => (icon::HOURGLASS.to_string(), p.purple, format!("waiting on you: {}", act.text), p.purple),
                    _ => (icon::BULB.to_string(), p.grey1, if act.text.is_empty() { "thinking".to_string() } else { act.text.clone() }, p.grey1),
                };
                let x = g.put_clipped(3, help_y, &gl, Style::fg(gc).on(band).bold(), r);
                // no tool pill: the strip says what's happening in words, not which tool (sulin,
                // 2026-11-08: "bash shouldn't have a pill in the bottom bar")
                let mut right = components::age_short(age);
                if *agents > 0 {
                    // the strip of glance mode has no room for the word
                    right = if glance { format!("+{agents}  {right}") } else { format!("+{agents} agent{}  {right}", if *agents == 1 { "" } else { "s" }) };
                }
                let rw = vellum::text::width(&right) as u16;
                let room = (r.x + r.w).saturating_sub(x + 2 + rw + 2) as usize;
                g.put_clipped(x + 1, help_y, &vellum::text::truncate(&text, room), Style::fg(tc).on(band).italic_if(act.state == "thinking"), r);
                g.put_clipped((r.x + r.w).saturating_sub(rw), help_y, &right, Style::fg(p.grey1).on(band), r);
                // spinner frames while a tool runs, else the age's own pace
                wake.every(now, if act.state == "running" && m.on { 0.1 } else { age_pace(age) });
            }
        }
        // toasts: full-width boxes at the bottom of the view, the oldest lowest and newer ones
        // stacking above it, two at most (sulin, 2026-11-08: "take the entire width, always be on
        // the bottom, stack, borders", then "oldest at the bottom")
        {
            let mut shown: Vec<&(crate::toast::Toast, f32)> = self.toasts.iter().collect();
            shown.sort_by(|a, b| a.0.sent.cmp(&b.0.sent));
            let shown = &shown[shown.len().saturating_sub(2)..];
            let mut y = vbot;
            for (t, left) in shown.iter() {
                if y < vtop + 3 {
                    break;
                }
                y -= 3;
                let style = if t.countdown.is_empty() { inp.cfg.toast_countdown.as_str() } else { t.countdown.as_str() };
                let border = if t.border.is_empty() { inp.cfg.toast_border.as_str() } else { t.border.as_str() };
                // inset a cell from each side of the pane
                draw_toast(&mut g, p, t, *left, 2, y, lw.saturating_sub(4), style, border);
                self.hits.push((2, y, lw.saturating_sub(4), format!("mg:toast/{}", t.id)));
                // the whole box, border and padding too: `hit_at` asks here first
                self.overlays.push((Rect::new(2, y, lw.saturating_sub(4), 3), format!("mg:toast/{}", t.id)));
            }
            if self.toasts.iter().any(|(t, l)| *l < 1.0 && t.level != "error") {
                wake.every(now, 0.2);
            }
        }
        // a search being typed takes the help row: `/query▏`
        if let Some(q) = self.prompt.as_deref().filter(|_| h >= 12) {
            g.fill(Rect::new(1, help_y, w.saturating_sub(2), 1), Style::default().on(p.bg0));
            let x = g.put_clipped(2, help_y, "/", Style::fg(p.aqua).on(p.bg0).bold(), Rect::new(2, help_y, w.saturating_sub(3), 1));
            let x = g.put_clipped(x, help_y, q, Style::fg(p.fg).on(p.bg0), Rect::new(2, help_y, w.saturating_sub(3), 1));
            g.put_clipped(x, help_y, "▏", Style::fg(p.aqua).on(p.bg0), Rect::new(2, help_y, w.saturating_sub(3), 1));
        }
        // help line: only the keys that do something on the focused component (k9s, gitui)
        if h >= 12 && self.prompt.is_none() && self.review && !glance {
            // review mode: the reactions, then how to leave
            let keys = [("drag", "quote"), ("click", "row"), ("+", "more"), ("-", "less"), ("=", "keep"), ("!", "disagree"), ("?", "why"), ("esc", "done")];
            g.fill(Rect::new(1, help_y, w.saturating_sub(2), 1), Style::default().on(p.bg0));
            parts::help_line(&mut g, 2, help_y, &keys, p, p.bg0, Rect::new(2, help_y, w.saturating_sub(3), 1));
        }
        if h >= 12 && self.prompt.is_none() && !self.review && !glance && (live.is_none() || (focus_on && !self.help_open)) {
            let mut keys: Vec<(&str, &str)> = vec![("j/k", "move")];
            match self.focus_kind.as_str() {
                "section" => keys.push(("za", "fold")),
                "todo" => keys.push(("space", "cycle")),
                "question" => keys.push(("1-9", "pick")),
                "claim" => keys.push(("x", "strike")),
                "diff" => keys.extend([("a", "approve"), ("r", "reject")]),
                "link" => keys.push(("⏎", "open")),
                // a status, a row, a table's row: whatever has a target opens it
                _ if self.focus.as_ref().and_then(|f| inp.doc.find(f)).map(|b| b.opt("target").is_some()).unwrap_or(false) => keys.push(("⏎", "open")),
                _ => {}
            }
            // inside a tabs component (or on its strip): the keys that switch its tabs
            let mut at = self.focus.as_ref().and_then(|f| inp.doc.find(f));
            while let Some(b) = at {
                if b.kind == "tabs" {
                    keys.push(("]t/[t", "switch tab"));
                    break;
                }
                at = inp.doc.parent_of(&b.id);
            }
            // the panel's keys while it is open; closed, its key shows where there is detail
            let has_detail = self.focus.as_ref().and_then(|f| inp.doc.find(f)).map(|b| b.opt("detail").is_some() || b.f.contains_key("cells")).unwrap_or(false);
            if self.detail_rect.is_some() {
                if self.detail_max > 0 {
                    keys.push(("J/K", "scroll detail"));
                }
                keys.push(("d", "close detail"));
            } else if has_detail || self.help_open {
                keys.push(("d", "detail"));
            }
            if self.tabs.len() > 1 && self.help_open {
                keys.push(("←/→", "tabs"));
            }
            if self.help_open {
                keys.extend([("gg/G", "top/end"), ("zh", "fold head"), ("]]/[[", "sections"), ("]c/[c", "changes"), ("/", "find"), ("c", "review"), ("i", "ids"), ("S", "stop Claude"), ("zM/zR", "fold all"), ("m", if inp.mouse { "mouse off" } else { "mouse on" }), ("q", "quit")]);
            } else {
                // while changes wait unseen, the key that walks them
                if self.unseen_n > 0 {
                    keys.push(("]c", "next change"));
                }
                keys.push(("?", "more"));
            }
            parts::help_line(&mut g, 2, help_y, &keys, p, p.bg0, Rect::new(2, help_y, w.saturating_sub(3), 1));
        }
        // the reminders: the author's keys in aqua (the pane's own keys below are grey)
        for (i, row) in hint_rows.iter().enumerate() {
            let y = help_y - hints_h + i as u16;
            let clip = Rect::new(2, y, w.saturating_sub(4), 1);
            let mut x = 2u16;
            for (s, k) in row {
                let st = match k {
                    HintRun::Key => Style::fg(p.aqua).on(p.bg0).bold(),
                    HintRun::Text => Style::fg(p.grey2).on(p.bg0),
                    HintRun::Sep => Style::fg(p.bg4).on(p.bg0),
                };
                x = g.put_clipped(x, y, s, st, clip);
            }
        }
        // a half-typed count or sequence, vim's showcmd
        if !self.pending_keys.is_empty() && h >= 12 {
            let pw = width(&self.pending_keys) as u16;
            g.put(w.saturating_sub(pw + 2), help_y, &self.pending_keys, Style::fg(p.yellow).on(p.bg0).bold());
        }

        // the Lip Gloss status bar
        let open_q = {
            let mut n = 0;
            let empty = HashMap::new();
            let cx = self.ctx(inp, p, &empty, &empty);
            walk(&inp.doc.blocks, &mut |b, _, _| {
                if b.kind == "question" && !components::question_resolved(&cx, b) && !inp.state.decisions.contains_key(&b.id) {
                    n += 1;
                }
            });
            n
        };
        let mut nuggets: Vec<(String, Rgb)> = Vec::new();
        if !m.on {
            nuggets.push(("motion off".into(), Rgb::hex("#4a4a48")));
        }
        if inp.mouse {
            nuggets.push(("mouse".into(), Rgb::hex("#4a4a48")));
        }
        if open_q > 0 {
            nuggets.push((format!("{} {open_q} open", icon::QUESTION), LIPGLOSS_STATUS.nugget_a));
        }
        nuggets.push((
            if inp.unread > 0 { format!("{} {} for Claude", icon::SEND, inp.unread) } else { format!("{} Claude is current", icon::CHECK) },
            LIPGLOSS_STATUS.nugget_b,
        ));
        let sb = StatusBar { key: &format!("{} MARGIN", icon::LEAF), key_bg: None, text: &inp.doc.id, dim_text: &project_label(inp.project), nuggets };
        // glance mode: no status bar, the strip is the title and the head
        if !glance {
            parts::status_bar(&mut g, Rect::new(0, status_y, w, 1), &sb, &LIPGLOSS_STATUS, a);
        }
        let _ = self.loaded_at;
        Frame { grid: g, images, wake }
    }

    fn ctx<'a>(&'a self, inp: &'a Input, p: &'a vellum::color::Palette, flash: &'a HashMap<String, f32>, prog: &'a HashMap<String, f32>) -> Ctx<'a> {
        Ctx {
            doc: inp.doc,
            state: inp.state,
            p,
            now: inp.now,
            wall: inp.wall,
            m: inp.cfg.motion(),
            delivered: inp.delivered,
            flash,
            progress: prog,
            kitty: inp.kitty,
            cell: inp.cell,
            images: &self.images,
            focused: if inp.focused_pane { self.focus.as_deref() } else { None },
            pulse: &self.pulse,
            pulse_period: inp.cfg.pulse_period,
            pulse_breaths: inp.cfg.pulse_breaths,
            mouse: inp.mouse,
            hover: self.hover.as_deref(),
            flog: inp.cfg.graph_glyphs == "flog",
        }
    }

    /// A `tabs` component's strip: its sections' titles on one row over a thin rule, so the
    /// strip reads as the header of what is under it. It speaks the pane's tab bar's language a
    /// step quieter: the section on show is a pill on the raised ground (the top bar's is a
    /// saturated fill), the others dim text in the cells a pill would take, one of them with the
    /// count of blocks in it that changed since he looked. Each is a button
    /// (`mg:subtab/<tabs id>/<section id>`) over its whole pill. Titles are labels: they shorten
    /// to fit.
    fn draw_subtabs(&self, inp: &Input, b: &Block, w: u16, depth: usize, first: bool) -> components::Drawn {
        let p = &EVERFOREST;
        let top: u16 = if first { 0 } else { 1 };
        let mut g = Grid::new(w, 2 + top);
        g.fill(g.area(), Style::default().on(p.bg0));
        let x0: u16 = if depth > 0 { 3 } else { 1 };
        // the rule under the strip, the width of the block
        for x in x0..w.saturating_sub(1) {
            g.put(x, top + 1, "─", Style::fg(p.bg2).on(p.bg0));
        }
        let on = subtab_active(b, inp.view).map(|c| c.id.as_str());
        let unseen = |c: &Block| {
            let mut n = 0;
            walk(std::slice::from_ref(c), &mut |x, _, _| n += self.retained.get(&x.id).map(|a| a.change.is_some()) .unwrap_or(false) as usize);
            n
        };
        let tabs: Vec<(&Block, usize)> = b.children.iter().map(|c| (c, if Some(c.id.as_str()) == on { 0 } else { unseen(c) })).collect();
        let room = w.saturating_sub(x0 + 1) as usize;
        // a cap (or its blank) and a blank each side of the label
        let need = |cap: usize| tabs.iter().map(|(c, n)| 4 + width(&truncate(subtab_label(c), cap)) + if *n > 0 { 2 + n.to_string().len() } else { 0 }).sum::<usize>();
        let cap = [40usize, 24, 16, 10, 6].into_iter().find(|&c| need(c) <= room).unwrap_or(4);
        let clip = Rect::new(x0, top, w.saturating_sub(x0 + 1), 1);
        let mut buttons = Vec::new();
        let mut x = x0;
        for (c, n) in &tabs {
            let active = Some(c.id.as_str()) == on;
            let xs = x;
            let label = truncate(subtab_label(c), cap);
            if active {
                x = parts::pill(&mut g, x, top, &format!(" {label} "), p.bg2, p.fg, p.bg0, clip);
            } else {
                x = g.put_clipped(x, top, &format!("  {label}"), Style::fg(p.grey1).on(p.bg0), clip);
                if *n > 0 {
                    x = g.put_clipped(x, top, &format!(" +{n}"), Style::fg(p.orange).on(p.bg0), clip);
                }
                x = g.put_clipped(x, top, "  ", Style::default().on(p.bg0), clip);
            }
            if x > xs {
                buttons.push((xs, top, x - xs, format!("mg:subtab/{}/{}", b.id, c.id)));
            }
        }
        components::Drawn { grid: Some(g), buttons, lead: top, ..Default::default() }
    }

    /// Draw the head's lines from row `y0`, at most `max_rows` of them, and return the rows used.
    /// One row each, cut with an ellipsis; `wrap` (glance mode) wraps each to the width instead.
    /// Every row is a button to its block (`mg:head/<id>`), and each id on the needs line its own.
    #[allow(clippy::too_many_arguments)]
    fn draw_head(&mut self, g: &mut Grid, lines: &[HeadLine], inp: &Input, y0: u16, max_rows: u16, wrap: bool, wake: &mut Wake) -> u16 {
        let p = &EVERFOREST;
        let m = inp.cfg.motion();
        // the list's columns: the detail panel may hold the rest
        let w = if self.list_w > 0 { self.list_w.min(g.w) } else { g.w };
        let (gx, tx) = (2u16, 2 + components::TX);
        // text ends a cell short of the scrollbar's column, as rows do
        let edge = w.saturating_sub(2);
        let end = y0 + max_rows;
        let mut y = y0;
        for l in lines {
            if y >= end {
                break;
            }
            let (glyph, gc, tc) = match l.kind {
                HeadKind::Purpose => (icon::PIN.to_string(), p.green, p.grey2),
                HeadKind::Running => (parts::spinner(inp.now, m).to_string(), p.yellow, p.fg),
                HeadKind::Needs => (icon::BELL.to_string(), p.purple, p.purple),
            };
            if l.kind == HeadKind::Running {
                // spinner frames, or just the elapsed time's pace
                wake.every(inp.now, if m.on { 0.1 } else { 1.0 });
            }
            let tok = format!("mg:head/{}", l.target);
            let text = format!("{}{}", l.text, l.ids.join(", "));
            g.put(gx, y, &glyph, Style::fg(gc).on(p.bg0).bold());
            if wrap {
                // three rows a line at most: a long goal must not push the other two off the strip
                const WRAP_MAX: usize = HEAD_WRAP;
                let tw = edge.saturating_sub(tx).max(1) as usize;
                let mut rows = vellum::text::wrap(&text, tw);
                if rows.len() > WRAP_MAX {
                    rows.truncate(WRAP_MAX);
                    let last = rows.pop().unwrap_or_default();
                    rows.push(format!("{}…", truncate(&last, tw.saturating_sub(1)).trim_end_matches('…')));
                }
                // the elapsed time ends the last row, which gives up text for it
                let right = l.right.replace("  ", " · ");
                let rw = width(&right);
                if rw > 0 && rw + 4 < tw {
                    let last = rows.pop().unwrap_or_default();
                    rows.push(truncate(&last, tw - rw - 3).into_owned());
                }
                let n = rows.len();
                for (i, row) in rows.iter().enumerate() {
                    if y >= end {
                        break;
                    }
                    if i + 1 == n && rw > 0 && rw + 4 < tw {
                        g.put(edge.saturating_sub(rw as u16), y, &right, Style::fg(p.grey1).on(p.bg0));
                    }
                    g.put_clipped(tx, y, row, Style::fg(tc).on(p.bg0), Rect::new(tx, y, edge.saturating_sub(tx), 1));
                    if !l.target.is_empty() {
                        self.hits.push((1, y, w.saturating_sub(2), tok.clone()));
                    }
                    y += 1;
                }
                continue;
            }
            // authored text wraps, at every width (sulin, 2026-11-10: only what feeds pull in may
            // be cut): up to HEAD_WRAP rows a line. The elapsed time stays on the first row, and
            // the text wraps clear of it there
            let rw = width(&l.right) as u16;
            let full = edge.saturating_sub(tx).max(1) as usize;
            let first_w = full.saturating_sub(if rw > 0 { rw as usize + 2 } else { 0 }).max(1);
            let mut rows = if inp.view.head_folded { vec![truncate(&text, first_w).into_owned()] } else { vellum::text::wrap(&text, first_w) };
            if rw > 0 && rows.len() > 1 {
                // the rows after the first have the whole width
                let head = rows.remove(0);
                let rest = text.strip_prefix(head.as_str()).unwrap_or("").trim_start().to_string();
                rows = std::iter::once(head).chain(vellum::text::wrap(&rest, full)).collect();
            }
            if rows.len() > HEAD_WRAP {
                rows.truncate(HEAD_WRAP);
                let last = rows.pop().unwrap_or_default();
                rows.push(format!("{}…", truncate(&last, full.saturating_sub(1)).trim_end_matches('…')));
            }
            // each id is its own target, ahead of its row's: found in order in the wrapped rows
            let mut ids = l.ids.iter().peekable();
            for (i, row) in rows.iter().enumerate() {
                if y >= end {
                    break;
                }
                let row = if i == 0 { row.as_str() } else { row.trim_start() };
                g.put_clipped(tx, y, row, Style::fg(tc).on(p.bg0), Rect::new(tx, y, edge.saturating_sub(tx), 1));
                if i == 0 && rw > 0 {
                    g.put(edge.saturating_sub(rw), y, &l.right, Style::fg(p.grey1).on(p.bg0));
                }
                let mut from = if i == 0 { l.text.len().min(row.len()) } else { 0 };
                while let Some(id) = ids.peek() {
                    let Some(at) = row.get(from..).and_then(|r| r.find(id.as_str())) else { break };
                    let x = tx + width(&row[..from + at]) as u16;
                    self.hits.push((x, y, width(id) as u16, format!("mg:head/{id}")));
                    from += at + id.len();
                    ids.next();
                }
                if !l.target.is_empty() {
                    self.hits.push((1, y, w.saturating_sub(2), tok.clone()));
                }
                y += 1;
            }
        }
        y - y0
    }

    /// Bring a block to the top of the view on the next frame (a click on a head line).
    pub fn goto(&mut self, id: &str) {
        self.goto = Some(id.to_string());
    }

    /// The changes tour (`]c`, `[c`): the next block, forward or back and wrapping, that changed
    /// since sulin last looked. Starts from where the tour last stopped, else the focus, else
    /// the top of the view. Blocks inside a folded section aren't in the list and are skipped.
    pub fn change_step(&mut self, forward: bool) -> Option<String> {
        let stops: Vec<usize> = self
            .order
            .iter()
            .enumerate()
            .filter(|(_, id)| self.changed.contains(id) || self.retained.get(id).map(|n| n.change.is_some()).unwrap_or(false))
            .map(|(i, _)| i)
            .collect();
        let pos = |id: &String| self.order.iter().position(|x| x == id);
        let toured = self.tour_at.as_ref().and_then(pos);
        let next = match toured.or_else(|| self.focus.as_ref().and_then(pos)).or_else(|| self.anchor.as_ref().and_then(|a| pos(&a.0))) {
            // a tour not begun starts on the first change at or after where he is
            Some(here) if forward => stops.iter().find(|&&i| i > here || (toured.is_none() && i == here)).or(stops.first()),
            Some(here) => stops.iter().rev().find(|&&i| i < here).or(stops.last()),
            None if forward => stops.first(),
            None => stops.last(),
        };
        let id = self.order[*next?].clone();
        self.tour_at = Some(id.clone());
        self.reveal(&id);
        Some(id)
    }

    fn set_anchor_from_scroll(&mut self) {
        // the topmost item whose span covers the scroll line
        let mut best: Option<(&String, i32)> = None;
        for (id, &(y, h)) in &self.item_pos {
            if id.starts_with('~') || h == 0 {
                continue;
            }
            if y <= self.scroll && self.scroll < y + h as i32 && best.map(|(_, by)| y > by).unwrap_or(true) {
                best = Some((id, y));
            }
        }
        self.anchor = best.map(|(id, y)| (id.clone(), self.scroll - y));
    }

    /// The token of the overlay (a toast) covering (x, y), if one does.
    pub fn overlay_at(&self, x: u16, y: u16) -> Option<&str> {
        self.overlays.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(_, t)| t.as_str())
    }

    /// The button token at (x, y), if a button is drawn there. An overlay wins over the buttons
    /// it covers: dismissing a toast must not press what he can't see under it.
    pub fn hit_at(&self, x: u16, y: u16) -> Option<&str> {
        if let Some(t) = self.overlay_at(x, y) {
            return Some(t);
        }
        self.hits.iter().find(|(hx, hy, hw, _)| *hy == y && x >= *hx && x < hx + hw).map(|(_, _, _, t)| t.as_str())
    }

    /// Is (x, y) in the detail panel (its rule included)?
    pub fn in_detail(&self, x: u16, y: u16) -> bool {
        self.detail_rect.map(|r| r.contains(x, y)).unwrap_or(false)
    }

    /// The block whose row is at (x, y): `item_at`, but never a row beside or behind the detail
    /// panel. What the pointer uses.
    pub fn row_at(&self, x: u16, y: u16) -> Option<&str> {
        if self.in_detail(x, y) || x >= self.list_w.max(1) {
            return None;
        }
        self.item_at(y)
    }

    /// Scroll the detail panel by `d` lines (`J` / `K`, the wheel over it).
    pub fn detail_scroll_by(&mut self, d: i32) {
        self.detail_scroll = (self.detail_scroll as i32 + d).clamp(0, self.detail_max as i32) as usize;
    }

    /// The block drawn at screen row `y`, if any (ghosts and the rule excluded).
    pub fn item_at(&self, y: u16) -> Option<&str> {
        // a row under a toast isn't the block he's pointing at
        if y < self.view_top || y >= self.view_top + self.view_h || self.overlays.iter().any(|(r, _)| y >= r.y && y < r.bottom()) {
            return None;
        }
        let cy = (y - self.view_top) as i32 + self.scroll;
        self.item_pos
            .iter()
            .filter(|(id, &(iy, h))| !id.starts_with('~') && h > 0 && iy <= cy && cy < iy + h as i32)
            .map(|(id, _)| id.as_str())
            .next()
    }

    pub fn scroll_by(&mut self, d: i32) {
        self.scroll = (self.scroll + d).clamp(0, (self.total_h - self.view_h as i32).max(0));
        self.set_anchor_from_scroll();
    }

    pub fn move_focus(&mut self, d: i32) {
        if self.focusable.is_empty() {
            return;
        }
        let i = self.focus.as_ref().and_then(|f| self.focusable.iter().position(|x| x == f)).map(|i| i as i32).unwrap_or(-1);
        let n = (i + d).clamp(0, self.focusable.len() as i32 - 1) as usize;
        self.focus = Some(self.focusable[n].clone());
        self.scroll_to_focus = true;
    }

    /// Bring an item into view: focus it if it takes focus, else scroll it to the top.
    pub fn reveal(&mut self, id: &str) {
        if self.focusable.iter().any(|f| f == id) {
            self.focus = Some(id.to_string());
            self.scroll_to_focus = true;
        } else if let Some(&(y, _)) = self.item_pos.get(id) {
            self.scroll = y.clamp(0, (self.total_h - self.view_h as i32).max(0));
            self.set_anchor_from_scroll();
        }
    }

    /// The next item (forward or back, wrapping) whose text or title holds `q`, ignoring case,
    /// starting after the focused item or the top of the view.
    pub fn find(&self, doc: &Doc, q: &str, forward: bool) -> Option<String> {
        let q = q.to_lowercase();
        if q.is_empty() || self.order.is_empty() {
            return None;
        }
        let here = self.focus.as_ref().or(self.anchor.as_ref().map(|a| &a.0)).and_then(|f| self.order.iter().position(|x| x == f)).unwrap_or(0);
        let n = self.order.len();
        (1..=n)
            .map(|k| if forward { (here + k) % n } else { (here + n - k % n) % n })
            .map(|i| &self.order[i])
            .find(|id| doc.find(id).map(|b| format!("{} {}", b.s("text"), b.s("title")).to_lowercase().contains(&q)).unwrap_or(false))
            .cloned()
    }

    /// Move focus `n` section headers forward or back (`]]`, `[[`).
    pub fn section_step(&mut self, doc: &Doc, n: i32) {
        let secs: Vec<&String> = self.focusable.iter().filter(|id| doc.find(id).map(|b| b.kind == "section").unwrap_or(false)).collect();
        if secs.is_empty() {
            return;
        }
        let pos = |id: &str| self.order.iter().position(|x| x == id).unwrap_or(0) as i32;
        let here = self.focus.as_deref().map(pos).unwrap_or(-1);
        let target = if n > 0 {
            secs.iter().filter(|s| pos(s) > here).nth(n as usize - 1).or(secs.last())
        } else {
            secs.iter().rev().filter(|s| pos(s) < here).nth((-n) as usize - 1).or(secs.first())
        };
        if let Some(t) = target.map(|t| t.to_string()) {
            self.focus = Some(t);
            self.scroll_to_focus = true;
        }
    }

    pub fn flash(&mut self, id: &str, now: f64) {
        self.retained.flash(id, now);
        self.pulse.remove(id);
    }

    /// Blocks changed since this scene's artifact was last seen: a tab's badge while another tab
    /// is on screen (the app feeds a tab out of view through `set_doc`, never through `frame`).
    pub fn unseen_count(&self) -> usize {
        self.retained.nodes.values().filter(|n| n.unseen && !n.leaving).count()
    }
}

fn components_resolved(inp: &Input, b: &Block) -> bool {
    let empty = HashMap::new();
    let imgs = HashMap::new();
    let pulse = HashMap::new();
    let cx = Ctx {
        doc: inp.doc,
        state: inp.state,
        p: &EVERFOREST,
        now: inp.now,
        wall: inp.wall,
        m: inp.cfg.motion(),
        delivered: inp.delivered,
        flash: &empty,
        progress: &empty,
        kitty: false,
        cell: inp.cell,
        images: &imgs,
        focused: None,
        pulse: &pulse,
        pulse_period: 1.0,
        pulse_breaths: 0.0,
        mouse: false,
        hover: None,
        flog: false,
    };
    components::question_resolved(&cx, b)
}

/// The activity the strip shows: the main session's latest, else a subagent's, plus how many
/// subagents are active. Activity older than its freshness window is a session that went away.
/// One toast as a box three rows tall at (x, y), `w` wide: a border in the level's colour, the
/// glyph and text inside, the row it's about (if any) dim on the right. No countdown on show:
/// it stays fully lit, then fades into the background over its last moments (sulin didn't like
/// a draining edge, 2026-11-08).
fn draw_toast(g: &mut Grid, p: &vellum::color::Palette, t: &crate::toast::Toast, left: f32, x: u16, y: u16, w: u16, style: &str, border: &str) {
    if w < 8 {
        return;
    }
    let (gl, col) = match t.level.as_str() {
        "ok" => (icon::DONE, p.green),
        "warn" => (icon::WARN, p.yellow),
        "error" => (icon::BLOCKED, p.red),
        _ => (icon::INFO, p.blue),
    };
    // the fade: the last ~20% of its time (1.5 s of 8) eases everything toward the background;
    // every style fades at the very end, "fade" shows nothing else
    let fade = (1.0 - (left / 0.2).clamp(0.0, 1.0)) as f32;
    let errorish = t.level == "error";
    let dim = |c: vellum::color::Rgb| c.mix(p.bg0, fade * 0.9);
    let (col, fg_main, fg_dim) = (dim(col), dim(p.fg), dim(p.grey1));
    // a ground tinted with the level's colour
    let ground = col.mix(p.bg0, 0.78).mix(p.bg0, fade);
    let area = Rect::new(x, y, w, 3);
    let inner = w.saturating_sub(2);
    let corners = match border {
        "light" => Some(parts::Corners::Light),
        "heavy" => Some(parts::Corners::Heavy),
        "double" => Some(parts::Corners::Double),
        "none" => None,
        _ => Some(parts::Corners::Round),
    };
    match corners {
        Some(c) => {
            g.fill(Rect::new(x + 1, y + 1, inner, 1), Style::default().on(ground));
            parts::frame(g, area, &parts::Ring::Plain(col), c, p.bg0, 0.0, Motion::off(), area);
        }
        // no frame: the tinted ground fills the whole box
        None => g.fill(area, Style::default().on(ground)),
    }
    // the countdown, by style; an error has none: it waits for a click
    let secs = (left.clamp(0.0, 1.0) as f64 * t.ttl).ceil() as u32;
    let mut right_w = 0u16;
    if !errorish {
        match style {
            "bar" => {
                // ▰ for the time left, ▱ for the time spent (sulin's pick of symbols)
                let n = 12usize;
                let on = ((left.clamp(0.0, 1.0) * n as f32).ceil() as usize).min(n);
                let s: String = (0..n).map(|i| if i < on { '▰' } else { '▱' }).collect();
                right_w = n as u16 + 1;
                g.put(x + w.saturating_sub(right_w + 1), y + 1, &s, Style::fg(col).on(ground));
            }
            "number" => {
                let s = format!("{secs}s");
                right_w = width(&s) as u16 + 1;
                g.put(x + w.saturating_sub(right_w + 1), y + 1, &s, Style::fg(col).on(ground).bold());
            }
            "dots" => {
                let n = 8usize;
                let on = ((left.clamp(0.0, 1.0) * n as f32).ceil() as usize).min(n);
                let s: String = (0..n).map(|i| if i < on { '●' } else { '○' }).collect();
                right_w = n as u16 + 1;
                g.put(x + w.saturating_sub(right_w + 1), y + 1, &s, Style::fg(col).on(ground));
            }
            "pie" => {
                // Nerd Font's circle slices, 8 steps from full to one slice
                let k = ((left.clamp(0.0, 1.0) * 8.0).ceil() as u32).clamp(1, 8);
                let gl = char::from_u32(0xF0A9E + k - 1).unwrap_or('●');
                let mut b = [0u8; 4];
                right_w = 2;
                g.put(x + w.saturating_sub(right_w + 1), y + 1, gl.encode_utf8(&mut b), Style::fg(col).on(ground).bold());
            }
            "edge" => {
                let lit = ((inner as f32) * left.clamp(0.0, 1.0)).round() as u16;
                for i in 0..inner {
                    g.put(x + 1 + i, y + 2, "─", Style::fg(if i < lit { col } else { p.bg3 }).on(p.bg0));
                }
            }
            _ => {}
        }
    }
    let gx = g.put(x + 2, y + 1, gl, Style::fg(col).on(ground).bold());
    let about = if t.at.is_empty() { String::new() } else { format!("on {}", t.at) };
    let aw = width(&about) as u16 + if right_w > 0 { right_w + 1 } else { 0 };
    let room = inner.saturating_sub(aw + 6) as usize;
    // two cells after the glyph, as rows have (a nerd-font icon draws over its neighbour)
    g.put(gx + 2, y + 1, &vellum::text::truncate(&t.text, room), Style::fg(fg_main).on(ground).bold());
    if !about.is_empty() {
        g.put(x + w.saturating_sub(aw + 2), y + 1, &about, Style::fg(fg_dim).on(ground));
    }
}

/// A note's reaction as one gutter glyph.
pub fn note_glyph(r: &str) -> &'static str {
    match r {
        "more" => "+",
        "less" => "−",
        "keep" => "✓",
        "disagree" => "✗",
        "why" => "?",
        _ => "•",
    }
}

fn live_activity(acts: &[crate::store::Activity], wall: f64) -> Option<(crate::store::Activity, usize)> {
    let age = |a: &crate::store::Activity| wall - crate::util::parse_rfc3339(&a.since).unwrap_or(0.0);
    // a turn that ended stays "your turn" for an hour; a session silent mid-turn for 15 minutes is gone
    let fresh = |a: &&crate::store::Activity| age(a) < if a.state == "idle" { 3600.0 } else { 900.0 };
    let agents = acts.iter().filter(|a| !a.agent.is_empty() && a.state != "idle" && age(a) < 300.0).count();
    let main = acts.iter().filter(|a| a.agent.is_empty()).find(fresh).or_else(|| acts.iter().filter(|a| !a.agent.is_empty() && age(a) < 300.0).next())?;
    let agents = if main.agent.is_empty() { agents } else { agents.saturating_sub(1) };
    Some((main.clone(), agents))
}

/// The plan bar's cells on an agent's row.
const AGENT_BAR_W: u16 = 5;
/// The phase column: the longest phase word, `waiting 12m`.
const AGENT_PHASE_W: u16 = 11;

/// Where the columns of the agents' rows sit, shared by every row so they line up: the phase
/// word, the plan bar, and the plan's count with the widest count's width.
struct AgentCols {
    phase_x: u16,
    bar_x: u16,
    count_x: u16,
    count_w: u16,
}

/// The agents' columns for an item grid `iw` wide. The name column is as wide as the longest
/// name when the row has the room, and never under 12 cells.
fn agent_cols(groups: &[crate::agents::Group], iw: u16, mouse: bool) -> AgentCols {
    let all = || groups.iter().flat_map(|g| &g.agents);
    let count_w = all().filter_map(|a| a.progress.as_ref()).map(|p| format!("{}/{}", p.done, p.total).len() as u16).max().unwrap_or(0);
    let plan_w = if count_w > 0 { AGENT_BAR_W + 1 + count_w + 2 } else { 0 };
    // a fan-out's rows sit two cells in; the columns are measured from there for every row
    let who_x = 5 + components::TX;
    let fixed = who_x + 2 + AGENT_PHASE_W + 1 + plan_w + if mouse { 9 } else { 0 } + 6;
    let widest = all().map(|a| width(&a.who()) as u16).max().unwrap_or(0);
    let who_w = widest.min(iw.saturating_sub(fixed).max(12));
    let phase_x = who_x + who_w + 2;
    let bar_x = phase_x + AGENT_PHASE_W + 1;
    AgentCols { phase_x, bar_x, count_x: bar_x + AGENT_BAR_W + 1, count_w }
}

/// How often a printed age needs a frame: each second under a minute, each minute under an hour,
/// then hourly. An idle pane showing "3h ago" should sleep, not wake every 30 s to redraw it.
fn age_pace(age: f64) -> f64 {
    if age < 60.0 {
        1.0
    } else if age < 3600.0 {
        60.0
    } else {
        3600.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn frame_text(sc: &mut Scene, doc: &Doc, now: f64) -> String {
        frame_text_w(sc, doc, now, 60)
    }

    fn frame_text_w(sc: &mut Scene, doc: &Doc, now: f64, w: u16) -> String {
        let cfg = Config::default();
        let (state, view) = (State::default(), View::default());
        let inp = Input {
            doc, state: &state, view: &view, delivered: 0, unread: 0, now, wall: 0.0, cfg: &cfg, kitty: false,
            cell: (10, 20), focused_pane: false, mouse: false, project: "p", w, h: 16, activity: &[],
        };
        sc.frame(&inp).grid.to_text()
    }

    /// A proposal shows under its component with accept and decline until sulin answers that
    /// proposal; a newer one (another `at`) shows again.
    #[test]
    fn proposal_shows_until_answered() {
        let cfg = Config::default();
        let view = View::default();
        let doc: Doc = serde_json::from_value(json!({"id": "a", "rev": 1, "blocks": [
            {"type": "question", "id": "Q1", "text": "cli or mcp?", "options": ["cli", "mcp"], "default": "cli",
             "proposal": {"verb": "pick", "value": "mcp", "why": "the agent needs it", "at": "2026-11-06T10:00:00Z"}}]})).unwrap();
        let text = |state: &State| {
            let mut sc = Scene::new(&cfg);
            let inp = Input { doc: &doc, state, view: &view, delivered: 0, unread: 0, now: 0.0, wall: 0.0, cfg: &cfg, kitty: false,
                cell: (10, 20), focused_pane: false, mouse: false, project: "p", w: 70, h: 20, activity: &[] };
            sc.frame(&inp).grid.to_text()
        };
        let open = text(&State::default());
        assert!(open.contains("proposes") && open.contains("pick mcp") && open.contains("mg:accept/Q1") && open.contains("mg:decline/Q1"), "{open}");
        let mut st = State::default();
        let dec = |at: &str| crate::store::Decision { kind: "proposal".into(), value: json!({"at": at, "accepted": false}), by: crate::who::HUMAN.into(), at: at.into(), reason: None, via: None, seq: 1, under: None };
        st.decisions.insert("Q1#proposal".into(), dec("2026-11-06T10:00:00Z"));
        let done = text(&st);
        assert!(!done.contains("proposes") && !done.contains("Orphaned"), "{done}");
        st.decisions.insert("Q1#proposal".into(), dec("2026-11-06T09:00:00Z"));
        assert!(text(&st).contains("proposes"), "an answer to an older proposal leaves the new one open");
    }

    /// The title edge carries a count badge of blocks changed since sulin last looked, so a pane he
    /// isn't reading says how much moved (there is no in-list rule: a row of its own made the content jump).
    #[test]
    fn title_badge_counts_unseen_changes() {
        let cfg = Config::default();
        let state = State::default();
        let view = View { seen_at: "2026-11-06T10:00:00Z".into(), ..View::default() };
        let d1: Doc = serde_json::from_value(json!({"id": "a", "title": "plan", "rev": 1, "blocks": [
            {"type": "text", "id": "x", "text": "one"}, {"type": "text", "id": "y", "text": "two"}, {"type": "text", "id": "z", "text": "three"}]})).unwrap();
        let mut d2 = d1.clone();
        d2.rev = 2;
        for (id, t) in [("x", "uno"), ("y", "dos")] {
            d2.apply(&crate::doc::Op::Set { id: id.into(), field: "text".into(), value: json!(t) }).unwrap();
        }
        let mut sc = Scene::new(&cfg);
        let mut top = |doc: &Doc, now: f64| {
            let inp = Input { doc, state: &state, view: &view, delivered: 0, unread: 0, now, wall: 0.0, cfg: &cfg, kitty: false,
                cell: (10, 20), focused_pane: false, mouse: false, project: "p", w: 60, h: 16, activity: &[] };
            sc.frame(&inp).grid.to_text().lines().next().unwrap_or("").to_string()
        };
        assert!(!top(&d1, 0.0).contains(" new "));
        // a change seen within UNSEEN_TAG never shows the badge (a feed writing while he watches);
        // one that stays unseen does
        let t = top(&d2, 5.0);
        assert!(!t.contains(" new "), "{t}");
        let t = top(&d2, 5.0 + UNSEEN_TAG);
        assert!(t.contains(" 2 new "), "{t}");
    }

    /// A removed section and its child collapse and leave (a feed rewrites structure often, so a
    /// lingering ghost showed up as soon as feeds did), and while they collapse they keep their
    /// old order.
    #[test]
    fn removed_section_and_children_leave() {
        let with: Doc = serde_json::from_value(json!({"id": "a", "rev": 1, "blocks": [{"type": "section", "id": "jj", "title": "jj", "children": [
            {"type": "row", "id": "c1", "text": "commit one"},
            {"type": "section", "id": "wc", "title": "working copy", "children": [{"type": "row", "id": "f0", "text": "recs.ts"}]}]}]})).unwrap();
        let mut without = with.clone();
        without.rev = 2;
        without.apply(&crate::doc::Op::Rm { id: "wc".into() }).unwrap();
        let mut sc = Scene::new(&Config::default());
        assert!(frame_text(&mut sc, &with, 0.0).contains("recs.ts"));
        let mid = frame_text(&mut sc, &without, 10.0);
        if let (Some(h), Some(f)) = (mid.find("working copy"), mid.find("recs.ts")) {
            assert!(h < f, "ghosts keep their order:\n{mid}");
        }
        for t in [10.05, 10.3, 11.0, 12.0] {
            frame_text(&mut sc, &without, t);
        }
        let end = frame_text(&mut sc, &without, 13.0);
        assert!(!end.contains("recs.ts") && !end.contains("working copy"), "{end}");
    }

    /// Once a removed block has collapsed it never draws again. The ghost used to outlive its
    /// leaving node by one frame and draw at full height, and with nothing animating that frame
    /// stayed up to 30 s; only the frame after the collapse shows it, so every frame is checked.
    #[test]
    fn a_collapsed_ghost_never_draws_again() {
        let with: Doc = serde_json::from_value(json!({"id": "a", "rev": 1, "blocks": [
            {"type": "row", "id": "keep", "text": "stays put"},
            {"type": "row", "id": "gone", "text": "lone row"},
            {"type": "section", "id": "wc", "title": "working copy", "children": [{"type": "row", "id": "f0", "text": "recs.ts"}]}]})).unwrap();
        let mut without = with.clone();
        without.rev = 2;
        without.apply(&crate::doc::Op::Rm { id: "gone".into() }).unwrap();
        without.apply(&crate::doc::Op::Rm { id: "wc".into() }).unwrap();
        let clean = |t: f64, text: &str| {
            assert!(text.contains("stays put"), "t={t}:\n{text}");
            for gone in ["lone row", "working copy", "recs.ts"] {
                assert!(!text.contains(gone), "{gone:?} drawn at t={t}, after its collapse:\n{text}");
            }
        };
        // at the frame rate: the collapse (0.18 s) runs, then three seconds of frames
        let mut sc = Scene::new(&Config::default());
        assert!(frame_text(&mut sc, &with, 0.0).contains("lone row"));
        assert!(frame_text(&mut sc, &without, 10.0).contains("lone row"), "it collapses from where it was");
        for i in 1..=100 {
            let t = 10.0 + i as f64 / 30.0;
            let text = frame_text(&mut sc, &without, t);
            if t > 10.25 {
                clean(t, &text);
            }
        }
        // and when the pane slept through the collapse: the first frame after it, and the next
        let mut sc = Scene::new(&Config::default());
        frame_text(&mut sc, &with, 0.0);
        frame_text(&mut sc, &without, 10.0);
        for t in [11.0, 11.03, 11.5, 12.0, 41.0, 41.03] {
            clean(t, &frame_text(&mut sc, &without, t));
        }
    }

    /// Rows removed inside a folded section were never on screen, so they leave without a ghost.
    /// (Each got one, placed at the top of the pane because its predecessor wasn't listed;
    /// finished sections fold themselves and feeds rewrite them.)
    #[test]
    fn rows_removed_inside_a_fold_are_never_drawn() {
        let with: Doc = serde_json::from_value(json!({"id": "a", "rev": 1, "blocks": [
            {"type": "row", "id": "top", "text": "first row"},
            {"type": "section", "id": "s", "title": "finished", "children": [
                {"type": "row", "id": "h1", "text": "hidden one"}, {"type": "row", "id": "h2", "text": "hidden two"}, {"type": "row", "id": "h3", "text": "hidden three"}]},
            {"type": "row", "id": "below", "text": "row below"}]})).unwrap();
        let mut without = with.clone();
        without.rev = 2;
        without.apply(&crate::doc::Op::Rm { id: "h1".into() }).unwrap();
        without.apply(&crate::doc::Op::Rm { id: "h2".into() }).unwrap();
        // the row under the fold goes too: its nearest listed neighbour is the section header
        let mut last = without.clone();
        last.rev = 3;
        last.apply(&crate::doc::Op::Rm { id: "below".into() }).unwrap();
        let cfg = Config::default();
        let state = State::default();
        let mut view = View::default();
        view.folds.insert("s".into(), true);
        let mut sc = Scene::new(&cfg);
        let mut text = |doc: &Doc, now: f64| {
            let inp = Input { doc, state: &state, view: &view, delivered: 0, unread: 0, now, wall: 0.0, cfg: &cfg, kitty: false,
                cell: (10, 20), focused_pane: false, mouse: false, project: "p", w: 60, h: 16, activity: &[] };
            sc.frame(&inp).grid.to_text()
        };
        let t0 = text(&with, 0.0);
        assert!(t0.contains("finished") && !t0.contains("hidden"), "folded: {t0}");
        for i in 0..=60 {
            let t = 10.0 + i as f64 / 30.0;
            let f = text(&without, t);
            assert!(!f.contains("hidden"), "a row from inside the fold drawn at t={t}:\n{f}");
            assert!(f.find("first row") < f.find("finished"), "t={t}:\n{f}");
        }
        let f = text(&last, 20.0);
        let (sec, below) = (f.find("finished").unwrap(), f.find("row below").expect("it collapses in place"));
        assert!(sec < below, "the ghost sits under the folded section, not at the top:\n{f}");
        for t in [20.5, 20.53, 21.0] {
            let f = text(&last, t);
            assert!(!f.contains("row below") && !f.contains("hidden"), "t={t}:\n{f}");
        }
    }

    /// Wrapped text that reflows on a resize shows at its new height in the same frame: the
    /// height spring is for content changes, and animating a reflow read as the section sliding
    /// in while one-line sections snapped (2026-11-06).
    #[test]
    fn resize_reflow_snaps() {
        let doc: Doc = serde_json::from_value(json!({"id": "a", "rev": 1, "blocks": [{"type": "section", "id": "o", "title": "Overview", "children": [
            {"type": "bullet", "id": "b1", "text": "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu"},
            {"type": "bullet", "id": "b2", "text": "LAST"}]}]})).unwrap();
        let mut sc = Scene::new(&Config::default());
        frame_text_w(&mut sc, &doc, 0.0, 60);
        frame_text_w(&mut sc, &doc, 5.0, 60);
        // 40: the narrowest pane that still draws rows (under it is glance mode)
        let narrow = frame_text_w(&mut sc, &doc, 5.01, 40);
        assert!(narrow.contains("lambda") && narrow.contains("LAST"), "{narrow}");
    }

    /// Every component draws at any pane width without panicking (unsigned width math
    /// underflowed at 20-26 columns in debug builds).
    #[test]
    fn every_kind_at_every_width() {
        let doc: Doc = serde_json::from_value(json!({"id": "a", "rev": 1, "blocks": [{"type": "section", "id": "s", "title": "All", "children": [
            {"type": "text", "id": "x", "text": "hello **bold** `code`"},
            {"type": "code", "id": "cd", "text": "fn main() {\n\tprintln!(\"a long line that will not fit\");\n}", "language": "rust"},
            {"type": "tail", "id": "tl", "title": "cargo test", "lines": 4},
            {"type": "bullet", "id": "b", "text": "a bullet that wraps", "children": [{"type": "bullet", "id": "b2", "text": "sub"}]},
            {"type": "todo", "id": "t", "text": "todo", "status": "doing", "note": "n"},
            {"type": "progress", "id": "p", "of": "s"},
            {"type": "timer", "id": "tm", "since": "2026-11-06T10:00:00Z", "label": "l"},
            {"type": "status", "id": "st", "text": "cargo test", "state": "pass", "result": "ok", "spark": [1, 2, 3, 4, 5], "target": "https://x"},
            {"type": "question", "id": "q", "text": "which?", "options": ["a", "b"], "default": "a", "why": "why"},
            {"type": "claim", "id": "c", "text": "claim"},
            {"type": "decided", "id": "d", "text": "dec", "by": "claude"},
            {"type": "link", "id": "l", "text": "link", "target": "https://example.com"},
            {"type": "image", "id": "im", "path": "/nonexistent.png", "alt": "alt"},
            {"type": "row", "id": "r", "text": "row", "glyph": "spin", "tag": "t", "detail": "d"}]}]})).unwrap();
        for w in 1..48 {
            let mut sc = Scene::new(&Config::default());
            frame_text_w(&mut sc, &doc, 0.0, w);
        }
    }

    /// With motion off a spinner is a still glyph: the frame asks for no wake-up.
    #[test]
    fn motion_off_spinner_sleeps() {
        let doc: Doc = serde_json::from_value(json!({"id": "a", "rev": 1, "blocks": [{"type": "todo", "id": "t", "text": "x", "status": "doing"}]})).unwrap();
        let cfg = Config { motion: "off".into(), ..Config::default() };
        let (state, view) = (State::default(), View::default());
        let mut sc = Scene::new(&cfg);
        let inp = |now| Input {
            doc: &doc, state: &state, view: &view, delivered: 0, unread: 0, now, wall: 0.0, cfg: &cfg, kitty: false,
            cell: (10, 20), focused_pane: false, mouse: false, project: "p", w: 60, h: 16, activity: &[],
        };
        sc.frame(&inp(0.0));
        // slow clock ticks may remain (ages, fades); spinner frames (100 ms) must not
        for t in [5.0, 50.0] {
            let ms = sc.frame(&inp(t)).wake.timeout_ms(t);
            assert!(ms < 0 || ms >= 1000, "woke in {ms} ms at t={t}");
        }
    }

    /// With the mouse on, a hovered todo shows next/skip/done buttons as click targets, and
    /// nothing when not hovered.
    #[test]
    fn todo_controls_follow_who_owns_the_step() {
        let doc: Doc = serde_json::from_value(json!({"id": "a", "rev": 1, "blocks": [
            {"type": "todo", "id": "o", "text": "open"},
            {"type": "todo", "id": "r", "text": "running", "status": "doing"},
            {"type": "todo", "id": "d", "text": "finished", "status": "done"}]})).unwrap();
        let cfg = Config::default();
        let (state, view) = (State::default(), View::default());
        let mut sc = Scene::new(&cfg);
        let inp = Input {
            doc: &doc, state: &state, view: &view, delivered: 0, unread: 0, now: 0.0, wall: 0.0, cfg: &cfg, kitty: false,
            cell: (10, 20), focused_pane: false, mouse: true, project: "p", w: 60, h: 16, activity: &[],
        };
        let text = sc.frame(&inp).grid.to_text();
        let toks: Vec<&str> = sc.hits.iter().map(|h| h.3.as_str()).collect();
        // the open step's circle cycles to next; the running step has one stop pill; the finished
        // step has nothing: done is Claude's to set, never sulin's
        assert_eq!(toks, ["mg:todo/o=next", "mg:todo/r=skipped"], "{text}");
        assert!(text.contains(" stop ") && !text.contains(" done ") && !text.contains(" skip "), "{text}");
        let (x, y, _, _) = sc.hits[0].clone();
        assert_eq!(sc.hit_at(x + 1, y), Some("mg:todo/o=next"));
    }

    #[test]
    fn a_long_claim_wraps_and_renders_its_bold() {
        let doc: Doc = serde_json::from_value(json!({"id": "a", "rev": 1, "blocks": [
            {"type": "claim", "id": "c", "text": "**The lead** is bold and the rest of this claim runs well past one line of a narrow pane, ending in zebra"}
        ]})).unwrap();
        let text = frame_text_w(&mut Scene::new(&Config::default()), &doc, 0.0, 50);
        assert!(text.contains("zebra"), "the tail of the claim is on screen:\n{text}");
        assert!(text.contains("The lead") && !text.contains("**"), "bold markers are styling, not text:\n{text}");
    }

    #[test]
    fn a_question_lights_nothing_until_picked_and_its_answers_are_the_buttons() {
        let doc: Doc = serde_json::from_value(json!({"id": "a", "rev": 1, "blocks": [
            {"type": "question", "id": "Q1", "text": "which?", "options": ["files", "sqlite"], "suggest": "files"}
        ]})).unwrap();
        let (cfg, view, state) = (Config::default(), View::default(), State::default());
        let mut sc = Scene::new(&cfg);
        let inp = Input {
            doc: &doc, state: &state, view: &view, delivered: 0, unread: 0, now: 0.0, wall: 0.0, cfg: &cfg, kitty: false,
            cell: (10, 20), focused_pane: false, mouse: true, project: "p", w: 60, h: 16, activity: &[],
        };
        let text = sc.frame(&inp).grid.to_text();
        let toks: Vec<&str> = sc.hits.iter().map(|h| h.3.as_str()).filter(|t| t.starts_with("mg:pick/")).collect();
        assert_eq!(toks, vec!["mg:pick/Q1=files", "mg:pick/Q1=sqlite"], "{text}");
        assert!(!text.contains("mg:pick"), "with the mouse on, no token is drawn:\n{text}");
        assert!(!text.contains(icon::RADIO_ON), "no answer is lit before a pick:\n{text}");
        let files = text.lines().find(|l| l.contains("files")).unwrap();
        assert!(files.contains(icon::ROBOT) && !text.contains("default") && !text.contains("wait for me"), "the suggestion is a robot on its answer:\n{text}");
        assert!(sc.hits.iter().any(|h| h.3 == "mg:talk/Q1") && text.contains("talk about this"), "{text}");
    }

    #[test]
    fn a_focused_header_leaves_the_gap_above_it_unlit() {
        let doc: Doc = serde_json::from_value(json!({"id": "a", "rev": 1, "blocks": [
            {"type": "section", "id": "s1", "title": "One", "children": [{"type": "text", "id": "x", "text": "body"}]},
            {"type": "section", "id": "s2", "title": "Two", "children": []}
        ]})).unwrap();
        let (cfg, state) = (Config::default(), State::default());
        let view: View = serde_json::from_value(json!({"focus": "s2"})).unwrap();
        let mut sc = Scene::new(&cfg);
        let inp = Input {
            doc: &doc, state: &state, view: &view, delivered: 0, unread: 0, now: 0.0, wall: 0.0, cfg: &cfg, kitty: false,
            cell: (10, 20), focused_pane: true, mouse: true, project: "p", w: 40, h: 16, activity: &[],
        };
        sc.frame(&inp);
        sc.focus = Some("s2".into());
        let text = sc.frame(&inp).grid.to_text();
        let lit: Vec<&str> = text.lines().filter(|l| l.contains('▌')).collect();
        assert_eq!(lit.len(), 1, "one lit row:\n{text}");
        assert!(lit[0].contains("Two"), "and it is the header, not the blank row above:\n{text}");
    }

    #[test]
    fn a_long_step_wraps_and_its_note_drops_below() {
        let doc: Doc = serde_json::from_value(json!({"id": "a", "rev": 1, "blocks": [
            {"type": "todo", "id": "n1", "text": "**The lead** and then a step whose text runs well past one line of a narrow pane, ending in zebra", "note": "done when: the whole of this note can be read, down to quagga"},
            {"type": "todo", "id": "n2", "text": "short", "note": "aside"}
        ]})).unwrap();
        let text = frame_text_w(&mut Scene::new(&Config::default()), &doc, 0.0, 50);
        assert!(text.contains("zebra") && text.contains("quagga"), "nothing is cut:\n{text}");
        assert!(text.lines().any(|l| l.contains("short") && l.contains("aside")), "a short note stays on the row:\n{text}");
    }

    #[test]
    fn a_long_decided_row_wraps_and_renders_its_bold() {
        let doc: Doc = serde_json::from_value(json!({"id": "a", "rev": 1, "blocks": [
            {"type": "decided", "id": "d1", "text": "**Who it is for**: one user on two machines, each under its own name, ending in zebra", "by": "you"}
        ]})).unwrap();
        let text = frame_text_w(&mut Scene::new(&Config::default()), &doc, 0.0, 50);
        assert!(text.contains("zebra") && !text.contains("**"), "{text}");
    }

    #[test]
    fn an_old_age_is_redrawn_at_the_pace_it_changes() {
        assert_eq!((age_pace(5.0), age_pace(600.0), age_pace(7200.0)), (1.0, 60.0, 3600.0));
    }

    #[test]
    fn struck_claim_unstrikes_from_its_glyph() {
        let doc: Doc = serde_json::from_value(json!({"id": "a", "rev": 1, "blocks": [{"type": "claim", "id": "c", "text": "x"}]})).unwrap();
        let cfg = Config::default();
        let view = View::default();
        let open = State::default();
        let struck: State = serde_json::from_value(json!({"decisions": {"c": {"kind": "strike", "value": true, "by": "sulin", "at": ""}}})).unwrap();
        for (state, want) in [(&open, vec!["mg:strike/c"]), (&struck, vec!["mg:strike/c"])] {
            let mut sc = Scene::new(&cfg);
            let inp = Input {
                doc: &doc, state, view: &view, delivered: 0, unread: 0, now: 0.0, wall: 0.0, cfg: &cfg, kitty: false,
                cell: (10, 20), focused_pane: false, mouse: true, project: "p", w: 60, h: 16, activity: &[],
            };
            let text = sc.frame(&inp).grid.to_text();
            let toks: Vec<&str> = sc.hits.iter().map(|h| h.3.as_str()).collect();
            // the row is the one button either way: no strike pill, no unstrike pill
            assert_eq!(toks, want, "{text}");
            let row = text.lines().find(|l| l.contains(" c x")).unwrap();
            assert!(!row.contains("strike"), "{text}");
        }
    }

    #[test]
    fn diff_has_verdict_buttons() {
        let doc: Doc = serde_json::from_value(json!({"id": "a", "rev": 1, "blocks": [{"type": "diff", "id": "d1", "path": "x.rs", "adds": 1, "dels": 0, "rows": []}]})).unwrap();
        let cfg = Config::default();
        let (state, view) = (State::default(), View::default());
        let mut sc = Scene::new(&cfg);
        let inp = Input {
            doc: &doc, state: &state, view: &view, delivered: 0, unread: 0, now: 0.0, wall: 0.0, cfg: &cfg, kitty: false,
            cell: (10, 20), focused_pane: false, mouse: true, project: "p", w: 60, h: 16, activity: &[],
        };
        let text = sc.frame(&inp).grid.to_text();
        let toks: Vec<&str> = sc.hits.iter().map(|h| h.3.as_str()).collect();
        assert_eq!(toks, ["mg:approve/d1", "mg:reject/d1", "mg:ask/d1"], "{text}");
        // once he has a verdict, the verdict pill is the undo
        let asked: State = serde_json::from_value(json!({"decisions": {"d1": {"kind": "verdict", "value": "asked", "by": "sulin", "at": ""}}})).unwrap();
        let mut sc = Scene::new(&cfg);
        let text = sc.frame(&Input { state: &asked, ..inp }).grid.to_text();
        let toks: Vec<&str> = sc.hits.iter().map(|h| h.3.as_str()).collect();
        assert_eq!(toks, ["mg:ask/d1=clear"], "{text}");
    }

    /// A question card's status line stays inside its ring at the pane widths where it ran into
    /// the border (56-62): "you picked X · sent · Claude reads it next turn" and "default X
    /// applies …" wrap or truncate instead of overwriting it.
    #[test]
    fn question_card_stays_inside_its_ring() {
        let doc: Doc = serde_json::from_value(json!({"id": "a", "rev": 1, "blocks": [
            {"type": "question", "id": "Q1", "text": "Which transport should the server use?", "options": ["stdio", "http-sse", "websocket"],
             "default": "stdio", "applies": "at 14:30 if you don't pick"}]})).unwrap();
        let mut picked = State::default();
        picked.decisions.insert("Q1".into(), crate::store::Decision {
            kind: "pick".into(), value: json!("websocket"), by: crate::who::HUMAN.into(), at: String::new(), reason: None, via: None, seq: 0, under: None });
        let cfg = Config::default();
        let view = View::default();
        for (state, want) in [(State::default(), "applies"), (picked, "reads it")] {
            for w in 56..=62u16 {
                let mut sc = Scene::new(&cfg);
                let inp = Input {
                    doc: &doc, state: &state, view: &view, delivered: 0, unread: 0, now: 0.0, wall: 0.0, cfg: &cfg, kitty: false,
                    cell: (10, 20), focused_pane: false, mouse: false, project: "p", w, h: 30, activity: &[],
                };
                let text = sc.frame(&inp).grid.to_text();
                let rows: Vec<Vec<char>> = text.lines().map(|l| l.chars().collect()).collect();
                let top = rows.iter().position(|r| r.iter().filter(|&&c| c == '╭').count() >= 1 && r.contains(&'╮') && r.iter().position(|&c| c == '╭') > Some(0)).expect("card top");
                let edge = rows[top].iter().rposition(|&c| c == '╮').unwrap();
                let bottom = (top + 1..rows.len()).find(|&y| rows[y].get(edge) == Some(&'╯')).unwrap_or_else(|| panic!("card bottom at w={w}:\n{text}"));
                for y in top + 1..bottom {
                    assert_eq!(rows[y].get(edge), Some(&'│'), "row {y} overran the ring at w={w}:\n{text}");
                }
                assert!(text.contains(want), "status tail missing at w={w}:\n{text}");
            }
        }
    }

    #[test]
    fn find_wraps_and_section_step_jumps() {
        let doc: Doc = serde_json::from_value(json!({"id": "a", "rev": 1, "blocks": [
            {"type": "section", "id": "s1", "title": "First", "children": [{"type": "todo", "id": "t1", "text": "alpha"}, {"type": "bullet", "id": "b1", "text": "Beta notes"}]},
            {"type": "section", "id": "s2", "title": "Second", "children": [{"type": "todo", "id": "t2", "text": "gamma beta"}]}]})).unwrap();
        let mut sc = Scene::new(&Config::default());
        frame_text(&mut sc, &doc, 0.0);
        sc.focus = Some("t1".into());
        // case-insensitive, forward from the focus, wrapping
        assert_eq!(sc.find(&doc, "BETA", true).as_deref(), Some("b1"));
        sc.focus = Some("t2".into());
        assert_eq!(sc.find(&doc, "beta", true).as_deref(), Some("b1"), "wraps to the top");
        assert_eq!(sc.find(&doc, "nope", true), None);
        sc.focus = Some("t1".into());
        sc.section_step(&doc, 1);
        assert_eq!(sc.focus.as_deref(), Some("s2"));
        sc.section_step(&doc, -1);
        assert_eq!(sc.focus.as_deref(), Some("s1"));
    }

    #[test]
    fn notes_show_in_the_gutter_and_the_header_counts_the_draft() {
        let doc: Doc = serde_json::from_value(json!({"id": "a", "title": "plan", "rev": 1, "blocks": [
            {"type": "bullet", "id": "b1", "text": "a point"}, {"type": "bullet", "id": "b2", "text": "another"}]})).unwrap();
        let b1 = doc.find("b1").unwrap();
        let state: State = serde_json::from_value(json!({"decisions": {"b1#note#0000": {
            "kind": "note", "value": {"r": "less", "quote": "", "h": crate::store::row_hash(b1)}, "by": "sulin", "at": "", "seq": 5}}})).unwrap();
        let cfg = Config::default();
        let view = View::default();
        let mut sc = Scene::new(&cfg);
        sc.review = true;
        let inp = Input {
            doc: &doc, state: &state, view: &view, delivered: 0, unread: 0, now: 0.0, wall: 0.0, cfg: &cfg, kitty: false,
            cell: (10, 20), focused_pane: false, mouse: true, project: "p", w: 70, h: 16, activity: &[],
        };
        let text = sc.frame(&inp).grid.to_text();
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].contains("REVIEW") && lines[0].contains("1 note"), "{}", lines[0]);
        let row = lines.iter().find(|l| l.contains("a point")).unwrap();
        assert!(row.starts_with('−'), "the less glyph in the gutter: {row:?}");
        assert!(!lines.iter().find(|l| l.contains("another")).unwrap().starts_with('−'));
        assert!(!sc.hits.iter().any(|h| h.3 == "mg:review" || h.3 == "mg:stop") && !lines[0].contains("stop"), "the bar says the mode and holds no buttons: {}", lines[0]);
        // delivered: no longer a draft, the mark stays until Claude revises the row
        let text = sc.frame(&Input { delivered: 5, ..inp }).grid.to_text();
        assert!(!text.lines().next().unwrap().contains("note"), "{text}");
    }

    #[test]
    fn ids_claude_names_are_on_screen() {
        let doc: Doc = serde_json::from_value(json!({"id": "a", "rev": 1, "blocks": [
            {"type": "todo", "id": "r1", "text": "short id"},
            {"type": "todo", "id": "write-up-the-cause", "text": "slug id"},
            {"type": "bullet", "id": "doc-5eea", "text": "a doc row"}]})).unwrap();
        let mut sc = Scene::new(&Config::default());
        let t = frame_text(&mut sc, &doc, 0.0);
        assert!(t.contains("r1 short id"), "{t}");
        assert!(!t.contains("write-up-the-cause slug") && !t.contains("doc-5eea"), "long ids wait for i: {t}");
        sc.show_ids = true;
        let t = frame_text(&mut sc, &doc, 1.0);
        assert!(t.contains("write-up-the-cause") && t.contains("doc-5eea"), "{t}");
    }

    #[test]
    fn graph_nodes_are_the_status_and_the_button() {
        let doc: Doc = serde_json::from_value(json!({"id": "a", "rev": 1, "blocks": [{"type": "section", "id": "s", "title": "S", "children": [
            {"type": "todo", "id": "a1", "text": "first", "status": "done"},
            {"type": "todo", "id": "b1", "text": "side", "after": ["a1"]},
            {"type": "todo", "id": "c1", "text": "main", "after": ["a1"]}]}]})).unwrap();
        let cfg = Config { graph_glyphs: "flog".into(), ..Config::default() };
        let (state, view) = (State::default(), View::default());
        let mut sc = Scene::new(&cfg);
        let inp = Input {
            doc: &doc, state: &state, view: &view, delivered: 0, unread: 0, now: 0.0, wall: 0.0, cfg: &cfg, kitty: false,
            cell: (10, 20), focused_pane: false, mouse: true, project: "p", w: 60, h: 16, activity: &[],
        };
        let text = sc.frame(&inp).grid.to_text();
        // a1 done: a filled node branching down; c1 sits in the second lane (a hollow node)
        let line = |id: &str| text.lines().find(|l| l.contains(id)).unwrap().to_string();
        assert!(line("a1 first").contains(crate::graph::node_glyph(&crate::graph::Row { down: true, right: true, ..Default::default() }, true)), "{text}");
        let c = line("c1 main");
        let node_col = c.chars().position(|ch| ('\u{F5EE}'..='\u{F60D}').contains(&ch)).unwrap() as u16;
        assert!(node_col > 2, "c1's node is in the second lane: {c:?}");
        let y = text.lines().position(|l| l.contains("c1 main")).unwrap() as u16;
        assert_eq!(sc.hit_at(node_col, y), Some("mg:todo/c1=next"), "clicking the node cycles it");
    }

    #[test]
    fn graph_lines_blend_their_ends() {
        // a (done) → c (open) passes b's row halfway; a → b is done → done
        let doc: Doc = serde_json::from_value(json!({"id": "a", "rev": 1, "blocks": [{"type": "section", "id": "s", "title": "S", "children": [
            {"type": "todo", "id": "a1", "text": "first", "status": "done"},
            {"type": "todo", "id": "b1", "text": "second", "status": "done", "after": ["a1"]},
            {"type": "todo", "id": "c1", "text": "third", "after": ["a1"]}]}]})).unwrap();
        let cfg = Config { graph_glyphs: "flog".into(), ..Config::default() };
        let (state, view) = (State::default(), View::default());
        let mut sc = Scene::new(&cfg);
        let inp = Input {
            doc: &doc, state: &state, view: &view, delivered: 0, unread: 0, now: 0.0, wall: 0.0, cfg: &cfg, kitty: false,
            cell: (10, 20), focused_pane: false, mouse: false, project: "p", w: 60, h: 16, activity: &[],
        };
        let f = sc.frame(&inp);
        let text = f.grid.to_text();
        let y = text.lines().position(|l| l.contains("b1 second")).unwrap() as u16;
        // lines are solid: colour alone says what's reached (sulin didn't want dashes, 2026-11-08)
        let x = (0..60u16).find(|&x| f.grid.get(x, y).map(|c| c.sym == vellum::grid::Sym::Char('\u{F5D1}')).unwrap_or(false)).expect("a passing lane on b1's row");
        let p = &EVERFOREST;
        // grey is a colour like any other: halfway from done to open is halfway green to grey
        assert_eq!(f.grid.get(x, y).unwrap().style.fg, Some(p.green.mix_lch(p.grey1, 0.5)), "halfway along a done → open edge");
    }

    #[test]
    fn a_struck_claim_flags_the_steps_resting_on_it() {
        let doc: Doc = serde_json::from_value(json!({"id": "a", "rev": 1, "blocks": [
            {"type": "claim", "id": "A1", "text": "one process"},
            {"type": "todo", "id": "t1", "text": "rests on it", "rests_on": ["A1"]},
            {"type": "todo", "id": "t2", "text": "finished anyway", "status": "done", "rests_on": ["A1"]}]})).unwrap();
        let cfg = Config::default();
        let view = View::default();
        let standing = State::default();
        let struck: State = serde_json::from_value(json!({"decisions": {"A1": {"kind": "strike", "value": true, "by": "sulin", "at": ""}}})).unwrap();
        let t1 = doc.find("t1").unwrap();
        assert!(!components::stale(&standing, t1));
        assert!(components::stale(&struck, t1));
        assert!(!components::stale(&struck, doc.find("t2").unwrap()), "a finished step isn't flagged");
        for (state, want) in [(&standing, "A1"), (&struck, "✗ A1")] {
            let mut sc = Scene::new(&cfg);
            let inp = Input {
                doc: &doc, state, view: &view, delivered: 0, unread: 0, now: 0.0, wall: 0.0, cfg: &cfg, kitty: false,
                cell: (10, 20), focused_pane: false, mouse: true, project: "p", w: 70, h: 16, activity: &[],
            };
            let f = sc.frame(&inp);
            let text = f.grid.to_text();
            let line = text.lines().find(|l| l.contains("rests on it")).unwrap();
            assert!(line.contains(want), "{line}");
            // struck: the step's glyph is red
            let y = text.lines().position(|l| l.contains("rests on it")).unwrap() as u16;
            let red = (0..10u16).any(|x| f.grid.get(x, y).and_then(|c| c.style.fg) == Some(EVERFOREST.red));
            assert_eq!(red, state.decisions.contains_key("A1"), "{line}");
        }
    }

    #[test]
    fn open_to_open_lines_are_solid_and_light() {
        let doc: Doc = serde_json::from_value(json!({"id": "a", "rev": 1, "blocks": [{"type": "section", "id": "s", "title": "S", "children": [
            {"type": "todo", "id": "a1", "text": "first"},
            {"type": "todo", "id": "b1", "text": "second"},
            {"type": "todo", "id": "c1", "text": "third", "after": ["a1"]}]}]})).unwrap();
        let cfg = Config { graph_glyphs: "flog".into(), ..Config::default() };
        let (state, view) = (State::default(), View::default());
        let mut sc = Scene::new(&cfg);
        let inp = Input {
            doc: &doc, state: &state, view: &view, delivered: 0, unread: 0, now: 0.0, wall: 0.0, cfg: &cfg, kitty: false,
            cell: (10, 20), focused_pane: false, mouse: false, project: "p", w: 60, h: 16, activity: &[],
        };
        let f = sc.frame(&inp);
        let text = f.grid.to_text();
        let y = text.lines().position(|l| l.contains("b1 second")).unwrap() as u16;
        let lane = (0..60u16).filter_map(|x| f.grid.get(x, y)).find(|c| matches!(c.sym, vellum::grid::Sym::Char(ch) if ('\u{F5D0}'..='\u{F5D5}').contains(&ch))).expect("a lane passes b1");
        assert_eq!(lane.sym, vellum::grid::Sym::Char('\u{F5D1}'), "solid");
        assert_eq!(lane.style.fg, Some(EVERFOREST.grey1));
    }

    #[test]
    fn a_question_says_when_claude_goes_ahead() {
        let mk = |t4: &str, q: serde_json::Value| -> Doc {
            let mut qq = json!({"type": "question", "id": "Q1", "text": "mcp or cli?", "options": ["cli", "mcp"], "default": "cli"});
            for (k, v) in q.as_object().unwrap() {
                qq[k] = v.clone();
            }
            serde_json::from_value(json!({"id": "a", "rev": 1, "blocks": [qq, {"type": "todo", "id": "t4", "text": "build it", "status": t4}]})).unwrap()
        };
        let cfg = Config::default();
        let view = View::default();
        let run = |doc: &Doc, state: &State, wall: f64| -> (String, Vec<String>) {
            let mut sc = Scene::new(&cfg);
            let inp = Input {
                doc, state, view: &view, delivered: 0, unread: 0, now: 0.0, wall, cfg: &cfg, kitty: false,
                cell: (10, 20), focused_pane: false, mouse: true, project: "p", w: 80, h: 30, activity: &[],
            };
            let t = sc.frame(&inp).grid.to_text();
            (t, sc.hits.iter().map(|h| h.3.clone()).collect())
        };
        let open = State::default();
        let (t, hits) = run(&mk("todo", json!({"applies": "t4"})), &open, 0.0);
        assert!(t.contains("taken when t4 starts") && t.contains("talk about this"), "{t}");
        assert!(hits.contains(&"mg:talk/Q1".to_string()));
        assert!(t.lines().any(|l| l.contains("t4 build it") && l.contains("? Q1")), "the step links back: {t}");
        let (t, _) = run(&mk("doing", json!({"applies": "t4"})), &open, 0.0);
        assert!(t.contains("t4 started: taking the default now"), "{t}");
        let held: State = serde_json::from_value(json!({"decisions": {"Q1": {"kind": "hold", "value": true, "by": "sulin", "at": ""}}})).unwrap();
        let (t, _) = run(&mk("todo", json!({"applies": "t4"})), &held, 0.0);
        assert!(t.contains("you asked Claude to wait") && t.contains("stops at t4"), "{t}");
        let (t, _) = run(&mk("todo", json!({"takes_at": "2026-11-08T10:05:00Z"})), &open, crate::util::parse_rfc3339("2026-11-08T10:00:00Z").unwrap());
        assert!(t.contains("taken in 5m"), "{t}");
        let (t, _) = run(&mk("done", json!({"applied": true, "applied_at": "2026-11-08T10:00:00Z"})), &open, 0.0);
        assert!(t.contains("Q1") && t.contains("cli") && !t.contains("wait for me"), "resolved: {t}");
    }

    #[test]
    fn agents_section_groups_a_fan_out_with_progress_and_stops() {
        let doc: Doc = serde_json::from_value(json!({"id": "a", "rev": 1, "blocks": [{"type": "text", "id": "x", "text": "body"}]})).unwrap();
        let mk = |id: &str, fin: bool, said: &str| crate::agents::Agent { id: id.into(), description: format!("review {id}"), finished: fin, prompt: "p1".into(), said: said.into(), notes: vec!["reading the auth module".into(), said.into()], ..Default::default() };
        let cfg = Config::default();
        let (state, view) = (State::default(), View::default());
        let mut sc = Scene::new(&cfg);
        sc.agents = vec![crate::agents::Group { prompt: "p1".into(), agents: vec![mk("sec", true, "found 2 bugs"), mk("perf", false, "now profiling")] }];
        let inp = Input {
            doc: &doc, state: &state, view: &view, delivered: 0, unread: 0, now: 0.0, wall: 0.0, cfg: &cfg, kitty: false,
            cell: (10, 20), focused_pane: false, mouse: true, project: "p", w: 70, h: 24, activity: &[],
        };
        let t = sc.frame(&inp).grid.to_text();
        assert!(t.contains("Agents  1 running · 1 done"), "{t}");
        assert!(t.contains("fan-out · 1/2 done"), "{t}");
        // one line each: who, phase, and nothing of what it said until the row is opened
        let line = |t: &str, who: &str| t.lines().find(|l| l.contains(who)).unwrap_or("").to_string();
        assert!(line(&t, "review sec").contains("done") && line(&t, "review perf").contains("planning") && line(&t, "review perf").contains(" stop "), "{t}");
        assert!(!t.contains("now profiling") && !t.contains("found 2 bugs") && !t.contains("reading the auth module"), "closed rows are one line: {t}");
        assert!(sc.hits.iter().any(|h| h.3 == "mg:stopagent/perf"), "a running row has its stop");
        assert!(!sc.hits.iter().any(|h| h.3 == "mg:stopagent/sec"), "a finished one doesn't");
        // opened (a click on the row): its words as a chain, a finished one's result last
        assert!(sc.hits.iter().any(|h| h.3 == "mg:agent/perf"));
        sc.agents_open.extend(["perf".to_string(), "sec".to_string()]);
        sc.frame(&inp);
        let t = sc.frame(&Input { now: 5.0, ..inp }).grid.to_text();
        assert!(t.contains("now profiling") && t.contains("reading the auth module") && t.contains("found 2 bugs"), "{t}");
        assert!(!t.contains("Bash") && !t.contains("tools"), "no tool-level detail: {t}");
    }

    /// Transcript lines for an agent that started at 18:00:00 and made `calls`: (seconds in,
    /// tool, input, whether its result came back).
    fn agent_lines(calls: &[(u32, &str, serde_json::Value, bool)]) -> String {
        let at = |s: u32| format!("2026-11-10T18:{:02}:{:02}Z", s / 60, s % 60);
        let mut out = vec![json!({"type": "user", "promptId": "p", "timestamp": at(0), "message": {"content": "go"}}).to_string()];
        for (i, (secs, tool, input, back)) in calls.iter().enumerate() {
            out.push(json!({"type": "assistant", "timestamp": at(*secs), "message": {"stop_reason": "tool_use", "content": [{"type": "tool_use", "id": format!("tu{i}"), "name": tool, "input": input}]}}).to_string());
            if *back {
                out.push(json!({"type": "user", "timestamp": at(secs + 1), "message": {"content": [{"type": "tool_result", "tool_use_id": format!("tu{i}"), "content": "ok"}]}}).to_string());
            }
        }
        out.join("\n")
    }

    fn agents_frame(sc: &mut Scene, now: f64, wall: f64, w: u16) -> Frame {
        let doc: Doc = serde_json::from_value(json!({"id": "a", "rev": 1, "blocks": [{"type": "text", "id": "x", "text": "body"}]})).unwrap();
        let cfg = Config::default();
        let (state, view) = (State::default(), View::default());
        let inp = Input {
            doc: &doc, state: &state, view: &view, delivered: 0, unread: 0, now, wall, cfg: &cfg, kitty: false,
            cell: (10, 20), focused_pane: false, mouse: false, project: "p", w, h: 24, activity: &[],
        };
        sc.frame(&inp)
    }

    #[test]
    fn an_agent_is_one_line_of_who_phase_plan_and_elapsed() {
        let t0 = parse_rfc3339("2026-11-10T18:00:00Z").unwrap();
        let edits: Vec<(u32, &str, serde_json::Value, bool)> = (0..4).map(|i| (10 + i * 5, "Edit", json!({"file_path": "/r/src/a.rs"}), true)).collect();
        let mut coding = crate::agents::parsed_for_test("Add dashboard widgets", &agent_lines(&edits));
        coding.id = "w".into();
        coding.plan = Some(("a".into(), "ag-widgets".into()));
        coding.progress = Some(crate::agents::Progress { done: 3, total: 5, current: "Draw the run strip".into() });
        let mut bare = crate::agents::parsed_for_test("Survey the terminals", &agent_lines(&edits));
        bare.id = "s".into();
        let mut sc = Scene::new(&Config::default());
        sc.agents = vec![crate::agents::Group { prompt: "p".into(), agents: vec![coding] }, crate::agents::Group { prompt: "q".into(), agents: vec![bare] }];
        // twelve minutes in, its last call half a minute old
        let wall = t0 + 40.0;
        let f = agents_frame(&mut sc, 0.0, wall, 80);
        let t = f.grid.to_text();
        let row = t.lines().find(|l| l.contains("widgets")).unwrap_or_else(|| panic!("{t}"));
        for part in ["widgets", "coding", "▰▰▰▱▱", "3/5", "40s"] {
            assert!(row.contains(part), "{part} on the one line: {row}");
        }
        assert!(row.contains("Add dashboard widgets"), "the row is named by its description: {row}");
        assert!(!t.contains("Draw the run strip"), "the step waits for the row to open: {t}");
        // an agent that keeps no plan: no bar and no count, and its phase in the same column
        let other = t.lines().find(|l| l.contains("Survey the terminals")).unwrap_or_else(|| panic!("{t}"));
        assert!(other.contains("coding") && !other.contains('▰') && !other.contains('▱') && !other.contains('/'), "{other}");
        assert_eq!(row.find("coding"), other.find("coding"), "the phase column lines up:\n{row}\n{other}");
        // a running agent's spinner asks for frames
        let ms = agents_frame(&mut sc, 5.0, wall, 80).wake.timeout_ms(5.0);
        assert!((0..1000).contains(&ms), "spinner frames: {ms}");
        // opened: what it was sent to do and its plan's step in hand
        sc.agents_open.insert("w".into());
        agents_frame(&mut sc, 6.0, wall, 80);
        let t = agents_frame(&mut sc, 12.0, wall, 80).grid.to_text();
        assert!(t.contains("Add dashboard widgets") && t.contains("step Draw the run strip"), "{t}");
    }

    #[test]
    fn a_waiting_agent_is_still_and_says_how_long() {
        let t0 = parse_rfc3339("2026-11-10T18:00:00Z").unwrap();
        let mut a = crate::agents::parsed_for_test("Add tabs", &agent_lines(&[(10, "Edit", json!({"file_path": "/r/a.rs"}), true), (20, "Bash", json!({"command": "sleep 600"}), false)]));
        a.id = "t".into();
        a.progress = Some(crate::agents::Progress { done: 2, total: 4, current: String::new() });
        let mut sc = Scene::new(&Config::default());
        sc.agents = vec![crate::agents::Group { prompt: "p".into(), agents: vec![a] }];
        let wall = t0 + 20.0 + 125.0;
        let t = agents_frame(&mut sc, 0.0, wall, 80).grid.to_text();
        let row = t.lines().find(|l| l.contains("Add tabs")).unwrap_or_else(|| panic!("{t}"));
        assert!(row.contains("waiting 2m") && row.contains("▰▰▰▱▱ 2/4") && row.contains(icon::PAUSE), "{row}");
        assert!(t.contains("Agents  1 waiting"), "{t}");
        // motion is on, and still nothing asks for a sub-second frame: its glyph does not spin
        for now in [5.0, 50.0] {
            let ms = agents_frame(&mut sc, now, wall, 80).wake.timeout_ms(now);
            assert!(ms < 0 || ms >= 1000, "woke in {ms} ms at t={now}");
        }
    }

    #[test]
    fn your_steps_are_pink_and_yours_to_finish() {
        let doc: Doc = serde_json::from_value(json!({"id": "a", "rev": 1, "blocks": [
            {"type": "todo", "id": "t1", "text": "try it on the laptop", "owner": "you"},
            {"type": "todo", "id": "t2", "text": "claude's step"}]})).unwrap();
        let cfg = Config::default();
        let view = View::default();
        let open = State::default();
        let (t1, t2) = (doc.find("t1").unwrap(), doc.find("t2").unwrap());
        assert!(components::yours(&open, t1) && !components::yours(&open, t2));
        assert_eq!(components::cycle_for(&open, t1), Some("done"), "his own step: he finishes it");
        assert_eq!(components::cycle_for(&open, t2), Some("next"), "Claude's: next, never done");
        // he hands t1 back: his claim wins over the owner field
        let handed: State = serde_json::from_value(json!({"decisions": {"t1#own": {"kind": "own", "value": false, "by": "sulin", "at": ""}}})).unwrap();
        assert!(!components::yours(&handed, t1));
        let mut sc = Scene::new(&cfg);
        let inp = Input {
            doc: &doc, state: &open, view: &view, delivered: 0, unread: 0, now: 0.0, wall: 0.0, cfg: &cfg, kitty: false,
            cell: (10, 20), focused_pane: false, mouse: true, project: "p", w: 60, h: 16, activity: &[],
        };
        let f = sc.frame(&inp);
        let text = f.grid.to_text();
        let y = text.lines().position(|l| l.contains("try it on the laptop")).unwrap() as u16;
        assert!((0..8u16).any(|x| f.grid.get(x, y).and_then(|c| c.style.fg) == Some(EVERFOREST.purple)), "pink glyph: {text}");
        assert!(sc.hits.iter().any(|h| h.3 == "mg:todo/t1=done"), "his circle finishes it");
    }

    #[test]
    fn toasts_are_full_width_boxes_stacked_at_the_bottom() {
        let doc: Doc = serde_json::from_value(json!({"id": "a", "rev": 1, "blocks": [
            {"type": "todo", "id": "t1", "text": "run the tests"}, {"type": "todo", "id": "t2", "text": "next step"}]})).unwrap();
        let cfg = Config::default();
        let (state, view) = (State::default(), View::default());
        let mut sc = Scene::new(&cfg);
        let tt = |id: &str, at: &str, text: &str, level: &str, sent: &str| crate::toast::Toast { id: id.into(), at: at.into(), text: text.into(), level: level.into(), ttl: 8.0, sent: sent.into(), ..Default::default() };
        sc.toasts = vec![
            (tt("x0", "", "oldest, not shown", "info", "2026-11-08T10:00:00Z"), 1.0),
            (tt("x1", "t1", "132 passed", "ok", "2026-11-08T10:00:01Z"), 0.5),
            (tt("x2", "", "feed jj crashed", "error", "2026-11-08T10:00:02Z"), 1.0),
        ];
        let inp = Input {
            doc: &doc, state: &state, view: &view, delivered: 0, unread: 0, now: 0.0, wall: 0.0, cfg: &cfg, kitty: false,
            cell: (10, 20), focused_pane: false, mouse: true, project: "p", w: 70, h: 24, activity: &[],
        };
        let text = sc.frame(&inp).grid.to_text();
        let rows: Vec<&str> = text.lines().collect();
        let a = rows.iter().position(|l| l.contains("132 passed")).expect("the older of the two, lowest");
        let b = rows.iter().position(|l| l.contains("feed jj crashed")).expect("the newest, above it");
        assert_eq!(a, b + 3, "stacked: the oldest lowest, the newer one a box (3 rows) above: {text}");
        assert!(rows[a].contains("on t1"), "it names its row: {text}");
        assert!(!text.contains("oldest, not shown"), "two at most: {text}");
        assert!(rows[a - 1].contains('╔') && rows[a + 1].contains('╚'), "double-bordered by default: {text}");
        assert!(rows.iter().position(|l| l.contains("run the tests")).unwrap() < b, "the rows stay where they were: {text}");
        let toks: Vec<&str> = sc.hits.iter().map(|h| h.3.as_str()).collect();
        assert!(toks.contains(&"mg:toast/x1") && toks.contains(&"mg:toast/x2"), "{toks:?}");
    }

    /// A click anywhere in a toast's box is the toast's: it used to press the button drawn under
    /// it (dismissing an error toast struck a claim he couldn't see).
    #[test]
    fn a_toast_takes_the_clicks_over_what_it_covers() {
        let claims: Vec<serde_json::Value> = (1..=14).map(|i| json!({"type": "claim", "id": format!("A{i}"), "text": format!("claim {i}")})).collect();
        let doc: Doc = serde_json::from_value(json!({"id": "a", "rev": 1, "blocks": claims})).unwrap();
        let cfg = Config::default();
        let (state, view) = (State::default(), View::default());
        let mut sc = Scene::new(&cfg);
        sc.toasts = vec![(crate::toast::Toast { id: "x1".into(), text: "feed jj crashed".into(), level: "error".into(), ttl: 8.0, sent: "2026-11-08T10:00:00Z".into(), ..Default::default() }, 1.0)];
        let inp = Input {
            doc: &doc, state: &state, view: &view, delivered: 0, unread: 0, now: 0.0, wall: 0.0, cfg: &cfg, kitty: false,
            cell: (10, 20), focused_pane: false, mouse: true, project: "p", w: 60, h: 16, activity: &[],
        };
        let text = sc.frame(&inp).grid.to_text();
        let rows: Vec<&str> = text.lines().collect();
        let ty = rows.iter().position(|l| l.contains("feed jj crashed")).expect("the toast") as u16;
        let under: Vec<(u16, u16, u16, String)> = sc.hits.iter().filter(|h| h.3.starts_with("mg:strike/") && h.1 + 1 >= ty && h.1 <= ty + 1).cloned().collect();
        assert_eq!(under.iter().map(|h| h.1).collect::<HashSet<_>>().len(), 3, "claims lie under all three rows of the box: {text}");
        // every cell of the box, border and padding included
        for y in ty - 1..=ty + 1 {
            for x in 2..58 {
                assert_eq!(sc.hit_at(x, y), Some("mg:toast/x1"), "({x}, {y}): {text}");
                assert_eq!(sc.overlay_at(x, y), Some("mg:toast/x1"));
            }
            assert_eq!(sc.item_at(y), None, "no row to hover or note under the toast");
        }
        // the rows it doesn't cover still answer
        let (x, y, _, tok) = sc.hits.iter().find(|h| h.3.starts_with("mg:strike/") && h.1 + 1 < ty).cloned().expect("a claim above the toast");
        assert_eq!(sc.hit_at(x, y), Some(tok.as_str()));
        assert!(sc.item_at(y).is_some() && sc.overlay_at(x, y).is_none());
    }

    #[test]
    fn the_live_strip_has_round_ends_that_text_never_covers() {
        let doc: Doc = serde_json::from_value(json!({"id": "a", "rev": 1, "blocks": [{"type": "text", "id": "x", "text": "body"}]})).unwrap();
        let cfg = Config::default();
        let (state, view) = (State::default(), View::default());
        let act = [crate::store::Activity { session: "s".into(), agent: String::new(), state: "running".into(), tool: "Bash".into(), text: "Build the pane".into(), since: "2026-11-08T10:00:00Z".into() }];
        let mut sc = Scene::new(&cfg);
        let inp = Input {
            doc: &doc, state: &state, view: &view, delivered: 0, unread: 0, now: 0.0, wall: crate::util::parse_rfc3339("2026-11-08T10:00:05Z").unwrap(), cfg: &cfg, kitty: false,
            cell: (10, 20), focused_pane: false, mouse: true, project: "p", w: 60, h: 16, activity: &act,
        };
        let text = sc.frame(&inp).grid.to_text();
        let strip = text.lines().find(|l| l.contains("Build the pane")).unwrap().to_string();
        assert!(strip.contains(STRIP_CAP_L) && strip.contains(STRIP_CAP_R), "both caps: {strip:?}");
        assert!(!strip.contains("Bash"), "no tool pill: {strip:?}");
    }

    /// A step shows how long it took (done) or has run (doing) at its first line's right, from
    /// the stamps the store keeps beside its status; a step without stamps shows none, the text
    /// wraps clear, and sulin's own done mark is the finish of a step he finished.
    #[test]
    fn steps_show_their_durations_from_the_stamps() {
        let doc: Doc = serde_json::from_value(json!({"id": "a", "rev": 1, "updated": "2026-11-10T08:00:00Z", "blocks": [
            {"type": "todo", "id": "d", "text": "finished step", "status": "done"},
            {"type": "todo", "id": "old", "text": "finished before the stamps", "status": "done"},
            {"type": "todo", "id": "r", "text": "running step", "status": "doing"},
            {"type": "todo", "id": "w", "text": "a done step whose text runs well past one line of the pane and must wrap clear of the duration zebra", "status": "done"},
            {"type": "todo", "id": "o", "text": "open step with a stale stamp"},
            {"type": "todo", "id": "y", "text": "try the build", "owner": "you"}]})).unwrap();
        let cfg = Config { motion: "off".into(), ..Config::default() };
        let view = View::default();
        let mut state = State::default();
        let mut dec = |k: &str, kind: &str, by: &str, at: &str, v: serde_json::Value| {
            state.decisions.insert(k.into(), serde_json::from_value(json!({"kind": kind, "value": v, "by": by, "at": at})).unwrap());
        };
        dec("d#time", "time", "claude", "", json!({"started": "2026-11-10T10:00:00Z", "done": "2026-11-10T10:12:30Z"}));
        dec("r#time", "time", "claude", "", json!({"started": "2026-11-10T10:20:00Z"}));
        dec("w#time", "time", "claude", "", json!({"started": "2026-11-10T09:00:00Z", "done": "2026-11-10T09:00:45Z"}));
        dec("o#time", "time", "claude", "", json!({"started": "2026-11-10T09:00:00Z"}));
        dec("y#time", "time", "claude", "", json!({"started": "2026-11-10T10:00:00Z"}));
        dec("y", "status", crate::who::HUMAN, "2026-11-10T10:07:00Z", json!("done"));
        let wall = crate::util::parse_rfc3339("2026-11-10T11:24:10Z").unwrap();
        let inp = |mouse| Input {
            doc: &doc, state: &state, view: &view, delivered: 0, unread: 0, now: 0.0, wall, cfg: &cfg, kitty: false,
            cell: (10, 20), focused_pane: false, mouse, project: "p", w: 60, h: 16, activity: &[],
        };
        let mut sc = Scene::new(&cfg);
        let f = sc.frame(&inp(true));
        let (text, ms) = (f.grid.to_text(), f.wake.timeout_ms(0.0));
        let line = |pat: &str| text.lines().find(|l| l.contains(pat)).unwrap_or_else(|| panic!("no {pat:?} in\n{text}")).trim_end().to_string();
        assert!(line("finished step").ends_with(" 12m"), "{text}");
        assert!(line("before the stamps").ends_with("stamps"), "no stamps, no duration:\n{text}");
        assert!(line("stale stamp").ends_with("stamp"), "an open step shows none:\n{text}");
        assert!(line("try the build").ends_with(" 7m"), "his mark is the finish:\n{text}");
        // the running step: elapsed, then its stop pill
        let run = line("running step");
        assert!(run.contains(" 1h04 ") && run.find("1h04") < run.find("stop"), "{text}");
        // the wrapped step keeps its duration on the first line, and no text runs under it
        let first = line("a done step whose");
        assert!(first.ends_with(" 45s") && text.contains("zebra"), "{text}");
        let edge = first.chars().count() - 4;
        let next = text.lines().skip_while(|l| !l.contains("a done step whose")).nth(1).unwrap().trim_end();
        assert!(next.chars().count() < edge, "the text wraps clear of the duration:\n{text}");
        // motion off: the hour-scale clock asks for a frame every 30 s, never spinner frames
        assert!((29_000..=30_000).contains(&ms), "woke in {ms} ms");
        // mouse off: the token stays rightmost, the elapsed time left of it
        let text = Scene::new(&cfg).frame(&inp(false)).grid.to_text();
        let run = text.lines().find(|l| l.contains("running step")).unwrap().trim_end().to_string();
        assert!(run.ends_with("1h04 mg:todo/r"), "{text}");
    }

    /// With the mouse on, a link and a status with a target carry no `mg:open` token: the row
    /// is the button. With it off the token is the double-click route.
    #[test]
    fn link_and_status_rows_are_their_own_open_button() {
        let doc: Doc = serde_json::from_value(json!({"id": "a", "rev": 1, "blocks": [
            {"type": "link", "id": "l", "text": "the design note", "target": "https://example.com"},
            {"type": "status", "id": "st", "text": "cargo test", "state": "pass", "result": "ok", "target": "https://x"},
            {"type": "status", "id": "s2", "text": "cargo build", "state": "pass", "result": "ok"}]})).unwrap();
        let cfg = Config::default();
        let (state, view) = (State::default(), View::default());
        let inp = |mouse| Input {
            doc: &doc, state: &state, view: &view, delivered: 0, unread: 0, now: 0.0, wall: 0.0, cfg: &cfg, kitty: false,
            cell: (10, 20), focused_pane: false, mouse, project: "p", w: 60, h: 16, activity: &[],
        };
        let mut sc = Scene::new(&cfg);
        let text = sc.frame(&inp(true)).grid.to_text();
        assert!(!text.contains("mg:open"), "no tokens with the mouse on:\n{text}");
        let opens: Vec<&str> = sc.hits.iter().map(|h| h.3.as_str()).filter(|t| t.starts_with("mg:open/")).collect();
        assert_eq!(opens, ["mg:open/l", "mg:open/st"], "a status without a target has nothing to open");
        let (x, y, bw, _) = sc.hits.iter().find(|h| h.3 == "mg:open/l").cloned().unwrap();
        assert!(bw > 40 && sc.hit_at(x + 6, y) == Some("mg:open/l"), "the whole row answers");
        let text = Scene::new(&cfg).frame(&inp(false)).grid.to_text();
        assert!(text.contains("mg:open/l") && text.contains("mg:open/st"), "{text}");
    }

    /// A doc for the head: a goal, a running step, an open question, and `rows` filler rows.
    fn head_doc(rows: usize) -> Doc {
        let mut blocks = vec![json!({"type": "section", "id": "goal", "title": "Goal", "children": [
            {"type": "text", "id": "g", "text": "See **where the work stands** at a glance"},
            {"type": "todo", "id": "t1", "text": "wire the head", "status": "doing"},
            {"type": "todo", "id": "t2", "text": "test the head", "status": "doing"},
            {"type": "question", "id": "Q1", "text": "cli or mcp?", "options": ["cli", "mcp"]},
            {"type": "todo", "id": "t3", "text": "read the frame", "owner": "you"}]})];
        blocks.extend((0..rows).map(|i| json!({"type": "text", "id": format!("r{i}"), "text": format!("filler row {i}")})));
        serde_json::from_value(json!({"id": "a", "title": "plan", "rev": 1, "blocks": blocks})).unwrap()
    }

    /// The head (goal, running step, what needs him) shows only when the content overflows the
    /// view, stays put while the content scrolls under it, and its lines are buttons; rows under
    /// it still map to their blocks.
    #[test]
    fn the_head_shows_on_overflow_and_does_not_scroll() {
        let cfg = Config::default();
        // everything on screen: the head would repeat it
        let mut sc = Scene::new(&cfg);
        let t = frame_text(&mut sc, &head_doc(0), 0.0);
        assert!(!t.contains("need you") && sc.head_h == 0, "{t}");
        // overflowing: three lines and a rule, markup stripped, the second running step a count
        let doc = head_doc(30);
        let mut sc = Scene::new(&cfg);
        let t = frame_text(&mut sc, &doc, 0.0);
        let head = |t: &str| t.lines().skip(2).take(4).map(String::from).collect::<Vec<_>>();
        let h0 = head(&t);
        assert!(h0[0].contains("See where the work stands at a glance"), "{t}");
        assert!(h0[1].contains("wire the head") && h0[1].contains("+1"), "{t}");
        assert!(h0[2].contains("2 need you: Q1, t3"), "{t}");
        assert!(h0[3].contains("─────"), "{t}");
        assert_eq!((sc.head_h, sc.view_top), (4, 6));
        // a head line is a button, an id on the needs line its own; the head is no block's row
        assert_eq!(sc.hit_at(10, 2), Some("mg:head/g"));
        assert_eq!(sc.hit_at(10, 3), Some("mg:head/t1"));
        let x3 = h0[2].chars().position(|c| c == 't').unwrap() as u16;
        assert_eq!(sc.hit_at(x3, 4), Some("mg:head/t3"));
        assert_eq!(sc.item_at(4), None);
        assert_eq!(sc.item_at(6), Some("goal"));
        // scrolled: the head holds, the content moved under it, and rows still map to blocks
        sc.scroll_by(14);
        let t = frame_text(&mut sc, &doc, 1.0);
        assert_eq!(head(&t), h0, "{t}");
        assert!(!t.contains("cli or mcp?"), "{t}");
        let first = t.lines().nth(6).unwrap().to_string();
        let id = sc.item_at(6).unwrap().to_string();
        assert!(first.contains(doc.find(&id).unwrap().s("text")), "{id}: {first}");
        // a click on a head line brings its block to the top of the view
        sc.goto("Q1");
        frame_text(&mut sc, &doc, 2.0);
        assert_eq!(sc.item_at(6), Some("Q1"));
        assert_eq!(sc.focus.as_deref(), Some("Q1"));
    }

    /// A `tabs` component draws a strip of its sections' titles and only the section on show
    /// under it: the first by default, another once the view names it. The switch swaps the
    /// content in one frame; the sections' own headers never draw; a change in a section out of
    /// view counts on its tab.
    /// A document's `purpose` is the head's first line, shown even when nothing scrolls; the
    /// running and needs lines still wait for an overflow. It replaces the derived goal line.
    #[test]
    fn the_purpose_line_always_shows_and_replaces_the_goal_line() {
        let cfg = Config::default();
        let mut doc: Doc = serde_json::from_value(json!({"id": "plan", "rev": 1, "blocks": [
            {"type": "todo", "id": "t1", "text": "wire the head", "status": "doing"},
            {"type": "question", "id": "Q1", "text": "cli or mcp?", "options": ["cli", "mcp"]}]})).unwrap();
        doc.purpose = "Ship the **head** so the work reads at a glance".into();
        let mut sc = Scene::new(&cfg);
        let t = frame_text(&mut sc, &doc, 0.0);
        let rows: Vec<&str> = t.lines().collect();
        assert!(rows[2].contains("Ship the head so the work reads at a glance"), "{t}");
        assert!(rows[3].contains("─────") && !t.contains("need you"), "nothing scrolls: the purpose alone: {t}");
        assert_eq!(sc.head_h, 2);
        assert_eq!(sc.hit_at(10, 2), None, "the document's own line goes to no block");
        // overflowing: the purpose first, then running and needs; the goal section's text is not
        // said twice
        let mut doc = head_doc(30);
        doc.purpose = "Ship the head".into();
        let mut sc = Scene::new(&cfg);
        let t = frame_text(&mut sc, &doc, 0.0);
        let rows: Vec<&str> = t.lines().collect();
        assert!(rows[2].contains("Ship the head") && rows[3].contains("wire the head") && rows[4].contains("2 need you"), "{t}");
        assert_eq!(sc.head_h, 4);
        // a document without one still derives the line from its goal section
        let t = frame_text(&mut Scene::new(&cfg), &head_doc(30), 0.0);
        assert!(t.lines().nth(2).unwrap().contains("See where the work stands at a glance"), "{t}");
    }

    /// j/k stop on rows, not on a rule, a table's header or a run strip; a row with a target
    /// says that enter opens it.
    #[test]
    fn focus_skips_furniture_and_a_target_shows_its_key() {
        let doc: Doc = serde_json::from_value(json!({"id": "a", "rev": 1, "blocks": [
            {"type": "rule", "id": "ru", "text": "Reviews"},
            {"type": "table", "id": "tb", "columns": [{"key": "t", "title": "Title"}], "children": [
                {"type": "tr", "id": "r1", "cells": {"t": "one"}, "target": "https://example.com/1"},
                {"type": "tr", "id": "r2", "cells": {"t": "two"}}]},
            {"type": "runs", "id": "rs", "label": "nightly", "values": ["ok", "fail"]},
            {"type": "row", "id": "w1", "text": "deploy", "target": "https://example.com/d"}]})).unwrap();
        let cfg = Config::default();
        let mut sc = Scene::new(&cfg);
        let t = frame_text(&mut sc, &doc, 0.0);
        assert_eq!(sc.focusable, ["r1", "r2", "w1"]);
        assert!(t.contains("⏎ open"), "r1 has a target: {t}");
        sc.move_focus(1);
        assert!(!frame_text(&mut sc, &doc, 1.0).contains("⏎ open"), "r2 has none");
        sc.move_focus(1);
        assert!(frame_text(&mut sc, &doc, 2.0).contains("⏎ open"), "a feed row with a target");
    }

    /// The focused row is never cut: a table row's long title and a feed row's long text end in
    /// `…` until the row has the focus, then wrap; moving on restores the line. The rows above
    /// don't move, and the row the focus moves to keeps its screen row.
    #[test]
    fn the_focused_row_draws_in_full_and_the_view_holds_still() {
        let long = "Retry the scheduler with a capped exponential backoff so a flapping worker cannot starve the queue behind it";
        let mut blocks = vec![
            json!({"type": "table", "id": "tb", "columns": [{"key": "n", "title": "#", "kind": "num"}, {"key": "t", "title": "Title"}, {"key": "who", "title": "Who", "kind": "who"}], "children": [
                {"type": "tr", "id": "r1", "cells": {"n": 1, "t": "short one", "who": "ada"}},
                {"type": "tr", "id": "r2", "cells": {"n": 2, "t": long, "who": "grace"}},
                {"type": "tr", "id": "r3", "cells": {"n": 3, "t": format!("Third: {long}"), "who": "ken"}}]}),
            json!({"type": "row", "id": "w1", "text": format!("deploy: {long}"), "meta": "4m"}),
        ];
        blocks.extend((0..30).map(|i| json!({"type": "row", "id": format!("f{i}"), "text": format!("filler {i}")})));
        let doc: Doc = serde_json::from_value(json!({"id": "a", "rev": 1, "blocks": blocks})).unwrap();
        let cfg = Config::default();
        let (state, view) = (State::default(), View::default());
        let mut sc = Scene::new(&cfg);
        let mut now = 0.0;
        let mut frame = |sc: &mut Scene| {
            now += 1.0;
            let inp = Input { doc: &doc, state: &state, view: &view, delivered: 0, unread: 0, now, wall: 0.0, cfg: &cfg, kitty: false,
                cell: (10, 20), focused_pane: true, mouse: false, project: "p", w: 60, h: 24, activity: &[] };
            sc.frame(&inp).grid.to_text()
        };
        let row_of = |t: &str, what: &str| t.lines().position(|l| l.contains(what)).unwrap_or_else(|| panic!("{what} not in:\n{t}"));
        // focus on r1: the long rows are one line each, cut
        let t = frame(&mut sc);
        assert_eq!(sc.focus.as_deref(), Some("r1"));
        let (y2, y3) = (row_of(&t, "Retry the scheduler"), row_of(&t, "Third: Retry"));
        assert!(t.lines().nth(y2).unwrap().contains('…') && y3 == y2 + 1 && !t.contains("behind it"), "{t}");
        assert!(t.lines().nth(row_of(&t, "deploy: Retry")).unwrap().contains('…'), "{t}");
        // focus r2: its title wraps inside its column, all of it; the other cells stay on the
        // first line, and the rows above it have not moved
        let y1 = row_of(&t, "short one");
        sc.move_focus(1);
        let t = frame(&mut sc);
        assert_eq!((row_of(&t, "short one"), row_of(&t, "Retry the scheduler")), (y1, y2), "{t}");
        let l2 = t.lines().nth(y2).unwrap();
        assert!(!l2.contains('…') && l2.contains("grace") && t.contains("behind it"), "{t}");
        let col = l2.find("Retry").unwrap();
        assert_eq!(t.lines().nth(y2 + 1).unwrap().find(|c: char| !c.is_whitespace() && c != '▌'), Some(col), "the wrapped rows sit in the title's column:\n{t}");
        assert!(row_of(&t, "Third: Retry") > y2 + 1 && t.lines().nth(row_of(&t, "Third: Retry")).unwrap().contains('…'), "{t}");
        // focus a feed row: it wraps, and the table row it left is one cut line again
        sc.move_focus(2);
        let t = frame(&mut sc);
        assert_eq!(sc.focus.as_deref(), Some("w1"));
        let yw = row_of(&t, "deploy: Retry");
        assert!(!t.lines().nth(yw).unwrap().contains('…') && t.lines().nth(yw).unwrap().contains("4m") && t.matches("behind it").count() == 1, "{t}");
        assert!(t.lines().nth(row_of(&t, "2 Retry the scheduler")).unwrap().contains('…'), "{t}");
        // scrolled: moving from an expanded row to the one under it, that row keeps its screen row
        sc.move_focus(-1);
        frame(&mut sc);
        sc.scroll_by(3);
        let t = frame(&mut sc);
        assert_eq!(sc.focus.as_deref(), Some("r3"));
        let at = row_of(&t, "deploy: Retry");
        sc.move_focus(1);
        let t = frame(&mut sc);
        assert_eq!(row_of(&t, "deploy: Retry"), at, "the row the focus moved to stays put:\n{t}");
    }

    /// A pane's tabs sit on the title line, where the title would be, with what changed at the
    /// right; the content starts where a single artifact's does. The age goes before the tabs
    /// collapse, and a strip gets the chooser.
    #[test]
    fn tabs_sit_on_the_title_line() {
        let doc: Doc = serde_json::from_value(json!({"id": "tour", "title": "A tour", "rev": 1, "blocks": [{"type": "todo", "id": "t1", "text": "first row"}]})).unwrap();
        let cfg = Config::default();
        let (state, view) = (State::default(), View::default());
        let tab = |t: &str, needs: bool| crate::tabs::TabInfo { art: t.into(), title: t.into(), needs, ..Default::default() };
        let mut sc = Scene::new(&cfg);
        sc.tabs = vec![tab("tour", false), tab("PR reviews", true), tab("goal", false)];
        let mut frame = |sc: &mut Scene, w: u16| {
            let inp = Input { doc: &doc, state: &state, view: &view, delivered: 0, unread: 0, now: 0.0, wall: 0.0, cfg: &cfg, kitty: false,
                cell: (10, 20), focused_pane: true, mouse: true, project: "p", w, h: 16, activity: &[] };
            sc.frame(&inp).grid.to_text()
        };
        let t = frame(&mut sc, 90);
        let l: Vec<&str> = t.lines().collect();
        assert!(l[0].contains("\u{e0b6} 1 tour \u{e0b4}─  2 PR reviews ●  ─  3 goal ") && l[0].contains("ago") && !l[0].contains("A tour"), "the tab on screen is the title's pill: {}", l[0]);
        assert!(l[1].trim().is_empty() && l[2].contains("first row"), "content starts where a single artifact's does: {t}");
        assert_eq!(sc.view_top, 2);
        let col = |s: &str| l[0][..l[0].find(s).unwrap()].chars().count() as u16;
        assert_eq!(sc.hit_at(col("PR reviews"), 0), Some("mg:tab/1"));
        assert_eq!(sc.hit_at(col("goal"), 0), Some("mg:tab/2"));
        assert!(!sc.hits.iter().any(|h| h.3 == "mg:stop" || h.3 == "mg:review"), "the line holds no buttons");
        // too narrow for the tabs and the age: the age goes, the tabs stay whole
        let t = frame(&mut sc, 44);
        let l0 = t.lines().next().unwrap();
        assert!(l0.contains("1 tour") && l0.contains("3 goal") && !l0.contains("ago"), "{l0}");
        // a strip: the chooser
        let t = frame(&mut sc, 34);
        assert!(t.lines().next().unwrap().contains("‹ 1/3 tour ›"), "{t}");
        assert_eq!(sc.hit_at(2, 0), Some("mg:tab/prev"));
    }

    /// The status bar names the project as `--project` takes it, never as a made-up path.
    #[test]
    fn the_status_bar_names_the_project_not_a_path() {
        assert_eq!(project_label("jobs"), "jobs");
        assert_eq!(project_label("home-me-project"), "home-me-project");
        let long = project_label("home-someone-work-clients-acme-dashboards");
        assert!(long.starts_with('…') && long.ends_with("acme-dashboards") && long.chars().count() == 24, "{long}");
        let d: Doc = serde_json::from_value(json!({"id": "board", "title": "Board", "rev": 1, "blocks": [{"type": "text", "id": "t", "text": "x"}]})).unwrap();
        let (cfg, state, view) = (Config::default(), State::default(), View::default());
        let inp = Input { doc: &d, state: &state, view: &view, delivered: 0, unread: 0, now: 0.0, wall: 0.0, cfg: &cfg, kitty: false,
            cell: (10, 20), focused_pane: false, mouse: false, project: "jobs", w: 70, h: 12, activity: &[] };
        let t = Scene::new(&cfg).frame(&inp).grid.to_text();
        let bar = t.lines().last().unwrap();
        assert!(bar.contains("board jobs") && !bar.contains("~/"), "{bar}");
    }

    #[test]
    fn sub_tabs_show_one_section_at_a_time() {
        let cfg = Config::default();
        let state = State::default();
        let d1: Doc = serde_json::from_value(json!({"id": "a", "title": "reviews", "rev": 1, "blocks": [
            {"type": "text", "id": "intro", "text": "above the strip"},
            {"type": "tabs", "id": "views", "children": [
                {"type": "section", "id": "mine", "title": "Mine", "children": [
                    {"type": "row", "id": "m1", "text": "my first review"}, {"type": "row", "id": "m2", "text": "my second review"}]},
                {"type": "section", "id": "team", "title": "Team", "children": [{"type": "row", "id": "t1", "text": "a team review"}]},
                {"type": "section", "id": "done", "title": "Done", "children": [{"type": "row", "id": "d1", "text": "a finished review"}]}]},
            {"type": "text", "id": "outro", "text": "under the tabs"}]}))
        .unwrap();
        let mut view = View { seen_at: "2026-11-06T10:00:00Z".into(), ..View::default() };
        let mut sc = Scene::new(&cfg);
        let mut frame = |sc: &mut Scene, doc: &Doc, view: &View, now: f64| {
            let inp = Input { doc, state: &state, view, delivered: 0, unread: 0, now, wall: 0.0, cfg: &cfg, kitty: false,
                cell: (10, 20), focused_pane: false, mouse: true, project: "p", w: 60, h: 20, activity: &[] };
            sc.frame(&inp).grid.to_text()
        };
        let t = frame(&mut sc, &d1, &view, 0.0);
        let strip = t.lines().find(|l| l.contains("Mine")).unwrap().to_string();
        assert_eq!(strip.trim(), "\u{e0b6} Mine \u{e0b4}  Team    Done", "the one on show is a pill, the others keep a pill's cells: {t}");
        let sy0 = t.lines().position(|l| l.contains("Mine")).unwrap();
        assert!(t.lines().nth(sy0 + 1).unwrap().trim().chars().all(|c| c == '─') && t.lines().nth(sy0 + 1).unwrap().contains("────────"), "a rule under the strip: {t}");
        assert!(t.contains("my first review") && t.contains("my second review") && !t.contains("a team review") && !t.contains("a finished review"), "{t}");
        assert!(t.contains("above the strip") && t.contains("under the tabs"), "{t}");
        assert!(!t.contains("mg:fold/mine"), "the section on show has no header of its own: {t}");
        // only what is on show is in the list: focus and search never land in a hidden section
        assert_eq!(sc.order, ["intro", "views", "m1", "m2", "outro"]);
        assert!(sc.focusable.contains(&"views".to_string()));
        // each title is a button to its section
        let sy = t.lines().position(|l| l.contains("Mine")).unwrap() as u16;
        let col = |s: &str| strip[..strip.find(s).unwrap()].chars().count() as u16;
        assert_eq!(sc.hit_at(col("Team"), sy), Some("mg:subtab/views/team"));
        assert_eq!(sc.hit_at(col("Done") + 2, sy), Some("mg:subtab/views/done"));
        assert_eq!(sc.hit_at(col("Mine"), sy), Some("mg:subtab/views/mine"));
        // switched: the very next frame is the other section, whole, with nothing left easing
        view.subtabs.insert("views".into(), "team".into());
        let t = frame(&mut sc, &d1, &view, 1.0);
        assert!(t.contains("a team review") && !t.contains("my first review") && !t.contains("my second review"), "{t}");
        assert_eq!(sc.order, ["intro", "views", "t1", "outro"]);
        let outro_y = t.lines().position(|l| l.contains("under the tabs")).unwrap();
        assert_eq!(frame(&mut sc, &d1, &view, 1.5).lines().position(|l| l.contains("under the tabs")).unwrap(), outro_y, "nothing moves after the switch");
        // a change in a section out of view counts on its tab
        let mut d2 = d1.clone();
        d2.rev = 2;
        d2.apply(&crate::doc::Op::Set { id: "m1".into(), field: "text".into(), value: json!("my first review, updated") }).unwrap();
        let t = frame(&mut sc, &d2, &view, 2.0);
        assert!(t.lines().any(|l| l.contains("Mine +1") && l.contains("Team") && !l.contains("Team +")), "{t}");
        // a section the view names that is gone: back to the first
        view.subtabs.insert("views".into(), "nope".into());
        assert!(frame(&mut sc, &d2, &view, 3.0).contains("my first review, updated"));
    }

    /// A doc for the detail panel: rows with a detail, a target, a long detail, and nothing.
    fn detail_doc() -> Doc {
        let long: String = (1..=40).map(|i| format!("line {i} of the long detail")).collect::<Vec<_>>().join("\n");
        serde_json::from_value(json!({"id": "a", "title": "reviews", "rev": 1, "blocks": [
            {"type": "section", "id": "s", "title": "Incoming", "children": [
                {"type": "row", "id": "r1", "text": "Fix the scheduler", "detail": "Fails **one run in five** on main.\nSecond line.", "meta": "kim", "target": "https://example.test/1"},
                {"type": "row", "id": "r2", "text": "Bump the toolchain", "detail": long},
                {"type": "row", "id": "r3", "text": "A bare row"}]}]}))
        .unwrap()
    }

    /// The detail panel beside the list in a wide pane: the list keeps its columns left of a
    /// rule, the panel shows the focused block and follows the focus at once, and a pointer in
    /// the panel never lands on the list row beside it.
    #[test]
    fn detail_panel_sits_beside_the_list_when_wide() {
        let cfg = Config::default();
        let state = State::default();
        let doc = detail_doc();
        let mut view = View { detail: true, ..View::default() };
        let mut sc = Scene::new(&cfg);
        sc.focus = Some("r1".into());
        let mut frame = |sc: &mut Scene, view: &View, w: u16, now: f64| {
            let inp = Input { doc: &doc, state: &state, view, delivered: 0, unread: 0, now, wall: 0.0, cfg: &cfg, kitty: false,
                cell: (10, 20), focused_pane: true, mouse: false, project: "p", w, h: 24, activity: &[] };
            sc.frame(&inp).grid.to_text()
        };
        let t = frame(&mut sc, &view, 110, 0.0);
        let l: Vec<&str> = t.lines().collect();
        let col = |row: &str, s: &str| row.find(s).map(|b| row[..b].chars().count() as u16);
        // 110 columns: the panel has 44, its rule in column 66
        let rule = col(l[4], "│").unwrap();
        assert_eq!(rule, 66, "{t}");
        assert!(col(l[2], "Fix the scheduler").unwrap() > rule && l[3].contains("row · r1"), "{t}");
        assert!(t.contains("Fails one run in five on main.") && t.contains("Second line."), "{t}");
        assert!(t.contains("meta    kim") && t.contains("target  https://example.test/1") && t.contains("⏎ open the target"), "{t}");
        // the list is whole, left of the rule
        let row_y = l.iter().position(|r| r.contains("Bump the toolchain")).unwrap() as u16;
        assert!(col(l[row_y as usize], "Bump the toolchain").unwrap() < rule, "{t}");
        // hit-testing: the list's rows left of the rule, nothing in the panel
        assert_eq!(sc.row_at(10, row_y), Some("r2"));
        assert_eq!(sc.row_at(rule, row_y), None);
        assert_eq!(sc.row_at(rule + 10, row_y), None);
        assert!(sc.in_detail(rule + 10, row_y) && !sc.in_detail(10, row_y));
        // no button of the list reaches into the panel; the panel's own is the open hint
        let panel_hits: Vec<&str> = sc.hits.iter().filter(|h| h.0 + h.2 > rule && h.1 >= 2).map(|h| h.3.as_str()).collect();
        assert_eq!(panel_hits, ["mg:open/r1"], "{:?}", sc.hits);
        // moving the focus: the very next frame shows the new block, whole (no easing)
        // (the panel alone: a row draws its own detail's first line in the list)
        let side = |t: &str| t.lines().map(|r| r.chars().skip(rule as usize + 1).collect::<String>()).collect::<Vec<_>>().join("\n");
        sc.move_focus(1);
        let t = side(&frame(&mut sc, &view, 110, 0.016));
        assert!(t.contains("row · r2") && t.contains("line 1 of the long detail") && !t.contains("Second line."), "{t}");
        // longer than the panel: it scrolls on its own, and the list stays where it was
        let list_scroll = sc.scroll;
        sc.detail_scroll_by(30);
        let whole = frame(&mut sc, &view, 110, 0.032);
        let t = side(&whole);
        assert!(t.contains("line 30 of the long detail") && !t.contains("line 1 of the long detail"), "{t}");
        assert_eq!(sc.scroll, list_scroll);
        assert!(whole.contains("J/K scroll detail"), "{whole}");
        sc.detail_scroll_by(1000);
        let t = side(&frame(&mut sc, &view, 110, 0.05));
        assert!(t.contains("line 40 of the long detail"), "{t}");
        // a block with nothing more says so, and the scroll starts over
        sc.move_focus(1);
        let t = frame(&mut sc, &view, 110, 0.07);
        assert!(t.contains("nothing more on this row") && sc.detail_scroll == 0, "{t}");
        // off: the frame is the one without a panel
        view.detail = false;
        let off = frame(&mut sc, &view, 110, 1.0);
        assert!(!off.contains("nothing more") && !sc.in_detail(rule + 10, row_y), "{off}");
        assert_eq!(sc.row_at(rule + 10, row_y), Some("r2"));
    }

    /// Narrower than 100 columns the panel is under the list: a rule, then the block. The list's
    /// viewport ends above it, so a click in the panel is no row's.
    #[test]
    fn detail_panel_sits_under_the_list_when_narrow() {
        let cfg = Config::default();
        let state = State::default();
        let doc = detail_doc();
        let view = View { detail: true, ..View::default() };
        let mut sc = Scene::new(&cfg);
        sc.focus = Some("r1".into());
        let inp = Input { doc: &doc, state: &state, view: &view, delivered: 0, unread: 0, now: 0.0, wall: 0.0, cfg: &cfg, kitty: false,
            cell: (10, 20), focused_pane: true, mouse: false, project: "p", w: 70, h: 30, activity: &[] };
        let t = sc.frame(&inp).grid.to_text();
        let l: Vec<&str> = t.lines().collect();
        // rows 2..27 were the list's: the panel has the last 35% (8 rows), a rule on its first
        let rule = l.iter().position(|r| r.trim_start().starts_with("────") && !r.contains("reviews")).unwrap() as u16;
        assert_eq!((rule, sc.view_top + sc.view_h), (19, 19), "{t}");
        assert!(l[20].contains("Fix the scheduler") && l[21].contains("row · r1"), "{t}");
        assert!(l[23].contains("Fails one run in five on main."), "{t}");
        assert!(l[28].contains("j/k move") && l[29].contains("MARGIN"), "{t}");
        // the list above it is whole and full width
        assert!(l[3].contains("Fix the scheduler") && l[3].contains("mg:open/r1") || l[3].contains("Fix the scheduler"), "{t}");
        assert_eq!(sc.row_at(10, 3), Some("r1"));
        for y in rule..27 {
            assert_eq!(sc.row_at(10, y), None, "row {y} is the panel's");
            assert!(sc.in_detail(10, y));
        }
        // a pane too short for a panel draws none
        let mut sc = Scene::new(&cfg);
        let short = Input { h: 15, ..inp };
        let t = sc.frame(&short).grid.to_text();
        assert!(!t.contains("row · r1") && !sc.in_detail(10, 10), "{t}");
    }

    /// The artifact's reminders sit above the key hints, wrap rather than cut, take the list's
    /// last rows (never a row of their own from the status bar), and go in glance mode and in a
    /// short pane. Without reminders the frame is what it was.
    #[test]
    fn hints_row_sits_above_the_key_hints() {
        let cfg = Config::default();
        let (state, view) = (State::default(), View::default());
        let mut doc = head_doc(30);
        let frame = |doc: &Doc, w: u16, h: u16| {
            let mut sc = Scene::new(&cfg);
            let inp = Input { doc, state: &state, view: &view, delivered: 0, unread: 0, now: 0.0, wall: 0.0, cfg: &cfg, kitty: false,
                cell: (10, 20), focused_pane: true, mouse: false, project: "p", w, h, activity: &[] };
            let t = sc.frame(&inp).grid.to_text();
            (t, sc.view_top + sc.view_h)
        };
        let (plain, bottom0) = frame(&doc, 60, 20);
        doc.hints = vec!["r  re-request review".into(), "merge queue freezes Fri 3pm".into()];
        let (t, bottom) = frame(&doc, 60, 20);
        let l: Vec<&str> = t.lines().collect();
        assert_eq!(l[17].trim(), "r re-request review • merge queue freezes Fri 3pm", "{t}");
        assert!(l[18].contains("j/k move") && l[19].contains("MARGIN"), "{t}");
        assert_eq!(bottom, bottom0 - 1, "the list gives up one row");
        assert!(l[16].trim().is_empty(), "a blank row between the list and the reminders: {t}");
        assert!(plain.lines().nth(17).unwrap().trim().is_empty(), "{plain}");
        // narrower: each reminder on its own row; a long one wraps under its own text
        doc.hints.push("m  merge when green, but only after the release branch is cut on Thursday".into());
        let (t, _) = frame(&doc, 44, 24);
        let l: Vec<&str> = t.lines().collect();
        assert_eq!(l[18].trim(), "r re-request review", "{t}");
        assert_eq!(l[19].trim(), "merge queue freezes Fri 3pm", "{t}");
        assert_eq!(l[20].trim(), "m merge when green, but only after the", "{t}");
        assert_eq!(l[21], "    release branch is cut on Thursday", "{t}");
        assert!(!t.contains('…') || !l[18..22].iter().any(|r| r.contains('…')), "{t}");
        // glance mode and a short pane drop them
        assert!(!frame(&doc, 30, 24).0.contains("re-request"));
        assert!(!frame(&doc, 60, 13).0.contains("re-request"));
    }

    #[test]
    fn hints_flow_and_cap() {
        let rows = hint_flow(&["a  one".into(), "two".into(), "  ".into()], 40);
        assert_eq!(rows, vec![vec![("a ".to_string(), HintRun::Key), ("one".to_string(), HintRun::Text), (" • ".to_string(), HintRun::Sep), ("two".to_string(), HintRun::Text)]]);
        // more rows than the cap: the frame cuts the rows, never a reminder mid-word
        let many: Vec<String> = (0..9).map(|i| format!("reminder number {i} takes a whole row")).collect();
        assert_eq!(hint_flow(&many, 40).len(), 9);
    }

    /// The head's lines are authored text: they wrap rather than cut, at any width. The elapsed
    /// time stays on the running line's first row, and the list starts under the wrapped head.
    #[test]
    fn the_head_wraps_long_lines() {
        let cfg = Config::default();
        let goal = "In a long session, see at a glance where the work stands and what the agents are doing now";
        let step = "Wire the wrapped head into the frame and keep the viewport under it correct";
        let mut blocks = vec![json!({"type": "section", "id": "goal", "title": "Goal", "children": [
            {"type": "text", "id": "g", "text": goal},
            {"type": "todo", "id": "t1", "text": step, "status": "doing"},
            {"type": "todo", "id": "t2", "text": "another", "status": "doing"},
            {"type": "question", "id": "Q1", "text": "cli or mcp?", "options": ["cli", "mcp"]}]})];
        blocks.extend((0..40).map(|i| json!({"type": "text", "id": format!("r{i}"), "text": format!("filler row {i}")})));
        let doc: Doc = serde_json::from_value(json!({"id": "a", "title": "plan", "rev": 1, "blocks": blocks})).unwrap();
        let (state, view) = (State::default(), View::default());
        let mut sc = Scene::new(&cfg);
        let inp = Input { doc: &doc, state: &state, view: &view, delivered: 0, unread: 0, now: 0.0, wall: 0.0, cfg: &cfg, kitty: false,
            cell: (10, 20), focused_pane: false, mouse: false, project: "p", w: 60, h: 30, activity: &[] };
        let t = sc.frame(&inp).grid.to_text();
        let head: Vec<&str> = t.lines().skip(2).take(sc.head_h as usize).collect();
        let flat = head.iter().map(|l| l.trim()).collect::<Vec<_>>().join(" ").split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(!flat.contains('…'), "{t}");
        assert!(flat.contains(goal), "the whole goal: {t}");
        // the running step's first row ends in the count; its text continues on the next row
        let first = head.iter().position(|l| l.contains("Wire the wrapped head")).unwrap();
        assert!(head[first].trim_end().ends_with("+1"), "{t}");
        let step_text = format!("{} {}", head[first].replace("+1", "").trim(), head[first + 1].trim());
        assert!(step_text.split_whitespace().collect::<Vec<_>>().join(" ").ends_with(step), "{t}");
        assert!(head.last().unwrap().contains("─────"), "{t}");
        // goal 2 rows, step 2, needs 1, the rule: the list starts under them, and maps rows right
        assert_eq!((sc.head_h, sc.view_top), (6, 8), "{t}");
        assert_eq!(sc.item_at(7), None);
        assert_eq!(sc.item_at(8), Some("goal"));
        // the needs line's id is still its own target
        let ny = 2 + head.iter().position(|l| l.contains("need")).unwrap() as u16;
        let nx = head[(ny - 2) as usize].chars().position(|c| c == 'Q').unwrap() as u16;
        assert_eq!(sc.hit_at(nx, ny), Some("mg:head/Q1"));
    }

    /// The head folds (`zh`, a click on its rule): open, a long line wraps to three rows at most;
    /// folded, each line is one row, cut. The list follows the head's height, its lines still
    /// jump to their rows, and the fold is the view's, so it comes back with the view file.
    #[test]
    fn the_folded_head_is_two_rows_and_still_says_what_needs_you() {
        let cfg = Config::default();
        let goal = "The purpose of this whole dashboard, said at such length that at sixty columns it needs more than three rows to say: where every review stands, who is waiting on whom, what broke overnight, and which of them is yours to move today";
        let mut blocks = vec![json!({"type": "section", "id": "goal", "title": "Goal", "children": [
            {"type": "text", "id": "g", "text": goal},
            {"type": "todo", "id": "t1", "text": "wire the head", "status": "doing"},
            {"type": "question", "id": "Q1", "text": "cli or mcp?", "options": ["cli", "mcp"]}]})];
        blocks.extend((0..40).map(|i| json!({"type": "text", "id": format!("r{i}"), "text": format!("filler row {i}")})));
        let doc: Doc = serde_json::from_value(json!({"id": "a", "title": "plan", "rev": 1, "blocks": blocks})).unwrap();
        let state = State::default();
        let frame = |sc: &mut Scene, view: &View| {
            let inp = Input { doc: &doc, state: &state, view, delivered: 0, unread: 0, now: 0.0, wall: 0.0, cfg: &cfg, kitty: false,
                cell: (10, 20), focused_pane: false, mouse: false, project: "p", w: 60, h: 30, activity: &[] };
            sc.frame(&inp).grid.to_text()
        };
        // open: the purpose takes its three rows (and is cut only past them), then the others
        let mut view = View::default();
        let mut sc = Scene::new(&cfg);
        let t = frame(&mut sc, &view);
        let l: Vec<&str> = t.lines().collect();
        assert!(l[2].contains("The purpose of this whole dashboard") && l[4].trim_end().ends_with('…') && !l[2].contains('…') && !l[3].contains('…'), "{t}");
        assert!(l[5].contains("wire the head") && l[6].contains("1 needs you: Q1"), "{t}");
        assert!(l[7].contains(icon::DOWN) && l[7].contains("─────"), "{t}");
        assert_eq!((sc.head_h, sc.view_top), (6, 8));
        assert_eq!(sc.hit_at(10, 4), Some("mg:head/g"), "every row of a wrapped line is its button");
        assert_eq!(sc.hit_at(10, 5), Some("mg:head/t1"));
        assert_eq!(sc.hit_at(30, 7), Some("mg:headfold"));
        assert_eq!(sc.item_at(8), Some("goal"));
        // folded: two rows, the first line cut and the rule; the other lines are hidden, and
        // the rule's right end still says that something waits on him
        view.head_folded = true;
        let t = frame(&mut sc, &view);
        let l: Vec<&str> = t.lines().collect();
        assert!(l[2].contains("The purpose of this whole dashboard") && l[2].trim_end().ends_with('…'), "{t}");
        assert!(l[3].contains(icon::CHEVRON_UP) && l[3].contains("─────") && l[3].contains(" 1 needs you "), "{t}");
        assert!(!t.contains("1 needs you: Q1") && !l[4].contains("wire the head"), "{t}");
        assert_eq!((sc.head_h, sc.view_top), (2, 4));
        assert_eq!(sc.hit_at(10, 2), Some("mg:head/g"));
        assert_eq!(sc.hit_at(30, 3), Some("mg:headfold"));
        assert_eq!(sc.item_at(4), Some("goal"));
        // with no purpose the running step is the line; with neither, the needs line itself,
        // and then the rule has nothing more to say
        let only: Doc = serde_json::from_value(json!({"id": "a", "rev": 1, "blocks": std::iter::once(json!({"type": "question", "id": "Q1", "text": "cli or mcp?", "options": ["cli", "mcp"]}))
            .chain((0..40).map(|i| json!({"type": "text", "id": format!("r{i}"), "text": format!("filler row {i}")}))).collect::<Vec<_>>()})).unwrap();
        let inp = Input { doc: &only, state: &state, view: &view, delivered: 0, unread: 0, now: 0.0, wall: 0.0, cfg: &cfg, kitty: false,
            cell: (10, 20), focused_pane: false, mouse: false, project: "p", w: 60, h: 30, activity: &[] };
        let mut sc2 = Scene::new(&cfg);
        let t = sc2.frame(&inp).grid.to_text();
        let l: Vec<&str> = t.lines().collect();
        assert!(l[2].contains("1 needs you: Q1") && !l[3].contains("needs you") && sc2.head_h == 2, "{t}");
        // the fold is in the view file's JSON, and only when set
        let json = serde_json::to_string(&view).unwrap();
        let back: View = serde_json::from_str(&json).unwrap();
        assert!(back.head_folded && json.contains("head_folded"), "{json}");
        assert!(!serde_json::to_string(&View::default()).unwrap().contains("head_folded"));
    }

    /// Content inserted at the top while the view is at the very top stays in view; scrolled
    /// down, the anchor still holds his place.
    #[test]
    fn a_view_at_the_top_sticks_to_the_top() {
        let cfg = Config::default();
        let d1 = head_doc(30);
        let mut d2 = d1.clone();
        d2.rev = 2;
        d2.blocks.insert(0, serde_json::from_value(json!({"type": "text", "id": "fresh", "text": "landed while he was away"})).unwrap());
        let mut sc = Scene::new(&cfg);
        frame_text(&mut sc, &d1, 0.0);
        frame_text(&mut sc, &d2, 5.0);
        let t = frame_text(&mut sc, &d2, 9.0);
        assert_eq!(sc.scroll, 0);
        assert!(t.lines().nth(sc.view_top as usize).unwrap().contains("landed while he was away"), "{t}");
        let mut sc = Scene::new(&cfg);
        frame_text(&mut sc, &d1, 0.0);
        sc.scroll_by(9);
        let at = sc.item_at(sc.view_top).map(String::from);
        frame_text(&mut sc, &d2, 5.0);
        let t = frame_text(&mut sc, &d2, 9.0);
        assert!(sc.scroll > 9, "{t}");
        assert_eq!(sc.item_at(sc.view_top).map(String::from), at);
    }

    /// The title bar says how long ago the doc changed, and how much changed unseen, never the rev.
    #[test]
    fn the_title_bar_says_what_changed_not_the_rev() {
        let cfg = Config::default();
        let state = State::default();
        let view = View { seen_at: "2026-11-06T10:00:00Z".into(), ..View::default() };
        let mut d1 = head_doc(0);
        d1.updated = "2026-11-06T10:00:00Z".into();
        let wall = parse_rfc3339(&d1.updated).unwrap() + 53.0;
        let mut d2 = d1.clone();
        d2.rev = 207;
        d2.apply(&crate::doc::Op::Set { id: "g".into(), field: "text".into(), value: json!("a new goal") }).unwrap();
        let mut sc = Scene::new(&cfg);
        let mut top = |doc: &Doc, now: f64| {
            let inp = Input { doc, state: &state, view: &view, delivered: 0, unread: 0, now, wall, cfg: &cfg, kitty: false,
                cell: (10, 20), focused_pane: false, mouse: false, project: "p", w: 60, h: 16, activity: &[] };
            sc.frame(&inp).grid.to_text().lines().next().unwrap_or("").to_string()
        };
        let t = top(&d1, 0.0);
        assert!(t.contains(" 53s ago ") && !t.contains("rev") && !t.contains("new"), "{t}");
        top(&d2, 1.0);
        let t = top(&d2, 1.0 + UNSEEN_TAG);
        assert!(t.contains(" 1 new · 53s ") && !t.contains("rev") && !t.contains("207"), "{t}");
    }

    /// `]c` and `[c` walk the blocks changed since he last looked, in order, wrapping; the list
    /// outlives the change bars, and the next batch replaces it.
    #[test]
    fn the_changes_tour_visits_each_change_and_wraps() {
        let cfg = Config::default();
        let d1 = head_doc(30);
        let mut d2 = d1.clone();
        d2.rev = 2;
        for id in ["r2", "r20"] {
            d2.apply(&crate::doc::Op::Set { id: id.into(), field: "text".into(), value: json!(format!("changed {id}")) }).unwrap();
        }
        let mut sc = Scene::new(&cfg);
        frame_text(&mut sc, &d1, 0.0);
        assert_eq!(sc.change_step(true), None, "nothing changed yet");
        frame_text(&mut sc, &d2, 1.0);
        assert_eq!(sc.change_step(true).as_deref(), Some("r2"));
        assert_eq!(sc.change_step(true).as_deref(), Some("r20"));
        // the stop is scrolled into view
        let t = frame_text(&mut sc, &d2, 2.0);
        assert!(t.contains("changed r20"), "{t}");
        assert_eq!(sc.change_step(true).as_deref(), Some("r2"), "wraps");
        assert_eq!(sc.change_step(false).as_deref(), Some("r20"), "and back");
        // seen and faded: the tour still has them
        sc.retained.mark_seen(2.0);
        frame_text(&mut sc, &d2, 30.0);
        assert!(!sc.retained.nodes.values().any(|n| n.change.is_some()));
        assert_eq!(sc.change_step(false).as_deref(), Some("r2"));
        // the next batch replaces the list
        let mut d3 = d2.clone();
        d3.rev = 3;
        d3.apply(&crate::doc::Op::Set { id: "r9".into(), field: "text".into(), value: json!("changed r9") }).unwrap();
        frame_text(&mut sc, &d3, 31.0);
        assert_eq!(sc.change_step(true).as_deref(), Some("r9"));
        assert_eq!(sc.change_step(true).as_deref(), Some("r9"));
    }

    /// Narrower than 40 columns the pane is a strip: the title bar and the head, wrapped to the
    /// width, with no rows, key hints or status bar.
    #[test]
    fn glance_mode_is_the_title_and_the_wrapped_head() {
        let cfg = Config::default();
        let doc = head_doc(0);
        let mut sc = Scene::new(&cfg);
        let t = frame_text_w(&mut sc, &doc, 0.0, 30);
        let l: Vec<&str> = t.lines().collect();
        assert!(l[0].contains("plan"), "{t}");
        assert!(l[2].contains("See where the work") && l[3].contains("at a glance"), "{t}");
        assert!(l[4].contains("wire the head") && l[4].contains("+1"), "{t}");
        assert!(l[5].contains("2 need you: Q1, t3"), "{t}");
        assert!(!t.contains("MARGIN") && !t.contains("j/k") && !t.contains("cli or mcp?") && !t.contains("Goal"), "{t}");
        assert_eq!(sc.hit_at(8, 3), Some("mg:head/g"));
        assert_eq!(sc.item_at(6), None);
        // at 40 the rows are back
        let t = frame_text_w(&mut Scene::new(&cfg), &doc, 0.0, 40);
        assert!(t.contains("MARGIN") && t.contains("mg:todo/t1"), "{t}");
    }

    /// A dashboard through the whole frame: a rule, a table's header, and its rows as their own
    /// focusable items in the table's columns.
    #[test]
    fn a_table_draws_its_rows_as_items_in_shared_columns() {
        let doc: Doc = serde_json::from_value(json!({"id": "a", "rev": 1, "blocks": [
            {"type": "rule", "id": "r", "text": "Reviews"},
            {"type": "table", "id": "prs", "columns": [{"key": "pr", "title": "PR", "kind": "id"}, {"key": "title", "title": "Title"}, {"key": "n", "title": "N", "kind": "num"}], "children": [
                {"type": "tr", "id": "p1", "cells": {"pr": "#1", "title": "first", "n": 5}},
                {"type": "tr", "id": "p2", "cells": {"pr": "#22", "title": "the second", "n": 120}}]}]})).unwrap();
        let mut sc = Scene::new(&Config::default());
        let t = frame_text(&mut sc, &doc, 0.0);
        assert!(t.contains("── Reviews ──") && t.contains("PR  Title        N") && t.contains("#1  first        5") && t.contains("#22 the second 120"), "{t}");
        assert!(sc.focusable.contains(&"p1".to_string()) && sc.focusable.contains(&"p2".to_string()));
    }
}
