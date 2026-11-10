//! Dashboard widgets: a labeled rule, name badges, id colors, run strips and a table, all drawn
//! from one [`Theme`] so an app restyles them in one place.
//!
//! The widgets speak in [`Piece`]s: a string and a style without a ground. A caller builds a
//! cell's pieces (`who_pieces`, `run_pieces`, or its own), and `put_pieces` / `table_cell` lay them
//! on whatever ground the row has (a zebra stripe, a focus tint). Nothing here knows about
//! margin's blocks: margin's components parse a block and call these.

use crate::color::{Palette, Rgb};
use crate::grid::Grid;
use crate::layout::Rect;
use crate::style::Style;
use crate::text::{graphemes, width};
use std::collections::HashMap;

/// Styled text without a ground; the row it lands on supplies the ground.
pub type Piece = (String, Style);

/// A glyph and its color: one run outcome, or one person's badge.
#[derive(Clone, Debug, PartialEq)]
pub struct Mark {
    pub glyph: String,
    pub color: Rgb,
}

impl Mark {
    pub fn new(glyph: &str, color: Rgb) -> Mark {
        Mark { glyph: glyph.into(), color }
    }
}

/// A per-name override: either half may be left to the hash.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WhoOverride {
    pub glyph: Option<String>,
    pub color: Option<Rgb>,
}

/// How each run outcome draws, and whether a blank cell separates the squares (off by default:
/// the small square leaves its own gap inside its cell).
#[derive(Clone, Debug, PartialEq)]
pub struct RunTheme {
    pub ok: Mark,
    pub fail: Mark,
    pub skip: Mark,
    pub running: Mark,
    /// No run in that slot (a `null`): a faint dot keeps the positions aligned.
    pub none: Mark,
    pub gap: bool,
}

/// Everything the widgets read to decide how they look.
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    /// Tables stripe alternate rows unless the table says otherwise.
    pub zebra: bool,
    /// The stripe's ground.
    pub zebra_ground: Rgb,
    /// The glyph a rule repeats (one cell wide).
    pub rule_glyph: String,
    pub rule_color: Rgb,
    pub rule_label: Rgb,
    /// Table header titles.
    pub header: Rgb,
    /// Bold text in running prose (a `**lead-in**`): the one color that says "read this first".
    /// Orange, not yellow: margin already spends yellow on a running step, a claim's mark and
    /// id, and warnings, and a yellow lead-in ran straight into a claim's yellow id.
    pub emphasis: Rgb,
    /// Badge glyphs and colors a name hashes into; ids hash into the colors only.
    pub who_glyphs: Vec<String>,
    pub who_colors: Vec<Rgb>,
    /// Overrides by lowercased name.
    pub who_names: HashMap<String, WhoOverride>,
    pub runs: RunTheme,
}

/// Twelve one-cell glyphs that stay distinct at a glance.
pub const WHO_GLYPHS: [&str; 12] = ["●", "■", "▲", "▼", "◆", "★", "♥", "♣", "♠", "✚", "◗", "⬢"];

/// Six hues that fill the gaps between the palette's accents (sky, blue, violet, magenta, lime,
/// tan), at about the accents' lightness so none shouts on the dark ground. No red and no grey:
/// those mean failure and inactive.
pub const WHO_EXTRA_COLORS: [Rgb; 6] = [
    Rgb::hex("#62b8f0"),
    Rgb::hex("#8f9df8"),
    Rgb::hex("#b88cf0"),
    Rgb::hex("#ee7fd0"),
    Rgb::hex("#c8dc50"),
    Rgb::hex("#b89a78"),
];

impl Theme {
    /// The default look on palette `p`.
    pub fn new(p: &Palette) -> Theme {
        let mut who_colors = vec![p.orange, p.yellow, p.green, p.aqua, p.blue, p.purple];
        who_colors.extend(WHO_EXTRA_COLORS);
        Theme {
            zebra: false,
            zebra_ground: p.bg1,
            rule_glyph: "─".into(),
            rule_color: p.bg4,
            rule_label: p.grey1,
            header: p.grey1,
            // bold is the standard foreground unless the config names a color: one color on every
            // lead-in marked nothing, since color has to mean something where it appears
            emphasis: p.fg,
            who_glyphs: WHO_GLYPHS.iter().map(|s| s.to_string()).collect(),
            who_colors,
            who_names: HashMap::new(),
            runs: RunTheme {
                ok: Mark::new("▪", p.green),
                fail: Mark::new("✗", p.red),
                skip: Mark::new("▪", p.grey0),
                running: Mark::new("▪", p.yellow),
                none: Mark::new("·", p.bg4),
                gap: false,
            },
        }
    }

    /// A name's badge: glyph and color from two different halves of the name's hash, so the
    /// twelve glyphs and twelve colors give 144 pairs. The same name gives the same badge
    /// everywhere, whatever its case. An override replaces either half.
    pub fn badge(&self, name: &str) -> Mark {
        let key = name.trim().to_lowercase();
        let h = name_hash(&key);
        let pick = |n: usize, bits: u64| if n == 0 { 0 } else { (bits % n as u64) as usize };
        let mut glyph = self.who_glyphs.get(pick(self.who_glyphs.len(), h >> 32)).cloned().unwrap_or_else(|| "●".into());
        let mut color = self.who_colors.get(pick(self.who_colors.len(), h & 0xffff_ffff)).copied().unwrap_or_default();
        if let Some(o) = self.who_names.get(&key) {
            if let Some(g) = &o.glyph {
                glyph = g.clone();
            }
            if let Some(c) = o.color {
                color = c;
            }
        }
        Mark { glyph, color }
    }

    /// An id's color (a PR number, a ticket key): hashed like a name's, with no glyph.
    pub fn id_color(&self, id: &str) -> Rgb {
        let h = name_hash(&id.trim().to_lowercase());
        match self.who_colors.len() {
            0 => Rgb::default(),
            n => self.who_colors[((h & 0xffff_ffff) % n as u64) as usize],
        }
    }
}

/// A stable 64-bit hash of a string: FNV-1a, then a splitmix finalizer so both halves vary
/// with every input byte (FNV alone leaves the high half of a short string's hash nearly fixed).
/// Written out here, not `std`'s hasher, because a badge must not change between runs or builds.
pub fn name_hash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h ^= h >> 30;
    h = h.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94d0_49bb_1331_11eb);
    h ^ (h >> 31)
}

/// The cell after a symbol whose size must not vary: a no-break space. Terminals such as Ghostty
/// and kitty enlarge a symbol into a following blank cell, and which symbols they enlarge
/// depends on the glyph, so a row of badges came out at uneven sizes. A no-break space is not
/// blank to them: every symbol before one stays in its one cell. Used between a badge's symbol
/// and its name, and after the last mark of a run strip. (Icons that read better enlarged, a
/// section's chevron or a status glyph, keep their ordinary space.)
pub const SYMBOL_SPACE: char = '\u{a0}';

/// A name as its badge: the glyph, a [`SYMBOL_SPACE`], the name, in the badge's color.
pub fn who_pieces(th: &Theme, name: &str) -> Vec<Piece> {
    let name = name.trim();
    if name.is_empty() {
        return Vec::new();
    }
    let m = th.badge(name);
    vec![(format!("{}{SYMBOL_SPACE}{name}", m.glyph), Style::fg(m.color))]
}

/// Put a [`SYMBOL_SPACE`] in the cell at `x`, on `ground`: the cell after a strip's last mark,
/// so the newest run is drawn the size of the rest.
pub fn symbol_guard(g: &mut Grid, x: u16, y: u16, ground: Rgb, clip: Rect) {
    let mut b = [0u8; 4];
    g.put_clipped(x, y, SYMBOL_SPACE.encode_utf8(&mut b), Style::default().on(ground), clip);
}

/// An id in its hashed color.
pub fn id_pieces(th: &Theme, id: &str) -> Vec<Piece> {
    if id.is_empty() {
        return Vec::new();
    }
    vec![(id.to_string(), Style::fg(th.id_color(id)))]
}

pub fn pieces_width(ps: &[Piece]) -> usize {
    ps.iter().map(|(s, _)| width(s)).sum()
}

/// Cut pieces to `max` cells, ending in a dim `…` when anything was dropped.
pub fn truncate_pieces(ps: Vec<Piece>, max: usize) -> Vec<Piece> {
    if pieces_width(&ps) <= max {
        return ps;
    }
    if max == 0 {
        return Vec::new();
    }
    let mut out: Vec<Piece> = Vec::new();
    let mut left = max - 1;
    let mut last = Style::default();
    'all: for (s, st) in ps {
        last = st;
        let mut kept = String::new();
        for (g, gw) in graphemes(&s) {
            if gw > left {
                if !kept.is_empty() {
                    out.push((kept, st));
                }
                break 'all;
            }
            kept.push_str(g);
            left -= gw;
        }
        out.push((kept, st));
    }
    out.push(("…".into(), last));
    out
}

/// Draw pieces from `x` on `ground`; returns the x after the last cell.
pub fn put_pieces(g: &mut Grid, x: u16, y: u16, ps: &[Piece], ground: Rgb, clip: Rect) -> u16 {
    let mut x = x;
    for (s, st) in ps {
        x = g.put_clipped(x, y, s, st.on(ground), clip);
    }
    x
}

/// A horizontal rule `w` cells wide, with an optional label inset two cells from the left:
/// `── Reviews ─────────`.
#[allow(clippy::too_many_arguments)]
pub fn rule(g: &mut Grid, x: u16, y: u16, w: u16, label: &str, th: &Theme, ground: Rgb, clip: Rect) {
    let line = Style::fg(th.rule_color).on(ground);
    let put_line = |g: &mut Grid, from: u16, to: u16| {
        let mut cx = from;
        while cx < to {
            cx = g.put_clipped(cx, y, &th.rule_glyph, line, clip).max(cx + 1);
        }
    };
    let end = x.saturating_add(w);
    if label.is_empty() || w < 8 {
        put_line(g, x, end);
        return;
    }
    put_line(g, x, x + 2);
    let room = (w - 5) as usize;
    let e = g.put_clipped(x + 3, y, &crate::text::truncate(label, room), Style::fg(th.rule_label).on(ground).bold(), clip);
    put_line(g, e + 1, end);
}

/// One run's outcome. `None` is a slot where nothing ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    Fail,
    Skip,
    Running,
    None,
}

/// The outcome names a strip accepts, besides null.
pub const OUTCOMES: [&str; 4] = ["ok", "fail", "skip", "running"];

impl Outcome {
    /// `ok`, `fail`, `skip`, `running`, or `None`/"" for no run; anything else is not an outcome.
    pub fn parse(s: Option<&str>) -> Option<Outcome> {
        Some(match s {
            None | Some("") => Outcome::None,
            Some("ok") => Outcome::Ok,
            Some("fail") => Outcome::Fail,
            Some("skip") => Outcome::Skip,
            Some("running") => Outcome::Running,
            _ => return None,
        })
    }
}

/// Cells a strip of `n` runs takes: with gaps, a blank between squares and none after the last.
pub fn runs_width(n: usize, gap: bool) -> u16 {
    (if gap { (n * 2).saturating_sub(1) } else { n }) as u16
}

/// How many runs fit in `w` cells.
pub fn runs_fit(w: u16, gap: bool) -> usize {
    if gap { (w as usize + 1) / 2 } else { w as usize }
}

/// A run strip: oldest left, newest right, each run its outcome's glyph and color. When the
/// values don't fit `max` cells the oldest drop off, so the newest always shows.
pub fn run_pieces(th: &Theme, values: &[Outcome], max: u16) -> Vec<Piece> {
    let gap = th.runs.gap;
    let n = values.len().min(runs_fit(max, gap));
    let shown = &values[values.len() - n..];
    let mut out: Vec<Piece> = Vec::with_capacity(n);
    for (i, v) in shown.iter().enumerate() {
        let m = match v {
            Outcome::Ok => &th.runs.ok,
            Outcome::Fail => &th.runs.fail,
            Outcome::Skip => &th.runs.skip,
            Outcome::Running => &th.runs.running,
            Outcome::None => &th.runs.none,
        };
        let mut s = m.glyph.clone();
        if gap && i + 1 < n {
            s.push(' ');
        }
        out.push((s, Style::fg(m.color).bold_if(*v == Outcome::Fail)));
    }
    out
}

/// "2/20 failed" over the runs that happened (empty slots don't count), and whether the newest
/// of them failed. `None` when nothing ran.
pub fn run_summary(values: &[Outcome]) -> Option<(String, bool)> {
    let ran: Vec<&Outcome> = values.iter().filter(|v| **v != Outcome::None).collect();
    if ran.is_empty() {
        return None;
    }
    let failed = ran.iter().filter(|v| ***v == Outcome::Fail).count();
    Some((format!("{failed}/{} failed", ran.len()), **ran.last().unwrap() == Outcome::Fail))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Left,
    Right,
    Center,
}

/// One table column as the layout sees it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Column {
    pub title: String,
    pub align: Align,
    /// The widest cell in this column (the title is counted by the layout).
    pub natural: u16,
    /// The narrowest this column gets while anything optional is still on screen. A flexible
    /// column shrinks to it first; once every optional column is gone, a fixed column with
    /// `min < natural` (a run strip) gives cells, and last a flexible one goes under its `min`,
    /// down to [`FLEX_FLOOR`].
    pub min: u16,
    /// Shares the leftover width with the other flexible columns (text). Fixed columns take
    /// their natural width.
    pub flex: bool,
    /// May be dropped when the table doesn't fit.
    pub optional: bool,
    /// Its cells are a strip of marks with blanks between them (a run strip). Two strips side by
    /// side stand [`STRIP_GAP`] cells apart: at the usual one cell the gap between the columns
    /// equals the gap between two marks, and the two strips read as one.
    pub strip: bool,
}

/// Blank cells between two neighboring strip columns.
pub const STRIP_GAP: u16 = 3;

/// Where a visible column sits: its index in the columns, its x from the table's left, its width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    pub col: usize,
    pub x: u16,
    pub w: u16,
}

/// The fewest cells a flexible column keeps when nothing else can give: three letters and `…`.
pub const FLEX_FLOOR: u16 = 4;

/// Lay columns out in `avail` cells, one blank cell between columns ([`STRIP_GAP`] between two
/// strips).
///
/// Fixed columns take their natural width (the wider of title and cells); flexible columns share
/// what is left, none wider than its natural width, the narrow ones served first. When the
/// flexible columns would fall under their `min`, optional columns drop from the right instead.
/// With none left to drop, shrinkable fixed columns give up cells (rightmost first), then the
/// flexible ones go under `min` down to [`FLEX_FLOOR`]. Anything still too wide runs off the
/// right edge, where drawing clips it.
pub fn table_layout(cols: &[Column], avail: u16) -> Vec<Slot> {
    let nat = |c: &Column| c.natural.max(width(&c.title) as u16).max(1);
    let min = |c: &Column| c.min.clamp(1, nat(c));
    let mut vis: Vec<usize> = (0..cols.len()).collect();
    let gap = |a: usize, b: usize| if cols[a].strip && cols[b].strip { STRIP_GAP } else { 1 };
    let need = |vis: &[usize]| -> u32 {
        let cells: u32 = vis.iter().map(|&i| if cols[i].flex { min(&cols[i]) } else { nat(&cols[i]) } as u32).sum();
        cells + vis.windows(2).map(|p| gap(p[0], p[1]) as u32).sum::<u32>()
    };
    while need(&vis) > avail as u32 {
        match vis.iter().rposition(|&i| cols[i].optional) {
            Some(p) => {
                vis.remove(p);
            }
            None => break,
        }
    }
    let mut ws: Vec<u16> = vis.iter().map(|&i| if cols[i].flex { min(&cols[i]) } else { nat(&cols[i]) }).collect();
    let over = need(&vis).saturating_sub(avail as u32);
    if over > 0 {
        // squeeze shrinkable fixed columns, then flexible ones under their minimum; rightmost
        // first in each pass
        let mut over = over.min(u16::MAX as u32) as u16;
        for flex in [false, true] {
            for (k, &i) in vis.iter().enumerate().rev() {
                if cols[i].flex != flex {
                    continue;
                }
                let floor = if flex { FLEX_FLOOR.min(ws[k]) } else { min(&cols[i]) };
                let give = (ws[k] - floor).min(over);
                ws[k] -= give;
                over -= give;
            }
        }
    } else {
        // hand the spare cells to the flexible columns: the ones nearest their natural width fill
        // first, the rest split what remains evenly
        let mut spare = avail as u32 - need(&vis);
        let mut open: Vec<usize> = (0..vis.len()).filter(|&k| cols[vis[k]].flex && ws[k] < nat(&cols[vis[k]])).collect();
        open.sort_by_key(|&k| nat(&cols[vis[k]]) - ws[k]);
        while !open.is_empty() && spare > 0 {
            let share = (spare / open.len() as u32).max(1) as u16;
            let k = open.remove(0);
            let add = (nat(&cols[vis[k]]) - ws[k]).min(share).min(spare as u16);
            ws[k] += add;
            spare -= add as u32;
        }
    }
    let mut x = 0u16;
    let mut out: Vec<Slot> = Vec::with_capacity(vis.len());
    for (k, (&col, w)) in vis.iter().zip(ws).enumerate() {
        if k > 0 {
            x = x.saturating_add(gap(vis[k - 1], col));
        }
        out.push(Slot { col, x, w });
        x = x.saturating_add(w);
    }
    out
}

/// The cells a laid-out table spans.
pub fn table_width(slots: &[Slot]) -> u16 {
    slots.last().map(|s| s.x + s.w).unwrap_or(0)
}

/// Draw one cell's pieces in its slot: cut to the slot's width with `…`, then aligned.
#[allow(clippy::too_many_arguments)]
pub fn table_cell(g: &mut Grid, x0: u16, y: u16, slot: Slot, align: Align, ps: Vec<Piece>, ground: Rgb, clip: Rect) {
    let ps = truncate_pieces(ps, slot.w as usize);
    let pad = slot.w.saturating_sub(pieces_width(&ps) as u16);
    let x = x0 + slot.x + match align {
        Align::Left => 0,
        Align::Right => pad,
        Align::Center => pad / 2,
    };
    put_pieces(g, x, y, &ps, ground, clip);
}

/// A table's header: the titles in bold on row `y` (when any column has one), and a thin rule
/// under them. Returns the rows used: 2 with titles, 1 without.
#[allow(clippy::too_many_arguments)]
pub fn table_header(g: &mut Grid, x0: u16, y: u16, cols: &[Column], slots: &[Slot], th: &Theme, ground: Rgb, clip: Rect) -> u16 {
    let titled = slots.iter().any(|s| !cols[s.col].title.is_empty());
    let mut y = y;
    if titled {
        for s in slots {
            let c = &cols[s.col];
            table_cell(g, x0, y, *s, c.align, vec![(c.title.clone(), Style::fg(th.header).bold())], ground, clip);
        }
        y += 1;
    }
    let line = Style::fg(th.rule_color).on(ground);
    let mut cx = x0;
    let end = x0 + table_width(slots);
    while cx < end {
        cx = g.put_clipped(cx, y, &th.rule_glyph, line, clip).max(cx + 1);
    }
    1 + titled as u16
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::EVERFOREST;

    fn th() -> Theme {
        Theme::new(&EVERFOREST)
    }

    fn col(title: &str, natural: u16) -> Column {
        Column { title: title.into(), natural, min: natural, ..Default::default() }
    }

    fn text(ps: &[Piece]) -> String {
        ps.iter().map(|(s, _)| s.as_str()).collect()
    }

    #[test]
    fn badges_are_stable_distinct_and_overridable() {
        let mut t = th();
        assert_eq!(t.badge("ada"), t.badge("ada"), "the same name, the same badge");
        assert_eq!(t.badge("Ada "), t.badge("ada"), "case and stray spaces don't change it");
        // pinned: a badge must not change between runs, builds or machines
        assert_eq!(name_hash("ada"), name_hash("ada"));
        assert_ne!(name_hash("ada"), name_hash("adb"));
        let names = ["ada", "grace", "linus", "margaret", "ken", "dennis", "barbara", "edsger", "donald", "alan", "john", "frances"];
        let badges: Vec<Mark> = names.iter().map(|n| t.badge(n)).collect();
        let mut pairs: Vec<(String, Rgb)> = badges.iter().map(|m| (m.glyph.clone(), m.color)).collect();
        pairs.sort_by(|a, b| (a.0.as_str(), a.1 .0, a.1 .1, a.1 .2).cmp(&(b.0.as_str(), b.1 .0, b.1 .1, b.1 .2)));
        pairs.dedup();
        assert!(pairs.len() >= 11, "twelve names, {} distinct badges", pairs.len());
        // glyph and color move independently: neither is a function of the other
        let glyphs: std::collections::HashSet<&str> = badges.iter().map(|m| m.glyph.as_str()).collect();
        let colors: std::collections::HashSet<Rgb> = badges.iter().map(|m| m.color).collect();
        assert!(glyphs.len() >= 6 && colors.len() >= 6, "{} glyphs, {} colors", glyphs.len(), colors.len());
        t.who_names.insert("ada".into(), WhoOverride { glyph: Some("λ".into()), color: None });
        let hashed = th().badge("ada");
        assert_eq!(t.badge("ADA"), Mark { glyph: "λ".into(), color: hashed.color }, "the override wins, half by half");
        t.who_names.insert("ada".into(), WhoOverride { glyph: None, color: Some(Rgb(1, 2, 3)) });
        assert_eq!(t.badge("ada"), Mark { glyph: hashed.glyph, color: Rgb(1, 2, 3) });
        assert_eq!(t.id_color("PR-12"), t.id_color("pr-12"));
        assert_eq!(text(&who_pieces(&t, "grace")), format!("{}\u{a0}grace", t.badge("grace").glyph));
    }

    /// A badge's symbol and name are joined by a no-break space: one cell wide, never a place
    /// to wrap, and still there when the name is cut.
    #[test]
    fn a_badge_is_one_word_with_a_one_cell_no_break_space() {
        let t = th();
        let ps = who_pieces(&t, "grace");
        assert_eq!(width("\u{a0}"), 1);
        assert_eq!(pieces_width(&ps), 1 + 1 + 5);
        assert_eq!(ps[0].0.chars().nth(1), Some(SYMBOL_SPACE));
        // wrapping breaks at spaces only: the badge moves to the next line whole
        let mut line = vec![("reviewed by".to_string(), Style::default())];
        line.push((" ".to_string(), Style::default()));
        line.extend(ps.clone());
        let rows = crate::text::wrap_runs(&line, 13);
        assert_eq!(rows.len(), 2);
        assert_eq!(text(&rows[1]), text(&ps), "symbol and name stay together");
        assert_eq!(crate::text::wrap(&text(&ps), 3).concat(), text(&ps).replace(' ', ""), "no break point inside it: only a hard split of the word");
        // cut to four cells: the symbol, its no-break space, one letter, the ellipsis
        let cut = text(&truncate_pieces(ps.clone(), 4));
        assert_eq!(cut, format!("{}\u{a0}g…", t.badge("grace").glyph));
        // the guard after a strip is the same character
        let mut g = Grid::new(4, 1);
        let a = g.area();
        symbol_guard(&mut g, 2, 0, EVERFOREST.bg0, a);
        let mut b = [0u8; 4];
        assert_eq!(g.get(2, 0).unwrap().sym.as_str(&mut b), "\u{a0}");
    }

    #[test]
    fn default_glyphs_are_one_cell_and_colors_stand_apart() {
        let t = th();
        assert_eq!((t.who_glyphs.len(), t.who_colors.len()), (12, 12));
        for g in t.who_glyphs.iter().chain([&t.rule_glyph, &t.runs.ok.glyph, &t.runs.fail.glyph, &t.runs.skip.glyph, &t.runs.running.glyph, &t.runs.none.glyph]) {
            assert_eq!(width(g), 1, "{g} must be one cell");
        }
        let uniq: std::collections::HashSet<&String> = t.who_glyphs.iter().collect();
        assert_eq!(uniq.len(), 12);
        let dist = |a: Rgb, b: Rgb| {
            let (a, b) = (a.to_oklab(), b.to_oklab());
            ((a.l - b.l).powi(2) + (a.a - b.a).powi(2) + (a.b - b.b).powi(2)).sqrt()
        };
        let p = &EVERFOREST;
        for (i, a) in t.who_colors.iter().enumerate() {
            for b in &t.who_colors[i + 1..] {
                assert!(dist(*a, *b) > 0.035, "{} and {} read as one color ({})", a.to_hex(), b.to_hex(), dist(*a, *b));
            }
            // red means failure, grey means inactive: no badge may pass for either
            assert!(dist(*a, p.red) > 0.06, "{} is too close to red", a.to_hex());
            let lab = a.to_oklab();
            assert!(lab.a.hypot(lab.b) > 0.045, "{} reads as grey", a.to_hex());
            assert!(a.to_oklab().l - p.bg0.to_oklab().l > 0.3, "{} is too dark for the ground", a.to_hex());
        }
    }

    #[test]
    fn run_strip_has_a_cross_for_failure_and_keeps_the_newest() {
        let v = [Outcome::Ok, Outcome::Fail, Outcome::Skip, Outcome::Running, Outcome::None, Outcome::Ok];
        // by default the squares sit in adjacent cells, one cell a run
        let ps = run_pieces(&th(), &v, 40);
        assert_eq!(text(&ps), "▪✗▪▪·▪");
        assert_eq!(pieces_width(&ps) as u16, runs_width(6, false));
        assert_eq!(text(&run_pieces(&th(), &v, 3)), "▪·▪", "three cells hold the three newest");
        // with the gap a theme may ask for
        let mut t = th();
        t.runs.gap = true;
        let ps = run_pieces(&t, &v, 40);
        assert_eq!(text(&ps), "▪ ✗ ▪ ▪ · ▪");
        assert_eq!(pieces_width(&ps) as u16, runs_width(6, true));
        let colors: Vec<Rgb> = ps.iter().map(|(_, s)| s.fg.unwrap()).collect();
        let p = &EVERFOREST;
        assert_eq!(colors, [p.green, p.red, p.grey0, p.yellow, p.bg4, p.green]);
        // five cells hold three runs: the three newest
        let ps = run_pieces(&t, &v, 5);
        assert_eq!(text(&ps), "▪ · ▪");
        assert_eq!(ps[0].1.fg, Some(p.yellow), "the oldest dropped off the left");
        let mut tight = th();
        tight.runs.gap = false;
        assert_eq!(text(&run_pieces(&tight, &v, 4)), "▪▪·▪");
        assert_eq!((runs_fit(5, true), runs_fit(6, true), runs_width(0, true)), (3, 3, 0));
        assert_eq!(run_summary(&v), Some(("1/5 failed".into(), false)), "the empty slot isn't a run");
        assert_eq!(run_summary(&[Outcome::Ok, Outcome::Fail, Outcome::None]), Some(("1/2 failed".into(), true)));
        assert_eq!(run_summary(&[Outcome::None]), None);
        assert_eq!(Outcome::parse(Some("fail")), Some(Outcome::Fail));
        assert_eq!(Outcome::parse(None), Some(Outcome::None));
        assert_eq!(Outcome::parse(Some("flaky")), None);
    }

    #[test]
    fn rule_runs_the_width_with_its_label_inset() {
        let t = th();
        let mut g = Grid::new(24, 2);
        let a = g.area();
        rule(&mut g, 1, 0, 20, "Reviews", &t, EVERFOREST.bg0, a);
        rule(&mut g, 1, 1, 20, "", &t, EVERFOREST.bg0, a);
        let s = g.to_text();
        let rows: Vec<&str> = s.lines().collect();
        assert_eq!(rows[0].trim_end(), " ── Reviews ─────────");
        assert_eq!(rows[1].trim_end(), format!(" {}", "─".repeat(20)));
        assert_eq!(g.get(4, 0).unwrap().style.fg, Some(t.rule_label));
        assert_eq!(g.get(1, 0).unwrap().style.fg, Some(t.rule_color));
    }

    #[test]
    fn table_columns_take_natural_widths_and_text_takes_the_rest() {
        let cols = vec![col("id", 4), Column { title: "title".into(), natural: 30, min: 8, flex: true, ..Default::default() }, col("age", 3)];
        // room for everything: each column at its natural width, one cell between
        let s = table_layout(&cols, 60);
        assert_eq!(s, vec![Slot { col: 0, x: 0, w: 4 }, Slot { col: 1, x: 5, w: 30 }, Slot { col: 2, x: 36, w: 3 }]);
        // narrower: the fixed columns hold, the text column gives
        let s = table_layout(&cols, 25);
        assert_eq!(s, vec![Slot { col: 0, x: 0, w: 4 }, Slot { col: 1, x: 5, w: 16 }, Slot { col: 2, x: 22, w: 3 }]);
        assert_eq!(table_width(&s), 25);
        // a title wider than its cells sets the width
        assert_eq!(table_layout(&[col("author", 3)], 40)[0].w, 6);
        // two text columns: the short one keeps its natural width, the long one takes the rest
        let two = vec![Column { natural: 6, min: 4, flex: true, ..Default::default() }, Column { natural: 50, min: 4, flex: true, ..Default::default() }];
        let s = table_layout(&two, 31);
        assert_eq!((s[0].w, s[1].w), (6, 24));
    }

    #[test]
    fn optional_columns_drop_from_the_right_before_text_is_squeezed() {
        let cols = vec![
            col("id", 4),
            Column { title: "who".into(), natural: 8, min: 8, optional: true, ..Default::default() },
            Column { title: "title".into(), natural: 30, min: 8, flex: true, ..Default::default() },
            Column { title: "ci".into(), natural: 19, min: 9, optional: true, ..Default::default() },
            col("age", 3),
        ];
        let shown = |w| table_layout(&cols, w).iter().map(|s| s.col).collect::<Vec<_>>();
        assert_eq!(shown(70), [0, 1, 2, 3, 4]);
        assert_eq!(shown(46), [0, 1, 2, 3, 4], "everything fits with the text at its minimum");
        assert_eq!(shown(45), [0, 1, 2, 4], "the rightmost optional column goes first");
        assert_eq!(shown(25), [0, 2, 4]);
        let s = table_layout(&cols, 25);
        assert_eq!(s[1].w, 16, "and the text takes what the dropped columns freed");
        // nothing optional left and still too wide: text at its minimum, the strip gives up cells
        let hard = vec![Column { natural: 30, min: 8, flex: true, ..Default::default() }, Column { natural: 19, min: 9, ..Default::default() }];
        let s = table_layout(&hard, 20);
        assert_eq!((s[0].w, s[1].w), (8, 11));
        // tighter still: the strip at its minimum, then the text under its own, never under the floor
        let s = table_layout(&hard, 16);
        assert_eq!((s[0].w, s[1].w), (6, 9));
        let s = table_layout(&hard, 5);
        assert_eq!((s[0].w, s[1].w), (FLEX_FLOOR, 9), "what is left runs off the edge");
        // two strips side by side stand apart by more than the blank between two marks; a strip
        // beside anything else keeps the one cell
        let strip = |n| Column { natural: n, min: n, strip: true, ..Default::default() };
        let s = table_layout(&[col("id", 4), strip(9), strip(9), col("age", 3)], 60);
        assert_eq!(s.iter().map(|s| s.x).collect::<Vec<_>>(), [0, 5, 5 + 9 + STRIP_GAP, 5 + 9 + STRIP_GAP + 9 + 1]);
        assert_eq!(table_width(&s), 4 + 1 + 9 + STRIP_GAP + 9 + 1 + 3);
    }

    #[test]
    fn cells_truncate_and_align_and_the_header_sits_over_a_rule() {
        let t = th();
        let cols = vec![col("name", 6), Column { title: "n".into(), natural: 3, min: 3, align: Align::Right, ..Default::default() }];
        let slots = table_layout(&cols, 20);
        let mut g = Grid::new(14, 3);
        let a = g.area();
        let bg = EVERFOREST.bg0;
        assert_eq!(table_header(&mut g, 1, 0, &cols, &slots, &t, bg, a), 2);
        table_cell(&mut g, 1, 2, slots[0], Align::Left, vec![("abcdefghij".into(), Style::default())], bg, a);
        table_cell(&mut g, 1, 2, slots[1], Align::Right, vec![("7".into(), Style::default())], bg, a);
        let s = g.to_text();
        let rows: Vec<&str> = s.lines().map(|l| l.trim_end()).collect();
        assert_eq!(rows, [" name     n", " ──────────", " abcde…   7"]);
        assert!(g.get(1, 0).unwrap().style.bold);
        // no titles: the rule alone
        let bare = vec![col("", 4)];
        assert_eq!(table_header(&mut Grid::new(8, 2), 0, 0, &bare, &table_layout(&bare, 8), &t, bg, a), 1);
        let cut = truncate_pieces(vec![("ab".into(), Style::default()), ("cdef".into(), Style::default().bold())], 4);
        assert_eq!((text(&cut), cut.last().unwrap().1.bold), ("abc…".to_string(), true));
    }
}
