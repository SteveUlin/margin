//! margin's artifact components, drawn with vellum parts. Each draws one block into its own grid
//! (width = content width + 2: column 0 holds the focus caret, cards use the extra columns for their
//! ring) and returns it at its natural height. The scene animates heights and positions; components
//! only draw the state they're given.

use crate::doc::{Block, Doc};
use crate::select::short;
use crate::store::{Decision, State};
use crate::theme;
use crate::util::{local_hm, parse_rfc3339};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use vellum::anim::Motion;
use vellum::color::{Palette, Rgb};
use vellum::grid::Grid;
use vellum::image::Image;
use vellum::layout::Rect;
use vellum::parts::{self, Ring};
use vellum::style::Style;
use vellum::text::{truncate, width, wrap, wrap_runs};
use vellum::widgets::{self, Align, Outcome, Piece, Slot, Theme};

/// The text column after a row's leading icon: the icon, then two cells. Nerd-font icons draw
/// wider than their one cell, so a single space reads as touching.
pub(crate) const TX: u16 = 3;

/// The pane's icons: Nerd Font glyphs (private-use codepoints), each with the ordinary character
/// that stands in for it where no Nerd Font can be assumed (`margin render` without `--ansi`,
/// through [`plain_text`]). One table, so an icon can't be added without its stand-in.
macro_rules! icons {
    ($($(#[$doc:meta])* $name:ident = $glyph:literal => $plain:literal),* $(,)?) => {
        pub mod icon {
            $($(#[$doc])* pub const $name: &str = $glyph;)*
            /// Every icon above with its plain-text stand-in (one cell wide, no private-use codepoint).
            pub const PLAIN: &[(&str, &str)] = &[$(($glyph, $plain)),*];
        }
    };
}

icons! {
    DONE = "\u{f058}" => "✓",
    TODO = "\u{f10c}" => "○",
    BLOCKED = "\u{f057}" => "✗",
    CHECK = "\u{f00c}" => "✓",
    INFO = "\u{f05a}" => "ℹ",
    CLOCK = "\u{f017}" => "◷",
    QUESTION = "\u{f059}" => "?",
    BULB = "\u{f0eb}" => "✦",
    ROBOT = "\u{f06a9}" => "◈",
    USER = "\u{f007}" => "☺",
    EYE = "\u{f06e}" => "◎",
    HOURGLASS = "\u{f252}" => "⧗",
    RIGHT = "\u{f054}" => "▸",
    DOWN = "\u{f078}" => "▾",
    IMAGE = "\u{f03e}" => "▣",
    LINK = "\u{f0c1}" => "↗",
    SEND = "\u{f1d8}" => "➤",
    LEAF = "\u{f06c}" => "❧",
    PLAN = "\u{f0ae}" => "≡",
    DIFF = "\u{f440}" => "±",
    RADIO_ON = "\u{f192}" => "◉",
    RADIO_OFF = "\u{f10c}" => "○",
    WARN = "\u{f071}" => "⚠",
    PIN = "\u{f08d}" => "⚑",
    BELL = "\u{f0f3}" => "!",
    LOCK = "\u{f023}" => "⊠",
    COMMIT = "\u{f417}" => "◇",
    DOT = "\u{f192}" => "◉",
    FILE = "\u{f15b}" => "▤",
    STOP = "\u{f28d}" => "□",
    PAUSE = "\u{f04c}" => "‖",
    FEED = "\u{f09e}" => "≋",
    MOON = "\u{f186}" => "☾",
    TERM = "\u{f120}" => "❯",
    SKIP = "\u{f05e}" => "⊘",
    UP = "\u{f062}" => "↑",
    /// The up chevron of DOWN and RIGHT's family: a folded head.
    CHEVRON_UP = "\u{f077}" => "▴",
}

/// The shapes outside the icon set that are Nerd Font glyphs too: a pill's round caps (the tab
/// on show reads `[ Mine ]`) and the live strip's slanted ends.
const PLAIN_SHAPES: &[(&str, &str)] = &[(parts::CAP_L, "["), (parts::CAP_R, "]"), (crate::scene::STRIP_CAP_L, "◢"), (crate::scene::STRIP_CAP_R, "◣")];

/// A frame's text for a reader with no Nerd Font (`margin render` without `--ansi`): each
/// private-use glyph becomes its stand-in, one cell for one cell, so columns keep their places.
/// Without this a `state` cell of `done` or `fail` printed as a blank (or a box), and the column
/// read as broken. A private-use codepoint with no stand-in (an author's own glyph) becomes `·`.
pub fn plain_text(text: &str) -> String {
    let private = |c: char| matches!(c as u32, 0xE000..=0xF8FF | 0xF0000..=0x10FFFD);
    text.chars()
        .map(|c| {
            if !private(c) {
                return c;
            }
            let mut b = [0u8; 4];
            let s: &str = c.encode_utf8(&mut b);
            icon::PLAIN.iter().chain(PLAIN_SHAPES).find(|(g, _)| *g == s).and_then(|(_, plain)| plain.chars().next()).unwrap_or('·')
        })
        .collect()
}

/// Compact age for rows: "now", "12s", "4m", "2h", "3d".
pub fn age_short(secs: f64) -> String {
    let s = secs.max(0.0) as i64;
    match s {
        0..=2 => "now".into(),
        3..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        3600..=86399 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86400),
    }
}

/// How often an age label needs a redraw: every second under a minute, else every 30 s.
fn age_tick(secs: f64) -> f64 {
    if secs < 60.0 {
        1.0
    } else {
        30.0
    }
}

/// How long a step took or has run: "45s", "12m", "1h04", "3d". Unlike an age it never reads
/// "now": a duration that short is still a number.
pub fn took_short(secs: f64) -> String {
    let s = secs.max(0.0) as i64;
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        3600..=86399 => format!("{}h{:02}", s / 3600, s / 60 % 60),
        _ => format!("{}d", s / 86400),
    }
}

/// A step's duration in seconds, from the stamps the store keeps beside its status (state.json
/// `<id>#time`: `started` on the first doing, `done` on done; `Store::patch`): running, the time
/// since it started; done, start to finish, where sulin's own done mark is the finish when he
/// made it. None for any other status and for a step without stamps (one older than the
/// stamps, or never set to doing).
pub fn step_secs(state: &State, b: &Block, st: &str, wall: f64) -> Option<f64> {
    let t = &state.decisions.get(&crate::store::step_time_key(&b.id))?.value;
    let start = t["started"].as_str().and_then(parse_rfc3339)?;
    let end = match st {
        "doing" => wall,
        "done" => state.decisions.get(&b.id)
            .filter(|d| d.kind == "status" && crate::who::is_human(&d.by))
            .and_then(|d| parse_rfc3339(&d.at))
            .or_else(|| t["done"].as_str().and_then(parse_rfc3339))?,
        _ => return None,
    };
    Some((end - start).max(0.0))
}

fn tone(p: &Palette, name: &str, dflt: Rgb) -> Rgb {
    match name {
        "green" | "ok" => p.green,
        "yellow" | "warn" => p.yellow,
        "red" | "bad" => p.red,
        "orange" | "changed" => p.orange,
        "blue" | "claude" => p.blue,
        "aqua" => p.aqua,
        "purple" | "you" => p.purple,
        "grey" | "dim" => p.grey1,
        "fg" => p.fg,
        _ => dflt,
    }
}

/// Everything a component may read while drawing.
pub struct Ctx<'a> {
    pub doc: &'a Doc,
    pub state: &'a State,
    pub p: &'a Palette,
    pub now: f64,
    pub wall: f64,
    pub m: Motion,
    /// Highest event seq any Claude session has been handed.
    pub delivered: u64,
    /// Flash intensity per id (selection acknowledgement).
    pub flash: &'a HashMap<String, f32>,
    /// Animated progress values per progress id.
    pub progress: &'a HashMap<String, f32>,
    pub kitty: bool,
    pub cell: (u16, u16),
    pub images: &'a HashMap<String, Option<Image>>,
    pub focused: Option<&'a str>,
    /// Pulse start per question id (a card that needs sulin breathes when it appears).
    pub pulse: &'a HashMap<String, f64>,
    pub pulse_period: f32,
    pub pulse_breaths: f32,
    /// Mouse reporting is on: a click on a section header folds it, so the header needs no
    /// `mg:fold` token (that token is the double-click route when Zellij owns the mouse).
    pub mouse: bool,
    /// The block under the pointer (mouse mode), so its buttons can show.
    pub hover: Option<&'a str>,
    /// Draw graphs in Flog symbols (config graph_glyphs = "flog"), else box-drawing.
    pub flog: bool,
}

/// What a drawn block wants beyond its cells.
#[derive(Default)]
pub struct Drawn {
    pub grid: Option<Grid>,
    /// Kitty image placements: (image id key, x, y, cols, rows) relative to the block grid.
    pub images: Vec<(String, u16, u16, u16, u16)>,
    /// The block keeps moving on its own (spinner, timer): ask for frames at this period.
    pub tick: Option<f64>,
    /// A card ring that pulses: ask for frames while it breathes.
    pub ring: Option<Ring>,
    /// Clickable buttons: (x, y, width, token) relative to the block grid; a click inside one
    /// applies the token.
    pub buttons: Vec<(u16, u16, u16, String)>,
    /// Blank rows above the content (a section header's breathing room): the focus highlight
    /// starts below them.
    pub lead: u16,
}

/// Does this block draw in full? The focused row is never cut: whatever a row shortens with `…`
/// to stay one line (a table cell, a feed row's text, a log line) wraps instead while the row
/// has the focus, so every cut can be read by moving onto it. Moving off restores the one line.
pub fn in_full(cx: &Ctx, b: &Block) -> bool {
    cx.focused == Some(b.id.as_str())
}

/// Wrap `s` to `max` cells when `full`, else cut it to one line with `…`.
fn fit_lines(s: &str, max: usize, full: bool) -> Vec<String> {
    if full {
        wrap(s, max.max(1))
    } else {
        vec![truncate(s, max).into_owned()]
    }
}

pub fn decision<'a>(cx: &'a Ctx, id: &str) -> Option<&'a Decision> {
    cx.state.decisions.get(id)
}

pub fn todo_status(cx: &Ctx, b: &Block) -> String {
    status_of(cx.state, b)
}

/// What a click on a todo's circle (or space on it) sets: next, then skipped, then clear (the
/// step is Claude's again). None on a running step (it has a stop pill) and on one Claude
/// finished: done is Claude's to set, never sulin's (2026-11-08).
pub fn todo_cycle(st: &str, mine: bool) -> Option<&'static str> {
    match st {
        "doing" if !mine => None,
        "done" if !mine => None,
        "next" => Some("skipped"),
        "skipped" if mine => Some("clear"),
        _ if mine => Some("clear"),
        _ => Some("next"),
    }
}

/// Is this step sulin's own (r2)? His claim (`o` in the pane: a `<id>#own` decision) wins over
/// Claude's `owner` field, so he can take a step or hand it back.
pub fn yours(state: &State, b: &Block) -> bool {
    match state.decisions.get(&format!("{}#own", b.id)) {
        Some(d) => d.value == serde_json::json!(true),
        None => b.s("owner") == "you",
    }
}

/// What the circle (or space, or a bare token) does on this step. Claude's steps cycle next →
/// skipped → back (done is Claude's); sulin's own steps go open → done → open, the one place he
/// sets done, because the work is his.
pub fn cycle_for(state: &State, b: &Block) -> Option<&'static str> {
    let st = status_of(state, b);
    let mine = todo_mine(state, &b.id);
    if yours(state, b) {
        return Some(if st == "done" { if mine { "clear" } else { "todo" } } else { "done" });
    }
    todo_cycle(&st, mine)
}

/// A section's fold when sulin hasn't chosen one: the `folded` field, or folded because the work
/// in it is finished (every todo done or skipped, no open question). A finished thread keeps
/// its one header line, so the pane shows what's live without Claude pruning it.
pub fn default_folded(state: &State, b: &Block) -> bool {
    if b.flag("folded") {
        return true;
    }
    let (mut todos, mut open) = (0, false);
    crate::doc::walk(&b.children, &mut |c, _, _| match c.kind.as_str() {
        "todo" => {
            todos += 1;
            if !matches!(status_of(state, c).as_str(), "done" | "skipped") {
                open = true;
            }
        }
        "question" => {
            if !state.decisions.contains_key(&c.id) && !c.flag("applied") {
                open = true;
            }
        }
        _ => {}
    });
    todos > 0 && !open
}

/// An id short enough to show on its row (`t1`, `nx4`, `A1`); a longer one is a slug made from
/// the text and only shows when sulin asks for ids (`i`).
pub fn short_id(id: &str) -> bool {
    id.chars().count() <= 8 && !id.contains('-')
}

/// A todo status's colour: its glyph, its graph node, and the lines between nodes.
pub fn status_color(p: &Palette, st: &str) -> Rgb {
    match st {
        "done" => p.green,
        "doing" => p.yellow,
        "blocked" => p.red,
        "skipped" => p.grey1,
        "next" => p.purple,
        // open: the lighter grey, so a line fading to or from it stays visible
        _ => p.grey1,
    }
}

/// The claims a step rests on (`rests_on`), each with whether sulin has struck it.
pub fn rests_on(state: &State, b: &Block) -> Vec<(String, bool)> {
    b.strs("rests_on").into_iter().map(|c| {
        let struck = state.decisions.get(&c).map(|d| d.kind == "strike").unwrap_or(false);
        (c, struck)
    }).collect()
}

/// A step is stale when a claim it rests on is struck and the step isn't finished.
pub fn stale(state: &State, b: &Block) -> bool {
    !matches!(status_of(state, b).as_str(), "done" | "skipped") && rests_on(state, b).iter().any(|(_, s)| *s)
}

pub fn todo_mine(state: &State, id: &str) -> bool {
    state.decisions.get(id).map(|d| crate::who::is_human(&d.by)).unwrap_or(false)
}

pub fn status_of(state: &State, b: &Block) -> String {
    state.decisions.get(&b.id)
        .filter(|d| d.kind == "status")
        .and_then(|d| d.value.as_str().map(String::from))
        .unwrap_or_else(|| if b.s("status").is_empty() { "todo".into() } else { b.s("status").to_string() })
}

/// A question counts as resolved once Claude has been handed sulin's pick, or once Claude applied
/// the default (`applied: true`).
pub fn question_resolved(cx: &Ctx, b: &Block) -> bool {
    if b.flag("applied") {
        return true;
    }
    decision(cx, &b.id).map(|d| d.kind == "pick" && d.seq > 0 && d.seq <= cx.delivered).unwrap_or(false)
}

fn put(g: &mut Grid, x: u16, y: u16, s: &str, st: Style) -> u16 {
    g.put(x, y, s, st)
}

fn right(g: &mut Grid, y: u16, s: &str, st: Style, w: u16) -> u16 {
    let x = w.saturating_sub(width(s) as u16 + 1);
    g.put(x, y, s, st);
    x
}

fn tok(g: &mut Grid, cx: &Ctx, x: u16, y: u16, t: &str, ground: Rgb, flash: f32) -> u16 {
    let a = g.area();
    parts::token(g, x, y, t, cx.p, ground, flash, a)
}

fn fl(cx: &Ctx, id: &str) -> f32 {
    cx.flash.get(id).copied().unwrap_or(0.0)
}

fn new_grid(w: u16, h: u16, ground: Rgb) -> Grid {
    let mut g = Grid::new(w, h);
    let a = g.area();
    g.fill(a, Style::default().on(ground));
    g
}

/// An inline span of rich text.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Span {
    Plain,
    Bold,
    Code,
    /// A person's badge (`@[name]`): the text already holds glyph and name, this is its color.
    Who(Rgb),
}

/// A name `@[…]` may hold: short, on one line, and free of the other markers.
fn who_name(n: &str) -> bool {
    !n.is_empty() && n.trim() == n && n.chars().count() <= 48 && !n.contains(['[', '\n', '`', '*', '@'])
}

/// Inline markup for text and bullets: `**bold**`, `` `code` `` and `@[name]`, a person's badge
/// (the glyph and color every mention of that name gets; the theme decides them). An unclosed or
/// empty marker stays literal, so ordinary asterisks and backticks survive, and so does an `@[`
/// glued to the word before it (`user@[::1]` is an address, not a mention).
pub fn markup(s: &str) -> Vec<(String, Span)> {
    let mut out = Vec::new();
    let mut plain = String::new();
    let mut rest = s;
    while let Some(ch) = rest.chars().next() {
        if rest.starts_with("@[") && !plain.chars().next_back().map(|c| c.is_alphanumeric()).unwrap_or(false) {
            if let Some(name) = rest[2..].find(']').map(|e| &rest[2..2 + e]).filter(|n| who_name(n)) {
                if !plain.is_empty() {
                    out.push((std::mem::take(&mut plain), Span::Plain));
                }
                let m = theme::current().badge(name);
                // no-break spaces: a badge wraps as one word, glyph and name together, and the
                // one after the glyph keeps every badge's symbol the same size (SYMBOL_SPACE)
                out.push((format!("{}{}{}", m.glyph, widgets::SYMBOL_SPACE, name.replace(' ', "\u{a0}")), Span::Who(m.color)));
                rest = &rest[2 + name.len() + 1..];
                continue;
            }
        }
        let (open, kind) = if rest.starts_with("**") { ("**", Span::Bold) } else if ch == '`' { ("`", Span::Code) } else { ("", Span::Plain) };
        if !open.is_empty() {
            let body = &rest[open.len()..];
            if let Some(end) = body.find(open).filter(|&e| e > 0) {
                if !plain.is_empty() {
                    out.push((std::mem::take(&mut plain), Span::Plain));
                }
                out.push((body[..end].to_string(), kind));
                rest = &body[end + open.len()..];
                continue;
            }
        }
        plain.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    if !plain.is_empty() {
        out.push((plain, Span::Plain));
    }
    out
}

/// Draw one wrapped line of rich text; code takes aqua. `emph`: bold takes the theme's emphasis
/// color, so a lead-in reads first when scanning. A caller passes false when the line already
/// has a color that means something (a tone, a warning, a finished step's dim): there bold stays
/// in the line's own color, so the one emphasis color never overrides a meaning.
#[allow(clippy::too_many_arguments)]
fn rich(g: &mut Grid, cx: &Ctx, x: u16, y: u16, line: &[(String, Span)], base: Rgb, italic: bool, emph: bool) -> u16 {
    let p = cx.p;
    let e = emph.then(|| theme::current().emphasis);
    let mut x = x;
    for (s, sp) in line {
        x = put(g, x, y, s, span_style(p, *sp, base, italic, e).on(p.bg0));
    }
    x
}

/// A span's style, without a ground. Bold is `emph` when given, else the base color.
fn span_style(p: &Palette, sp: Span, base: Rgb, italic: bool, emph: Option<Rgb>) -> Style {
    match sp {
        Span::Plain => Style::fg(base).italic_if(italic),
        Span::Bold => Style::fg(emph.unwrap_or(base)).bold(),
        Span::Code => Style::fg(p.aqua),
        Span::Who(c) => Style::fg(c),
    }
}

/// Cut styled runs to `max` cells, ending in `…` when anything was dropped.
fn truncate_runs(runs: Vec<(String, Span)>, max: usize) -> Vec<(String, Span)> {
    let total: usize = runs.iter().map(|(s, _)| width(s)).sum();
    if total <= max {
        return runs;
    }
    let mut out = Vec::new();
    let mut left = max.saturating_sub(1);
    for (s, sp) in runs {
        let w = width(&s);
        if w <= left {
            left -= w;
            out.push((s, sp));
            continue;
        }
        let cut = truncate(&s, left + 1).trim_end_matches('…').to_string();
        out.push((cut, sp));
        break;
    }
    out.push(("…".into(), Span::Plain));
    out
}

/// Draw one block. `w` is the grid width (content + 2), `depth` its nesting, `first` whether it's
/// the first top-level item (sections leave a blank row above themselves otherwise).
pub fn draw(cx: &Ctx, b: &Block, w: u16, depth: usize, first: bool) -> Drawn {
    let mut d = draw_kind(cx, b, w, depth, first);
    if let (Some(pr), Some(g)) = (crate::select::open_proposal(b, cx.state), d.grid.as_ref()) {
        let (g2, btns) = proposal_strip(cx, &b.id, pr, g, w, if depth > 0 { 3 } else { 1 });
        d.grid = Some(g2);
        d.buttons.extend(btns);
    }
    d
}

/// Claude's proposal to change one of the person's decisions, under the block it targets: what and why
/// on one line, accept and decline on the next (buttons in mouse mode, tokens otherwise). Purple
/// is his colour: the decision is still his.
fn proposal_strip(cx: &Ctx, id: &str, pr: &serde_json::Value, g: &Grid, w: u16, ind: u16) -> (Grid, Vec<(u16, u16, u16, String)>) {
    let (p, g0) = (cx.p, cx.p.bg0);
    let h = g.area().h;
    let mut out = new_grid(w, h + 2, g0);
    let a = out.area();
    out.blit(g, 0, h, 0, 0, a);
    let what = format!("{}{}", pr["verb"].as_str().unwrap_or("?"), pr["value"].as_str().map(|v| format!(" {v}")).unwrap_or_default());
    let x = put(&mut out, ind + 2, h, icon::ROBOT, Style::fg(p.purple).on(g0).bold());
    // (two cells after the robot: the icon draws wider than its cell, as TX allows for on rows)
    let x = put(&mut out, x + 2, h, "proposes", Style::fg(p.grey1).on(g0));
    let x = parts::tag(&mut out, x + 1, h, &format!(" {what} "), p.purple, g0, a);
    let why = pr["why"].as_str().unwrap_or("");
    if !why.is_empty() {
        let room = w.saturating_sub(x + 2) as usize;
        put(&mut out, x + 1, h, &truncate(why, room), Style::fg(p.grey2).on(g0));
    }
    let mut buttons = Vec::new();
    let mut x = ind + 4;
    for (verb, label, c) in [("accept", " accept ", p.green), ("decline", " decline ", p.grey1)] {
        let t = short(verb, id, None);
        if cx.mouse {
            let e = parts::tag(&mut out, x, h + 1, label, c, g0, a);
            buttons.push((x, h + 1, e - x, t));
            x = e + 1;
        } else {
            x = tok(&mut out, cx, x, h + 1, &t, g0, fl(cx, id)) + 2;
        }
    }
    (out, buttons)
}

fn draw_kind(cx: &Ctx, b: &Block, w: u16, depth: usize, first: bool) -> Drawn {
    let g0 = cx.p.bg0;
    let ind: u16 = if depth > 0 { 3 } else { 1 };
    let p = cx.p;
    match b.kind.as_str() {
        "section" => {
            let open = !b.flag("_folded");
            // breathing room above a top-level section, or an open nested one; a run of folded
            // nested sections stays tight so it reads as one list
            let top = if first || (depth > 0 && !open) { 0 } else { 1 };
            let mut g = new_grid(w, 1 + top, g0);
            let y = top;
            let x0 = if depth > 0 { 3 } else { 1 };
            let (ch, chc) = if open { (icon::DOWN, p.purple) } else { (icon::RIGHT, p.grey1) };
            put(&mut g, x0, y, ch, Style::fg(chc).on(g0).bold());
            // headers carry colour so the eye finds them: green at the top level, blue nested
            let tc = tone(p, b.s("tone"), if depth > 0 { p.blue } else { p.green });
            let mut x = put(&mut g, x0 + 2, y, b.s("title"), Style::fg(tc).on(g0).bold());
            let n = b.children.len();
            if n > 0 {
                let a = g.area();
                x = parts::tag(&mut g, x + 1, y, &format!("{n}"), p.grey2, g0, a);
            }
            // without mouse reporting, a double-clicked token is the only pointer route to a fold
            let mut edge = w;
            if n > 0 && !cx.mouse {
                let t = short("fold", &b.id, None);
                let tw = width(&t) as u16;
                if w > x + tw + 4 {
                    edge = w.saturating_sub(tw + 1);
                    tok(&mut g, cx, edge, y, &t, g0, fl(cx, &b.id));
                }
            }
            let note = if open { b.s("note").to_string() } else { b.opt("summary").unwrap_or(b.s("note")).to_string() };
            if !note.is_empty() {
                let room = edge.saturating_sub(x + 3) as usize;
                if room > 4 {
                    right(&mut g, y, &truncate(&note, room), Style::fg(p.grey1).on(g0), edge.saturating_sub(1).max(1));
                }
            }
            Drawn { grid: Some(g), lead: top, ..Default::default() }
        }
        "text" => {
            let style = b.s("style");
            let base = match style {
                "dim" | "note" => p.grey1,
                "warn" => p.yellow,
                _ => p.fg,
            };
            let base = tone(p, b.s("tone"), base);
            let lines = wrap_runs(&markup(b.s("text")), w.saturating_sub(ind + 1) as usize);
            let mut g = new_grid(w, lines.len() as u16, g0);
            for (i, l) in lines.iter().enumerate() {
                rich(&mut g, cx, ind, i as u16, l, base, style == "note", b.s("tone").is_empty() && style != "warn");
            }
            Drawn { grid: Some(g), ..Default::default() }
        }
        "tail" => {
            // a command's output: a header (spinner or outcome, title, time), then while it runs
            // (or when opened) its last lines in a gutter box, errors red; done, it's one line
            let t = b.f.get("_tail").cloned().unwrap_or_default();
            let state = t["state"].as_str().unwrap_or("").to_string();
            let open = t["open"].as_bool().unwrap_or(false);
            let running = state == "running";
            let all: Vec<String> = t["lines"].as_array().map(|a| a.iter().filter_map(|l| l.as_str().map(String::from)).collect()).unwrap_or_default();
            let n = if open { 30 } else if running { b.num("lines").unwrap_or(6.0) as usize } else { 0 };
            let lines = &all[all.len().saturating_sub(n)..];
            // focused: the title and each line of output wrap; otherwise a line is cut
            let full = in_full(cx, b);
            let started = t["started"].as_str().and_then(parse_rfc3339);
            let ended = t["ended"].as_str().and_then(parse_rfc3339).or(if running { Some(cx.wall) } else { None });
            let took = match (started, ended) {
                (Some(a), Some(z)) => age_short((z - a).max(0.0)),
                _ => String::new(),
            };
            let right = match state.as_str() {
                "ok" => format!("done · {took}"),
                "failed" => format!("exit {} · {took}", t["code"].as_i64().unwrap_or(-1)),
                "running" => took.clone(),
                _ => "not run".into(),
            };
            let rx = w.saturating_sub(width(&right) as u16 + 1);
            let title = fit_lines(b.s("title"), rx.saturating_sub(ind + TX + 1) as usize, full);
            let body: Vec<(String, bool)> = lines
                .iter()
                .flat_map(|l| {
                    let err = crate::tail::is_error(l);
                    fit_lines(&l.replace('\t', "  "), w.saturating_sub(ind + 3) as usize, full).into_iter().map(move |x| (x, err))
                })
                .collect();
            let head_h = title.len().max(1) as u16;
            let mut g = new_grid(w, head_h + body.len() as u16, g0);
            let (gl, col) = match state.as_str() {
                "running" => (parts::spinner(cx.now, cx.m).to_string(), p.yellow),
                "ok" => (icon::DONE.to_string(), p.green),
                "failed" => (icon::BLOCKED.to_string(), p.red),
                _ => (icon::CLOCK.to_string(), p.grey1),
            };
            put(&mut g, ind, 0, &gl, Style::fg(col).on(g0).bold());
            put(&mut g, rx, 0, &right, Style::fg(if state == "failed" { p.red } else { p.grey1 }).on(g0));
            for (i, l) in title.iter().enumerate() {
                put(&mut g, ind + TX, i as u16, l, Style::fg(p.fg).on(g0).bold());
            }
            for (i, (l, err)) in body.iter().enumerate() {
                let y = head_h + i as u16;
                put(&mut g, ind, y, "▏", Style::fg(p.bg4).on(g0));
                put(&mut g, ind + 2, y, l, Style::fg(if *err { p.red } else { p.grey1 }).on(g0));
            }
            let tick = if running { Some(if cx.m.on { 0.1 } else { 1.0 }) } else { None };
            Drawn { grid: Some(g), tick, buttons: vec![(0, 0, w, format!("mg:tail/{}", b.id))], ..Default::default() }
        }
        "code" => {
            // monospace lines on a raised ground, a gutter rule, no wrap: a long line ends in …
            let lines: Vec<&str> = b.s("text").lines().collect();
            let mut g = new_grid(w, lines.len().max(1) as u16, g0);
            let room = w.saturating_sub(ind + 4) as usize;
            for (i, l) in lines.iter().enumerate() {
                let y = i as u16;
                g.tint(Rect::new(ind, y, w.saturating_sub(ind + 1), 1), p.bg1);
                put(&mut g, ind, y, "▏", Style::fg(p.bg4).on(p.bg1));
                put(&mut g, ind + 2, y, &truncate(&l.replace('\t', "    "), room), Style::fg(p.aqua).on(p.bg1));
            }
            let lang = b.s("language");
            if !lang.is_empty() && !lines.is_empty() {
                let lw = width(lang) as u16;
                if w > ind + lw + 12 && width(lines[0]) as u16 + lw + 4 < room as u16 {
                    put(&mut g, w.saturating_sub(lw + 2), 0, lang, Style::fg(p.grey0).on(p.bg1));
                }
            }
            Drawn { grid: Some(g), ..Default::default() }
        }
        "bullet" => {
            // `_level` (bullets above this one) and `_gap` come from the scene's flatten
            let level = b.num("_level").unwrap_or(0.0) as u16;
            let gap = b.flag("_gap") as u16;
            let x = ind + 2 * level;
            let glyph = ["•", "◦", "▪"][level as usize % 3];
            let (gc, tc) = if level == 0 { (p.green, p.fg) } else { (p.grey1, p.grey2) };
            let gc = tone(p, b.s("tone"), gc);
            let tc = if b.s("tone").is_empty() { tc } else { gc };
            let lines = wrap_runs(&markup(b.s("text")), w.saturating_sub(x + 3) as usize);
            let mut g = new_grid(w, lines.len() as u16 + gap, g0);
            put(&mut g, x, gap, glyph, Style::fg(gc).on(g0).bold());
            for (i, l) in lines.iter().enumerate() {
                rich(&mut g, cx, x + 2, gap + i as u16, l, tc, false, b.s("tone").is_empty());
            }
            Drawn { grid: Some(g), ..Default::default() }
        }
        "todo" => {
            // a step's text wraps (a graph row keeps to one line, so its lanes stay joined); the
            // grid starts tall and is cut to the rows used
            const TODO_ROWS: u16 = 16;
            let mut g = new_grid(w, if b.f.contains_key("_graph") { 1 } else { TODO_ROWS }, g0);
            let st = todo_status(cx, b);
            let mine = decision(cx, &b.id).map(|d| crate::who::is_human(&d.by)).unwrap_or(false);
            let mut tick = None;
            let (gl, col) = match st.as_str() {
                "done" => (icon::DONE.to_string(), p.green),
                "doing" => {
                    tick = Some(0.1);
                    (parts::spinner(cx.now, cx.m).to_string(), p.yellow)
                }
                "blocked" => (icon::BLOCKED.to_string(), p.red),
                "skipped" => (icon::SKIP.to_string(), p.grey1),
                "next" => (icon::UP.to_string(), p.purple),
                _ => (icon::TODO.to_string(), status_color(p, "todo")),
            };
            // a step resting on a struck claim is stale: red, whatever its status says
            let is_stale = stale(cx.state, b);
            // sulin's own step: pink while open (his colour), with his mark after the text
            let own = yours(cx.state, b);
            let col = if is_stale { p.red } else if own && !matches!(st.as_str(), "done" | "skipped") { p.purple } else { col };
            let f = fl(cx, &b.id);
            // in a section with `after` edges the status glyph becomes the graph's node: filled
            // done, hollow open, coloured by status, with the lanes drawn around it (graph.rs)
            let lane = b.f.get("_graph").cloned();
            let gx0 = ind;
            let ind = match &lane {
                Some(gv) => {
                    let colors: Vec<Option<Rgb>> = gv["colors"].as_array().map(|a| a.iter().map(|c| c.as_str().map(Rgb::hex)).collect()).unwrap_or_default();
                    let row = crate::graph::Row {
                        edges: Vec::new(),
                        cells: gv["cells"].as_str().unwrap_or("").chars().collect(),
                        node: gv["node"].as_u64().unwrap_or(0) as usize,
                        up: gv["u"].as_bool().unwrap_or(false),
                        down: gv["d"].as_bool().unwrap_or(false),
                        left: gv["l"].as_bool().unwrap_or(false),
                        right: gv["r"].as_bool().unwrap_or(false),
                    };
                    let node = crate::graph::node_glyph(&row, matches!(st.as_str(), "done" | "doing"));
                    for (i, c) in row.cells.iter().enumerate() {
                        // a line takes its edge's colour (the scene blends the two steps')
                        let lc = colors.get(i).copied().flatten().unwrap_or(p.grey0);
                        let c = *c;
                        let (ch, stl) = if c == crate::graph::NODE { (node, Style::fg(col.mix(p.purple, f)).on(g0).bold()) } else { (c, Style::fg(lc).on(g0)) };
                        let ch = crate::graph::glyph(ch, cx.flog);
                        let mut buf = [0u8; 4];
                        put(&mut g, gx0 + i as u16, 0, ch.encode_utf8(&mut buf), stl);
                    }
                    // the text starts after the lanes; the node column is the clickable glyph
                    (gx0 + row.cells.len() as u16).saturating_sub(TX)
                }
                None => {
                    put(&mut g, ind, 0, &gl, Style::fg(col.mix(p.purple, f)).on(g0).bold());
                    ind
                }
            };
            let node_x = lane.as_ref().map(|gv| gx0 + gv["node"].as_u64().unwrap_or(0) as u16).unwrap_or(gx0);
            // its id, dim, the way claims show theirs: what Claude names in chat, sulin can see.
            // A long id (a slug made from the text) stays hidden; `i` shows every id
            let tx = if short_id(&b.id) {
                put(&mut g, ind + TX, 0, &b.id, Style::fg(p.grey0).on(g0)) + 1
            } else {
                ind + TX
            };
            // sulin's controls. With the mouse on they are buttons on every open row, always in
            // reach (they act on release without a drag, so a stray press-and-drag hits nothing);
            // with the mouse off the one `mg:todo` token is the double-click route
            let tokv = |v: &str| short("todo", &b.id, Some(v));
            // done is Claude's to set, never sulin's: a running step offers stop, and every
            // other open step cycles from its circle (todo_cycle)
            let mut btns: Vec<(&str, String, Rgb)> = if cx.mouse && st == "doing" && !mine { vec![("stop", tokv("skipped"), p.red)] } else { vec![] };
            // a pill is the label plus a space and a cap each side; one cell between pills. On a
            // narrow pane the text keeps at least 16 cells: drop next, then skip
            let pills = |v: &[(&str, String, Rgb)]| v.iter().map(|(l, _, _)| width(l) as u16 + 5).sum::<u16>().saturating_sub(1);
            while btns.len() > 1 && w < ind + TX + 1 + pills(&btns) + 1 + 16 {
                btns.remove(0);
            }
            let bw = pills(&btns);
            let t = short("todo", &b.id, None);
            let tw = width(&t) as u16;
            let show_tok = !cx.mouse && st != "done" && w > tw + 24;
            // the control's cells at the right edge, margin included
            let ctl_w = if !btns.is_empty() { bw + 1 } else if show_tok { tw + 1 } else { 1 };
            // how long it took (done) or has run (doing): dim, at the first line's right, left of
            // any control. The text wraps clear of it; a pane too narrow for both drops it
            let secs = step_secs(cx.state, b, &st, cx.wall);
            // (a step marked doing and done in the same second took no time worth a label)
            let took = secs.filter(|s| st == "doing" || *s >= 1.0).map(took_short).filter(|d| w >= tx + 1 + ctl_w + width(d) as u16 + 2 + 16);
            if let (Some(s), true, true) = (secs, st == "doing", took.is_some()) {
                // the spinner's 0.1 s frames repaint the clock while motion is on; with motion
                // off the scene drops those, so the clock asks for its own: 1 s, then 30 s
                if !cx.m.on {
                    tick = Some(age_tick(s));
                }
            }
            let took_w = took.as_ref().map(|d| width(d) as u16 + 1).unwrap_or(0);
            // the text's right limit: the control (a token keeps a cell more), then the duration
            let right_w = ctl_w + show_tok as u16 + took_w + (took_w > 0 && ctl_w == 1) as u16;
            let text_room = w.saturating_sub(tx + 1 + right_w) as usize;
            let lines = if lane.is_some() {
                vec![truncate_runs(markup(b.s("text")), text_room)]
            } else {
                let mut l = wrap_runs(&markup(b.s("text")), text_room.max(1));
                l.truncate(TODO_ROWS as usize - 4);
                if l.is_empty() { vec![vec![]] } else { l }
            };
            let base = if matches!(st.as_str(), "done" | "skipped") { p.grey1 } else { p.fg };
            // everything after the text (marks, tags, the note) sits on its last line
            let ly = lines.len() as u16 - 1;
            let mut x = tx;
            for (i, runs) in lines.iter().enumerate() {
                let y = i as u16;
                x = if st == "doing" {
                    // the running step reads bold throughout
                    let mut x = tx;
                    for (s, sp) in runs {
                        let c = match sp {
                            Span::Code => p.aqua,
                            Span::Who(c) => *c,
                            _ => p.fg,
                        };
                        x = put(&mut g, x, y, s, Style::fg(c).on(g0).bold());
                    }
                    x
                } else if st == "skipped" {
                    let mut x = tx;
                    for (s, _) in runs {
                        x = put(&mut g, x, y, s, Style::fg(p.grey1).on(g0).strike());
                    }
                    x
                } else {
                    // a finished step stays dim throughout: its bold keeps the dim
                    rich(&mut g, cx, tx, y, runs, base, false, st != "done")
                };
            }
            if mine || own {
                x = put(&mut g, x + 1, ly, icon::USER, Style::fg(p.purple).on(g0));
            }
            // open questions whose default is taken when this step starts: `? Q1`, yellow, or
            // purple while sulin holds it
            let mut qs = Vec::new();
            crate::doc::walk(&cx.doc.blocks, &mut |q, _, _| {
                if q.kind == "question" && q.s("applies") == b.id && !question_resolved(cx, q) {
                    let held = decision(cx, &q.id).map(|d| d.kind == "hold").unwrap_or(false);
                    qs.push((q.id.clone(), held));
                }
            });
            for (q, held) in qs {
                let edge = w.saturating_sub(right_w);
                let t = format!("? {q}");
                if x + width(&t) as u16 + 4 > edge {
                    break;
                }
                let a = g.area();
                x = parts::tag(&mut g, x + 1, ly, &t, if held { p.purple } else { p.yellow }, g0, a);
            }
            // the claims it rests on: a dim id while they stand, red and crossed once struck
            for (c, struck) in rests_on(cx.state, b) {
                let edge = w.saturating_sub(right_w);
                let t = if struck { format!("✗ {c}") } else { c.clone() };
                if x + width(&t) as u16 + 4 > edge {
                    break;
                }
                let a = g.area();
                x = if struck { parts::tag(&mut g, x + 1, ly, &t, p.red, g0, a) } else { put(&mut g, x + 2, ly, &t, Style::fg(p.grey0).on(g0).italic()) };
            }
            let edge = w.saturating_sub(right_w);
            let aside = b.s("note");
            let mut used = ly + 1;
            if !aside.is_empty() {
                // the note follows the text when it fits whole, else it takes its own lines below
                let room = edge.saturating_sub(x + 2) as usize;
                if lane.is_some() || width(aside) <= room {
                    if room > 3 {
                        put(&mut g, x + 2, ly, &truncate(aside, room), Style::fg(p.grey1).on(g0));
                    }
                } else {
                    for l in wrap(aside, w.saturating_sub(tx + 2) as usize).iter().take(3) {
                        put(&mut g, tx, used, l, Style::fg(p.grey1).on(g0));
                        used += 1;
                    }
                }
            }
            let mut buttons = Vec::new();
            // the circle is the control: next → skip → back to Claude's. Two cells wide, since a
            // nerd-font icon draws over its neighbour
            if let Some(v) = cycle_for(cx.state, b).filter(|_| cx.mouse) {
                buttons.push((node_x, 0, 2, tokv(v)));
            }
            if !btns.is_empty() {
                let a = g.area();
                let mut bx = w.saturating_sub(bw + 1);
                for (label, token, c) in btns {
                    let x0 = bx;
                    bx = parts::tag(&mut g, bx, 0, &format!(" {label} "), c, g0, a) + 1;
                    buttons.push((x0, 0, bx - 1 - x0, token));
                }
            } else if show_tok {
                tok(&mut g, cx, w.saturating_sub(tw + 1), 0, &t, g0, f);
            }
            if let Some(d) = &took {
                let dc = if st == "doing" { p.grey1 } else { p.grey0 };
                put(&mut g, w.saturating_sub(ctl_w + took_w + (ctl_w > 1) as u16 - 1), 0, d, Style::fg(dc).on(g0));
            }
            if used < g.area().h {
                let mut cut = new_grid(w, used, g0);
                let a = cut.area();
                cut.blit(&g, 0, used, 0, 0, a);
                g = cut;
            }
            Drawn { grid: Some(g), tick, buttons, ..Default::default() }
        }
        "progress" => {
            let mut g = new_grid(w, 1, g0);
            let (done, total) = progress_counts(cx.doc, cx, b);
            let target = if total > 0 { done as f32 / total as f32 } else { b.num("value").unwrap_or(0.0) as f32 };
            let v = cx.progress.get(&b.id).copied().unwrap_or(target);
            let label = b.s("label");
            let mut x = ind;
            if !label.is_empty() {
                x = put(&mut g, x, 0, label, Style::fg(p.grey1).on(g0)) + 1;
            }
            let tail = if total > 0 { format!("{done}/{total}") } else { String::new() };
            let pct = format!("{:>3}%", (target * 100.0).round() as i32);
            let bw = w.saturating_sub(x + width(&tail) as u16 + width(&pct) as u16 + 5);
            let a = g.area();
            parts::progress_bar(&mut g, x, 0, bw, v, p.green, p.aqua, p.bg2, a);
            let mut tx = x + bw + 2;
            if !tail.is_empty() {
                tx = put(&mut g, tx, 0, &tail, Style::fg(p.fg).on(g0).bold()) + 1;
            }
            let pc = if target >= 1.0 { p.green } else { p.grey1 };
            put(&mut g, tx, 0, &pct, Style::fg(pc).on(g0));
            Drawn { grid: Some(g), ..Default::default() }
        }
        "timer" => {
            let mut g = new_grid(w, 1, g0);
            let (secs, over) = if let Some(u) = b.opt("until").and_then(parse_rfc3339) {
                let left = (u - cx.wall).ceil() as i64;
                (left, left < 0)
            } else {
                let s = b.opt("since").and_then(parse_rfc3339).unwrap_or(cx.wall);
                ((cx.wall - s).floor() as i64, false)
            };
            let col = if over { p.red } else { p.grey2 };
            let mut x = put(&mut g, ind, 0, icon::CLOCK, Style::fg(col).on(g0));
            x = put(&mut g, x + 1, 0, &parts::clock_text(secs), Style::fg(col).on(g0).bold_if(over));
            if !b.s("label").is_empty() {
                put(&mut g, x + 2, 0, b.s("label"), Style::fg(p.grey1).on(g0));
            }
            Drawn { grid: Some(g), tick: Some(1.0), ..Default::default() }
        }
        "status" => {
            let state = b.s("state");
            // focused: the command and its result wrap, each in its own column
            let full = in_full(cx, b);
            let cmdw = (w / 2).saturating_sub(ind + TX) as usize;
            let cmd = fit_lines(b.s("text"), cmdw, full);
            let cmd_w = cmd.iter().map(|l| width(l)).max().unwrap_or(0) as u16;
            let rx = (ind + TX + cmdw as u16 + 2).max(ind + TX + cmd_w + 2);
            let res_cut = width(b.s("result")) > 14;
            let tok_w = if cx.mouse || b.opt("target").is_none() { 0 } else { width(&short("open", &b.id, None)) as u16 + 2 };
            let res = if full && res_cut { wrap(b.s("result"), w.saturating_sub(rx + 1 + tok_w).max(1) as usize) } else { vec![truncate(b.s("result"), 14).into_owned()] };
            let mut g = new_grid(w, cmd.len().max(res.len()).max(1) as u16, g0);
            let mut tick = None;
            let (gl, col) = match state {
                "pass" => (icon::DONE.to_string(), p.green),
                "fail" => (icon::BLOCKED.to_string(), p.red),
                _ => {
                    tick = Some(0.1);
                    (parts::spinner(cx.now, cx.m).to_string(), p.yellow)
                }
            };
            put(&mut g, ind, 0, &gl, Style::fg(col).on(g0).bold());
            for (i, l) in cmd.iter().enumerate() {
                put(&mut g, ind + TX, i as u16, l, Style::fg(p.fg).on(g0));
            }
            let rcol = match state {
                "pass" => p.grey1,
                "fail" => p.red,
                _ => p.yellow,
            };
            let mut ex = rx;
            for (i, l) in res.iter().enumerate() {
                let e = put(&mut g, rx, i as u16, l, Style::fg(rcol).on(g0).bold_if(state == "fail"));
                if i == 0 {
                    ex = e;
                }
            }
            let spark: Vec<f32> = b.f.get("spark").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_f64().map(|f| f as f32)).collect()).unwrap_or_default();
            // a status with a target opens it: with the mouse on the row is the button, with it
            // off the `mg:open` token is the double-click route
            let open = b.opt("target").map(|_| short("open", &b.id, None));
            let buttons = open.iter().filter(|_| cx.mouse).map(|t| (ind, 0, w.saturating_sub(ind + 1), t.clone())).collect();
            let t = open.filter(|_| !cx.mouse);
            let tw = t.as_ref().map(|t| width(t) as u16 + 2).unwrap_or(0);
            if !spark.is_empty() {
                let sw = spark.len().min(w.saturating_sub(ex + 3 + tw) as usize);
                if sw >= 4 {
                    let a = g.area();
                    ex = parts::sparkline(&mut g, ex + 2, 0, &spark[spark.len() - sw..], p.aqua, p.yellow, g0, a);
                }
            }
            if let Some(t) = t {
                if w > ex + tw {
                    tok(&mut g, cx, (w + 1).saturating_sub(tw), 0, &t, g0, fl(cx, &b.id));
                }
            }
            Drawn { grid: Some(g), tick, buttons, ..Default::default() }
        }
        "question" => draw_question(cx, b, w),
        "claim" => draw_claim(cx, b, w, ind),
        "decided" => {
            let who = b.s("by");
            let g = decided_row(cx, ind, &b.id, b.s("text"), None, who == "you" || who == "sulin", b.opt("at"), w);
            Drawn { grid: Some(g), ..Default::default() }
        }
        "link" => {
            let mut g = new_grid(w, 1, g0);
            let target = b.s("target");
            let mut st = Style::fg(p.fg).on(g0);
            if target.starts_with("http") {
                st.link = g.link(target);
                st.ul = vellum::style::Underline::Single;
                st.ul_color = Some(p.bg4);
            }
            // with the mouse on the row is the button (the label keeps the token's room); with it
            // off the `mg:open` token is the double-click route
            let t = short("open", &b.id, None);
            let tw = if cx.mouse { 0 } else { width(&t) as u16 + 2 };
            let room = w.saturating_sub(ind + TX + 2 + tw) as usize;
            let label = if b.s("text").is_empty() { target } else { b.s("text") };
            // focused: a label too long for the line wraps
            let lines = fit_lines(label, room, in_full(cx, b));
            if lines.len() > 1 {
                let mut tall = new_grid(w, lines.len() as u16, g0);
                tall.links = std::mem::take(&mut g.links);
                g = tall;
            }
            put(&mut g, ind, 0, icon::LINK, Style::fg(p.aqua).on(g0));
            for (i, l) in lines.iter().enumerate() {
                put(&mut g, ind + TX, i as u16, l, st.with_fg(p.fg.mix(p.purple, fl(cx, &b.id))));
            }
            let mut buttons = Vec::new();
            if cx.mouse {
                buttons.extend((0..lines.len() as u16).map(|r| (ind, r, w.saturating_sub(ind + 1), t.clone())));
            } else {
                tok(&mut g, cx, w.saturating_sub(width(&t) as u16 + 1), 0, &t, g0, fl(cx, &b.id));
            }
            Drawn { grid: Some(g), buttons, ..Default::default() }
        }
        "image" => draw_image(cx, b, w, ind),
        "row" => draw_row(cx, b, w, ind),
        "rule" => {
            // a labeled rule is a heading: it keeps a blank row above itself, like a section
            let label = b.s("text");
            let top = (depth == 0 && !first && !label.is_empty()) as u16;
            let mut g = new_grid(w, 1 + top, g0);
            let a = g.area();
            widgets::rule(&mut g, ind, top, w.saturating_sub(ind + 1), label, &theme::current(), g0, a);
            Drawn { grid: Some(g), lead: top, ..Default::default() }
        }
        "table" => draw_table(cx, b, w, depth),
        "tr" => draw_tr(cx, b, w),
        "runs" => draw_runs(cx, b, w, ind),
        "feed" => draw_feed(cx, b, w, ind),
        "diff" => crate::diffblock::draw(cx, b, w, ind),
        k => {
            let mut g = new_grid(w, 1, g0);
            put(&mut g, ind, 0, &format!("{} unknown component {k}", icon::WARN), Style::fg(p.red).on(g0));
            Drawn { grid: Some(g), ..Default::default() }
        }
    }
}

pub fn progress_counts(doc: &Doc, cx: &Ctx, b: &Block) -> (usize, usize) {
    let Some(of) = b.opt("of") else { return (0, 0) };
    let Some(sec) = doc.find(of) else { return (0, 0) };
    let mut done = 0;
    let mut total = 0;
    crate::doc::walk(&sec.children, &mut |c, _, _| {
        if c.kind == "todo" {
            total += 1;
            if todo_status(cx, c) == "done" {
                done += 1;
            }
        }
    });
    (done, total)
}

/// A settled decision: its text wrapped with markup, the answer after it (on its own line when
/// the last one has no room), and who decided, with the time, at the first line's right.
fn decided_row(cx: &Ctx, ind: u16, id: &str, text: &str, answer: Option<(&str, bool)>, by_you: bool, at: Option<&str>, w: u16) -> Grid {
    let p = cx.p;
    let g0 = p.bg0;
    let when = at.and_then(parse_rfc3339).map(local_hm).unwrap_or_default();
    let rw = width(&when) as u16 + 3;
    let tx = ind + TX + width(id) as u16 + 1;
    let room = w.saturating_sub(tx + 1 + rw) as usize;
    let lines = wrap_runs(&markup(text), room.max(1));
    let last_w = lines.last().map(|l| l.iter().map(|(s, _)| width(s)).sum::<usize>()).unwrap_or(0);
    let ans_w = answer.map(|(a, _)| width(a) + 3).unwrap_or(0);
    let ans_below = ans_w > 0 && last_w + ans_w > room;
    let rows = lines.len().max(1) as u16;
    let mut grid = new_grid(w, rows + ans_below as u16, g0);
    let g = &mut grid;
    let y = 0;
    put(g, ind, y, icon::CHECK, Style::fg(p.green).on(g0).bold());
    put(g, ind + TX, y, id, Style::fg(p.green).on(g0).bold());
    let mut x = tx;
    for (i, l) in lines.iter().enumerate() {
        x = rich(g, cx, tx, i as u16, l, p.fg, false, true);
    }
    if let Some((a, mine)) = answer {
        let (ax, ay) = if ans_below { (tx, rows) } else { (x + 1, rows - 1) };
        let x = put(g, ax, ay, "→", Style::fg(p.grey1).on(g0));
        put(g, x + 1, ay, &truncate(a, w.saturating_sub(x + 2) as usize), Style::fg(if mine { p.purple } else { p.blue }).on(g0).bold());
    }
    let (gl, gc) = if by_you { (icon::USER, p.purple) } else { (icon::ROBOT, p.blue) };
    if !when.is_empty() {
        let tx = right(g, y, &when, Style::fg(p.grey0).on(g0), w);
        put(g, tx.saturating_sub(2), y, gl, Style::fg(gc).on(g0));
    } else {
        put(g, w.saturating_sub(2), y, gl, Style::fg(gc).on(g0));
    }
    grid
}

fn draw_question(cx: &Ctx, b: &Block, w: u16) -> Drawn {
    let p = cx.p;
    let g0 = p.bg0;
    let opts = b.strs("options");
    let default = b.s("default").to_string();
    let dec = decision(cx, &b.id).filter(|d| d.kind == "pick");
    let picked = dec.and_then(|d| d.value.as_str()).map(String::from);
    if question_resolved(cx, b) {
        let ans = picked.clone().unwrap_or(default.clone());
        let by_you = picked.is_some();
        // your pick's time, or when Claude took the default
        let at = dec.map(|d| d.at.as_str()).or_else(|| b.opt("applied_at"));
        let g = decided_row(cx, 1, &b.id, b.s("text"), Some((&ans, by_you)), by_you, at, w);
        return Drawn { grid: Some(g), ..Default::default() };
    }
    let ground = p.bg1;
    let inner = w.saturating_sub(6) as usize;
    let qlines = wrap(b.s("text"), inner.saturating_sub(width(&b.id) + 3));
    let why = if b.s("why").is_empty() { vec![] } else { wrap(b.s("why"), inner.saturating_sub(2)) };
    // the text area ends before `lim` (a pad column, then the ring): every row below wraps or
    // truncates to it, since a status line that ran into the ring at 56-62 columns broke the card
    let lim = w.saturating_sub(3);
    let x0 = 3;
    let avail = lim.saturating_sub(x0 + 2) as usize;
    // greedy rows of items, each `width + gap` wide; an item wider than a row gets one to itself
    let rows_of = |items: &[String], gap: usize| -> Vec<Vec<String>> {
        let mut rows: Vec<Vec<String>> = vec![vec![]];
        let mut rw = 0usize;
        for t in items {
            if rw + width(t) > avail && !rows.last().unwrap().is_empty() {
                rows.push(vec![]);
                rw = 0;
            }
            rows.last_mut().unwrap().push(t.clone());
            rw += width(t) + gap;
        }
        rows
    };
    // radios (glyph, space, label) and tokens wrap onto as many rows as they need
    // the answer Claude suggests carries a robot after its label; it is a mark, not a selection
    let suggest = b.s("suggest").to_string();
    let radio_rows = rows_of(&opts.iter().map(|o| format!("{} {o}{}", icon::RADIO_ON, if *o == suggest { format!(" {}", icon::ROBOT) } else { String::new() })).collect::<Vec<_>>(), 3);
    let toks: Vec<String> = opts.iter().map(|o| short("pick", &b.id, Some(o))).collect();
    // with the mouse on, the answers themselves are the buttons and the tokens stay off the card
    let tok_rows = if cx.mouse { vec![] } else { rows_of(&toks, 2) };
    // "talk about this": sulin wants the question discussed in chat before he picks
    let talking = picked.is_none() && decision(cx, &b.id).map(|d| d.kind == "talk").unwrap_or(false);
    // the status line while nothing is picked: a default only when one was asked for, else that
    // the card waits
    let (lead, lead_name) = if !default.is_empty() {
        (format!("{} default ", icon::HOURGLASS), default.clone())
    } else {
        (format!("{} waiting on your pick", icon::HOURGLASS), String::new())
    };
    // the status line: the lead (your pick, or the default) and its tail (sent, or when the
    // default applies) share a row when they fit, else the tail takes the next row
    let pill_text = picked
        .as_ref()
        .map(|pk| format!(" {} you picked {pk} ", icon::USER))
        .or_else(|| talking.then(|| format!(" {} you want to talk about this ", icon::USER)))
        .map(|t| truncate(&t, avail.saturating_sub(2)).into_owned());
    let lead_w = match &pill_text {
        Some(t) => width(t) + 2,
        None => width(&lead) + width(&lead_name),
    };
    // when Claude goes ahead (r4): a step ("taken when t4 starts"), a time (a countdown), or prose;
    // held, Claude stops there instead
    let held = decision(cx, &b.id).map(|d| d.kind == "hold").unwrap_or(false);
    let step = b.opt("applies").and_then(|a| cx.doc.find(a)).filter(|s| s.kind == "todo");
    let step_on = step.map(|s| matches!(status_of(cx.state, s).as_str(), "doing" | "done")).unwrap_or(false);
    let left = b.opt("takes_at").and_then(parse_rfc3339).map(|t| t - cx.wall);
    let mut tick = None;
    let (tail, tail_hot) = match (&picked, held) {
        (Some(_), _) => (Some(format!("{} sent · Claude reads it next turn", icon::SEND)), false),
        (None, true) => {
            let at = step.map(|s| format!("at {}", s.id)).or(left.map(|_| "at its time".to_string())).unwrap_or_else(|| "where it applies".into());
            (Some(format!("{} you asked Claude to wait · it stops {at}", icon::USER)), false)
        }
        (None, false) if talking => (Some(format!("{} sent · Claude raises it in chat", icon::SEND)), false),
        (None, false) if default.is_empty() => (None, false),
        (None, false) => match (step, left, b.opt("applies")) {
            (Some(s), _, _) if step_on => (Some(format!("{} started: taking the default now", s.id)), true),
            (Some(s), _, _) => (Some(format!("taken when {} starts", s.id)), false),
            (None, Some(l), _) if l <= 0.0 => (Some("taking the default now".into()), true),
            (None, Some(l), _) => {
                tick = Some(if l < 120.0 { 1.0 } else { 30.0 });
                (Some(format!("taken in {}", age_short(l))), false)
            }
            (None, None, Some(ap)) => (Some(format!("applies {ap}")), false),
            _ => (None, false),
        },
    };
    let tail_split = tail.as_ref().map(|t| lead_w + 1 + width(t) > avail).unwrap_or(false);
    let content = qlines.len() + why.len() + radio_rows.len() + tok_rows.len() + 1 + tail_split as usize;
    let h = content as u16 + 2;
    let mut g = new_grid(w, h, g0);
    let a = g.area();
    g.fill(Rect::new(1, 0, w.saturating_sub(2), h), Style::default().on(ground));
    // the ring: purple when focused; a calm breath while it waits on sulin, resting on a quiet edge
    let focused = cx.focused == Some(b.id.as_str());
    let rest = p.bg3;
    let ring = if focused {
        Ring::Plain(p.purple)
    } else if let Some(&since) = cx.pulse.get(&b.id) {
        Ring::Pulse { base: rest, peak: p.purple, period: cx.pulse_period, since, breaths: cx.pulse_breaths }
    } else {
        Ring::Plain(rest)
    };
    parts::frame(&mut g, Rect::new(1, 0, w.saturating_sub(2), h), &ring, parts::Corners::Round, g0, cx.now, cx.m, a);
    let mut y = 1;
    let bg = Style::default().on(ground);
    for (i, l) in qlines.iter().enumerate() {
        if i == 0 {
            put(&mut g, x0, y, icon::QUESTION, bg.with_fg(p.yellow).bold());
            let x = put(&mut g, x0 + 2, y, &b.id, bg.with_fg(p.yellow).bold());
            put(&mut g, x + 1, y, l, bg.with_fg(p.fg).bold());
        } else {
            put(&mut g, x0 + 3 + width(&b.id) as u16, y, l, bg.with_fg(p.fg).bold());
        }
        y += 1;
    }
    for l in &why {
        put(&mut g, x0 + 2, y, l, bg.with_fg(p.grey1).italic());
        y += 1;
    }
    let mut buttons = Vec::new();
    let mut oi = opts.iter();
    for row in &radio_rows {
        let mut x = x0 + 2;
        for o in oi.by_ref().take(row.len()) {
            // nothing is lit until sulin picks: a default or a suggestion is a line below, not a state
            let chosen = picked.as_deref() == Some(o);
            let col = if chosen { p.purple } else { p.grey1 };
            put(&mut g, x, y, if chosen { icon::RADIO_ON } else { icon::RADIO_OFF }, bg.with_fg(col).bold_if(chosen));
            let room = lim.saturating_sub(x + 2) as usize;
            let mut e = put(&mut g, x + 2, y, &truncate(o, room), bg.with_fg(if chosen { p.fg } else { p.grey2 }).bold_if(chosen));
            if *o == suggest && e + 3 <= lim {
                e = put(&mut g, e + 1, y, icon::ROBOT, bg.with_fg(p.blue));
            }
            if cx.mouse {
                buttons.push((x, y, e - x, short("pick", &b.id, Some(o))));
            }
            x = e + 3;
        }
        y += 1;
    }
    let f = fl(cx, &b.id);
    for row in &tok_rows {
        let mut x = x0 + 2;
        for t in row {
            // the flash lands on the token sulin picked
            let this = picked.as_deref().map(|pk| t.ends_with(&format!("={pk}"))).unwrap_or(false);
            x = tok(&mut g, cx, x, y, t, ground, if this { f } else { 0.0 }) + 2;
        }
        y += 1;
    }
    let x = x0 + 2;
    let e = if let Some(pill) = &pill_text {
        parts::pill(&mut g, x, y, pill, p.purple, p.bg0, ground, a)
    } else {
        let e = put(&mut g, x, y, &lead, bg.with_fg(p.grey1));
        put(&mut g, e, y, &truncate(&lead_name, lim.saturating_sub(e) as usize), bg.with_fg(p.grey2))
    };
    // at the status line's right: "talk about this", or "never mind" to take it back
    let mut tlim = lim;
    if cx.mouse && picked.is_none() && !held {
        let (label, c) = if talking { (" never mind ", p.purple) } else { (" talk about this ", p.purple) };
        let bw = width(label) as u16 + 2;
        let bx = lim.saturating_sub(bw);
        if bx > e + 8 {
            let x1 = parts::tag(&mut g, bx, y, label, c, ground, a);
            buttons.push((bx, y, x1 - bx, short("talk", &b.id, None)));
            tlim = bx.saturating_sub(1);
        }
    }
    if let Some(t) = &tail {
        let (tx, ty) = if tail_split { (x, y + 1) } else { (e + 1, y) };
        let lim_here = if ty == y { tlim } else { lim };
        let col = if held { p.purple } else if tail_hot { p.yellow } else { p.grey1 };
        put(&mut g, tx, ty, &truncate(t, lim_here.saturating_sub(tx) as usize), bg.with_fg(col).bold_if(tail_hot));
    }
    Drawn { grid: Some(g), ring: Some(ring), buttons, tick, ..Default::default() }
}

fn draw_claim(cx: &Ctx, b: &Block, w: u16, ind: u16) -> Drawn {
    let p = cx.p;
    let g0 = p.bg0;
    let dec = decision(cx, &b.id).filter(|d| d.kind == "strike");
    let ack = b.opt("ack");
    let reason = dec.and_then(|d| d.reason.clone());
    let f = fl(cx, &b.id);
    let struck = dec.is_some();
    // the control: with the mouse on, a strike pill that is always there, and the glyph itself
    // (bulb, or the ✕ once struck) toggles, so unstrike is a click on the ✕ with no pill; with
    // the mouse off, the `mg:strike` token, which toggles
    let t = short("strike", &b.id, None);
    let right_w = if cx.mouse { 1 } else { width(&t) as u16 + 2 };
    let (gl, gc) = if struck { (icon::BLOCKED, p.red) } else { (icon::BULB, p.yellow) };
    // the text wraps under itself, clear of the control on the right, with `**bold**` and `code`
    let x = ind + TX + width(&b.id) as u16;
    let room = w.saturating_sub(x + 2 + right_w) as usize;
    let lines = wrap_runs(&markup(b.s("text")), room.max(1));
    let rows = (lines.len() as u16).max(1);
    let mut g = new_grid(w, rows + reason.is_some() as u16 + ack.is_some() as u16, g0);
    
    put(&mut g, ind, 0, gl, Style::fg(gc).on(g0).bold());
    put(&mut g, ind + TX, 0, &b.id, Style::fg(gc).on(g0).bold());
    for (i, l) in lines.iter().enumerate() {
        if struck {
            let mut lx = x + 1;
            for (s, _) in l {
                lx = put(&mut g, lx, i as u16, s, Style::fg(p.grey1).on(g0).strike());
            }
        } else {
            rich(&mut g, cx, x + 1, i as u16, l, p.fg, false, true);
        }
    }
    let mut buttons = Vec::new();
    if cx.mouse {
        // the claim is its own button: a click anywhere on its text strikes it, and again unstrikes
        for r in 0..rows {
            buttons.push((ind, r, w.saturating_sub(ind + 1), t.clone()));
        }
    } else {
        tok(&mut g, cx, w.saturating_sub(width(&t) as u16 + 1), 0, &t, g0, f);
    }
    let mut y = rows;
    if let Some(r) = reason {
        put(&mut g, ind + 4, y, &truncate(&format!("you: “{r}”"), w.saturating_sub(ind + 5) as usize), Style::fg(p.purple).on(g0).italic());
        y += 1;
    }
    if let Some(ac) = ack {
        put(&mut g, ind + 4, y, icon::ROBOT, Style::fg(p.blue).on(g0));
        let x = put(&mut g, ind + 4 + TX, y, "Claude", Style::fg(p.aqua).on(g0).bold());
        put(&mut g, x + 1, y, &truncate(ac, w.saturating_sub(x + 2) as usize), Style::fg(p.grey2).on(g0));
    }
    Drawn { grid: Some(g), buttons, ..Default::default() }
}

/// A row's state glyph by name (`spin done fail you perm idle ended commit wc file term`, or any
/// character), its color, and whether it spins. Rows and a table's `state` cells share it.
fn row_glyph(cx: &Ctx, name: &str) -> (String, Rgb, bool) {
    let p = cx.p;
    match name {
        "spin" => (parts::spinner(cx.now, cx.m).to_string(), p.yellow, true),
        "done" | "ok" => (icon::DONE.into(), p.green, false),
        "fail" => (icon::BLOCKED.into(), p.red, false),
        "you" | "bell" => (icon::BELL.into(), p.purple, false),
        "perm" | "lock" => (icon::LOCK.into(), p.orange, false),
        "idle" => (icon::MOON.into(), p.grey1, false),
        "ended" => (icon::STOP.into(), p.grey0, false),
        "commit" => (icon::COMMIT.into(), p.blue, false),
        "wc" => (icon::DOT.into(), p.aqua, false),
        "file" => (icon::FILE.into(), p.grey1, false),
        "term" => (icon::TERM.into(), p.grey1, false),
        "" => ("•".into(), p.grey1, false),
        g => (g.to_string(), p.grey1, false),
    }
}

/// One line of a ledger or monitor, written by a feed or by Claude: a state glyph, an optional
/// tag pill, the text, a grey aside, and a right-aligned live age (`since`) or `meta`. `detail`
/// wraps under it, dim: `DETAIL_ROWS` rows at most, then cut (the detail panel has all of it).
/// The age ticks in the renderer, so a feed needn't rewrite a row just because time passed. A
/// row with a `target` opens it on click.
fn draw_row(cx: &Ctx, b: &Block, w: u16, ind: u16) -> Drawn {
    /// Rows a row's detail takes before it is cut.
    const DETAIL_ROWS: usize = 3;
    let p = cx.p;
    let g0 = p.bg0;
    // focused: nothing on the row is cut. Its text wraps in its column, an aside that was cut
    // goes under the text in full, and the detail drops its cap
    let full = in_full(cx, b);
    let dw = w.saturating_sub(ind + TX + 1).max(1) as usize;
    let mut detail = b.opt("detail").map(|d| wrap(d.trim_end(), dw)).unwrap_or_default();
    if detail.len() > DETAIL_ROWS && !full {
        detail.truncate(DETAIL_ROWS);
        let last = detail.pop().unwrap_or_default();
        detail.push(format!("{}…", truncate(&last, dw.saturating_sub(1)).trim_end_matches('…')));
    }
    // a tall grid, cut to the rows used once the text is laid out
    let mut g = new_grid(w, if full { 64 } else { 1 + detail.len() as u16 }, g0);
    let a = g.area();
    let (gl, dcol, spins) = row_glyph(cx, b.s("glyph"));
    let mut tick = spins.then_some(0.1);
    let gc = tone(p, b.s("tone"), dcol);
    put(&mut g, ind, 0, &gl, Style::fg(gc).on(g0).bold());
    let mut x = ind + TX;
    if let Some(t) = b.opt("tag") {
        x = parts::tag(&mut g, x, 0, &truncate(t, if full { (w / 2) as usize } else { 14 }), tone(p, b.s("tag_tone"), p.grey2), g0, a) + 1;
    }
    // whose row it is: the name's badge, before the text
    if let Some(n) = b.opt("who") {
        let ps = widgets::truncate_pieces(widgets::who_pieces(&theme::current(), n), 18);
        x = widgets::put_pieces(&mut g, x, 0, &ps, g0, a) + 1;
    }
    // right edge: a live age, else meta
    let age = b.opt("since").and_then(parse_rfc3339).map(|t| cx.wall - t);
    let right_s = match age {
        Some(s) => {
            tick = Some(tick.map(|t: f64| t.min(age_tick(s))).unwrap_or(age_tick(s)));
            age_short(s)
        }
        None => b.s("meta").to_string(),
    };
    let rx = w.saturating_sub(width(&right_s) as u16 + 1);
    if !right_s.is_empty() {
        let fresh = age.map(|s| s < 10.0).unwrap_or(false);
        put(&mut g, rx, 0, &right_s, Style::fg(if fresh { p.fg } else { p.grey1 }).on(g0));
    }
    let aside = b.s("aside");
    let aw = if aside.is_empty() { 0 } else { (width(aside) as u16).min(w / 3) };
    let ax = rx.saturating_sub(aw + 2);
    let text_room = ax.saturating_sub(x + 1) as usize;
    let tcol = tone(p, b.s("text_tone"), p.fg);
    let mut text = fit_lines(b.s("text"), text_room.max(4), full);
    text.truncate(40);
    let mut tx = x;
    for (i, l) in text.iter().enumerate() {
        let e = put(&mut g, x, i as u16, l, Style::fg(tcol).on(g0).bold_if(b.flag("bold")));
        if i == 0 {
            tx = e;
        }
    }
    let acol = tone(p, b.s("aside_tone"), p.grey1);
    let aside_cut = width(aside) > aw as usize || ax <= tx;
    if aw > 0 && ax > tx && !(full && aside_cut) {
        put(&mut g, ax, 0, &truncate(aside, aw as usize), Style::fg(acol).on(g0));
    }
    let mut y = text.len().max(1) as u16;
    if full && aside_cut && !aside.is_empty() {
        for l in wrap(aside, dw).iter().take(8) {
            put(&mut g, ind + TX, y, l, Style::fg(acol).on(g0));
            y += 1;
        }
    }
    for l in detail.iter().take(g.area().h.saturating_sub(y) as usize) {
        put(&mut g, ind + TX, y, l, Style::fg(p.grey1).on(g0).italic());
        y += 1;
    }
    if y < g.area().h {
        let mut cut = new_grid(w, y, g0);
        let ca = cut.area();
        cut.blit(&g, 0, y, 0, 0, ca);
        g = cut;
    }
    let mut buttons = Vec::new();
    if cx.mouse && b.opt("target").is_some() {
        buttons.push((ind, 0, w.saturating_sub(ind + 1), short("open", &b.id, None)));
    }
    Drawn { grid: Some(g), tick, buttons, ..Default::default() }
}

/// The sooner of a pending redraw period and `t`.
fn sooner(tick: &mut Option<f64>, t: f64) {
    *tick = Some(tick.map_or(t, |x| x.min(t)));
}

/// How a table column's cells are read and drawn (doc.rs `COL_KINDS`).
#[derive(Clone, Copy, PartialEq, Debug)]
enum ColKind {
    Text,
    Num,
    Who,
    Id,
    Runs,
    Age,
    State,
}

/// The most runs a strip column shows unless its `width` says otherwise, and the fewest it keeps
/// when a narrow pane squeezes it.
const RUNS_MAX: usize = 20;
const RUNS_MIN: usize = 5;
/// The narrowest a text column gets while an optional column is still showing: under this, the
/// optional columns go first. (With none left, text shrinks on, to vellum's `FLEX_FLOOR`.)
const TEXT_MIN: u16 = 20;
/// An age column's width: "now", "59s", "23h", "999d".
const AGE_W: u16 = 4;

/// One table laid out for one pane width: where each visible column sits. Every row of the table
/// draws from the same one.
pub(crate) struct TableLayout {
    /// The table's left edge (its indent).
    ind: u16,
    keys: Vec<String>,
    kinds: Vec<ColKind>,
    cols: Vec<widgets::Column>,
    slots: Vec<Slot>,
    zebra: bool,
    /// Per column: the cells a `runs` column keeps after its strip for the failure count
    /// (`summary: true`), the blank between them included; 0 for every other column.
    sum_w: Vec<u16>,
}

/// A runs cell's failure count as a `summary: true` column prints it: `1/6`, and whether the
/// newest run failed. None when nothing ran.
fn runs_count_text(v: Option<&serde_json::Value>) -> Option<(String, bool)> {
    widgets::run_count(&outcomes(v)).map(|(failed, ran, newest)| (format!("{failed}/{ran}"), newest))
}

/// A cell's value as text: strings as written, numbers and bools printed, anything else empty.
fn cell_text(v: Option<&serde_json::Value>) -> String {
    match v {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Number(n)) => n.to_string(),
        Some(serde_json::Value::Bool(b)) => b.to_string(),
        _ => String::new(),
    }
}

/// A run strip's values; a word that isn't an outcome (the schema refuses them) reads as no run.
fn outcomes(v: Option<&serde_json::Value>) -> Vec<Outcome> {
    v.and_then(|v| v.as_array()).map(|a| a.iter().map(|x| Outcome::parse(x.as_str()).unwrap_or(Outcome::None)).collect()).unwrap_or_default()
}

/// A time as `since` fields write it: RFC 3339 text, or unix seconds.
fn cell_time(v: Option<&serde_json::Value>) -> Option<f64> {
    let v = v?;
    v.as_f64().or_else(|| v.as_str().and_then(parse_rfc3339))
}

/// The cells a value wants in a column of `kind`, before any squeezing.
fn cell_natural(kind: ColKind, v: Option<&serde_json::Value>, th: &Theme) -> u16 {
    let w = match kind {
        ColKind::Text => markup(&cell_text(v)).iter().map(|(s, _)| width(s)).sum(),
        ColKind::Num | ColKind::Id => width(&cell_text(v)),
        ColKind::Who => widgets::pieces_width(&widgets::who_pieces(th, &cell_text(v))),
        ColKind::Runs => widgets::runs_width(outcomes(v).len().min(RUNS_MAX), th.runs.gap) as usize,
        ColKind::Age => AGE_W as usize,
        // an icon and a blank: nerd-font icons draw wider than their cell
        ColKind::State => 2,
    };
    w.min(u16::MAX as usize) as u16
}

/// A cell's pieces for a slot `w` cells wide. A live cell (an age, a spinner) asks for its
/// redraw through `tick`.
fn cell_pieces(cx: &Ctx, th: &Theme, kind: ColKind, v: Option<&serde_json::Value>, w: u16, sum_w: u16, tick: &mut Option<f64>) -> Vec<Piece> {
    let p = cx.p;
    match kind {
        ColKind::Text => markup(&cell_text(v)).into_iter().map(|(s, sp)| (s, span_style(p, sp, p.fg, false, Some(th.emphasis)))).collect(),
        ColKind::Num => vec![(cell_text(v), Style::fg(p.fg))],
        ColKind::Who => widgets::who_pieces(th, &cell_text(v)),
        ColKind::Id => widgets::id_pieces(th, &cell_text(v)),
        ColKind::Runs => {
            let mut ps = widgets::run_pieces(th, &outcomes(v), w.saturating_sub(sum_w));
            // the count sits in its own slot after the strip, right-aligned, so the strips of
            // every row still end in one column
            if let Some((text, newest_failed)) = runs_count_text(v).filter(|_| sum_w > 0) {
                let pad = (sum_w as usize).saturating_sub(width(&text) + 1);
                // the cell after the newest run is not blank (widgets::SYMBOL_SPACE)
                ps.push((format!("{}{}", widgets::SYMBOL_SPACE, " ".repeat(pad)), Style::default()));
                ps.push((text, Style::fg(if newest_failed { p.red } else { p.grey1 })));
            }
            ps
        }
        ColKind::Age => match cell_time(v) {
            Some(t) => {
                let s = cx.wall - t;
                sooner(tick, age_tick(s));
                vec![(age_short(s), Style::fg(if s < 10.0 { p.fg } else { p.grey1 }))]
            }
            None => Vec::new(),
        },
        ColKind::State => match cell_text(v).as_str() {
            "" => Vec::new(),
            name => {
                let (gl, c, spins) = row_glyph(cx, name);
                if spins {
                    sooner(tick, 0.1);
                }
                vec![(gl, Style::fg(c).bold())]
            }
        },
    }
}

/// Lay a table out for a grid `w` wide: read its columns, measure every row's cells once, and
/// let vellum place the columns.
fn table_layout_of(t: &Block, depth: usize, w: u16, th: &Theme) -> TableLayout {
    LAYOUTS.with(|n| n.set(n.get() + 1));
    let ind: u16 = if depth > 0 { 3 } else { 1 };
    let specs = t.f.get("columns").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let (mut keys, mut kinds, mut cols, mut sum_w) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for c in &specs {
        let key = c["key"].as_str().unwrap_or("").to_string();
        let kind = match c["kind"].as_str().unwrap_or("text") {
            "num" => ColKind::Num,
            "who" => ColKind::Who,
            "id" => ColKind::Id,
            "runs" => ColKind::Runs,
            "age" => ColKind::Age,
            "state" => ColKind::State,
            _ => ColKind::Text,
        };
        // numbers, ages and strips read from the right: digits line up, the newest run lines up
        let align = match c["align"].as_str() {
            Some("left") => Align::Left,
            Some("right") => Align::Right,
            Some("center") => Align::Center,
            _ if matches!(kind, ColKind::Num | ColKind::Age | ColKind::Runs) => Align::Right,
            _ => Align::Left,
        };
        let natural = t.children.iter().map(|r| cell_natural(kind, r.f.get("cells").and_then(|cs| cs.get(&key)), th)).max().unwrap_or(0);
        let fixed = c["width"].as_u64().map(|n| n.clamp(1, 500) as u16);
        // a runs column's failure count: as wide as the widest count of any row, plus a blank
        let count = if kind == ColKind::Runs && c["summary"].as_bool().unwrap_or(false) {
            t.children.iter().filter_map(|r| runs_count_text(r.f.get("cells").and_then(|cs| cs.get(&key)))).map(|(s, _)| width(&s) as u16 + 1).max().unwrap_or(0)
        } else {
            0
        };
        let (natural, min, flex) = match (fixed, kind) {
            (Some(n), _) => (n, n, false),
            (None, ColKind::Text) => (natural, TEXT_MIN, true),
            (None, ColKind::Runs) => (natural + count, widgets::runs_width(RUNS_MIN, th.runs.gap) + count, false),
            (None, _) => (natural, natural, false),
        };
        sum_w.push(count);
        cols.push(widgets::Column { title: c["title"].as_str().unwrap_or("").to_string(), align, natural, min, flex, optional: c["optional"].as_bool().unwrap_or(false), strip: kind == ColKind::Runs });
        keys.push(key);
        kinds.push(kind);
    }
    let slots = widgets::table_layout(&cols, w.saturating_sub(ind + 1));
    let zebra = t.f.get("zebra").and_then(|z| z.as_bool()).unwrap_or(th.zebra);
    TableLayout { ind, keys, kinds, cols, slots, zebra, sum_w }
}

/// The layouts of every table in the doc being drawn, built once per doc revision, pane width
/// and theme, so a 200-row table is measured once per frame at most and each row only looks its
/// table up. `old` keeps the previous revision's rows: a row that was just removed still draws
/// (as a ghost, while it animates out) in the columns it had.
#[derive(Default)]
struct TableCache {
    /// (doc address, doc id, rev, grid width, theme generation)
    key: Option<(usize, String, u64, u16, u64)>,
    tables: HashMap<String, Rc<TableLayout>>,
    /// row id → its table's layout and its index among the rows
    rows: HashMap<String, (Rc<TableLayout>, usize)>,
    old_tables: HashMap<String, Rc<TableLayout>>,
    old_rows: HashMap<String, (Rc<TableLayout>, usize)>,
}

thread_local! {
    static TABLES: RefCell<TableCache> = RefCell::new(TableCache::default());
    /// Layouts computed on this thread (tests count them).
    static LAYOUTS: Cell<usize> = const { Cell::new(0) };
}

/// Run `f` on the table cache, rebuilt first if the doc, the width or the theme changed.
fn with_tables<R>(cx: &Ctx, w: u16, f: impl FnOnce(&TableCache) -> R) -> R {
    let key = (cx.doc as *const Doc as usize, cx.doc.id.clone(), cx.doc.rev, w, theme::generation());
    TABLES.with(|c| {
        let mut c = c.borrow_mut();
        if c.key.as_ref() != Some(&key) {
            // another revision (or another doc): what was laid out is now the past
            if c.key.as_ref().map(|k| (&k.1, k.2)) != Some((&key.1, key.2)) {
                c.old_tables = std::mem::take(&mut c.tables);
                c.old_rows = std::mem::take(&mut c.rows);
            }
            c.tables.clear();
            c.rows.clear();
            let th = theme::current();
            crate::doc::walk(&cx.doc.blocks, &mut |b, _, depth| {
                if b.kind == "table" {
                    let lay = Rc::new(table_layout_of(b, depth, w, &th));
                    for (i, r) in b.children.iter().enumerate() {
                        c.rows.insert(r.id.clone(), (lay.clone(), i));
                    }
                    c.tables.insert(b.id.clone(), lay);
                }
            });
            c.key = Some(key);
        }
        f(&c)
    })
}

/// A table's own two rows: the column titles, dim and bold, over a thin rule. Its rows are its
/// children (`tr`), each drawn by `draw_tr` in the columns laid out here.
fn draw_table(cx: &Ctx, b: &Block, w: u16, depth: usize) -> Drawn {
    let g0 = cx.p.bg0;
    let th = theme::current();
    let lay = with_tables(cx, w, |c| c.tables.get(&b.id).or_else(|| c.old_tables.get(&b.id)).cloned())
        // a table that isn't in the doc (a ghost from before this pane opened) lays itself out
        .unwrap_or_else(|| Rc::new(table_layout_of(b, depth, w, &th)));
    let empty = b.opt("empty").filter(|_| b.children.is_empty());
    let mut g = new_grid(w, 2 + empty.is_some() as u16, g0);
    let a = g.area();
    let used = widgets::table_header(&mut g, lay.ind, 0, &lay.cols, &lay.slots, &th, g0, a);
    let mut rows = used;
    if let Some(e) = empty {
        put(&mut g, lay.ind, used, &truncate(e, w.saturating_sub(lay.ind + 1) as usize), Style::fg(cx.p.grey1).on(g0).italic());
        rows += 1;
    }
    if rows < g.area().h {
        let mut cut = new_grid(w, rows, g0);
        let ca = cut.area();
        cut.blit(&g, 0, rows, 0, 0, ca);
        g = cut;
    }
    Drawn { grid: Some(g), ..Default::default() }
}

/// One table row: each cell in its column's slot, cut with `…` (a table is the one place text
/// doesn't wrap, until the row has the focus: `in_full`). Alternate rows take the theme's stripe when the table is zebra. A row with a
/// `target` opens it on click.
fn draw_tr(cx: &Ctx, b: &Block, w: u16) -> Drawn {
    let p = cx.p;
    let g0 = p.bg0;
    let cells = b.f.get("cells");
    let Some((lay, idx)) = with_tables(cx, w, |c| c.rows.get(&b.id).or_else(|| c.old_rows.get(&b.id)).cloned()) else {
        // a row with no table to ask (it never had one in this pane): its cells, plainly
        let mut g = new_grid(w, 1, g0);
        let line: Vec<String> = cells.and_then(|c| c.as_object()).map(|o| o.values().map(|v| cell_text(Some(v))).filter(|s| !s.is_empty()).collect()).unwrap_or_default();
        put(&mut g, 3, 0, &truncate(&line.join("  "), w.saturating_sub(4) as usize), Style::fg(p.grey1).on(g0));
        return Drawn { grid: Some(g), ..Default::default() };
    };
    let th = theme::current();
    // focused: a text cell too long for its column wraps inside it and the row grows; the other
    // columns stay on the first line
    let full = in_full(cx, b);
    let mut tick = None;
    let lines: Vec<Vec<Vec<Piece>>> = lay
        .slots
        .iter()
        .map(|slot| {
            let kind = lay.kinds[slot.col];
            let ps = cell_pieces(cx, &th, kind, cells.and_then(|c| c.get(&lay.keys[slot.col])), slot.w, lay.sum_w[slot.col], &mut tick);
            if full && kind == ColKind::Text && widgets::pieces_width(&ps) > slot.w as usize {
                wrap_runs(&ps, slot.w.max(1) as usize)
            } else {
                vec![ps]
            }
        })
        .collect();
    let rows = lines.iter().map(|l| l.len()).max().unwrap_or(1).clamp(1, 40) as u16;
    let mut g = new_grid(w, rows, g0);
    let a = g.area();
    let ground = if lay.zebra && idx % 2 == 1 { th.zebra_ground } else { g0 };
    if ground != g0 {
        g.tint(Rect::new(lay.ind, 0, widgets::table_width(&lay.slots), rows), ground);
    }
    for (slot, cell) in lay.slots.iter().zip(lines) {
        for (i, ps) in cell.into_iter().take(rows as usize).enumerate() {
            let (align, pw) = (lay.cols[slot.col].align, widgets::pieces_width(&ps).min(slot.w as usize) as u16);
            // the cell after a strip's last mark: not blank, so the newest run is drawn the
            // size of the others (widgets::SYMBOL_SPACE)
            if lay.kinds[slot.col] == ColKind::Runs && pw > 0 {
                let end = lay.ind + slot.x + match align {
                    Align::Left => pw,
                    Align::Right => slot.w,
                    Align::Center => (slot.w - pw) / 2 + pw,
                };
                widgets::symbol_guard(&mut g, end, i as u16, ground, a);
            }
            widgets::table_cell(&mut g, lay.ind, i as u16, *slot, align, ps, ground, a);
        }
    }
    let mut buttons = Vec::new();
    if cx.mouse && b.opt("target").is_some() {
        buttons.extend((0..rows).map(|r| (lay.ind, r, w.saturating_sub(lay.ind + 1), short("open", &b.id, None))));
    }
    Drawn { grid: Some(g), tick, buttons, ..Default::default() }
}

/// A run strip on its own line: the label at the left, the strip against the right edge (so a
/// stack of strips lines up on the newest run), and, when asked, the failure count after it: dim,
/// red when the newest run failed.
fn draw_runs(cx: &Ctx, b: &Block, w: u16, ind: u16) -> Drawn {
    /// The summary's slot: "20/20 failed". Fixed, so strips above and below end in one column.
    const SUMMARY_W: u16 = 12;
    let p = cx.p;
    let g0 = p.bg0;
    let th = theme::current();
    let mut g = new_grid(w, 1, g0);
    let a = g.area();
    let vals = outcomes(b.f.get("values"));
    let edge = w.saturating_sub(1);
    let end = if b.flag("summary") { edge.saturating_sub(SUMMARY_W + 2) } else { edge };
    // the label keeps up to half the line; the strip takes the rest, newest runs first
    let label = truncate(b.s("label"), (end.saturating_sub(ind) / 2) as usize);
    let lx = put(&mut g, ind, 0, &label, Style::fg(p.fg).on(g0));
    let room = end.saturating_sub(lx + if label.is_empty() { 0 } else { 2 });
    let ps = widgets::run_pieces(&th, &vals, room);
    widgets::put_pieces(&mut g, end.saturating_sub(widgets::pieces_width(&ps) as u16), 0, &ps, g0, a);
    if !ps.is_empty() {
        // the cell after the newest run: not blank (widgets::SYMBOL_SPACE)
        widgets::symbol_guard(&mut g, end, 0, g0, a);
    }
    if let Some((text, newest_failed)) = widgets::run_summary(&vals).filter(|_| b.flag("summary")) {
        right(&mut g, 0, &text, Style::fg(if newest_failed { p.red } else { p.grey1 }).on(g0), w);
    }
    Drawn { grid: Some(g), ..Default::default() }
}

/// A feed's health line (written by the daemon, feed.rs `Worker::write_health`): what it is,
/// whether it's live, and when it last wrote; a restart counts down; an error shows under it.
fn draw_feed(cx: &Ctx, b: &Block, w: u16, ind: u16) -> Drawn {
    let p = cx.p;
    let g0 = p.bg0;
    let state = b.s("state");
    let note = b.opt("note");
    let mut g = new_grid(w, 1 + note.is_some() as u16, g0);
    let mut tick = None;
    let until = b.opt("until").and_then(parse_rfc3339).map(|u| u - cx.wall);
    let (gl, col, label) = match state {
        "live" | "running" => (icon::FEED.to_string(), p.green, "live".to_string()),
        "restarting" => {
            tick = Some(0.1);
            let left = until.map(|s| s.max(0.0).ceil() as i64).unwrap_or(0);
            (parts::spinner(cx.now, cx.m).to_string(), p.orange, if left > 0 { format!("crashed · restart in {left}s") } else { "restarting".into() })
        }
        "failed" => (icon::BLOCKED.into(), p.red, "failed".into()),
        "paused" => (icon::PAUSE.into(), p.grey1, "paused".into()),
        "removed" => (icon::STOP.into(), p.grey0, "removed".into()),
        s => (icon::STOP.into(), p.grey0, if s.is_empty() { "starting".into() } else { s.to_string() }),
    };
    let restarts = b.num("restarts").unwrap_or(0.0) as i64;
    let right_s = match b.opt("last").and_then(parse_rfc3339) {
        Some(t) => {
            let s = cx.wall - t;
            tick = Some(tick.unwrap_or(age_tick(s)).min(age_tick(s)));
            format!("wrote {}", age_short(s))
        }
        None => "no data yet".into(),
    };
    let rx = w.saturating_sub(width(&right_s) as u16 + 1);
    put(&mut g, ind, 0, &gl, Style::fg(col).on(g0).bold());
    let mut x = put(&mut g, ind + TX, 0, "feed", Style::fg(p.grey1).on(g0));
    x = put(&mut g, x + 1, 0, &truncate(b.s("name"), 16), Style::fg(p.grey2).on(g0).bold());
    x = put(&mut g, x + 1, 0, "·", Style::fg(p.grey0).on(g0));
    let lroom = rx.saturating_sub(x + 3) as usize;
    x = put(&mut g, x + 1, 0, &truncate(&label, lroom.max(1)), Style::fg(col).on(g0).bold_if(state != "live"));
    let trig = b.s("trigger");
    let mut extra = trig.to_string();
    if restarts > 0 {
        extra = format!("{extra} · {restarts} restart{}", if restarts == 1 { "" } else { "s" });
    }
    let room = rx.saturating_sub(x + 4) as usize;
    if room > 6 && !extra.is_empty() {
        put(&mut g, x + 1, 0, "·", Style::fg(p.grey0).on(g0));
        put(&mut g, x + 3, 0, &truncate(&extra, room), Style::fg(p.grey0).on(g0));
    }
    put(&mut g, rx, 0, &right_s, Style::fg(p.grey1).on(g0));
    if let Some(n) = note {
        put(&mut g, ind + TX, 1, &truncate(n, w.saturating_sub(ind + TX + 1) as usize), Style::fg(p.red).on(g0).italic());
    }
    Drawn { grid: Some(g), tick, ..Default::default() }
}

fn draw_image(cx: &Ctx, b: &Block, w: u16, ind: u16) -> Drawn {
    let p = cx.p;
    let g0 = p.bg0;
    let img = cx.images.get(&b.id).and_then(|i| i.as_ref());
    let alt = if b.s("alt").is_empty() { b.s("path") } else { b.s("alt") };
    let Some(img) = img else {
        let mut g = new_grid(w, 1, g0);
        put(&mut g, ind, 0, icon::WARN, Style::fg(p.red).on(g0));
        put(&mut g, ind + TX, 0, &truncate(&format!("image missing: {}", b.s("path")), w.saturating_sub(ind + TX + 1) as usize), Style::fg(p.red).on(g0));
        return Drawn { grid: Some(g), ..Default::default() };
    };
    let maxc = w.saturating_sub(ind + 1);
    let cols = b.num("cols").map(|c| (c as u16).min(maxc)).unwrap_or(maxc);
    let mut rows = img.rows_for(cols, cx.cell.0, cx.cell.1);
    let maxr = b.num("rows").map(|r| r as u16).unwrap_or(18);
    let cols = if rows > maxr {
        rows = maxr;
        // keep the aspect: narrow the slot to the rows we allow
        ((maxr as f32 * cx.cell.1 as f32 * img.w as f32 / img.h.max(1) as f32) / cx.cell.0 as f32).round().max(1.0) as u16
    } else {
        cols
    };
    let mut g = new_grid(w, rows + 1, g0);
    put(&mut g, ind, 0, icon::IMAGE, Style::fg(p.blue).on(g0));
    let mut x = put(&mut g, ind + TX, 0, &truncate(alt, w.saturating_sub(ind + 20) as usize), Style::fg(p.grey2).on(g0).italic());
    let mut images = vec![];
    if cx.kitty {
        images.push((b.id.clone(), ind, 1, cols, rows));
    } else {
        vellum::image::halfblocks(&mut g, img, ind, 1, cols, rows, g0);
        let a = g.area();
        x = parts::tag(&mut g, x + 2, 0, "image: blocks", p.grey1, g0, a);
    }
    let _ = x;
    Drawn { grid: Some(g), images, ..Default::default() }
}

pub trait ItalicIf {
    fn italic_if(self, b: bool) -> Self;
}
impl ItalicIf for Style {
    fn italic_if(self, b: bool) -> Style {
        if b {
            self.italic()
        } else {
            self
        }
    }
}

#[cfg(test)]
mod cycle_tests {
    use super::{default_folded, todo_cycle};
    use crate::doc::Block;
    use crate::store::State;

    #[test]
    fn finished_sections_fold_themselves() {
        let sec = |kids: serde_json::Value| -> Block { serde_json::from_value(serde_json::json!({"type": "section", "id": "s", "children": kids})).unwrap() };
        let st = State::default();
        let done = sec(serde_json::json!([{"type": "todo", "id": "a", "status": "done"}, {"type": "todo", "id": "b", "status": "skipped"}]));
        assert!(default_folded(&st, &done));
        let live = sec(serde_json::json!([{"type": "todo", "id": "a", "status": "done"}, {"type": "todo", "id": "b"}]));
        assert!(!default_folded(&st, &live));
        let asking = sec(serde_json::json!([{"type": "todo", "id": "a", "status": "done"}, {"type": "question", "id": "q", "text": "?", "options": ["x"]}]));
        assert!(!default_folded(&st, &asking), "an open question keeps it open");
        assert!(!default_folded(&st, &sec(serde_json::json!([{"type": "text", "id": "t", "text": "notes"}]))), "no todos: not a thread");
    }

    #[test]
    fn circle_cycles_next_skip_clear_and_never_done() {
        assert_eq!(todo_cycle("todo", false), Some("next"));
        assert_eq!(todo_cycle("next", true), Some("skipped"));
        assert_eq!(todo_cycle("skipped", true), Some("clear"));
        // Claude's own next or skip still cycles from where it stands
        assert_eq!(todo_cycle("next", false), Some("skipped"));
        assert_eq!(todo_cycle("skipped", false), Some("next"));
        // running and finished steps are Claude's: no circle action
        assert_eq!(todo_cycle("doing", false), None);
        assert_eq!(todo_cycle("done", false), None);
        // an old mark of his (done, say) clears
        assert_eq!(todo_cycle("done", true), Some("clear"));
        for st in ["todo", "next", "skipped", "doing", "done", "blocked"] {
            for mine in [false, true] {
                assert_ne!(todo_cycle(st, mine), Some("done"));
            }
        }
    }
}

#[cfg(test)]
mod dash_tests {
    use super::*;
    use serde_json::json;
    use vellum::color::EVERFOREST;

    /// Draw every block of `doc` the way the scene stacks them (each at its depth, top to
    /// bottom), with the clock at `wall`. Returns the text and each block's `Drawn` by id.
    fn render_at(doc: &Doc, w: u16, mouse: bool, wall: f64) -> (String, HashMap<String, Drawn>) {
        let state = State::default();
        let (flash, progress, images, pulse) = (HashMap::new(), HashMap::new(), HashMap::new(), HashMap::new());
        let cx = Ctx {
            doc, state: &state, p: &EVERFOREST, now: 0.0, wall, m: Motion { on: false, slow: 1.0 }, delivered: 0, flash: &flash,
            progress: &progress, kitty: false, cell: (10, 20), images: &images, focused: None, pulse: &pulse, pulse_period: 2.4,
            pulse_breaths: 0.0, mouse, hover: None, flog: false,
        };
        let (mut text, mut drawn, mut first) = (String::new(), HashMap::new(), true);
        crate::doc::walk(&doc.blocks, &mut |b, _, depth| {
            let d = draw(&cx, b, w, depth, first);
            first = false;
            for l in d.grid.as_ref().unwrap().to_text().lines() {
                text.push_str(l.trim_end());
                text.push('\n');
            }
            drawn.insert(b.id.clone(), d);
        });
        (text, drawn)
    }

    fn render(doc: &Doc, w: u16) -> String {
        render_at(doc, w, false, 0.0).0
    }

    fn doc(rev: u64, blocks: serde_json::Value) -> Doc {
        let d: Doc = serde_json::from_value(json!({"id": "dash", "rev": rev, "blocks": blocks})).unwrap();
        d.blocks.iter().for_each(|b| crate::doc::check_block(b).unwrap());
        d
    }

    fn reviews(rev: u64, extra: serde_json::Value) -> Doc {
        let mut table = json!({"type": "table", "id": "prs", "columns": [
            {"key": "pr", "title": "PR", "kind": "id"},
            {"key": "by", "title": "Author", "kind": "who", "optional": true},
            {"key": "title", "title": "Title"},
            {"key": "ci", "title": "CI", "kind": "runs", "optional": true},
            {"key": "n", "title": "+/-", "kind": "num"}],
            "children": [
            {"type": "tr", "id": "pr1", "cells": {"pr": "#412", "by": "ada", "title": "Cache the **layout** per table", "ci": ["ok", "ok", "fail", "ok"], "n": 120}, "target": "https://example.com/412"},
            {"type": "tr", "id": "pr2", "cells": {"pr": "#7", "by": "grace", "title": "Fix it", "ci": ["ok", "running"], "n": 8}},
            {"type": "tr", "id": "pr3", "cells": {"pr": "#1024", "by": "ada", "title": "A much longer title that will need cutting somewhere", "n": 1500}}]});
        for (k, v) in extra.as_object().unwrap() {
            table[k] = v.clone();
        }
        doc(rev, json!([table]))
    }

    #[test]
    fn a_table_has_a_header_and_columns_at_their_natural_widths() {
        let d = reviews(1, json!({}));
        let text = render(&d, 100);
        let th = theme::current();
        let (ada, grace) = (th.badge("ada").glyph, th.badge("grace").glyph);
        let want = format!(
            " PR    Author  Title                                                  CI  +/-\n ────────────────────────────────────────────────────────────────────────────\n #412  {ada} ada   Cache the layout per table                           ▪▪✗▪  120\n #7    {grace} grace Fix it                                                 ▪▪    8\n #1024 {ada} ada   A much longer title that will need cutting somewhere      1500\n"
        );
        assert_eq!(text.replace('\u{a0}', " "), want, "fixed columns at the widest of title and cells; the strip and the number, and their titles, read from the right");
        // a badge's symbol and a strip's last mark are followed by a no-break space, so the
        // terminal draws every one of them the same size
        assert!(text.contains(&format!("{ada}\u{a0}ada")) && text.contains("▪▪✗▪\u{a0} 120") && text.contains("▪▪\u{a0}   8"), "{text:?}");
        // the header is dim and bold; a name keeps one color wherever it appears; an id has its own
        let (_, dr) = render_at(&d, 100, false, 0.0);
        let head = dr["prs"].grid.as_ref().unwrap().get(1, 0).unwrap().style;
        assert_eq!((head.fg, head.bold), (Some(EVERFOREST.grey1), true));
        let fg = |id: &str, x: u16| dr[id].grid.as_ref().unwrap().get(x, 0).unwrap().style.fg;
        assert_eq!(fg("pr1", 7), Some(th.badge("ada").color));
        assert_eq!(fg("pr1", 7), fg("pr3", 9), "ada is ada in every row");
        assert_eq!(fg("pr1", 1), Some(th.id_color("#412")));
        assert!(dr["pr1"].grid.as_ref().unwrap().get(25, 0).unwrap().style.bold, "markup in a text cell: **layout**");
    }

    #[test]
    fn a_narrow_table_drops_optional_columns_then_cuts_text() {
        let d = reviews(1, json!({}));
        // a grid 46 wide holds every column with the title at its 20 cells; one fewer and CI goes
        let head = |w| render(&d, w).lines().next().unwrap().to_string();
        assert!(head(46).contains("Author") && head(46).contains("CI"), "{}", render(&d, 46));
        assert!(render(&d, 46).contains("A much longer title… "), "text is cut with an ellipsis, never wrapped:\n{}", render(&d, 46));
        let text = render(&d, 45);
        assert!(head(45).contains("Author") && !head(45).contains("CI"), "the rightmost optional column goes first:\n{text}");
        assert!(text.contains("A much longer title tha… 1500"), "and the title takes the cells it freed:\n{text}");
        let text = render(&d, 40);
        assert!(!head(40).contains("Author") && !head(40).contains("CI") && head(40).contains("PR") && head(40).contains("+/-"), "{text}");
        // nothing optional is left at 24: now the text gives, and the fixed columns hold
        let text = render(&d, 24);
        assert!(text.contains("#1024 A much lon… 1500"), "{text}");
        assert_eq!(text.lines().count(), 5, "one line per row at any width");
        for w in 4..40 {
            assert_eq!(render(&d, w).lines().count(), 5, "width {w}");
        }
    }

    #[test]
    fn a_table_is_laid_out_once_however_many_rows_it_has() {
        let rows: Vec<serde_json::Value> = (0..200).map(|i| json!({"type": "tr", "id": format!("r{i}"), "cells": {"a": format!("row {i}"), "b": i}})).collect();
        let d = doc(1, json!([{"type": "table", "id": "big", "columns": [{"key": "a", "title": "A"}, {"key": "b", "title": "B", "kind": "num"}], "children": rows}]));
        let before = LAYOUTS.with(|n| n.get());
        let text = render(&d, 60);
        assert_eq!(LAYOUTS.with(|n| n.get()) - before, 1, "201 blocks drawn, one layout");
        assert_eq!(text.lines().count(), 202);
        render(&d, 60);
        assert_eq!(LAYOUTS.with(|n| n.get()) - before, 1, "and the next frame of the same revision reuses it");
        render(&d, 50);
        assert_eq!(LAYOUTS.with(|n| n.get()) - before, 2, "a new width lays out again");
        // a row removed in the next revision still draws in its old columns (it is a ghost
        // while it animates out); one that never had a table draws its cells plainly
        let gone = d.blocks[0].children[199].clone();
        let mut d2 = d.clone();
        d2.rev = 2;
        d2.blocks[0].children.truncate(199);
        let (_, dr) = render_at(&Doc { blocks: vec![d2.blocks[0].clone(), gone], ..d2.clone() }, 50, false, 0.0);
        assert!(dr["r199"].grid.as_ref().unwrap().to_text().trim_end().ends_with("199"));
        let orphan: Block = serde_json::from_value(json!({"type": "tr", "id": "lost", "cells": {"a": "alone"}})).unwrap();
        let (text, _) = render_at(&Doc { blocks: vec![orphan], ..d2 }, 50, false, 0.0);
        assert_eq!(text.trim(), "alone");
    }

    #[test]
    fn zebra_is_off_by_default_and_on_when_the_table_or_the_config_says() {
        let stripes = |d: &Doc| -> Vec<bool> {
            let (_, dr) = render_at(d, 60, false, 0.0);
            ["pr1", "pr2", "pr3"].iter().map(|id| dr[*id].grid.as_ref().unwrap().get(8, 0).unwrap().style.bg == Some(EVERFOREST.bg1)).collect()
        };
        assert_eq!(stripes(&reviews(1, json!({}))), [false, false, false], "off unless asked");
        assert_eq!(stripes(&reviews(2, json!({"zebra": true}))), [false, true, false], "every second row takes the second ground");
        let (_, dr) = render_at(&reviews(2, json!({"zebra": true})), 60, false, 0.0);
        let g = dr["pr2"].grid.as_ref().unwrap();
        assert_eq!(g.get(0, 0).unwrap().style.bg, Some(EVERFOREST.bg0), "the gutter (focus caret, change bar) stays clear");
        // the config default, and a table that turns it off again
        let mut th = Theme::new(&EVERFOREST);
        th.zebra = true;
        theme::set(th);
        assert_eq!(stripes(&reviews(3, json!({}))), [false, true, false]);
        assert_eq!(stripes(&reviews(4, json!({"zebra": false}))), [false, false, false]);
        theme::set(Theme::new(&EVERFOREST));
    }

    #[test]
    fn rows_open_their_target_and_live_cells_ask_for_frames() {
        let d = doc(1, json!([{"type": "table", "id": "jobs", "columns": [
            {"key": "st", "kind": "state"}, {"key": "job", "title": "Job"}, {"key": "at", "title": "Age", "kind": "age"}],
            "empty": "nothing running", "children": [
            {"type": "tr", "id": "j1", "cells": {"st": "ok", "job": "backup", "at": "1970-01-01T00:00:00Z"}, "target": "https://example.com/j1", "detail": "kept, not drawn"},
            {"type": "tr", "id": "j2", "cells": {"st": "spin", "job": "sync by @[ada]", "at": 995}},
            {"type": "tr", "id": "j3", "cells": {"job": "idle"}}]}]));
        let (text, dr) = render_at(&d, 40, true, 1000.0);
        let rows: Vec<&str> = text.lines().collect();
        assert!(rows[0].ends_with("Job            Age"), "{text}");
        assert!(rows[2].ends_with("backup         16m") && rows[3].ends_with("ada   5s"), "ages count from the clock, right-aligned:\n{text}");
        assert!(!text.contains("kept, not drawn") && !text.contains("nothing running"));
        assert_eq!(dr["j1"].buttons.len(), 1);
        assert_eq!(dr["j1"].buttons[0].3, "mg:open/j1", "the row is the button");
        assert!(dr["j2"].buttons.is_empty(), "no target, nothing to open");
        assert_eq!((dr["j1"].tick, dr["j2"].tick, dr["j3"].tick), (Some(30.0), Some(0.1), None), "an old age ticks slowly, a spinner fast, a still row not at all");
        assert!(rows[3].contains(&format!("sync by {}\u{a0}ada", theme::current().badge("ada").glyph)), "a mention in a text cell: {text}");
        assert!(render_at(&d, 40, false, 1000.0).1["j1"].buttons.is_empty(), "without the mouse a row has no button");
        let none = doc(2, json!([{"type": "table", "id": "jobs", "columns": [{"key": "job", "title": "Job"}], "empty": "nothing running"}]));
        assert_eq!(render(&none, 40), " Job\n ───\n nothing running\n");
    }

    #[test]
    fn run_strips_draw_in_a_column_and_alone() {
        let hist: Vec<&str> = (0..30).map(|i| if i == 3 || i == 29 { "fail" } else { "ok" }).collect();
        let d = doc(1, json!([
            {"type": "runs", "id": "n1", "label": "nightly build", "values": ["ok", "fail", "skip", null, "running"], "summary": true},
            {"type": "runs", "id": "n2", "label": "backup", "values": hist, "summary": true},
            {"type": "runs", "id": "n3", "label": "quiet", "values": ["ok", "ok"]},
            {"type": "table", "id": "t", "columns": [{"key": "job", "title": "Job"}, {"key": "ci", "title": "Last runs", "kind": "runs"}, {"key": "w", "title": "W", "kind": "runs", "width": 3}], "children": [
                {"type": "tr", "id": "a", "cells": {"job": "long history", "ci": hist, "w": hist}},
                {"type": "tr", "id": "b", "cells": {"job": "short", "ci": ["fail", "ok"], "w": ["ok"]}}]}]));
        let (text, dr) = render_at(&d, 60, false, 0.0);
        assert!(text.contains("▪✗▪·▪\u{a0}   1/4 failed"), "the cell after a strip is a no-break space: {text:?}");
        let text = text.replace('\u{a0}', " ");
        let rows: Vec<&str> = text.lines().collect();
        assert!(rows[0].starts_with(" nightly build") && rows[0].ends_with("▪✗▪·▪    1/4 failed"), "a cross where it failed, the count after:\n{text}");
        assert!(rows[1].starts_with(" backup") && rows[1].ends_with("▪▪✗   2/30 failed"), "the newest that fit, the oldest dropped:\n{text}");
        assert_eq!(rows[0].chars().count(), rows[1].chars().count(), "the count's slot is fixed, so strips end in one column");
        assert_eq!(rows[1].matches(['▪', '✗']).count(), 30, "all thirty fit, a cell each: {}", rows[1]);
        assert!(rows[2].ends_with("▪▪") && !rows[2].contains("failed"), "no summary unless asked");
        let p = &EVERFOREST;
        let last = |id: &str| {
            let g = dr[id].grid.as_ref().unwrap();
            (0..g.area().w).rev().filter_map(|x| g.get(x, 0)).find(|c| c.style.fg.is_some() && c.style.fg != Some(p.fg)).map(|c| c.style.fg.unwrap())
        };
        assert_eq!(last("n1"), Some(p.grey1), "the count is dim");
        assert_eq!(last("n2"), Some(p.red), "and red when the newest run failed");
        // in a table: the column caps at the newest 20 runs, a width caps it further, and short
        // histories sit at the right so the newest run of every row shares a column
        let strip = format!("{}✗", "▪".repeat(19));
        assert!(rows[5].contains(&strip) && !rows[5].contains(&format!("▪{strip}")), "the newest 20 of 30:\n{text}");
        assert!(rows[5].ends_with("▪▪✗"), "three cells hold the three newest: {}", rows[5]);
        assert!(rows[6].ends_with("✗▪     ▪"), "two strips stand three cells apart: {}", rows[6]);
        assert_eq!(rows[5].chars().count(), rows[6].chars().count());
    }

    /// `summary: true` on a runs column: the failure count after each strip, in one slot (so the
    /// strips still end in one column), dim, red when the newest run failed, inside the column.
    #[test]
    fn a_runs_column_can_end_in_its_failure_count() {
        let fails: Vec<&str> = std::iter::repeat("ok").take(11).chain(["fail"]).collect();
        let table = |summary: bool| doc(1, json!([
            {"type": "table", "id": "t", "columns": [{"key": "job", "title": "Job"}, {"key": "ci", "title": "Runs", "kind": "runs", "summary": summary}], "children": [
                {"type": "tr", "id": "a", "cells": {"job": "deploy", "ci": ["ok", "fail", "ok", null, "ok", "ok", "ok"]}},
                {"type": "tr", "id": "b", "cells": {"job": "backup", "ci": fails}},
                {"type": "tr", "id": "c", "cells": {"job": "idle", "ci": []}}]}]));
        let (text, drawn) = render_at(&table(true), 60, false, 0.0);
        let text = text.replace('\u{a0}', " ");
        let rows: Vec<&str> = text.lines().collect();
        assert!(rows[2].ends_with("▪✗▪·▪▪▪  1/6"), "empty slots don't count: {text}");
        assert!(rows[3].ends_with(&format!("{}✗ 1/12", "▪".repeat(11))), "{text}");
        assert_eq!(rows[2].chars().count(), rows[3].chars().count(), "the counts end in one column, and so do the strips");
        assert_eq!(rows[4].trim(), "idle", "no runs, no count: {text}");
        let p = &EVERFOREST;
        let last = |id: &str| {
            let g = drawn[id].grid.as_ref().unwrap();
            let line = g.to_text().lines().next().unwrap().to_string();
            g.get(line.chars().count() as u16 - 1, 0).unwrap().style.fg
        };
        assert_eq!((last("a"), last("b")), (Some(p.grey1), Some(p.red)), "dim; red when the newest run failed");
        // without it the column is exactly the strips
        let (plain, _) = render_at(&table(false), 60, false, 0.0);
        let plain = plain.replace('\u{a0}', " ");
        let prows: Vec<&str> = plain.lines().collect();
        assert!(prows[3].trim_end().ends_with(&format!("{}✗", "▪".repeat(11))), "{plain}");
        assert_eq!(rows[3].chars().count(), prows[3].trim_end().chars().count() + " 1/12".len(), "the count is part of the column's width");
        // it belongs to runs columns only, and is true or false
        let bad = |col: serde_json::Value| {
            let b: Block = serde_json::from_value(json!({"type": "table", "id": "t", "columns": [col]})).unwrap();
            crate::doc::check_block(&b).unwrap_err()
        };
        assert!(bad(json!({"key": "a", "summary": true})).contains("only a runs column"), "a text column");
        assert!(bad(json!({"key": "a", "kind": "runs", "summary": "yes"})).contains("true or false"));
    }

    /// Plain-text frames carry no private-use codepoint: every icon has a one-cell stand-in, the
    /// state glyphs a reader looks for are a check and a cross, and the grid itself is untouched.
    #[test]
    fn plain_text_swaps_nerd_font_glyphs_for_ordinary_characters() {
        let private = |c: char| matches!(c as u32, 0xE000..=0xF8FF | 0xF0000..=0x10FFFD);
        for (glyph, plain) in icon::PLAIN.iter().chain(PLAIN_SHAPES) {
            assert!(glyph.chars().count() == 1 && glyph.chars().all(private), "{glyph:?} is not a Nerd Font glyph");
            assert!(plain.chars().count() == 1 && !plain.chars().any(private) && width(plain) == 1, "{glyph:?} → {plain:?}: one ordinary cell");
            assert_eq!(plain_text(glyph), *plain);
        }
        assert_eq!((plain_text(icon::DONE), plain_text(icon::BLOCKED), plain_text(icon::TODO), plain_text(icon::RIGHT)), ("✓".into(), "✗".into(), "○".into(), "▸".into()));
        assert_eq!(plain_text("a \u{e123} b ▪✗ ● é"), "a · b ▪✗ ● é", "an unknown private glyph is a dot; ordinary text passes");
        let d = doc(1, json!([
            {"type": "section", "id": "s", "title": "Checks", "children": [
                {"type": "todo", "id": "t1", "text": "step", "status": "done"},
                {"type": "row", "id": "r1", "glyph": "fail", "text": "ingest"},
                {"type": "row", "id": "r2", "glyph": "you", "text": "waits"},
                {"type": "status", "id": "s1", "text": "deploy", "state": "pass"},
                {"type": "claim", "id": "c1", "text": "assumed"},
                {"type": "link", "id": "l1", "text": "runbook", "target": "https://example.com"},
                {"type": "table", "id": "t", "columns": [{"key": "st", "kind": "state"}, {"key": "n"}], "children": [
                    {"type": "tr", "id": "x", "cells": {"st": "done", "n": "backup"}},
                    {"type": "tr", "id": "y", "cells": {"st": "fail", "n": "mailer"}}]}]}]));
        let (raw, _) = render_at(&d, 60, false, 0.0);
        assert!(raw.chars().any(private), "the pane's own frame keeps its icons");
        let text = plain_text(&raw);
        assert!(!text.chars().any(private), "{text}");
        assert_eq!(text.lines().map(|l| l.chars().count()).collect::<Vec<_>>(), raw.lines().map(|l| l.chars().count()).collect::<Vec<_>>(), "one cell for one cell");
        let line = |word: &str| text.lines().find(|l| l.contains(word)).unwrap_or_else(|| panic!("{word}: {text}")).to_string();
        assert!(line("backup").contains('✓') && line("mailer").contains('✗'), "a state column reads: {text}");
        assert!(line("ingest").contains('✗') && line("step").contains('✓') && line("waits").contains('!'), "{text}");
    }

    #[test]
    fn bold_takes_the_emphasis_color_unless_the_line_has_its_own() {
        let d = doc(1, json!([
            {"type": "bullet", "id": "b", "text": "**Goal** ship it"},
            {"type": "bullet", "id": "bt", "tone": "red", "text": "**Broken** build"},
            {"type": "text", "id": "w", "style": "warn", "text": "**Careful** here"},
            {"type": "text", "id": "n", "style": "dim", "text": "**Note** aside"},
            {"type": "todo", "id": "t1", "text": "**Open** step"},
            {"type": "todo", "id": "t2", "status": "done", "text": "**Done** step"},
            {"type": "todo", "id": "t3", "status": "skipped", "text": "**Skipped** step"}]));
        let p = &EVERFOREST;
        // the default is the standard foreground; a configured color is what this test checks
        assert_eq!(theme::current().emphasis, p.fg);
        let mut th = (*theme::current()).clone();
        th.emphasis = p.orange;
        theme::set(th);
        let (_, dr) = render_at(&d, 50, false, 0.0);
        let em = p.orange;
        // the style of the first cell of `word` on the block's first row
        let at = |id: &str, word: &str| {
            let g = dr[id].grid.as_ref().unwrap();
            let line = g.to_text().lines().next().unwrap().to_string();
            let x = line[..line.find(word).unwrap()].chars().count() as u16;
            g.get(x, 0).unwrap().style
        };
        let look = |id: &str, word: &str| (at(id, word).fg.unwrap(), at(id, word).bold);
        assert_eq!((look("b", "Goal"), look("b", "ship")), ((em, true), (p.fg, false)), "the lead-in is colored, the rest is not");
        assert_eq!(look("bt", "Broken"), (p.red, true), "a tone keeps its color");
        assert_eq!(look("w", "Careful"), (p.yellow, true), "a warning stays the warning color");
        assert_eq!((look("n", "Note"), look("n", "aside")), ((em, true), (p.grey1, false)), "dim text still gets its lead-in");
        assert_eq!(look("t1", "Open"), (em, true));
        assert_eq!(look("t2", "Done"), (p.grey1, true), "a finished step stays dim");
        assert_eq!(at("t3", "Skipped").fg, Some(p.grey1));
    }

    #[test]
    fn a_rows_detail_wraps_under_it_and_two_strips_stand_apart() {
        let long = "first line of the detail\nsecond line, which is long enough that at forty columns it has to wrap onto further rows and then some more words to pass three rows";
        let d = doc(1, json!([
            {"type": "row", "id": "r1", "text": "deploy", "detail": long, "target": "https://example.com"},
            {"type": "table", "id": "t", "columns": [{"key": "a", "kind": "runs"}, {"key": "b", "kind": "runs"}], "children": [
                {"type": "tr", "id": "x", "cells": {"a": ["ok", "ok", "fail"], "b": ["ok", "skip", "ok"]}}]}]));
        let (text, drawn) = render_at(&d, 40, true, 0.0);
        let rows: Vec<&str> = text.lines().collect();
        let at = rows.iter().position(|l| l.contains("deploy")).unwrap();
        assert!(rows[at + 1].contains("first line of the detail") && rows[at + 2].contains("second line, which is long"), "a newline is a line break: {text}");
        assert!(rows[at + 3].trim_end().ends_with('…') && !rows[at + 4].contains("words"), "three rows, then cut: {text}");
        assert!(drawn["r1"].buttons.iter().any(|b| b.3 == "mg:open/r1"), "a row with a target is its button");
        assert!(text.replace('\u{a0}', " ").contains("▪▪✗   ▪▪▪"), "three blanks between two strips, none between marks: {text}");
    }

    #[test]
    fn a_rule_is_a_dim_line_with_its_label_inset() {
        let d = doc(1, json!([
            {"type": "rule", "id": "r0", "text": "Reviews"},
            {"type": "text", "id": "t", "text": "body"},
            {"type": "rule", "id": "r1"},
            {"type": "rule", "id": "r2", "text": "Services"}]));
        let (text, dr) = render_at(&d, 30, false, 0.0);
        let rows: Vec<&str> = text.lines().collect();
        assert_eq!(rows[0], format!(" ── Reviews {}", "─".repeat(17)));
        assert_eq!(rows[2], format!(" {}", "─".repeat(28)), "no label: the whole width");
        assert_eq!((rows[3], rows[4]), ("", format!(" ── Services {}", "─".repeat(16)).as_str()), "a labeled rule after content keeps a blank row above");
        assert_eq!((dr["r0"].lead, dr["r1"].lead, dr["r2"].lead), (0, 0, 1));
        let g = dr["r0"].grid.as_ref().unwrap();
        assert_eq!((g.get(1, 0).unwrap().style.fg, g.get(4, 0).unwrap().style.fg), (Some(EVERFOREST.bg4), Some(EVERFOREST.grey1)));
    }

    #[test]
    fn at_names_become_badges_in_running_text_and_on_rows() {
        let th = theme::current();
        let ada = th.badge("ada");
        assert_eq!(
            markup("ask @[ada] about **it**"),
            vec![("ask ".into(), Span::Plain), (format!("{}\u{a0}ada", ada.glyph), Span::Who(ada.color)), (" about ".into(), Span::Plain), ("it".into(), Span::Bold)]
        );
        assert_eq!(markup("@[Ada]")[0].1, Span::Who(ada.color), "case doesn't change the badge");
        assert_eq!(markup("@[Ada Lovelace]")[0].0, format!("{}\u{a0}Ada\u{a0}Lovelace", th.badge("ada lovelace").glyph), "a full name stays one word");
        // what must stay literal: an address, an email, an empty or unclosed marker, a list
        for s in ["mail root@[::1] now", "ada@example.com", "@[]", "@[ada", "a @ b [c]", "@[ a]", "see @[a\nb]"] {
            assert_eq!(markup(s), vec![(s.to_string(), Span::Plain)], "{s:?}");
        }
        let d = doc(1, json!([
            {"type": "bullet", "id": "b", "text": "review from @[ada] and @[grace] is still pending on this one"},
            {"type": "todo", "id": "t", "text": "ping @[ada]"},
            {"type": "row", "id": "r", "who": "grace", "text": "pushed 3 commits", "meta": "main"}]));
        let (text, _) = render_at(&d, 34, false, 0.0);
        let (wide, dr) = render_at(&d, 60, false, 0.0);
        let grace = th.badge("grace");
        assert!(text.contains(&format!("{}\u{a0}ada", ada.glyph)) && text.contains(&format!("ping {}\u{a0}ada", ada.glyph)), "{text}");
        assert!(text.lines().count() >= 4, "the bullet wraps: {text}");
        assert!(!text.lines().any(|l| l.trim_end().ends_with(ada.glyph.as_str()) || l.trim_end().ends_with(grace.glyph.as_str())), "a glyph never parts from its name:\n{text}");
        let row = wide.lines().last().unwrap();
        assert!(row.contains(&format!("{}\u{a0}grace pushed 3 commits", grace.glyph)) && row.ends_with("main"), "a no-break space after the symbol: {row}");
        let g = dr["r"].grid.as_ref().unwrap();
        assert_eq!(g.get(1 + TX, 0).unwrap().style.fg, Some(grace.color), "glyph and name in the name's color");
        assert_eq!(g.get(1 + TX + 2, 0).unwrap().style.fg, Some(grace.color));
    }
}
