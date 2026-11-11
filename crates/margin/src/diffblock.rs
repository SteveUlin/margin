//! The diff block. difftastic computes a structural diff (`DFT_UNSTABLE=yes difft --display json`)
//! and margin draws it: tinted lines, changed tokens in a stronger tint, Everforest syntax colours,
//! unified when narrow, side by side when wide. The JSON carries no line text (research.md §4), so
//! `margin diff` reads both files and stores aligned rows in the document: the renderer stays pure
//! and re-flows the same rows to any pane width.

use crate::components::{icon, Ctx, Drawn, TX};
use crate::doc::Block;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Command;
use vellum::color::Rgb;
use vellum::grid::Grid;
use vellum::layout::Rect;
use vellum::parts;
use vellum::style::Style;
use vellum::text::{truncate, width};

/// Where difftastic is: `$MARGIN_DIFFT`, else `difft` on PATH, else config `difft`, else the usual
/// nix profile dirs. A dev build run outside `nix develop` found no difft and failed with a bare
/// "No such file or directory" (2026-11-06), so the error names every way to fix it.
pub fn difft_bin() -> Result<std::path::PathBuf, String> {
    use std::path::PathBuf;
    if let Some(p) = std::env::var_os("MARGIN_DIFFT").filter(|p| !p.is_empty()) {
        return Ok(PathBuf::from(p));
    }
    let on_path = std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join("difft")).find(|f| f.is_file()));
    let cfg = crate::config::Config::load().difft;
    let home = std::env::var("HOME").unwrap_or_default();
    let user = std::env::var("USER").unwrap_or_default();
    let fallbacks = [format!("{home}/.nix-profile/bin/difft"), format!("/etc/profiles/per-user/{user}/bin/difft"), "/run/current-system/sw/bin/difft".into()];
    on_path
        .or_else(|| (!cfg.is_empty()).then(|| PathBuf::from(&cfg)).filter(|p| p.is_file()))
        .or_else(|| fallbacks.iter().map(PathBuf::from).find(|p| p.is_file()))
        .ok_or_else(|| {
            "difftastic (difft) not found: put difft on PATH (`nix develop` in the margin checkout, or `nix profile install nixpkgs#difftastic`), \
             set \"difft\": \"/path/to/difft\" in the margin config or MARGIN_DIFFT, or use the nix build (`nix build`), which wraps it in"
                .to_string()
        })
}

/// Build a `diff` block from two files. Keeps `context` unchanged lines around each change.
pub fn compute(id: &str, old: &str, new: &str, path: Option<&str>, context: usize) -> Result<Block, String> {
    let bin = difft_bin()?;
    let out = Command::new(&bin)
        .env("DFT_UNSTABLE", "yes")
        .args(["--display", "json", old, new])
        .output()
        .map_err(|e| format!("{}: {e}", bin.display()))?;
    let v: Value = serde_json::from_slice(&out.stdout).map_err(|e| format!("difft json: {e}"))?;
    let lt: Vec<String> = std::fs::read_to_string(old).unwrap_or_default().lines().map(String::from).collect();
    let rt: Vec<String> = std::fs::read_to_string(new).map_err(|e| format!("{new}: {e}"))?.lines().map(String::from).collect();
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
    let aligned: Vec<(Option<u64>, Option<u64>)> = v["aligned_lines"]
        .as_array()
        .map(|a| a.iter().map(|p| (p[0].as_u64(), p[1].as_u64())).collect())
        .unwrap_or_default();
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
    f.insert("path".into(), json!(path.unwrap_or(new)));
    f.insert("language".into(), v["language"].clone());
    f.insert("adds".into(), json!(adds));
    f.insert("dels".into(), json!(dels));
    f.insert("rows".into(), json!(rows));
    Ok(Block { kind: "diff".into(), id: id.into(), children: vec![], f })
}

// ---------------------------------------------------------------------------------------------
// drawing

struct Row {
    gap: bool,
    l: Option<u64>,
    r: Option<u64>,
    lt: Option<String>,
    rt: Option<String>,
    lc: Option<Vec<(usize, usize)>>,
    rc: Option<Vec<(usize, usize)>>,
}

fn spans(v: &Value) -> Option<Vec<(usize, usize)>> {
    v.as_array().map(|a| a.iter().map(|s| (s[0].as_u64().unwrap_or(0) as usize, s[1].as_u64().unwrap_or(0) as usize)).collect())
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

/// One line of code with Everforest's syntax colours (a small lexer: keywords, strings, numbers,
/// calls, types, comments, operators). Changed byte ranges get the stronger tint.
fn code(g: &mut Grid, x: u16, y: u16, src: &str, maxw: usize, bg: Option<Rgb>, hl: Rgb, changed: &[(usize, usize)], cx: &Ctx) {
    const KW: &[&str] = &[
        "const", "let", "return", "export", "function", "import", "from", "if", "else", "new", "for", "of", "await", "async",
        "type", "interface", "fn", "pub", "use", "mut", "impl", "struct", "enum", "match", "def", "class", "while", "in",
    ];
    let p = cx.p;
    let chars: Vec<(usize, char)> = src.char_indices().collect();
    let mut i = 0;
    let mut col = 0usize;
    let base = |s: Style| match bg {
        Some(b) => s.on(b),
        None => s.on(p.bg0),
    };
    while i < chars.len() && col < maxw {
        let (bi, c) = chars[i];
        let start = i;
        let (fg, italic) = if src[bi..].starts_with("//") || c == '#' {
            i = chars.len();
            (p.grey1, true)
        } else if c == '"' || c == '\'' || c == '`' {
            i += 1;
            while i < chars.len() && chars[i].1 != c {
                i += 1;
            }
            i = (i + 1).min(chars.len());
            (p.aqua, false)
        } else if c.is_ascii_digit() {
            while i < chars.len() && (chars[i].1.is_ascii_alphanumeric() || chars[i].1 == '.') {
                i += 1;
            }
            (p.purple, false)
        } else if c.is_alphabetic() || c == '_' || c == '$' {
            while i < chars.len() && (chars[i].1.is_alphanumeric() || chars[i].1 == '_' || chars[i].1 == '$') {
                i += 1;
            }
            let word: String = chars[start..i].iter().map(|x| x.1).collect();
            let next = chars.get(i).map(|x| x.1);
            let fg = if KW.contains(&word.as_str()) {
                p.red
            } else if next == Some('(') {
                p.green
            } else if word.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
                p.yellow
            } else {
                p.fg
            };
            (fg, false)
        } else if "=+-*/<>!&|?:".contains(c) {
            while i < chars.len() && "=+-*/<>!&|?:".contains(chars[i].1) {
                i += 1;
            }
            (p.orange, false)
        } else {
            i += 1;
            (p.grey2, false)
        };
        for &(bj, ch) in &chars[start..i] {
            if col >= maxw {
                break;
            }
            let on = changed.iter().any(|&(s, e)| bj >= s && bj < e);
            let mut st = base(Style::fg(fg));
            if on {
                st.bg = Some(hl);
            }
            if italic {
                st = st.italic();
            }
            let mut buf = [0u8; 4];
            g.put(x + col as u16, y, ch.encode_utf8(&mut buf), st);
            col += 1;
        }
    }
}

pub fn draw(cx: &Ctx, b: &Block, w: u16, ind: u16) -> Drawn {
    let p = cx.p;
    let g0 = p.bg0;
    let rs = rows(b);
    let side = w >= 100;
    let max_rows = b.num("max_rows").unwrap_or(16.0) as usize;
    // lay out the visual lines first
    enum Line<'a> {
        Gap,
        Uni(char, Option<u64>, &'a str, &'a [(usize, usize)]),
        Side(&'a Row),
    }
    let mut lines: Vec<Line> = Vec::new();
    for r in &rs {
        if r.gap {
            lines.push(Line::Gap);
        } else if side {
            lines.push(Line::Side(r));
        } else {
            let lch = r.lc.is_some();
            let rch = r.rc.is_some();
            if !lch && !rch {
                lines.push(Line::Uni(' ', r.r, r.rt.as_deref().unwrap_or(""), &[]));
            } else {
                if lch {
                    lines.push(Line::Uni('-', r.l, r.lt.as_deref().unwrap_or(""), r.lc.as_deref().unwrap_or(&[])));
                }
                if rch {
                    lines.push(Line::Uni('+', r.r, r.rt.as_deref().unwrap_or(""), r.rc.as_deref().unwrap_or(&[])));
                }
            }
        }
    }
    let hidden = lines.len().saturating_sub(max_rows);
    lines.truncate(max_rows);
    let verdict = cx.state.decisions.get(&b.id).filter(|d| d.kind == "verdict").and_then(|d| d.value.as_str().map(String::from));
    let h = 1 + lines.len() as u16 + (hidden > 0) as u16 + 1;
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
    let x0 = ind;
    let cw = w.saturating_sub(x0 + 1);
    for (i, ln) in lines.iter().enumerate() {
        let y = 1 + i as u16;
        match ln {
            Line::Gap => {
                g.put(x0, y, &"╌".repeat(cw as usize), Style::fg(p.bg3).on(g0));
            }
            Line::Uni(k, no, text, ch) => {
                let (bg, hl, nc) = match k {
                    '-' => (Some(p.bg_red), p.hl_red, p.red),
                    '+' => (Some(p.bg_green), p.hl_green, p.green),
                    _ => (None, g0, p.grey0),
                };
                if let Some(bg) = bg {
                    g.tint(Rect::new(x0, y, cw, 1), bg);
                }
                let gb = bg.unwrap_or(g0);
                g.put(x0, y, &no.map(|n| format!("{n:>3}")).unwrap_or("   ".into()), Style::fg(nc).on(gb));
                g.put(x0 + 4, y, &k.to_string(), Style::fg(nc).on(gb).bold());
                code(&mut g, x0 + 6, y, text, cw.saturating_sub(6) as usize, bg, hl, ch, cx);
            }
            Line::Side(r) => {
                let half = (cw - 1) / 2;
                for (no, text, ch, sx, minus) in [(r.l, &r.lt, &r.lc, x0, true), (r.r, &r.rt, &r.rc, x0 + half + 1, false)] {
                    let changed = ch.is_some();
                    let (bg, hl, nc) = if !changed {
                        (None, g0, p.grey0)
                    } else if minus {
                        (Some(p.bg_red), p.hl_red, p.red)
                    } else {
                        (Some(p.bg_green), p.hl_green, p.green)
                    };
                    if let Some(bg) = bg {
                        g.tint(Rect::new(sx, y, half, 1), bg);
                    }
                    let gb = bg.unwrap_or(g0);
                    g.put(sx, y, &no.map(|n| format!("{n:>3} ")).unwrap_or("    ".into()), Style::fg(nc).on(gb));
                    code(&mut g, sx + 4, y, text.as_deref().unwrap_or(""), half.saturating_sub(4) as usize, bg, hl, ch.as_deref().unwrap_or(&[]), cx);
                }
                g.put(x0 + half, y, "│", Style::fg(p.bg3).on(g0));
            }
        }
    }
    let mut y = 1 + lines.len() as u16;
    if hidden > 0 {
        g.put(x0 + 6, y, &format!("… {hidden} more lines"), Style::fg(p.grey1).on(g0).italic());
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
