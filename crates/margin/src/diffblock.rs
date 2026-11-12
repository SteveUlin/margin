//! The diff block. difftastic computes a structural diff (`DFT_UNSTABLE=yes difft --display json`)
//! and margin draws it: tinted lines, changed tokens in a stronger tint, the language's colors
//! (highlight.rs), unified when narrow, side by side when wide. The JSON carries no line text, so
//! `margin diff` reads both files and stores aligned rows in the document: the renderer stays pure
//! and re-flows the same rows to any pane width. difftastic is a program margin runs, never a
//! library: `difft_bin` finds it.

use crate::components::{icon, in_full, Ctx, Drawn, TX};
use crate::doc::Block;
use crate::highlight::{self, Role};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Command;
use vellum::color::Rgb;
use vellum::grid::Grid;
use vellum::layout::Rect;
use vellum::parts;
use vellum::style::Style;
use vellum::text::{truncate, width};

/// Where difftastic is, first hit wins: `$MARGIN_DIFFT`; `difft` on PATH; the config key `difft`;
/// the path compiled in by the nix build (`MARGIN_DIFFT_BUILTIN`, set in flake.nix: it survives
/// `margin pin`, which copies the binary out from behind the wrapper that sets PATH); the usual
/// nix profile dirs. A cargo build has no compiled-in path. A dev build run outside `nix develop`
/// found no difft and failed with a bare "No such file or directory" (2026-11-06), so the error
/// names the ways to fix it, in one line.
pub fn difft_bin() -> Result<std::path::PathBuf, String> {
    use std::path::PathBuf;
    if let Some(p) = std::env::var_os("MARGIN_DIFFT").filter(|p| !p.is_empty()) {
        return Ok(PathBuf::from(p));
    }
    let on_path = std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join("difft")).find(|f| f.is_file()));
    let cfg = crate::config::Config::load().difft;
    let builtin = option_env!("MARGIN_DIFFT_BUILTIN").unwrap_or("");
    let home = std::env::var("HOME").unwrap_or_default();
    let user = std::env::var("USER").unwrap_or_default();
    let fallbacks = [format!("{home}/.nix-profile/bin/difft"), format!("/etc/profiles/per-user/{user}/bin/difft"), "/run/current-system/sw/bin/difft".into()];
    on_path
        .or_else(|| (!cfg.is_empty()).then(|| PathBuf::from(&cfg)).filter(|p| p.is_file()))
        .or_else(|| (!builtin.is_empty()).then(|| PathBuf::from(builtin)).filter(|p| p.is_file()))
        .or_else(|| fallbacks.iter().map(PathBuf::from).find(|p| p.is_file()))
        .ok_or_else(|| "difft (difftastic) not found: put it on PATH, or set $MARGIN_DIFFT or the config key \"difft\" to its path (the nix build of margin has it built in)".to_string())
}

/// Build a `diff` block from two files: run difftastic on them and keep `context` unchanged
/// lines around each change.
pub fn compute(id: &str, old: &str, new: &str, path: Option<&str>, context: usize) -> Result<Block, String> {
    let bin = difft_bin()?;
    let out = Command::new(&bin)
        .env("DFT_UNSTABLE", "yes")
        .args(["--display", "json", old, new])
        .output()
        .map_err(|e| format!("{}: {e}", bin.display()))?;
    // difft says what is wrong on stderr (a missing file) and prints no JSON
    let v: Value = serde_json::from_slice(&out.stdout).map_err(|e| match String::from_utf8_lossy(&out.stderr).lines().find(|l| !l.trim().is_empty()) {
        Some(why) => format!("difft: {}", why.trim()),
        None => format!("difft printed no JSON ({e})"),
    })?;
    let lt: Vec<String> = std::fs::read_to_string(old).unwrap_or_default().lines().map(String::from).collect();
    let rt: Vec<String> = std::fs::read_to_string(new).map_err(|e| format!("{new}: {e}"))?.lines().map(String::from).collect();
    Ok(build(id, &v, &lt, &rt, path.unwrap_or(new), context))
}

/// The block for difftastic's JSON `v` and the two files' lines.
pub fn build(id: &str, v: &Value, lt: &[String], rt: &[String], path: &str, context: usize) -> Block {
    // changed spans per line, per side: byte ranges with difftastic's highlight class
    let mut lc: HashMap<u64, Vec<Value>> = HashMap::new();
    let mut rc: HashMap<u64, Vec<Value>> = HashMap::new();
    let mut lchanged = std::collections::HashSet::new();
    let mut rchanged = std::collections::HashSet::new();
    for chunk in v["chunks"].as_array().into_iter().flatten() {
        for e in chunk.as_array().into_iter().flatten() {
            for (side, map, set) in [("lhs", &mut lc, &mut lchanged), ("rhs", &mut rc, &mut rchanged)] {
                if let Some(n) = e[side]["line_number"].as_u64() {
                    set.insert(n);
                    let spans = map.entry(n).or_default();
                    for c in e[side]["changes"].as_array().into_iter().flatten() {
                        spans.push(json!([c["start"], c["end"], c["highlight"]]));
                    }
                }
            }
        }
    }
    // a file that is new or gone has no aligned lines: every line of it is the change
    let aligned: Vec<(Option<u64>, Option<u64>)> = match (v["aligned_lines"].as_array(), v["status"].as_str()) {
        (Some(a), _) => a.iter().map(|p| (p[0].as_u64(), p[1].as_u64())).collect(),
        (None, Some("created")) => (0..rt.len() as u64).map(|n| (None, Some(n))).collect(),
        (None, Some("deleted")) => (0..lt.len() as u64).map(|n| (Some(n), None)).collect(),
        _ => Vec::new(),
    };
    let is_change = |&(l, r): &(Option<u64>, Option<u64>)| {
        l.map(|l| lchanged.contains(&l)).unwrap_or(false) || r.map(|r| rchanged.contains(&r)).unwrap_or(false) || l.is_none() || r.is_none()
    };
    let keep: Vec<bool> = (0..aligned.len())
        .map(|i| {
            let lo = i.saturating_sub(context);
            let hi = (i + context + 1).min(aligned.len());
            (lo..hi).any(|j| is_change(&aligned[j]))
        })
        .collect();
    let mut rows = Vec::new();
    let (mut adds, mut dels) = (0, 0);
    let mut gap = false;
    for (i, &(l, r)) in aligned.iter().enumerate() {
        if !keep[i] {
            gap = true;
            continue;
        }
        if gap && !rows.is_empty() {
            rows.push(json!({"gap": true}));
        }
        gap = false;
        let lch = l.map(|l| lchanged.contains(&l) || r.is_none()).unwrap_or(false);
        let rch = r.map(|r| rchanged.contains(&r) || l.is_none()).unwrap_or(false);
        dels += lch as u32;
        adds += rch as u32;
        rows.push(json!({
            "l": l.map(|n| n + 1), "r": r.map(|n| n + 1),
            "lt": l.and_then(|n| lt.get(n as usize)).cloned(),
            "rt": r.and_then(|n| rt.get(n as usize)).cloned(),
            "lc": if lch { json!(l.and_then(|n| lc.get(&n)).cloned().unwrap_or_default()) } else { Value::Null },
            "rc": if rch { json!(r.and_then(|n| rc.get(&n)).cloned().unwrap_or_default()) } else { Value::Null },
        }));
    }
    let mut f = serde_json::Map::new();
    f.insert("path".into(), json!(path));
    f.insert("language".into(), v["language"].clone());
    f.insert("adds".into(), json!(adds));
    f.insert("dels".into(), json!(dels));
    f.insert("rows".into(), json!(rows));
    Block { kind: "diff".into(), id: id.into(), children: vec![], f }
}

// ---------------------------------------------------------------------------------------------
// drawing

/// A changed span of a line: its byte range, and the role difftastic's own class gives it (None
/// for `normal`: the language's color shows through).
type Span = (usize, usize, Option<Role>);

struct Row {
    gap: bool,
    l: Option<u64>,
    r: Option<u64>,
    lt: Option<String>,
    rt: Option<String>,
    lc: Option<Vec<Span>>,
    rc: Option<Vec<Span>>,
}

fn spans(v: &Value) -> Option<Vec<Span>> {
    let class = |c: &Value| match c.as_str() {
        Some("keyword") => Some(Role::Keyword),
        Some("comment") => Some(Role::Comment),
        Some("string") => Some(Role::Str),
        Some("type") => Some(Role::Name),
        Some("delimiter") => Some(Role::Punct),
        _ => None,
    };
    v.as_array().map(|a| a.iter().map(|s| (s[0].as_u64().unwrap_or(0) as usize, s[1].as_u64().unwrap_or(0) as usize, class(&s[2]))).collect())
}

fn rows(b: &Block) -> Vec<Row> {
    b.f.get("rows")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .map(|r| Row {
                    gap: r["gap"].as_bool().unwrap_or(false),
                    l: r["l"].as_u64(),
                    r: r["r"].as_u64(),
                    lt: r["lt"].as_str().map(String::from),
                    rt: r["rt"].as_str().map(String::from),
                    lc: spans(&r["lc"]),
                    rc: spans(&r["rc"]),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A block with no rows but a `text`: a plain unified diff, read line by line.
fn text_rows(text: &str) -> Vec<Row> {
    let line = |s: &str| Some(s.to_string());
    text.lines()
        .filter(|l| !l.starts_with("+++") && !l.starts_with("---") && !l.starts_with("diff ") && !l.starts_with("index "))
        .map(|l| {
            let blank = Row { gap: false, l: None, r: None, lt: None, rt: None, lc: None, rc: None };
            match l.chars().next() {
                Some('@') => Row { gap: true, ..blank },
                Some('+') => Row { rt: line(&l[1..]), rc: Some(Vec::new()), ..blank },
                Some('-') => Row { lt: line(&l[1..]), lc: Some(Vec::new()), ..blank },
                _ => Row { lt: line(l.get(1..).unwrap_or("")), rt: line(l.get(1..).unwrap_or("")), ..blank },
            }
        })
        .collect()
}

/// One cell of a drawn line.
type Cell = (char, Style);

/// A line of code as cells: each character in its language's color (`roles`, one per character;
/// none: plain), on the line's ground. A changed span takes the stronger tint, and the color of
/// difftastic's class for it where it has one: its change highlighting comes first.
fn cells(src: &str, roles: Option<&[Role]>, changed: &[Span], ground: Rgb, hl: Rgb, cx: &Ctx) -> Vec<Cell> {
    let mut out = Vec::new();
    for (ci, (bi, ch)) in src.char_indices().enumerate() {
        let span = changed.iter().find(|&&(s, e, _)| bi >= s && bi < e);
        let role = span.and_then(|s| s.2).or_else(|| roles.and_then(|r| r.get(ci).copied())).unwrap_or(Role::Plain);
        let st = Style::fg(highlight::color(cx.p, role)).on(if span.is_some() { hl } else { ground });
        if ch == '\t' {
            out.extend([(' ', st); 4]);
        } else {
            out.push((ch, st));
        }
    }
    out
}

/// Cells in rows of `maxw`: wrapped when `full` (the block has the focus), else one row cut with `…`.
fn fit(mut cells: Vec<Cell>, maxw: usize, full: bool, ground: Rgb, cx: &Ctx) -> Vec<Vec<Cell>> {
    let maxw = maxw.max(1);
    if cells.len() <= maxw {
        return vec![cells];
    }
    if full {
        return cells.chunks(maxw).map(|c| c.to_vec()).collect();
    }
    cells.truncate(maxw - 1);
    cells.push(('…', Style::fg(cx.p.grey1).on(ground)));
    vec![cells]
}

fn put_cells(g: &mut Grid, x: u16, y: u16, cells: &[Cell]) {
    let mut buf = [0u8; 4];
    for (i, (ch, st)) in cells.iter().enumerate() {
        g.put(x + i as u16, y, ch.encode_utf8(&mut buf), *st);
    }
}

/// The most rows a focused diff shows (unfocused: `max_rows`, default 16).
const FULL_ROWS: usize = 400;

pub fn draw(cx: &Ctx, b: &Block, w: u16, ind: u16) -> Drawn {
    let p = cx.p;
    let g0 = p.bg0;
    let mut rs = rows(b);
    if rs.is_empty() && !b.s("text").is_empty() {
        rs = text_rows(b.s("text"));
    }
    let side = w >= 100;
    // focused, the block is read: long lines wrap and every row shows
    let full = in_full(cx, b);
    let max_rows = if full { FULL_ROWS } else { b.num("max_rows").unwrap_or(16.0).max(1.0) as usize };
    let x0 = ind;
    let cw = w.saturating_sub(x0 + 1);
    let half = cw.saturating_sub(1) / 2;
    // each side's kept lines in the language's colors: the lines joined, parsed once (cached),
    // then read back by row. The label is difftastic's language name, else the path.
    let colors = |pick: fn(&Row) -> Option<&String>| -> Vec<Option<Vec<Role>>> {
        let text: Vec<&str> = rs.iter().filter_map(|r| pick(r).map(|s| s.as_str())).collect();
        let joined = text.join("\n");
        let hl = highlight::lines(b.s("language"), &joined).or_else(|| highlight::lines(b.s("path"), &joined));
        let mut n = 0;
        rs.iter()
            .map(|r| {
                pick(r).and_then(|_| {
                    n += 1;
                    hl.as_ref().and_then(|h| h.get(n - 1)).map(highlight::per_char)
                })
            })
            .collect()
    };
    let (lcol, rcol) = (colors(|r| r.lt.as_ref()), colors(|r| r.rt.as_ref()));
    // one side of one row, as drawn rows of cells: (ground, number color, wrapped cells)
    let one = |text: &Option<String>, roles: &Option<Vec<Role>>, ch: &Option<Vec<Span>>, minus: bool, maxw: usize| -> (Rgb, Rgb, Vec<Vec<Cell>>) {
        let (ground, hl, nc) = match (ch.is_some(), minus) {
            (false, _) => (g0, g0, p.grey0),
            (true, true) => (p.bg_red, p.hl_red, p.red),
            (true, false) => (p.bg_green, p.hl_green, p.green),
        };
        let c = cells(text.as_deref().unwrap_or(""), roles.as_deref(), ch.as_deref().unwrap_or(&[]), ground, hl, cx);
        (ground, nc, fit(c, maxw, full, ground, cx))
    };
    // lay out the visual lines first
    enum Line {
        Gap,
        /// sign, line number (the first row of a wrapped line only), ground, number color, cells
        Uni(char, Option<u64>, Rgb, Rgb, Vec<Cell>),
        /// per side: line number, ground, number color, cells
        Side([(Option<u64>, Rgb, Rgb, Vec<Cell>); 2]),
    }
    let mut lines: Vec<Line> = Vec::new();
    for (i, r) in rs.iter().enumerate() {
        if r.gap {
            lines.push(Line::Gap);
        } else if side {
            let (lg, ln, lrows) = one(&r.lt, &lcol[i], &r.lc, true, half.saturating_sub(4) as usize);
            let (rg, rn, rrows) = one(&r.rt, &rcol[i], &r.rc, false, half.saturating_sub(4) as usize);
            for k in 0..lrows.len().max(rrows.len()) {
                let no = |n: Option<u64>| n.filter(|_| k == 0);
                lines.push(Line::Side([(no(r.l), lg, ln, lrows.get(k).cloned().unwrap_or_default()), (no(r.r), rg, rn, rrows.get(k).cloned().unwrap_or_default())]));
            }
        } else {
            let maxw = cw.saturating_sub(6) as usize;
            let mut uni = |sign: char, no: Option<u64>, (ground, nc, rows): (Rgb, Rgb, Vec<Vec<Cell>>)| {
                for (k, c) in rows.into_iter().enumerate() {
                    lines.push(Line::Uni(if k == 0 { sign } else { ' ' }, no.filter(|_| k == 0), ground, nc, c));
                }
            };
            match (r.lc.is_some(), r.rc.is_some()) {
                (false, false) => uni(' ', r.r, one(&r.rt, &rcol[i], &None, false, maxw)),
                (lch, rch) => {
                    if lch {
                        uni('-', r.l, one(&r.lt, &lcol[i], &r.lc, true, maxw));
                    }
                    if rch {
                        uni('+', r.r, one(&r.rt, &rcol[i], &r.rc, false, maxw));
                    }
                }
            }
        }
    }
    let empty = lines.is_empty();
    let hidden = lines.len().saturating_sub(max_rows);
    lines.truncate(max_rows);
    let verdict = cx.state.decisions.get(&b.id).filter(|d| d.kind == "verdict").and_then(|d| d.value.as_str().map(String::from));
    let h = 1 + lines.len() as u16 + (hidden > 0 || empty) as u16 + 1;
    let mut g = Grid::new(w, h);
    let a = g.area();
    g.fill(a, Style::default().on(g0));
    // header: icon, path, +a -d, language
    g.put(ind, 0, icon::DIFF, Style::fg(p.blue).on(g0));
    let lang = b.s("language");
    let stat = format!("+{} -{}", b.num("adds").unwrap_or(0.0), b.num("dels").unwrap_or(0.0));
    let room = w.saturating_sub(ind + TX + 2 + width(&stat) as u16 + width(lang) as u16 + 4) as usize;
    let mut x = g.put(ind + TX, 0, &truncate(b.s("path"), room), Style::fg(p.fg).on(g0).bold());
    let adds = format!("+{}", b.num("adds").unwrap_or(0.0));
    x = g.put(x + 2, 0, &adds, Style::fg(p.green).on(g0));
    x = g.put(x + 1, 0, &format!("-{}", b.num("dels").unwrap_or(0.0)), Style::fg(p.red).on(g0));
    if !lang.is_empty() {
        parts::tag(&mut g, x + 2, 0, lang, p.grey2, g0, a);
    }
    for (i, ln) in lines.iter().enumerate() {
        let y = 1 + i as u16;
        match ln {
            Line::Gap => {
                g.put(x0, y, &"╌".repeat(cw as usize), Style::fg(p.bg3).on(g0));
            }
            Line::Uni(k, no, ground, nc, c) => {
                if *ground != g0 {
                    g.tint(Rect::new(x0, y, cw, 1), *ground);
                }
                g.put(x0, y, &no.map(|n| format!("{n:>3}")).unwrap_or("   ".into()), Style::fg(*nc).on(*ground));
                g.put(x0 + 4, y, &k.to_string(), Style::fg(*nc).on(*ground).bold());
                put_cells(&mut g, x0 + 6, y, c);
            }
            Line::Side(sides) => {
                for ((no, ground, nc, c), sx) in sides.iter().zip([x0, x0 + half + 1]) {
                    if *ground != g0 {
                        g.tint(Rect::new(sx, y, half, 1), *ground);
                    }
                    g.put(sx, y, &no.map(|n| format!("{n:>3} ")).unwrap_or("    ".into()), Style::fg(*nc).on(*ground));
                    put_cells(&mut g, sx + 4, y, c);
                }
                g.put(x0 + half, y, "│", Style::fg(p.bg3).on(g0));
            }
        }
    }
    let mut y = 1 + lines.len() as u16;
    if hidden > 0 {
        g.put(x0 + 6, y, &format!("… {hidden} more lines (focus the diff to read them)"), Style::fg(p.grey1).on(g0).italic());
        y += 1;
    } else if empty {
        g.put(x0 + TX, y, "no changes", Style::fg(p.grey1).on(g0).italic());
        y += 1;
    }
    let f = cx.flash.get(&b.id).copied().unwrap_or(0.0);
    let mut buttons = Vec::new();
    match verdict.as_deref() {
        Some(v) => {
            let col = match v {
                "approved" => p.green,
                "rejected" => p.red,
                _ => p.yellow,
            };
            let e = parts::pill(&mut g, x0, y, &format!(" {} you {v} ", icon::USER), col.mix(g0, f * 0.3), p.bg0, g0, a);
            if cx.mouse {
                // the verdict pill is the undo, like a todo's glyph: click it to take it back
                buttons.push((x0, y, e - x0, crate::select::short("ask", &b.id, Some("clear"))));
                g.put(e + 1, y, "click to take back", Style::fg(p.grey1).on(g0));
            } else {
                g.put(e + 1, y, if cx.linked { "Claude reads it next turn" } else { "sent" }, Style::fg(p.grey1).on(g0));
            }
        }
        None if cx.mouse => {
            // buttons, always shown: the verdict is the whole point of the block
            let mut x = x0;
            for (verb, c) in [("approve", p.green), ("reject", p.red), ("ask", p.yellow)] {
                let x1 = parts::tag(&mut g, x, y, &format!(" {verb} "), c, g0, a);
                buttons.push((x, y, x1 - x, crate::select::short(verb, &b.id, None)));
                x = x1 + 1;
            }
        }
        None => {
            let mut x = x0;
            for verb in ["approve", "reject", "ask"] {
                x = parts::token(&mut g, x, y, &crate::select::short(verb, &b.id, None), p, g0, 0.0, a) + 2;
            }
        }
    }
    Drawn { grid: Some(g), buttons, ..Default::default() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::State;
    use vellum::anim::Motion;
    use vellum::color::EVERFOREST;

    /// What `difft --display json` prints for the two files of `sample`: one line changed, one added.
    const DIFFT: &str = r#"{"aligned_lines":[[0,0],[1,1],[2,2],[null,3],[3,4]],"chunks":[[
        {"lhs":{"line_number":1,"changes":[{"start":9,"end":10,"content":"1","highlight":"normal"}]},
         "rhs":{"line_number":1,"changes":[{"start":9,"end":12,"content":"300","highlight":"normal"}]}},
        {"rhs":{"line_number":3,"changes":[{"start":0,"end":6,"content":"return","highlight":"keyword"},{"start":7,"end":10,"content":"cap","highlight":"normal"},{"start":10,"end":11,"content":";","highlight":"delimiter"}]}}]],
        "language":"C++","path":"new.cc","status":"changed"}"#;

    fn sample() -> Block {
        let lines = |s: &str| s.lines().map(String::from).collect::<Vec<_>>();
        let old = lines("// the cap\nint cap = 1;\nint f() {\n}");
        let new = lines("// the cap\nint cap = 300;\nint f() {\nreturn cap; // a line long enough that a narrow pane has to cut it somewhere\n}");
        build("d1", &serde_json::from_str(DIFFT).unwrap(), &old, &new, "src/cap.cc", 2)
    }

    fn text(b: &Block, w: u16, focus: Option<&str>) -> (String, Grid) {
        let doc: crate::doc::Doc = serde_json::from_value(json!({"id": "a", "rev": 1, "blocks": []})).unwrap();
        let state = State::default();
        let (flash, progress, images, pulse) = (HashMap::new(), HashMap::new(), HashMap::new(), HashMap::new());
        let cx = Ctx {
            doc: &doc, state: &state, p: &EVERFOREST, now: 0.0, wall: 0.0, m: Motion { on: false, slow: 1.0 }, delivered: 0, flash: &flash,
            progress: &progress, kitty: false, cell: (10, 20), images: &images, focused: focus, pulse: &pulse, pulse_period: 2.4,
            pulse_breaths: 0.0, mouse: false, hover: None, flog: false, linked: true, consumer: "Otto",
        };
        let g = draw(&cx, b, w, 1).grid.unwrap();
        (g.to_text().lines().map(|l| l.trim_end().to_string()).collect::<Vec<_>>().join("\n"), g)
    }

    #[test]
    fn a_canned_difft_result_becomes_rows_and_draws_unified_then_side_by_side() {
        let b = sample();
        assert_eq!((b.num("adds"), b.num("dels"), b.s("language"), b.s("path")), (Some(2.0), Some(1.0), "C++", "src/cap.cc"));
        let (t, g) = text(&b, 50, None);
        let lines: Vec<&str> = t.lines().collect();
        assert!(lines[0].contains("src/cap.cc") && lines[0].contains("+2 -1") && lines[0].contains("C++"), "{t}");
        assert_eq!(&lines[1..5], ["   1   // the cap", "   2 - int cap = 1;", "   2 + int cap = 300;", "   3   int f() {"], "{t}");
        // a line too long for the pane is cut with …, inside the pane
        assert!(lines[5].starts_with("   4 + return cap; // a line") && lines[5].ends_with('…') && lines[5].chars().count() <= 49, "{t}");
        assert!(lines[7].contains("mg:approve/d1") && lines[7].contains("mg:reject/d1") && lines[7].contains("mg:ask/d1"), "{t}");
        let p = &EVERFOREST;
        let at = |x: u16, y: u16| g.get(x, y).unwrap().style;
        // context in the language's colors; a changed line tinted, its changed token stronger
        assert_eq!((at(7, 1).fg, at(7, 1).bg), (Some(p.grey0), Some(p.bg0)), "the comment is dim");
        assert_eq!((at(7, 3).fg, at(7, 3).bg, at(17, 3).bg), (Some(p.orange), Some(p.bg_green), Some(p.hl_green)), "int on the added line; 300 is the change");
        assert_eq!((at(7, 5).fg, at(7, 5).bg), (Some(p.orange), Some(p.hl_green)), "difftastic's class colors a changed token");
        // focused, the long line wraps and nothing is cut
        let (full, _) = text(&b, 50, Some("d1"));
        assert!(!full.contains('…') && full.contains("somewhere") && full.lines().count() > t.lines().count(), "{full}");
        // wide: the two files side by side, the added line alone on the right
        let (wide, _) = text(&b, 120, None);
        let l: Vec<&str> = wide.lines().collect();
        assert!(l[2].contains("2 int cap = 1;") && l[2].contains("│  2 int cap = 300;"), "{wide}");
        assert!(l[4].trim_start().starts_with("│  4 return cap;"), "{wide}");
    }

    #[test]
    fn a_new_file_an_unchanged_one_and_a_plain_text_diff_all_draw() {
        let new: Vec<String> = vec!["a = 1".into(), "b = 2".into()];
        let created = build("d1", &json!({"language": "Python", "status": "created"}), &[], &new, "x.py", 2);
        let (t, _) = text(&created, 40, None);
        assert!(t.contains("+2 -0") && t.contains("  1 + a = 1") && t.contains("  2 + b = 2"), "{t}");
        let same = build("d1", &json!({"language": "Python", "status": "unchanged"}), &new, &new, "x.py", 2);
        assert!(text(&same, 40, None).0.contains("no changes"));
        let plain: Block = serde_json::from_value(json!({"type": "diff", "id": "d1", "path": "x.py", "text": "--- a/x.py\n+++ b/x.py\n@@ -1,2 +1,2 @@\n a = 1\n-b = 2\n+b = 3"})).unwrap();
        let (t, _) = text(&plain, 40, None);
        assert!(t.contains("      a = 1") && t.contains("    - b = 2") && t.contains("    + b = 3"), "{t}");
    }
}
