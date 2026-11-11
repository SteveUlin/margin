//! The detail panel: what the focused block has to say beyond its row. It sits inside the pane,
//! beside the list when the pane is at least `SIDE_W` columns wide and under it otherwise, is
//! toggled with `d` (remembered per tab in the view file), follows the focus, and scrolls on its
//! own (`J` / `K`, or the wheel over it).
//!
//! It reads the block through its field map, so any component shows up without the panel knowing
//! its type: the text in full (a row may have cut it), `detail` as wrapped markup with its line
//! breaks kept, then the other fields as dim `key  value` lines (a table row's `cells` one line
//! each), then how to open its `target`. Everything authored wraps; nothing here is cut.

use crate::components::{self, Span};
use crate::doc::Block;
use crate::store::State;
use serde_json::Value;
use vellum::color::{Palette, Rgb};
use vellum::grid::Grid;
use vellum::layout::Rect;
use vellum::style::Style;
use vellum::text::{width, wrap, wrap_runs};

/// The pane width from which the panel sits beside the list rather than under it.
pub const SIDE_W: u16 = 100;
/// The shortest pane that has room for a panel at all.
pub const MIN_H: u16 = 16;

/// How a run of the panel draws.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Run {
    Title,
    Dim,
    Text,
    Bold,
    Code,
    Who(Rgb),
    Key,
    Value,
    /// The key of the hint that opens the target.
    Hint,
}

pub type Line = Vec<(String, Run)>;

/// Where the panel goes in a pane `w` by `h` whose list would span rows `top..bottom`: beside the
/// list (two fifths of the width, a rule's column included) or under it (a third of the rows, a
/// rule's row included). None when the pane is too small for one.
pub fn place(w: u16, h: u16, top: u16, bottom: u16) -> Option<Rect> {
    let rows = bottom.saturating_sub(top);
    if h < MIN_H || rows < 9 {
        return None;
    }
    if w >= SIDE_W {
        let pw = (w * 2 / 5).clamp(36, 72);
        return Some(Rect::new(w - pw, top, pw, rows));
    }
    let ph = (rows * 35 / 100).max(7).min(rows - 4);
    Some(Rect::new(0, bottom - ph, w, ph))
}

/// Fields that say how a block draws, not what it is, and the ones the panel shows elsewhere.
const SKIP: &[&str] = &[
    "actions", "text", "title", "detail", "proposal", "rows", "spark", "tone", "tag_tone", "aside_tone", "text_tone", "bold", "folded", "style", "glyph", "max_rows", "cols", "columns",
];

fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(if *b { "yes" } else { "no" }.to_string()),
        Value::Array(a) if !a.is_empty() => {
            let parts: Vec<String> = a.iter().map(|x| scalar(x).unwrap_or_else(|| "·".into())).collect();
            // a list of words reads with commas; a run strip's marks read as a strip
            Some(parts.join(if a.iter().all(|x| x.is_string()) { ", " } else { " " }))
        }
        _ => None,
    }
}

/// A value that is a time in the past (RFC 3339) leads with how long ago that was.
fn with_age(val: String, wall: f64) -> String {
    match crate::util::parse_rfc3339(&val).map(|t| wall - t) {
        Some(age) if age >= 0.0 => format!("{} · {val}", crate::util::ago(age)),
        _ => val,
    }
}

/// The block's fields as (key, value), in the order it holds them: `cells` opened into one pair a
/// cell, a todo's status and duration from the state, presentation fields left out.
pub fn fields(b: &Block, state: &State, wall: f64) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let named = name(b);
    for (k, v) in &b.f {
        // the target closes the list, beside the hint that opens it
        if k.starts_with('_') || SKIP.contains(&k.as_str()) || k == "target" || (k == "status" && b.kind == "todo") {
            continue;
        }
        if let (true, Value::Object(cells)) = (k == "cells", v) {
            // the cell that names the row is the panel's first line already
            out.extend(cells.iter().filter(|(_, cv)| cv.as_str() != Some(named.as_str())).filter_map(|(ck, cv)| scalar(cv).map(|s| (ck.clone(), with_age(s, wall)))));
        } else if let Some(s) = scalar(v) {
            out.push((k.clone(), with_age(s, wall)));
        }
    }
    if let Some(t) = b.opt("target") {
        out.push(("target".into(), t.to_string()));
    }
    if b.kind == "todo" {
        let st = components::status_of(state, b);
        if let Some(secs) = components::step_secs(state, b, &st, wall) {
            out.insert(0, ("took".into(), components::took_short(secs)));
        }
        out.insert(0, ("status".into(), st));
    }
    if let Some(d) = state.decisions.get(&b.id).filter(|d| matches!(d.kind.as_str(), "pick" | "strike" | "verdict")) {
        let what = scalar(&d.value).unwrap_or_default();
        out.push((d.kind.clone(), format!("{what} · {}", d.by).trim_start_matches(" · ").to_string()));
    }
    out
}

/// What the block is called: its text or title, a table row's `title` cell or first text cell.
fn name(b: &Block) -> String {
    let cell = || {
        let cells = b.f.get("cells")?.as_object()?;
        cells.get("title").and_then(|v| v.as_str()).or_else(|| cells.values().find_map(|v| v.as_str())).map(String::from)
    };
    [b.s("text"), b.s("title")].into_iter().find(|s| !s.trim().is_empty()).map(String::from).or_else(cell).unwrap_or_else(|| b.id.clone())
}

fn run_of(s: Span) -> Run {
    match s {
        Span::Plain => Run::Text,
        Span::Bold => Run::Bold,
        Span::Code => Run::Code,
        Span::Who(c) => Run::Who(c),
    }
}

/// The panel's lines for a block, wrapped to `w` columns. Returns them with the index of the
/// line that opens the target, if the block has one.
pub fn lines(b: &Block, state: &State, wall: f64, w: usize) -> (Vec<Line>, Option<usize>) {
    let w = w.max(8);
    let mut out: Vec<Line> = Vec::new();
    let title: Vec<(String, Run)> = components::markup(&name(b)).into_iter().map(|(t, _)| (t, Run::Title)).collect();
    out.extend(wrap_runs(&title, w));
    out.push(vec![(format!("{} · {}", b.kind, b.id), Run::Dim)]);
    let detail = b.s("detail").trim_end();
    let fs = fields(b, state, wall);
    if detail.trim().is_empty() && fs.is_empty() {
        out.push(Vec::new());
        out.push(vec![("nothing more on this row".into(), Run::Dim)]);
        return (out, None);
    }
    if !detail.trim().is_empty() {
        out.push(Vec::new());
        // each line of the detail wraps on its own, so an empty line stays a paragraph break
        for para in detail.split('\n') {
            if para.trim().is_empty() {
                out.push(Vec::new());
                continue;
            }
            let runs: Vec<(String, Run)> = components::markup(para).into_iter().map(|(t, s)| (t, run_of(s))).collect();
            out.extend(wrap_runs(&runs, w));
        }
    }
    let mut open = None;
    if !fs.is_empty() {
        out.push(Vec::new());
        let kw = fs.iter().map(|(k, _)| width(k)).max().unwrap_or(0).min(14).min(w / 3);
        for (k, v) in &fs {
            let key = format!("{:<kw$}  ", vellum::text::truncate(k, kw));
            for (i, l) in wrap(v, w.saturating_sub(kw + 2).max(4)).into_iter().enumerate() {
                let lead = if i == 0 { key.clone() } else { " ".repeat(kw + 2) };
                out.push(vec![(lead, Run::Key), (l.trim_start().to_string(), Run::Value)]);
            }
        }
    }
    if b.opt("target").is_some() {
        out.push(Vec::new());
        open = Some(out.len());
        out.push(vec![("⏎".into(), Run::Hint), (" open the target".into(), Run::Dim)]);
    }
    (out, open)
}

/// The lines that list a block's actions in the panel: `r  retry the job`, one each. A click on
/// one fires it (the scene makes each line a button).
pub fn action_lines(actions: &[crate::doc::Action]) -> Vec<Line> {
    actions.iter().map(|a| vec![(a.key.to_string(), Run::Hint), (format!(" {}", a.label), Run::Dim)]).collect()
}

/// Draw the panel into `r` (its rule included) from line `scroll`. `side`: beside the list, a
/// vertical rule down its left edge; else under it, a rule along its top. `buttons`: the lines
/// that are clickable, each with its token; those in view come back as (x, y, width, token).
#[allow(clippy::too_many_arguments)]
pub fn draw(g: &mut Grid, r: Rect, side: bool, lines: &[Line], scroll: usize, buttons: &[(usize, String)], p: &Palette) -> Vec<(u16, u16, u16, String)> {
    g.fill(r, Style::default().on(p.bg0));
    let body = body(r, side);
    if side {
        for y in r.y..r.bottom() {
            g.put(r.x, y, "│", Style::fg(p.bg2).on(p.bg0));
        }
    } else {
        for x in 1..r.w.saturating_sub(1) {
            g.put(r.x + x, r.y, "─", Style::fg(p.bg2).on(p.bg0));
        }
    }
    let mut hits = Vec::new();
    for (i, line) in lines.iter().enumerate().skip(scroll).take(body.h as usize) {
        let y = body.y + (i - scroll) as u16;
        let clip = Rect::new(body.x, y, body.w, 1);
        let mut x = body.x;
        for (s, k) in line {
            let st = match k {
                Run::Title => Style::fg(p.fg).on(p.bg0).bold(),
                Run::Dim => Style::fg(p.grey1).on(p.bg0),
                Run::Text => Style::fg(p.grey2).on(p.bg0),
                Run::Bold => Style::fg(p.fg).on(p.bg0).bold(),
                Run::Code => Style::fg(p.aqua).on(p.bg1),
                Run::Who(c) => Style::fg(*c).on(p.bg0).bold(),
                Run::Key => Style::fg(p.grey0).on(p.bg0),
                Run::Value => Style::fg(p.grey2).on(p.bg0),
                Run::Hint => Style::fg(p.grey2).on(p.bg0).bold(),
            };
            x = g.put_clipped(x, y, s, st, clip);
        }
        if let Some((_, tok)) = buttons.iter().find(|(l, _)| *l == i) {
            hits.push((body.x, y, x.saturating_sub(body.x).max(1), tok.clone()));
        }
    }
    // more than fits: a track and thumb in its last column, as the list has
    let (total, vh) = (lines.len(), body.h as usize);
    if total > vh && vh > 1 {
        let thumb = ((vh * vh) as f32 / total as f32).round().clamp(1.0, vh as f32) as usize;
        let ty = ((vh - thumb) as f32 * scroll as f32 / (total - vh) as f32).round() as usize;
        let sx = r.right().saturating_sub(1);
        for i in 0..vh {
            let on = i >= ty && i < ty + thumb;
            g.put(sx, body.y + i as u16, if on { "┃" } else { "│" }, Style::fg(if on { p.grey0 } else { p.bg2 }).on(p.bg0));
        }
    }
    hits
}

/// The part of the panel text goes in: inside the rule, a column clear of it and of the edge.
pub fn body(r: Rect, side: bool) -> Rect {
    if side {
        Rect::new(r.x + 2, r.y, r.w.saturating_sub(4), r.h)
    } else {
        Rect::new(r.x + 2, r.y + 1, r.w.saturating_sub(4), r.h.saturating_sub(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn block(v: Value) -> Block {
        serde_json::from_value(v).unwrap()
    }

    fn text(ls: &[Line]) -> Vec<String> {
        ls.iter().map(|l| l.iter().map(|(s, _)| s.as_str()).collect::<String>().trim_end().to_string()).collect()
    }

    /// A table row read through its field map: the title cell names it, the detail wraps with
    /// its line breaks kept, each cell is a `key  value` line, and the target gets its hint.
    #[test]
    fn a_table_row_shows_its_detail_cells_and_target() {
        let b = block(json!({"type": "tr", "id": "pr7", "target": "https://example.test/pr/7",
            "cells": {"pr": "#7", "title": "Fix the flaky scheduler test", "why": "the nightly run needs it before Friday", "ci": ["ok", null, "fail"], "empty": "", "seen": "2026-11-06T10:00:00Z"},
            "detail": "Fails **one run in five** on `main`.\n\nSecond paragraph.", "tone": "red"}));
        let wall = crate::util::parse_rfc3339("2026-11-06T12:00:00Z").unwrap();
        let (ls, open) = lines(&b, &State::default(), wall, 40);
        let t = text(&ls);
        assert_eq!(t[0], "Fix the flaky scheduler test");
        assert_eq!(t[1], "tr · pr7");
        assert_eq!(&t[2..7], ["", "Fails one run in five on main.", "", "Second paragraph.", ""]);
        let has = |s: &str, k: Run| ls[3].iter().any(|(t, r)| t.trim() == s && *r == k);
        assert!(has("one run in five", Run::Bold) && has("main", Run::Code), "{:?}", ls[3]);
        // the title cell is the first line already; a long value wraps under itself
        assert_eq!(&t[7..11], ["pr      #7", "why     the nightly run needs it before", "        Friday", "ci      ok · fail"]);
        assert_eq!(t[11], "seen    2h ago · 2026-11-06T10:00:00Z");
        assert_eq!(t[12], "target  https://example.test/pr/7");
        assert!(!t.iter().any(|l| l.contains("tone") || l.contains("empty")), "{t:?}");
        assert_eq!(open, Some(14));
        assert_eq!(t[14], "⏎ open the target");
    }

    /// A todo says its status from the state and its note; a block with nothing more says so.
    #[test]
    fn a_todo_and_a_bare_row() {
        let b = block(json!({"type": "todo", "id": "t1", "text": "wire it", "note": "after the parser lands", "owner": "you", "status": "doing"}));
        let t = text(&lines(&b, &State::default(), 0.0, 40).0);
        assert!(t.contains(&"status  doing".to_string()) && t.contains(&"note    after the parser lands".to_string()) && t.contains(&"owner   you".to_string()), "{t:?}");
        assert_eq!(t.iter().filter(|l| l.starts_with("status")).count(), 1);
        let b = block(json!({"type": "row", "id": "r1", "text": "just a row"}));
        let (ls, open) = lines(&b, &State::default(), 0.0, 40);
        assert_eq!(text(&ls), ["just a row", "row · r1", "", "nothing more on this row"]);
        assert_eq!(open, None);
    }

    /// Beside the list from 100 columns, under it when narrower, nowhere in a pane too short.
    #[test]
    fn placement_wide_and_narrow() {
        assert_eq!(place(120, 40, 2, 37), Some(Rect::new(72, 2, 48, 35)));
        assert_eq!(place(100, 40, 2, 37), Some(Rect::new(60, 2, 40, 35)));
        assert_eq!(place(99, 40, 2, 37), Some(Rect::new(0, 25, 99, 12)));
        assert_eq!(place(80, 15, 2, 12), None);
        assert_eq!(place(80, 20, 2, 9), None);
    }
}
