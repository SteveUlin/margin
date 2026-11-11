//! Unicode plots for dashboards: sparklines, braille line and area plots, bar and column charts,
//! histograms, scatter plots, heat strips and box rows, all drawn from one [`PlotStyle`].
//!
//! Three layers, bottom up:
//!
//! - **Canvases.** [`Braille`] is a dot canvas (2x4 dots a cell) with `set`, `line`, `vline`,
//!   `hline` and `fill_rect`; [`Blocks`] is a column canvas in eighths of a cell. Both draw only
//!   their non-blank cells, so marks compose over whatever is underneath (a threshold line, a
//!   zebra stripe). [`column_glyph`] and [`bar_glyph`] are the eighth-block helpers on their own.
//! - **Scales.** [`nice_step`], [`ticks_in`], [`tick_labels`] and [`fmt_num`] turn a data range
//!   into round tick values and short labels (`0`, `0.5`, `15k`); [`resample`], [`bin`] and
//!   [`quantile`] reduce data to what fits.
//! - **Plots.** One-row plots return [`Piece`]s of an exact cell width, so they sit in a table
//!   cell or a row ([`Sparkline::pieces`], [`heat_pieces`], [`box_pieces`], [`BarChart::rows`]);
//!   the rest draw into a [`Grid`] within a [`Rect`] on the ground the caller supplies
//!   ([`LinePlot`], [`ColumnChart`], [`Histogram`], [`Scatter`], [`Heatmap`]).
//!
//! Conventions every plot follows:
//!
//! - A value that is not finite (NaN, an infinity) is a missing value: a gap in a line, a
//!   [`PlotStyle::missing_glyph`] in a sparkline, no bar. [`gaps`] turns `Option<f64>` into that.
//! - A drawing plot first fills its rectangle with the ground, so every row it owns is exactly
//!   the rectangle's width; a pieces plot returns exactly the width asked for.
//! - Nothing panics on any input. With no finite data a plot writes `no data`; a rectangle one
//!   row high becomes a sparkline; too narrow for the y labels drops them; too short for the x
//!   axis or the legend drops those first.
//! - Everything reads without color: glyph shape carries the value, color only adds the series
//!   and the threshold.
//! - Rectangles larger than 2048x1024 cells are plotted at that size.
//!
//! ```
//! use vellum::plot::{LinePlot, PlotStyle, Series, Sparkline};
//! use vellum::Grid;
//!
//! let st = PlotStyle::default();
//! let ground = vellum::color::EVERFOREST.bg0;
//! let ms = [12.0, 13.0, 12.0, 40.0, f64::NAN, 15.0];
//! // a table cell: exactly eight cells, the spike in the "over" color, a dot for the gap
//! let cell = Sparkline::new(&ms).threshold(30.0).pieces(8, &st);
//! assert_eq!(cell.iter().map(|(s, _)| s.as_str()).collect::<String>(), "  ▁▁▁█·▂");
//! // a braille plot with y labels and a threshold line
//! let mut g = Grid::new(40, 8);
//! let area = g.area();
//! LinePlot::new(&[Series::new("p99", &ms)]).threshold(30.0).draw(&mut g, area, &st, ground, area);
//! assert!(g.to_text().contains('╌'));
//! ```
//!
//! # Proposal: how an app exposes this
//!
//! 1. A `plot` block: `{type:"plot", kind:"line"|"area"|"bars"|"columns"|"hist"|"spark"|"heat",
//!    series:[{label, values, color?}], height, y:{min,max}, threshold, x:{labels|ends}}`.
//! 2. `values` is an array of numbers and nulls; null maps to NaN through [`gaps`].
//! 3. `kind:"line"` and `"area"` build a [`LinePlot`] (`fill` for area); `height` defaults to 8.
//! 4. `kind:"bars"` reads `series[0]` with `labels:[...]` into a [`BarChart`]; its height is the
//!    bar count, so `height` only caps it. `kind:"columns"` builds a [`ColumnChart`] the same way.
//! 5. `kind:"hist"` reads `series[0].values` as raw samples, plus `bins` and
//!    `marker:"mean"|"median"`, into a [`Histogram`].
//! 6. `kind:"spark"` and `"heat"` are one row a series: label left, strip right ([`Heatmap`]).
//! 7. A table column kind `spark` (`{kind:"spark", width:12, min, max, threshold, bucket}`) whose
//!    cells are number arrays, drawn with [`Sparkline::pieces`]; `strip`-like, fixed width.
//! 8. A table column kind `bar` (`{kind:"bar", width:10, max}`): one number a cell, drawn with
//!    [`bar_pieces`], so a table doubles as a bar chart.
//! 9. One `[plot]` config table maps onto [`PlotStyle`]: `series = [...]`, `marks =
//!    "braille"|"blocks"`, `threshold`, `over`; a color name resolves against the palette.
//! 10. Validation rejects an unknown `kind` and non-array `values`; it warns (not fails) when
//!     `threshold` lies outside a fixed `y` range, since the line then cannot show.

// `!(a > b)` is written on purpose throughout: it is also true when either side is NaN, which
// `a <= b` is not, and NaN must take the degenerate branch.
#![allow(clippy::neg_cmp_op_on_partial_ord)]

use crate::color::{Palette, Rgb, EVERFOREST};
use crate::grid::Grid;
use crate::layout::Rect;
use crate::style::Style;
use crate::text::{truncate, width};
use crate::widgets::Piece;
use std::borrow::Cow;

/// Bottom-filled eighth blocks: index `n` fills `n` eighths of a cell from below (0 is blank).
pub const EIGHTHS_UP: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Left-filled eighth blocks: index `n` fills `n` eighths of a cell from the left (0 is blank).
pub const EIGHTHS_RIGHT: [char; 9] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];

/// Four shades, light to solid, for heat strips.
pub const SHADES: [char; 4] = ['░', '▒', '▓', '█'];

/// The largest plot, in cells; larger rectangles and canvases are cut to this.
pub const MAX_CELLS: (u16, u16) = (2048, 1024);

/// Whether series draw as braille dots or as eighth-block columns.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Marks {
    /// 2x4 dots a cell: lines, several series, scatter.
    #[default]
    Braille,
    /// Eighth-block columns: for fonts without braille. A line plot shows its first series only.
    Blocks,
}

/// Everything the plots read to decide how they look.
#[derive(Clone, Debug, PartialEq)]
pub struct PlotStyle {
    pub marks: Marks,
    /// Series colors, cycled. No red: red means "over the threshold".
    pub series: Vec<Rgb>,
    /// Spines, rules and tick marks.
    pub axis: Rgb,
    /// Tick labels, bar values, the legend's text.
    pub label: Rgb,
    /// Category labels.
    pub text: Rgb,
    /// The threshold line and its label.
    pub threshold: Rgb,
    /// A value over the threshold (sparkline cells, bars, columns).
    pub over: Rgb,
    /// A missing value's mark.
    pub missing: Rgb,
    /// Heat strips blend from the first (low) to the second (high).
    pub heat: (Rgb, Rgb),
    /// One-cell glyphs; a glyph that is not one cell wide falls back to the default.
    pub missing_glyph: char,
    pub threshold_glyph: char,
    pub marker_glyph: char,
    pub legend_glyph: char,
}

impl PlotStyle {
    /// The default look on palette `p`.
    pub fn new(p: &Palette) -> PlotStyle {
        PlotStyle {
            marks: Marks::Braille,
            series: vec![p.blue, p.orange, p.green, p.purple, p.yellow, p.aqua],
            axis: p.bg4,
            label: p.grey1,
            text: p.fg,
            threshold: p.yellow,
            over: p.red,
            missing: p.bg4,
            heat: (p.bg4, p.orange),
            missing_glyph: '·',
            threshold_glyph: '╌',
            marker_glyph: '▲',
            legend_glyph: '●',
        }
    }

    /// The `i`th series color, cycling; the text color when there are none.
    pub fn series_color(&self, i: usize) -> Rgb {
        match self.series.len() {
            0 => self.text,
            n => self.series[i % n],
        }
    }
}

impl Default for PlotStyle {
    fn default() -> PlotStyle {
        PlotStyle::new(&EVERFOREST)
    }
}

/// `c` when it is one cell wide, else `fallback`: a configured glyph must not shift a row.
fn one_cell(c: char, fallback: char) -> char {
    let mut b = [0u8; 4];
    if width(c.encode_utf8(&mut b)) == 1 { c } else { fallback }
}

/// The glyph of a column `eighths` tall in the cell `row` rows above the baseline.
pub fn column_glyph(eighths: usize, row: usize) -> char {
    EIGHTHS_UP[eighths.saturating_sub(row.saturating_mul(8)).min(8)]
}

/// The glyph of a bar `eighths` long in the cell `col` cells from its start.
pub fn bar_glyph(eighths: usize, col: usize) -> char {
    EIGHTHS_RIGHT[eighths.saturating_sub(col.saturating_mul(8)).min(8)]
}

/// Missing values as NaN: the form every plot here takes.
pub fn gaps(values: &[Option<f64>]) -> Vec<f64> {
    values.iter().map(|v| v.unwrap_or(f64::NAN)).collect()
}

// ---------------------------------------------------------------------------------------------
// canvases

/// Unicode braille numbers its dots 1-2-3-7 down the left column and 4-5-6-8 down the right;
/// indexed here by `row + 4 * column`.
const BRAILLE_BITS: [u8; 8] = [0x01, 0x02, 0x04, 0x40, 0x08, 0x10, 0x20, 0x80];

/// A braille dot canvas: `w` x `h` cells addressed as `2w` x `4h` dots, (0, 0) at the top left.
/// Each cell keeps one color, the last one set in it; dots outside the canvas are ignored.
#[derive(Clone, Debug, PartialEq)]
pub struct Braille {
    w: u16,
    h: u16,
    bits: Vec<u8>,
    colors: Vec<Rgb>,
}

impl Braille {
    /// A blank canvas of `w` x `h` cells (cut to [`MAX_CELLS`]).
    pub fn new(w: u16, h: u16) -> Braille {
        let (w, h) = (w.min(MAX_CELLS.0), h.min(MAX_CELLS.1));
        let n = w as usize * h as usize;
        Braille { w, h, bits: vec![0; n], colors: vec![Rgb::default(); n] }
    }

    /// Dots across.
    pub fn dot_w(&self) -> i32 {
        self.w as i32 * 2
    }

    /// Dots down.
    pub fn dot_h(&self) -> i32 {
        self.h as i32 * 4
    }

    fn at(&self, x: i32, y: i32) -> Option<(usize, u8)> {
        if x < 0 || y < 0 || x >= self.dot_w() || y >= self.dot_h() {
            return None;
        }
        Some(((y / 4) as usize * self.w as usize + (x / 2) as usize, BRAILLE_BITS[(y % 4) as usize + 4 * (x % 2) as usize]))
    }

    /// Light the dot at (x, y).
    pub fn set(&mut self, x: i32, y: i32, color: Rgb) {
        if let Some((i, bit)) = self.at(x, y) {
            self.bits[i] |= bit;
            self.colors[i] = color;
        }
    }

    /// Put the dot at (x, y) out.
    pub fn unset(&mut self, x: i32, y: i32) {
        if let Some((i, bit)) = self.at(x, y) {
            self.bits[i] &= !bit;
        }
    }

    /// Whether the dot at (x, y) is lit.
    pub fn get(&self, x: i32, y: i32) -> bool {
        self.at(x, y).is_some_and(|(i, bit)| self.bits[i] & bit != 0)
    }

    /// A straight segment between two dots, ends included; the part outside the canvas is cut.
    pub fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, color: Rgb) {
        let (w, h) = (self.dot_w(), self.dot_h());
        if w == 0 || h == 0 {
            return;
        }
        let Some((mut x, mut y, x1, y1)) = clip_segment(x0 as f64, y0 as f64, x1 as f64, y1 as f64, (w - 1) as f64, (h - 1) as f64) else {
            return;
        };
        // Bresenham: connects samples without the staircase a vertical fill leaves on shallow slopes
        let (dx, dy) = ((x1 - x).abs(), -(y1 - y).abs());
        let (sx, sy) = (if x < x1 { 1 } else { -1 }, if y < y1 { 1 } else { -1 });
        let mut err = dx + dy;
        loop {
            self.set(x as i32, y as i32, color);
            if x == x1 && y == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                err += dx;
                y += sy;
            }
        }
    }

    /// A vertical run of dots in column `x` from `y0` to `y1`, ends included.
    pub fn vline(&mut self, x: i32, y0: i32, y1: i32, color: Rgb) {
        let (a, b) = (y0.min(y1).max(0), y0.max(y1).min(self.dot_h() - 1));
        for y in a..=b {
            self.set(x, y, color);
        }
    }

    /// A horizontal run of dots in row `y` from `x0` to `x1`, ends included.
    pub fn hline(&mut self, y: i32, x0: i32, x1: i32, color: Rgb) {
        let (a, b) = (x0.min(x1).max(0), x0.max(x1).min(self.dot_w() - 1));
        for x in a..=b {
            self.set(x, y, color);
        }
    }

    /// Every dot of the rectangle between two corners, corners included.
    pub fn fill_rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, color: Rgb) {
        let (a, b) = (x0.min(x1).max(0), x0.max(x1).min(self.dot_w() - 1));
        for x in a..=b {
            self.vline(x, y0, y1, color);
        }
    }

    /// The glyph and color of cell (cx, cy); `None` when no dot in it is lit.
    pub fn cell(&self, cx: u16, cy: u16) -> Option<(char, Rgb)> {
        if cx >= self.w || cy >= self.h {
            return None;
        }
        let i = cy as usize * self.w as usize + cx as usize;
        match self.bits[i] {
            0 => None,
            b => Some((char::from_u32(0x2800 + b as u32).unwrap_or(' '), self.colors[i])),
        }
    }

    /// Draw the lit cells with their top left at (x, y) on `ground`; blank cells are left alone.
    pub fn draw(&self, g: &mut Grid, x: u16, y: u16, ground: Rgb, clip: Rect) {
        draw_cells(g, x, y, self.w, self.h, ground, clip, |cx, cy| self.cell(cx, cy));
    }
}

/// An eighth-block column canvas: `w` columns, each a height in eighths of a cell up from the
/// bottom of `h` rows, with one color a column.
#[derive(Clone, Debug, PartialEq)]
pub struct Blocks {
    w: u16,
    h: u16,
    heights: Vec<u16>,
    colors: Vec<Rgb>,
}

impl Blocks {
    /// A blank canvas of `w` x `h` cells (cut to [`MAX_CELLS`]).
    pub fn new(w: u16, h: u16) -> Blocks {
        let (w, h) = (w.min(MAX_CELLS.0), h.min(MAX_CELLS.1));
        Blocks { w, h, heights: vec![0; w as usize], colors: vec![Rgb::default(); w as usize] }
    }

    /// The tallest a column gets, in eighths: eight a row.
    pub fn levels(&self) -> i32 {
        self.h as i32 * 8
    }

    /// Set column `x` to `eighths` tall (cut to the canvas).
    pub fn set(&mut self, x: i32, eighths: i32, color: Rgb) {
        if x >= 0 && x < self.w as i32 {
            self.heights[x as usize] = eighths.clamp(0, self.levels()) as u16;
            self.colors[x as usize] = color;
        }
    }

    /// Set columns `x0` to `x1`, ends included, to `eighths` tall.
    pub fn fill(&mut self, x0: i32, x1: i32, eighths: i32, color: Rgb) {
        let (a, b) = (x0.min(x1).max(0), x0.max(x1).min(self.w as i32 - 1));
        for x in a..=b {
            self.set(x, eighths, color);
        }
    }

    /// Column `x`'s height in eighths; 0 outside the canvas.
    pub fn height(&self, x: i32) -> i32 {
        if x >= 0 && x < self.w as i32 { self.heights[x as usize] as i32 } else { 0 }
    }

    /// The glyph and color of cell (cx, cy), `cy` counted from the top; `None` when blank.
    pub fn cell(&self, cx: u16, cy: u16) -> Option<(char, Rgb)> {
        if cx >= self.w || cy >= self.h {
            return None;
        }
        match column_glyph(self.heights[cx as usize] as usize, (self.h - 1 - cy) as usize) {
            ' ' => None,
            c => Some((c, self.colors[cx as usize])),
        }
    }

    /// Draw the filled cells with their top left at (x, y) on `ground`; blank cells are left alone.
    pub fn draw(&self, g: &mut Grid, x: u16, y: u16, ground: Rgb, clip: Rect) {
        draw_cells(g, x, y, self.w, self.h, ground, clip, |cx, cy| self.cell(cx, cy));
    }
}

/// Put a canvas's non-blank cells on the grid, visiting only the cells inside `clip`.
#[allow(clippy::too_many_arguments)]
fn draw_cells(g: &mut Grid, x: u16, y: u16, w: u16, h: u16, ground: Rgb, clip: Rect, cell: impl Fn(u16, u16) -> Option<(char, Rgb)>) {
    let clip = clip.intersect(g.area()).intersect(Rect::new(x, y, w, h));
    let mut b = [0u8; 4];
    for ty in clip.y..clip.bottom() {
        for tx in clip.x..clip.right() {
            if let Some((c, color)) = cell(tx - x, ty - y) {
                g.put_clipped(tx, ty, c.encode_utf8(&mut b), Style::fg(color).on(ground), clip);
            }
        }
    }
}

/// Liang-Barsky: the part of a segment inside [0, xmax] x [0, ymax], as rounded dot coordinates.
/// Without it a far-off endpoint would cost a step a dot all the way there.
fn clip_segment(x0: f64, y0: f64, x1: f64, y1: f64, xmax: f64, ymax: f64) -> Option<(i64, i64, i64, i64)> {
    let (dx, dy) = (x1 - x0, y1 - y0);
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (p, q) in [(-dx, x0), (dx, xmax - x0), (-dy, y0), (dy, ymax - y0)] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
        } else {
            let t = q / p;
            if p < 0.0 {
                if t > t1 {
                    return None;
                }
                t0 = t0.max(t);
            } else {
                if t < t0 {
                    return None;
                }
                t1 = t1.min(t);
            }
        }
    }
    let pt = |t: f64| (((x0 + t * dx).round().clamp(0.0, xmax)) as i64, ((y0 + t * dy).round().clamp(0.0, ymax)) as i64);
    let (a, b) = (pt(t0), pt(t1));
    Some((a.0, a.1, b.0, b.1))
}

// ---------------------------------------------------------------------------------------------
// scales, ticks and numbers

/// An axis range with either end left to the data.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Bounds {
    pub min: Option<f64>,
    pub max: Option<f64>,
}

impl Bounds {
    /// Both ends fixed.
    pub fn new(min: f64, max: f64) -> Bounds {
        Bounds { min: Some(min), max: Some(max) }
    }

    /// The low end fixed (a zero baseline), the high end from the data.
    pub fn from(min: f64) -> Bounds {
        Bounds { min: Some(min), max: None }
    }
}

/// Values this large would overflow the arithmetic of a scale, so they are cut to it.
const HUGE: f64 = 1e300;

fn tame(v: f64) -> f64 {
    v.clamp(-HUGE, HUGE)
}

fn fixed(v: Option<f64>) -> Option<f64> {
    v.filter(|x| x.is_finite()).map(tame)
}

/// The smallest and largest finite value, or `None` when there is none.
pub fn finite_range(values: impl IntoIterator<Item = f64>) -> Option<(f64, f64)> {
    let mut r: Option<(f64, f64)> = None;
    for v in values {
        if v.is_finite() {
            let v = tame(v);
            r = Some(match r {
                Some((lo, hi)) => (lo.min(v), hi.max(v)),
                None => (v, v),
            });
        }
    }
    r
}

/// A tick spacing of 1, 2 or 5 times a power of ten: the smallest one that covers `lo..hi`,
/// widened outward to its multiples, in at most `max_ticks` ticks. When none does (two ticks
/// around zero), the spacing with the fewest ticks.
pub fn nice_step(lo: f64, hi: f64, max_ticks: usize) -> f64 {
    let range = hi - lo;
    if !(range > 0.0) || !range.is_finite() {
        return 1.0;
    }
    let mag = 10f64.powi(range.log10().floor() as i32);
    if !(mag > 0.0) || !mag.is_finite() {
        return range;
    }
    let max = max_ticks.max(2) as f64;
    let mut fits: Option<f64> = None;
    let mut fewest = (f64::INFINITY, mag);
    for m in [10.0, 5.0, 2.0, 1.0, 0.5, 0.2, 0.1] {
        let s = m * mag;
        if !s.is_finite() {
            continue;
        }
        let count = (hi / s - 1e-9).ceil() - (lo / s + 1e-9).floor() + 1.0;
        if count <= max {
            fits = Some(s);
        }
        if count <= fewest.0 {
            fewest = (count, s);
        }
    }
    fits.unwrap_or(fewest.1)
}

/// The smallest 1-2-5 spacing with at most `max_ticks` of its multiples inside `lo..=hi`.
fn label_step(lo: f64, hi: f64, max_ticks: usize) -> f64 {
    let range = hi - lo;
    if !(range > 0.0) || !range.is_finite() {
        return 1.0;
    }
    let mag = 10f64.powi(range.log10().floor() as i32);
    if !(mag > 0.0) || !mag.is_finite() {
        return range;
    }
    let mut best = if (10.0 * mag).is_finite() { 10.0 * mag } else { mag };
    for m in [5.0, 2.0, 1.0, 0.5, 0.2, 0.1] {
        let s = m * mag;
        let inside = (hi / s + 1e-9).floor() - (lo / s - 1e-9).ceil() + 1.0;
        if inside > max_ticks.max(2) as f64 {
            break;
        }
        best = s;
    }
    best
}

/// The multiples of `step` within `lo..=hi`, ascending (at most 64).
pub fn ticks_in(lo: f64, hi: f64, step: f64) -> Vec<f64> {
    if !(step > 0.0) || !lo.is_finite() || !hi.is_finite() || hi < lo {
        return Vec::new();
    }
    let (a, b) = ((lo / step - 1e-9).ceil(), (hi / step + 1e-9).floor());
    if !a.is_finite() || !b.is_finite() {
        return Vec::new();
    }
    let n = (b - a + 1.0).clamp(0.0, 64.0) as usize;
    (0..n)
        .map(|i| {
            let v = (a + i as f64) * step;
            if v == 0.0 { 0.0 } else { v }
        })
        .collect()
}

/// Labels for ticks `step` apart: the decimals the step needs and no more, so they share a
/// width, with a `k`/`M`/`G`/`T` suffix once the step reaches a thousand.
pub fn tick_labels(values: &[f64], step: f64) -> Vec<String> {
    let m = step.abs();
    if !m.is_finite() || m == 0.0 || !(1e-6..1e15).contains(&m) {
        return values.iter().map(|v| if *v == 0.0 { "0".to_string() } else { format!("{v:.1e}") }).collect();
    }
    let (div, suffix) = if m >= 1e12 {
        (1e12, "T")
    } else if m >= 1e9 {
        (1e9, "G")
    } else if m >= 1e6 {
        (1e6, "M")
    } else if m >= 1e3 {
        (1e3, "k")
    } else {
        (1.0, "")
    };
    let dec = (-((m / div).log10() + 1e-9).floor()).clamp(0.0, 6.0) as usize;
    values
        .iter()
        .map(|v| {
            let x = v / div;
            if x == 0.0 {
                return format!("{:.dec$}", 0.0);
            }
            format!("{x:.dec$}{suffix}")
        })
        .collect()
}

/// A number in few cells: three significant digits, a `k`/`M`/`G`/`T` suffix from ten thousand
/// up, no trailing zeros, and `–` for a missing value.
pub fn fmt_num(v: f64) -> String {
    if !v.is_finite() {
        return "–".into();
    }
    let a = v.abs();
    if a == 0.0 {
        return "0".into();
    }
    if !(1e-3..1e15).contains(&a) {
        return format!("{v:.1e}");
    }
    let (div, suffix) = if a >= 1e12 {
        (1e12, "T")
    } else if a >= 1e9 {
        (1e9, "G")
    } else if a >= 1e6 {
        (1e6, "M")
    } else if a >= 1e4 {
        (1e3, "k")
    } else {
        (1.0, "")
    };
    let x = v / div;
    let dec = match x.abs() {
        m if m >= 100.0 => 0,
        m if m >= 10.0 => 1,
        m if m >= 1.0 => 2,
        _ => 3,
    };
    let mut s = format!("{x:.dec$}");
    if s.contains('.') {
        let keep = s.trim_end_matches('0').trim_end_matches('.').len();
        s.truncate(keep);
    }
    s.push_str(suffix);
    s
}

/// A resolved axis: its ends, its ticks and their labels.
struct Scale {
    lo: f64,
    hi: f64,
    ticks: Vec<f64>,
    labels: Vec<String>,
}

impl Scale {
    /// Fit `data` (plus `extra` values that must show, such as a threshold) inside `b`. An end
    /// the bounds leave open moves outward to a round tick; a fixed end stays where it is. A
    /// constant series gets a tenth of its value (at least 1) either side, not below zero when
    /// the value is not.
    ///
    /// Two spacings are at work. The open ends move to a multiple of a fine spacing (up to
    /// `fine` steps across), so little of the plot is spent on headroom; the labels sit on a
    /// coarser spacing with at most `max_ticks` of them inside the range. The top row of a plot
    /// is therefore often unlabeled.
    fn new(data: Option<(f64, f64)>, b: Bounds, extra: &[Option<f64>], max_ticks: usize, fine: usize) -> Scale {
        let (bmin, bmax) = (fixed(b.min), fixed(b.max));
        let (mut lo, mut hi) = data.unwrap_or((0.0, 1.0));
        for e in extra.iter().filter_map(|e| fixed(*e)) {
            lo = lo.min(e);
            hi = hi.max(e);
        }
        lo = bmin.unwrap_or(lo);
        hi = bmax.unwrap_or(hi);
        if !(hi > lo) {
            let pad = |c: f64| (c.abs() * 0.1).max(1.0);
            match (bmin, bmax) {
                (Some(_), _) => hi = lo + pad(lo),
                (None, Some(_)) => lo = hi - pad(hi),
                (None, None) => {
                    let c = lo;
                    hi = c + pad(c);
                    lo = if c >= 0.0 { (c - pad(c)).max(0.0) } else { c - pad(c) };
                }
            }
        }
        let step = nice_step(lo, hi, fine.max(max_ticks));
        if bmin.is_none() {
            lo = (lo / step + 1e-9).floor() * step;
        }
        if bmax.is_none() {
            hi = (hi / step - 1e-9).ceil() * step;
        }
        if !lo.is_finite() || !hi.is_finite() || !(hi - lo).is_finite() || !(hi > lo) {
            (lo, hi) = (0.0, 1.0);
        }
        let step = label_step(lo, hi, max_ticks);
        let ticks = ticks_in(lo, hi, step);
        let labels = tick_labels(&ticks, step);
        Scale { lo, hi, ticks, labels }
    }

    /// Where `v` sits between the ends: 0 at `lo`, 1 at `hi`, cut to that.
    fn frac(&self, v: f64) -> f64 {
        frac(v, self.lo, self.hi)
    }
}

/// Where `v` sits in `lo..hi`, cut to 0..1; the middle when the range is empty.
fn frac(v: f64, lo: f64, hi: f64) -> f64 {
    if !(hi > lo) {
        return 0.5;
    }
    let f = (tame(v) - lo) / (hi - lo);
    if f.is_finite() { f.clamp(0.0, 1.0) } else { 0.0 }
}

/// A range from bounds and data, each end fixed or taken from the data; (0, 0) with neither.
fn resolve(b: Bounds, data: Option<(f64, f64)>) -> (f64, f64) {
    let (dlo, dhi) = data.unwrap_or((0.0, 0.0));
    (fixed(b.min).unwrap_or(dlo), fixed(b.max).unwrap_or(dhi))
}

/// How many y labels suit `rows` rows: about one every other row, between 2 and 6.
fn y_ticks(rows: u16) -> usize {
    (rows as usize / 2 + 1).clamp(2, 6)
}

/// How many fine steps a y range of `rows` rows may span: about one a row.
fn y_fine(rows: u16) -> usize {
    (rows as usize + 1).clamp(2, 10)
}

/// The smallest of 1, 2, 5 times a power of ten that steps over `span` in under `target` ticks.
fn int_step(span: usize, target: usize) -> usize {
    let raw = span.div_ceil(target.max(2) - 1).max(1);
    let mut mag = 1usize;
    loop {
        for m in [1usize, 2, 5] {
            let s = m.saturating_mul(mag);
            if s >= raw {
                return s;
            }
        }
        mag = mag.saturating_mul(10);
    }
}

// ---------------------------------------------------------------------------------------------
// data reduction

/// How a bucket of several values becomes one cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Bucket {
    /// The newest value in the bucket.
    Last,
    /// The mean of the bucket.
    Mean,
    /// The largest value: a one-sample spike stays visible. The default.
    #[default]
    Max,
    /// The smallest value.
    Min,
}

/// Reduce `values` to at most `n`: a series no longer than `n` comes back as is; a longer one is
/// cut into `n` consecutive buckets of near-equal size (bucket `k` holds indices `k*len/n` up to
/// `(k+1)*len/n`), each reduced by `rule` over its finite values. A bucket with none is NaN.
pub fn resample(values: &[f64], n: usize, rule: Bucket) -> Vec<f64> {
    let len = values.len();
    if len <= n {
        return values.to_vec();
    }
    (0..n).map(|k| reduce(&values[bucket_start(k, len, n)..bucket_start(k + 1, len, n)], rule)).collect()
}

fn bucket_start(k: usize, len: usize, n: usize) -> usize {
    ((k as u128 * len as u128) / n.max(1) as u128) as usize
}

fn reduce(vs: &[f64], rule: Bucket) -> f64 {
    let mut it = vs.iter().copied().filter(|v| v.is_finite());
    let Some(first) = it.next() else { return f64::NAN };
    match rule {
        Bucket::Last => it.next_back().unwrap_or(first),
        Bucket::Max => it.fold(first, f64::max),
        Bucket::Min => it.fold(first, f64::min),
        Bucket::Mean => {
            // a running mean: a plain sum of huge values would overflow
            let (mut m, mut k) = (first, 1.0);
            for v in it {
                k += 1.0;
                m += (v - m) / k;
            }
            m
        }
    }
}

/// The mean of the finite values.
pub fn mean(values: &[f64]) -> Option<f64> {
    let m = reduce(values, Bucket::Mean);
    m.is_finite().then_some(m)
}

/// The `q` quantile (0..1) of the finite values, interpolated between neighbors; 0.5 is the median.
pub fn quantile(values: &[f64], q: f64) -> Option<f64> {
    let mut v: Vec<f64> = values.iter().copied().filter(|x| x.is_finite()).collect();
    v.sort_by(f64::total_cmp);
    sorted_quantile(&v, q)
}

fn sorted_quantile(v: &[f64], q: f64) -> Option<f64> {
    let last = v.len().checked_sub(1)?;
    let pos = (if q.is_finite() { q.clamp(0.0, 1.0) } else { 0.5 }) * last as f64;
    let i = (pos.floor() as usize).min(last);
    let j = (i + 1).min(last);
    Some(v[i] + (v[j] - v[i]) * (pos - i as f64))
}

/// A histogram's bins: equal widths over `lo..hi`.
#[derive(Clone, Debug, PartialEq)]
pub struct Bins {
    pub lo: f64,
    pub hi: f64,
    pub counts: Vec<u64>,
}

/// Count `samples` into `bins` equal-width bins over the data's range, or over `range` where it
/// fixes an end. Bin `i` holds `lo + i*w <= x < lo + (i+1)*w`, and the last bin also holds `hi`.
/// Values that are not finite, or outside a fixed range, are not counted. When every sample is
/// the same value the range is half a unit either side of it.
pub fn bin(samples: &[f64], bins: usize, range: Bounds) -> Bins {
    let bins = bins.clamp(1, 1 << 16);
    let (mut lo, mut hi) = resolve(range, finite_range(samples.iter().copied()));
    if !(hi > lo) {
        (lo, hi) = (lo - 0.5, lo + 0.5);
    }
    let mut counts = vec![0u64; bins];
    for x in samples.iter().copied().filter(|x| x.is_finite()).map(tame) {
        if x >= lo && x <= hi {
            let i = if hi > lo { (frac(x, lo, hi) * bins as f64) as usize } else { bins / 2 };
            counts[i.min(bins - 1)] += 1;
        }
    }
    Bins { lo, hi, counts }
}

/// Sturges' rule: `ceil(log2 n) + 1` bins for `n` samples.
pub fn sturges(n: usize) -> usize {
    if n <= 1 { 1 } else { (usize::BITS - (n - 1).leading_zeros()) as usize + 1 }
}

// ---------------------------------------------------------------------------------------------
// drawing helpers

fn cap(r: Rect) -> Rect {
    Rect::new(r.x, r.y, r.w.min(MAX_CELLS.0), r.h.min(MAX_CELLS.1))
}

/// Writes into one plot's rectangle: every write lands on the ground and inside the clip.
struct Pen<'g> {
    g: &'g mut Grid,
    clip: Rect,
    ground: Rgb,
}

impl<'g> Pen<'g> {
    /// Claim `r` (inside `clip`) and fill it with the ground.
    fn new(g: &'g mut Grid, r: Rect, ground: Rgb, clip: Rect) -> Pen<'g> {
        let clip = clip.intersect(r).intersect(g.area());
        g.fill(clip, Style::default().on(ground));
        Pen { g, clip, ground }
    }

    fn inside(&self, x: i64, y: i64) -> bool {
        x >= self.clip.x as i64 && x < self.clip.right() as i64 && y >= self.clip.y as i64 && y < self.clip.bottom() as i64
    }

    fn ch(&mut self, x: i64, y: i64, c: char, fg: Rgb) {
        if self.inside(x, y) {
            let mut b = [0u8; 4];
            self.g.put_clipped(x as u16, y as u16, c.encode_utf8(&mut b), Style::fg(fg).on(self.ground), self.clip);
        }
    }

    /// Text starting at (x, y); the part outside the clip is dropped.
    fn text(&mut self, x: i64, y: i64, s: &str, st: Style) {
        if x >= 0 && x < self.clip.right() as i64 && y >= self.clip.y as i64 && y < self.clip.bottom() as i64 {
            self.g.put_clipped(x as u16, y as u16, s, st.on(self.ground), self.clip);
        }
    }

    fn pieces(&mut self, x: i64, y: i64, ps: &[Piece]) {
        let mut x = x;
        for (s, st) in ps {
            self.text(x, y, s, *st);
            x += width(s) as i64;
        }
    }
}

/// The one line a plot with nothing to show writes, centered.
fn no_data(pen: &mut Pen, r: Rect, st: &PlotStyle) {
    let msg = truncate("no data", r.w as usize);
    let x = r.x as i64 + (r.w as i64 - width(&msg) as i64) / 2;
    pen.text(x, r.y as i64 + (r.h as i64 - 1) / 2, &msg, Style::fg(st.label));
}

/// Add one cell to a run of pieces, extending the last piece when the style is the same.
fn push_cell(out: &mut Vec<Piece>, c: char, st: Style) {
    match out.last_mut() {
        Some((s, last)) if *last == st => s.push(c),
        _ => out.push((c.to_string(), st)),
    }
}

/// The y labels and spine left of a plot body: one label a row at most, right-aligned against a
/// `┤`, with `│` on the rows between.
struct Gutter {
    labels: Vec<String>,
    thr_row: Option<usize>,
    w: u16,
}

impl Gutter {
    /// Labels for `scale`'s ticks on the rows `row_of` gives; the top and bottom ticks claim
    /// their rows first, and a threshold's label replaces a tick's on its row.
    fn new(scale: &Scale, rows: u16, row_of: &dyn Fn(f64) -> usize, thr: Option<(usize, String)>) -> Gutter {
        let mut labels = vec![String::new(); rows as usize];
        let n = scale.ticks.len();
        if rows > 0 {
            for i in (n.saturating_sub(1)..n).chain(0..n.saturating_sub(1)) {
                let row = row_of(scale.ticks[i]).min(rows as usize - 1);
                if labels[row].is_empty() {
                    labels[row].clone_from(&scale.labels[i]);
                }
            }
        }
        let mut thr_row = None;
        if let Some((row, text)) = thr {
            if row < labels.len() {
                labels[row] = text;
                thr_row = Some(row);
            }
        }
        let w = labels.iter().map(|l| width(l)).max().unwrap_or(0).min(u16::MAX as usize - 1) as u16 + 1;
        Gutter { labels, thr_row, w }
    }

    /// The gutter's width when the plot keeps at least four cells of body, else 0 (no gutter).
    fn fit(&self, on: bool, total: u16) -> u16 {
        if on && self.w.saturating_add(4) <= total { self.w } else { 0 }
    }

    fn draw(&self, pen: &mut Pen, x: i64, y: i64, gw: u16, st: &PlotStyle) {
        if gw == 0 {
            return;
        }
        let spine = x + gw as i64 - 1;
        for (i, l) in self.labels.iter().enumerate() {
            let lw = width(l) as i64;
            let shown = !l.is_empty() && lw < gw as i64;
            if shown {
                let color = if self.thr_row == Some(i) { st.threshold } else { st.label };
                pen.text(spine - lw, y + i as i64, l, Style::fg(color));
            }
            pen.ch(spine, y + i as i64, if shown { '┤' } else { '│' }, st.axis);
        }
    }
}

/// Places labels on one row so no two touch: a label is centered on its anchor, pulled inside
/// the row, and skipped when it would come within a cell of one already placed.
struct LabelRow {
    w: i64,
    taken: Vec<(i64, i64)>,
}

impl LabelRow {
    fn new(w: i64) -> LabelRow {
        LabelRow { w, taken: Vec::new() }
    }

    /// The start column for a label `lw` wide whose cell `offset` sits on column `anchor`.
    fn place(&mut self, anchor: i64, lw: i64, offset: i64) -> Option<i64> {
        if lw <= 0 || lw > self.w {
            return None;
        }
        let start = (anchor - offset).clamp(0, self.w - lw);
        let end = start + lw;
        if self.taken.iter().any(|&(a, b)| start < b + 1 && end + 1 > a) {
            return None;
        }
        self.taken.push((start, end));
        Some(start)
    }
}

/// An x axis under a plot body: a rule with a `┬` over each label that found room, the labels
/// on the row below.
fn x_axis(pen: &mut Pen, bx: i64, y: i64, corner: bool, body_w: i64, ticks: &[(i64, Cow<str>)], st: &PlotStyle) {
    if corner {
        pen.ch(bx - 1, y, '└', st.axis);
    }
    for c in 0..body_w {
        pen.ch(bx + c, y, '─', st.axis);
    }
    let mut row = LabelRow::new(body_w);
    for (col, label) in ticks {
        let label = truncate(label, body_w.max(0) as usize);
        let lw = width(&label) as i64;
        if let Some(start) = row.place(*col, lw, lw / 2) {
            pen.ch(bx + (*col).clamp(0, body_w - 1), y, '┬', st.axis);
            pen.text(bx + start, y + 1, &label, Style::fg(st.label));
        }
    }
}

// ---------------------------------------------------------------------------------------------
// sparkline

/// A one-row trend in eighth blocks: `▁▂▃▅▇█`, oldest left, newest right.
///
/// - Width: one cell a value. A series shorter than the width sits at the right edge behind
///   blanks, so the newest value is always the last cell; a longer one is resampled to the
///   width by [`Bucket`] (the bucket's max by default, so a spike survives).
/// - Height: the lowest value draws `▁` and the highest `█`, over the drawn values' own range
///   unless [`Bounds`] fix an end. A constant series draws `▄` (`▁` when the constant is zero).
/// - A missing value draws [`PlotStyle::missing_glyph`]; a value over the threshold takes
///   [`PlotStyle::over`].
#[derive(Clone, Debug, PartialEq)]
pub struct Sparkline<'a> {
    pub values: &'a [f64],
    pub bounds: Bounds,
    pub bucket: Bucket,
    pub color: Option<Rgb>,
    pub threshold: Option<f64>,
}

impl<'a> Sparkline<'a> {
    /// A sparkline of `values` with the defaults: its own range, max buckets, the first series color.
    pub fn new(values: &'a [f64]) -> Sparkline<'a> {
        Sparkline { values, bounds: Bounds::default(), bucket: Bucket::default(), color: None, threshold: None }
    }

    /// Fix either end of the range.
    pub fn bounds(mut self, b: Bounds) -> Self {
        self.bounds = b;
        self
    }

    /// How a longer series is resampled.
    pub fn bucket(mut self, b: Bucket) -> Self {
        self.bucket = b;
        self
    }

    /// The color of values at or under the threshold.
    pub fn color(mut self, c: Rgb) -> Self {
        self.color = Some(c);
        self
    }

    /// Values above `t` take the "over" color.
    pub fn threshold(mut self, t: f64) -> Self {
        self.threshold = Some(t);
        self
    }

    /// The strip as pieces exactly `width` cells wide.
    pub fn pieces(&self, width: u16, st: &PlotStyle) -> Vec<Piece> {
        let w = width as usize;
        let vals = resample(self.values, w, self.bucket);
        let mut out: Vec<Piece> = Vec::new();
        if vals.len() < w {
            out.push((" ".repeat(w - vals.len()), Style::default()));
        }
        let (lo, hi) = resolve(self.bounds, finite_range(vals.iter().copied()));
        let base = Style::fg(self.color.unwrap_or(st.series_color(0)));
        let (over, missing) = (Style::fg(st.over), Style::fg(st.missing));
        let gap = one_cell(st.missing_glyph, '·');
        for &v in &vals {
            if !v.is_finite() {
                push_cell(&mut out, gap, missing);
                continue;
            }
            let level = if hi > lo {
                1 + (frac(v, lo, hi) * 7.0).round() as usize
            } else if v == 0.0 {
                1
            } else {
                4
            };
            let is_over = self.threshold.is_some_and(|t| v > t);
            push_cell(&mut out, EIGHTHS_UP[level.min(8)], if is_over { over } else { base });
        }
        out
    }

    /// Draw the strip at (x, y), `width` cells wide, on `ground`.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(&self, g: &mut Grid, x: u16, y: u16, width: u16, st: &PlotStyle, ground: Rgb, clip: Rect) {
        let mut pen = Pen::new(g, Rect::new(x, y, width, 1), ground, clip);
        pen.pieces(x as i64, y as i64, &self.pieces(width, st));
    }
}

// ---------------------------------------------------------------------------------------------
// line / area plot

/// One line of a plot: a label for the legend, the values (NaN is a gap), an optional color.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Series<'a> {
    pub label: &'a str,
    pub values: &'a [f64],
    pub color: Option<Rgb>,
}

impl<'a> Series<'a> {
    /// A series in the style's next color.
    pub fn new(label: &'a str, values: &'a [f64]) -> Series<'a> {
        Series { label, values, color: None }
    }

    /// Its own color.
    pub fn color(mut self, c: Rgb) -> Self {
        self.color = Some(c);
        self
    }
}

/// What the x axis shows. Any of them takes two rows: a rule with tick marks, then the labels.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum XAxis<'a> {
    /// No x axis.
    #[default]
    None,
    /// A few round indices: `0`, `20`, `40`.
    Index,
    /// One label at each end: `"-24h"` and `"now"`.
    Ends(&'a str, &'a str),
    /// A label for each point; a few round indices' labels are shown, as many as fit.
    Labels(&'a [&'a str]),
}

/// A line or area plot in braille over a rectangle.
///
/// - x: point `i` of every series sits at the same place, the first at the left edge and the
///   last index of the longest series at the right; a shorter series ends early.
/// - y: the range covers the data and the threshold, widened to round ticks; [`Bounds`] fix
///   either end, and a value outside a fixed range rides the edge. A filled plot of
///   non-negative data starts at zero. Labels sit on round values inside the range, about one
///   every other row.
/// - Gaps: a missing value breaks the line; a point between two gaps is a single dot.
/// - Layout, top to bottom: the body with y labels on its left, the x axis (two rows), the
///   legend (one row, with two or more labeled series). The legend needs five rows and the x
///   axis four; one row draws the first series as a [`Sparkline`].
/// - [`Marks::Blocks`] draws the first series as eighth-block columns instead, from zero when
///   the data is non-negative, with the columns over the threshold in the "over" color.
#[derive(Clone, Debug, PartialEq)]
pub struct LinePlot<'a> {
    pub series: &'a [Series<'a>],
    pub y: Bounds,
    pub x: XAxis<'a>,
    pub threshold: Option<f64>,
    pub fill: bool,
    pub y_labels: bool,
    pub legend: bool,
}

impl<'a> LinePlot<'a> {
    /// A plot of `series` with y labels, a legend when it applies, no x axis and no fill.
    pub fn new(series: &'a [Series<'a>]) -> LinePlot<'a> {
        LinePlot { series, y: Bounds::default(), x: XAxis::None, threshold: None, fill: false, y_labels: true, legend: true }
    }

    /// Fix either end of the y range.
    pub fn y(mut self, b: Bounds) -> Self {
        self.y = b;
        self
    }

    /// Show an x axis.
    pub fn x(mut self, x: XAxis<'a>) -> Self {
        self.x = x;
        self
    }

    /// A horizontal reference line at `t`, labeled on the y axis.
    pub fn threshold(mut self, t: f64) -> Self {
        self.threshold = Some(t);
        self
    }

    /// Fill under each line: an area plot.
    pub fn fill(mut self, on: bool) -> Self {
        self.fill = on;
        self
    }

    /// Show or hide the y labels.
    pub fn y_labels(mut self, on: bool) -> Self {
        self.y_labels = on;
        self
    }

    /// Show or hide the legend.
    pub fn legend(mut self, on: bool) -> Self {
        self.legend = on;
        self
    }

    fn color(&self, i: usize, st: &PlotStyle) -> Rgb {
        self.series[i].color.unwrap_or(st.series_color(i))
    }

    /// Draw into `r` on `ground`.
    pub fn draw(&self, g: &mut Grid, r: Rect, st: &PlotStyle, ground: Rgb, clip: Rect) {
        let r = cap(r);
        let mut pen = Pen::new(g, r, ground, clip);
        if r.is_empty() {
            return;
        }
        let Some(range) = finite_range(self.series.iter().flat_map(|s| s.values.iter().copied())) else {
            no_data(&mut pen, r, st);
            return;
        };
        let n = self.series.iter().map(|s| s.values.len()).max().unwrap_or(0);
        let first = self.series.iter().position(|s| s.values.iter().any(|v| v.is_finite())).unwrap_or(0);
        if r.h == 1 {
            let spark = Sparkline { values: self.series[first].values, bounds: self.y, bucket: Bucket::Max, color: Some(self.color(first, st)), threshold: self.threshold };
            pen.pieces(r.x as i64, r.y as i64, &spark.pieces(r.w, st));
            return;
        }

        let blocks = st.marks == Marks::Blocks;
        let labeled = self.legend && !blocks && self.series.len() > 1 && self.series.iter().any(|s| !s.label.is_empty());
        let legend_h = (labeled && r.h >= 5) as u16;
        let x_h = if self.x != XAxis::None && r.h - legend_h >= 4 { 2 } else { 0 };
        let body_h = r.h - legend_h - x_h;
        let zero = ((self.fill || blocks) && range.0 >= 0.0).then_some(0.0);
        let scale = Scale::new(Some(range), self.y, &[self.threshold, zero], y_ticks(body_h), y_fine(body_h));
        let dh = body_h as i64 * 4;
        let ydot = |v: f64| ((1.0 - scale.frac(v)) * (dh - 1) as f64).round() as i64;
        let row_of = |v: f64| (ydot(v) / 4) as usize;
        let thr = self.threshold.filter(|t| t.is_finite() && *t >= scale.lo && *t <= scale.hi);
        let gut = Gutter::new(&scale, body_h, &row_of, thr.map(|t| (row_of(t), fmt_num(t))));
        let gw = gut.fit(self.y_labels, r.w);
        let body_w = r.w - gw;
        let (bx, by) = (r.x as i64 + gw as i64, r.y as i64);
        let dw = body_w as i64 * 2;
        let xdot = |i: usize| if n <= 1 { 0 } else { (i as f64 * (dw - 1) as f64 / (n - 1) as f64).round() as i64 };

        if let Some(t) = thr {
            let glyph = one_cell(st.threshold_glyph, '╌');
            for c in 0..body_w as i64 {
                pen.ch(bx + c, by + row_of(t) as i64, glyph, st.threshold);
            }
        }
        let Ok(bx16) = u16::try_from(bx) else { return };
        if blocks {
            let s = &self.series[first];
            let mut canvas = Blocks::new(body_w, body_h);
            for c in 0..body_w as usize {
                let v = column_value(s.values, n, body_w as usize, c);
                if v.is_finite() {
                    let e = ((scale.frac(v) * canvas.levels() as f64).round() as i32).max(1);
                    let color = if self.threshold.is_some_and(|t| v > t) { st.over } else { self.color(first, st) };
                    canvas.set(c as i32, e, color);
                }
            }
            canvas.draw(pen.g, bx16, r.y, ground, pen.clip);
        } else {
            let mut canvas = Braille::new(body_w, body_h);
            for (si, s) in self.series.iter().enumerate() {
                let color = self.color(si, st);
                let mut prev: Option<(i64, i64)> = None;
                for (i, &v) in s.values.iter().enumerate() {
                    if !v.is_finite() {
                        prev = None;
                        continue;
                    }
                    let p = (xdot(i), ydot(v));
                    match prev {
                        Some(q) => {
                            canvas.line(q.0 as i32, q.1 as i32, p.0 as i32, p.1 as i32, color);
                            if self.fill {
                                for x in q.0..=p.0 {
                                    let y = if p.0 == q.0 { q.1.min(p.1) } else { q.1 + ((p.1 - q.1) as f64 * (x - q.0) as f64 / (p.0 - q.0) as f64).round() as i64 };
                                    canvas.vline(x as i32, y as i32, dh as i32 - 1, color);
                                }
                            }
                        }
                        None => {
                            canvas.set(p.0 as i32, p.1 as i32, color);
                            if self.fill {
                                canvas.vline(p.0 as i32, p.1 as i32, dh as i32 - 1, color);
                            }
                        }
                    }
                    prev = Some(p);
                }
            }
            canvas.draw(pen.g, bx16, r.y, ground, pen.clip);
        }
        gut.draw(&mut pen, r.x as i64, by, gw, st);

        if x_h > 0 {
            let col = |i: usize| (xdot(i) / 2).clamp(0, body_w as i64 - 1);
            let mut ticks: Vec<(i64, Cow<str>)> = Vec::new();
            match self.x {
                XAxis::None => {}
                XAxis::Ends(a, b) => {
                    ticks.push((0, Cow::Borrowed(a)));
                    ticks.push((body_w as i64 - 1, Cow::Borrowed(b)));
                }
                XAxis::Index | XAxis::Labels(_) => {
                    let labels = if let XAxis::Labels(ls) = self.x { Some(ls) } else { None };
                    let count = labels.map_or(n, |ls| ls.len().min(n));
                    if count > 0 {
                        let widest = match labels {
                            Some(ls) => ls[..count].iter().map(|l| width(l)).max().unwrap_or(1),
                            None => (count - 1).to_string().len(),
                        };
                        let target = (body_w as usize / (widest + 2)).clamp(2, 6);
                        for i in (0..count).step_by(int_step(count - 1, target)) {
                            ticks.push((col(i), labels.map_or_else(|| Cow::Owned(i.to_string()), |ls| Cow::Borrowed(ls[i]))));
                        }
                    }
                }
            }
            x_axis(&mut pen, bx, by + body_h as i64, gw > 0, body_w as i64, &ticks, st);
        }
        if legend_h > 0 {
            let y = r.y as i64 + r.h as i64 - 1;
            let mut x = bx;
            let glyph = one_cell(st.legend_glyph, '●');
            for (si, s) in self.series.iter().enumerate().filter(|(_, s)| !s.label.is_empty()) {
                pen.ch(x, y, glyph, self.color(si, st));
                pen.text(x + 2, y, s.label, Style::fg(st.label));
                x += 2 + width(s.label) as i64 + 2;
            }
        }
    }
}

/// The value column `c` of `bw` shows for a series laid over `n` points: the mean of its bucket
/// when there are more points than columns, else the nearest point.
fn column_value(values: &[f64], n: usize, bw: usize, c: usize) -> f64 {
    if n == 0 || bw == 0 {
        return f64::NAN;
    }
    if n >= bw {
        let (a, b) = (bucket_start(c, n, bw).min(values.len()), bucket_start(c + 1, n, bw).min(values.len()));
        return reduce(&values[a..b], Bucket::Mean);
    }
    let i = if bw <= 1 { 0 } else { (c as f64 * (n - 1) as f64 / (bw - 1) as f64).round() as usize };
    values.get(i).copied().unwrap_or(f64::NAN)
}

// ---------------------------------------------------------------------------------------------
// bars

/// One bar: its label, its value (negative or missing draws no bar), an optional color.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bar<'a> {
    pub label: &'a str,
    pub value: f64,
    pub color: Option<Rgb>,
}

impl<'a> Bar<'a> {
    /// A bar in the style's first series color.
    pub fn new(label: &'a str, value: f64) -> Bar<'a> {
        Bar { label, value, color: None }
    }

    /// Its own color.
    pub fn color(mut self, c: Rgb) -> Self {
        self.color = Some(c);
        self
    }
}

/// A bar `width` cells wide filled to `value / max` in eighths of a cell: `█████▍    `. A
/// positive value shows at least `▏`.
pub fn bar_pieces(value: f64, max: f64, width: u16, color: Rgb) -> Vec<Piece> {
    let w = width as usize;
    let mut e = (frac(value, 0.0, max) * (w * 8) as f64).round() as usize;
    if value.is_finite() && value > 0.0 && max > 0.0 {
        e = e.max(1);
    } else {
        e = 0;
    }
    let e = e.min(w * 8);
    let mut s = String::with_capacity(w * 3);
    for c in 0..w {
        s.push(bar_glyph(e, c));
    }
    vec![(s, Style::fg(color))]
}

/// Horizontal bars, one row each: label left, bar in eighths, value right.
///
/// `checkout ███████████▌    184`. The bars share one scale from zero to the largest value (or
/// `max`). The labels take their widest label's width; when the row is too narrow for a
/// three-cell bar the labels shorten to four cells, then the values go, then the labels.
#[derive(Clone, Debug, PartialEq)]
pub struct BarChart<'a> {
    pub bars: &'a [Bar<'a>],
    pub max: Option<f64>,
    pub values: bool,
    pub threshold: Option<f64>,
}

impl<'a> BarChart<'a> {
    /// A chart of `bars` with values shown.
    pub fn new(bars: &'a [Bar<'a>]) -> BarChart<'a> {
        BarChart { bars, max: None, values: true, threshold: None }
    }

    /// The value a full-width bar stands for.
    pub fn max(mut self, m: f64) -> Self {
        self.max = Some(m);
        self
    }

    /// Show or hide the values at the right.
    pub fn values(mut self, on: bool) -> Self {
        self.values = on;
        self
    }

    /// Bars above `t` take the "over" color.
    pub fn threshold(mut self, t: f64) -> Self {
        self.threshold = Some(t);
        self
    }

    /// Every bar as a row of pieces exactly `width` cells wide.
    pub fn rows(&self, width: u16, st: &PlotStyle) -> Vec<Vec<Piece>> {
        self.rows_n(width, st, usize::MAX)
    }

    fn rows_n(&self, width: u16, st: &PlotStyle, limit: usize) -> Vec<Vec<Piece>> {
        let w = width as usize;
        let bars = &self.bars[..self.bars.len().min(limit)];
        let nums: Vec<String> = if self.values { bars.iter().map(|b| fmt_num(b.value)).collect() } else { Vec::new() };
        let mut lw = bars.iter().map(|b| crate::text::width(b.label)).max().unwrap_or(0);
        let mut vw = nums.iter().map(|s| crate::text::width(s)).max().unwrap_or(0);
        let fixed = |lw: usize, vw: usize| lw + (lw > 0) as usize + vw + (vw > 0) as usize;
        const MIN_BAR: usize = 3;
        if fixed(lw, vw) + MIN_BAR > w {
            lw = lw.saturating_sub(fixed(lw, vw) + MIN_BAR - w).max(lw.min(4));
        }
        if fixed(lw, vw) + MIN_BAR > w {
            vw = 0;
        }
        if fixed(lw, vw) + MIN_BAR > w {
            lw = 0;
        }
        let bw = w - fixed(lw, vw).min(w);
        let max = fixed_max(self.max, self.bars.iter().map(|b| b.value));
        bars.iter()
            .enumerate()
            .map(|(i, b)| {
                let mut row: Vec<Piece> = Vec::with_capacity(3);
                if lw > 0 {
                    let mut l = truncate(b.label, lw).into_owned();
                    let pad = lw + 1 - crate::text::width(&l).min(lw);
                    l.extend(std::iter::repeat_n(' ', pad));
                    row.push((l, Style::fg(st.text)));
                }
                let over = self.threshold.is_some_and(|t| b.value > t);
                let color = if over { st.over } else { b.color.unwrap_or(st.series_color(0)) };
                row.extend(bar_pieces(b.value, max, bw as u16, color));
                if vw > 0 {
                    let pad = vw + 1 - crate::text::width(&nums[i]).min(vw);
                    row.push((format!("{}{}", " ".repeat(pad), nums[i]), Style::fg(if over { st.over } else { st.label })));
                }
                row
            })
            .collect()
    }

    /// Draw one bar a row into `r`, as many as fit; returns the rows used.
    pub fn draw(&self, g: &mut Grid, r: Rect, st: &PlotStyle, ground: Rgb, clip: Rect) -> u16 {
        let r = cap(r);
        let mut pen = Pen::new(g, r, ground, clip);
        if r.is_empty() {
            return 0;
        }
        if self.bars.is_empty() {
            no_data(&mut pen, Rect::new(r.x, r.y, r.w, 1), st);
            return 1;
        }
        let rows = self.rows_n(r.w, st, r.h as usize);
        for (i, row) in rows.iter().enumerate() {
            pen.pieces(r.x as i64, r.y as i64 + i as i64, row);
        }
        rows.len() as u16
    }
}

/// The top of a zero-based scale: `max` when it is a positive number, else the largest finite
/// value, else 1.
fn fixed_max(max: Option<f64>, values: impl Iterator<Item = f64>) -> f64 {
    fixed(max).filter(|m| *m > 0.0).or(finite_range(values).map(|r| r.1).filter(|m| *m > 0.0)).unwrap_or(1.0)
}

// ---------------------------------------------------------------------------------------------
// columns and histogram

/// The shared part of a column chart: its y scale and labels for a body `rows` high.
struct ColPlan {
    scale: Scale,
    gut: Gutter,
    rows: u16,
}

impl ColPlan {
    fn new(top: f64, max: Option<f64>, thr: Option<f64>, rows: u16) -> ColPlan {
        let scale = Scale::new(Some((0.0, top.max(0.0))), Bounds { min: Some(0.0), max }, &[thr], y_ticks(rows), y_fine(rows));
        // a tick labels the row its value falls in (the row it tops when it sits on a row's edge);
        // zero labels the bottom row
        let row_of = |v: f64| (rows as f64 - (scale.frac(v) * rows as f64 - 1e-9).ceil()).clamp(0.0, rows.saturating_sub(1) as f64) as usize;
        let gut = Gutter::new(&scale, rows, &row_of, None);
        ColPlan { scale, gut, rows }
    }

    /// A column's height in eighths: a positive value shows at least one.
    fn eighths(&self, v: f64) -> i32 {
        if !v.is_finite() || v <= 0.0 {
            return 0;
        }
        ((self.scale.frac(v) * self.rows as f64 * 8.0).round() as i32).max(1)
    }

    /// Draw the gutter, the threshold line and the columns: column `k` is `bw` cells wide,
    /// `gap` cells from the next.
    #[allow(clippy::too_many_arguments)]
    fn draw(&self, pen: &mut Pen, r: Rect, gw: u16, vals: &[f64], color: &dyn Fn(usize, f64) -> Rgb, bw: usize, gap: usize, thr: Option<f64>, st: &PlotStyle) {
        let body_w = r.w - gw;
        let bx = r.x as i64 + gw as i64;
        if let Some(t) = thr.filter(|t| t.is_finite() && *t > 0.0 && *t <= self.scale.hi) {
            let row = self.rows as i64 - 1 - ((self.eighths(t) as i64 - 1) / 8).clamp(0, self.rows as i64 - 1);
            let glyph = one_cell(st.threshold_glyph, '╌');
            for c in 0..body_w as i64 {
                pen.ch(bx + c, r.y as i64 + row, glyph, st.threshold);
            }
        }
        let mut canvas = Blocks::new(body_w, self.rows);
        for (k, &v) in vals.iter().enumerate() {
            let x0 = k.saturating_mul(bw + gap);
            if x0 >= body_w as usize {
                break;
            }
            canvas.fill(x0 as i32, (x0 + bw - 1) as i32, self.eighths(v), color(k, v));
        }
        if let Ok(bx16) = u16::try_from(bx) {
            canvas.draw(pen.g, bx16, r.y, pen.ground, pen.clip);
        }
        self.gut.draw(pen, r.x as i64, r.y as i64, gw, st);
    }
}

/// Vertical bars with eighth-block tops: counts per day, durations per run.
///
/// - Scale: zero to the largest value (or `max`), widened to a round tick; y labels on the left.
/// - Width: `bar_width` cells a column and `gap` between (0 picks the widest that fits, up to
///   8). When the columns do not fit, they narrow to one cell, then lose the gap, and then
///   only the newest (rightmost) are shown.
/// - Labels: one row under the columns, each centered under its column; a label that would
///   touch its neighbor is skipped.
/// - A positive value shows at least `▁`; zero, negative and missing values show nothing.
/// - One row high, it becomes a [`Sparkline`].
#[derive(Clone, Debug, PartialEq)]
pub struct ColumnChart<'a> {
    pub bars: &'a [Bar<'a>],
    pub max: Option<f64>,
    pub bar_width: u16,
    pub gap: u16,
    pub y_labels: bool,
    pub x_labels: bool,
    pub threshold: Option<f64>,
}

impl<'a> ColumnChart<'a> {
    /// A chart of `bars`: automatic width, one cell between, both label sets on.
    pub fn new(bars: &'a [Bar<'a>]) -> ColumnChart<'a> {
        ColumnChart { bars, max: None, bar_width: 0, gap: 1, y_labels: true, x_labels: true, threshold: None }
    }

    /// The value the top of the plot stands for.
    pub fn max(mut self, m: f64) -> Self {
        self.max = Some(m);
        self
    }

    /// Cells a column and cells between columns; a width of 0 picks one.
    pub fn widths(mut self, bar_width: u16, gap: u16) -> Self {
        self.bar_width = bar_width;
        self.gap = gap;
        self
    }

    /// Show or hide the y labels and the labels under the columns.
    pub fn labels(mut self, y: bool, x: bool) -> Self {
        self.y_labels = y;
        self.x_labels = x;
        self
    }

    /// A reference line at `t`; columns above it take the "over" color.
    pub fn threshold(mut self, t: f64) -> Self {
        self.threshold = Some(t);
        self
    }

    /// Draw into `r` on `ground`.
    pub fn draw(&self, g: &mut Grid, r: Rect, st: &PlotStyle, ground: Rgb, clip: Rect) {
        let r = cap(r);
        let mut pen = Pen::new(g, r, ground, clip);
        if r.is_empty() {
            return;
        }
        let vals: Vec<f64> = self.bars.iter().map(|b| b.value).collect();
        let Some(range) = finite_range(vals.iter().copied()) else {
            no_data(&mut pen, r, st);
            return;
        };
        if r.h == 1 {
            let spark = Sparkline { values: &vals, bounds: Bounds { min: Some(0.0), max: self.max }, bucket: Bucket::Max, color: None, threshold: self.threshold };
            pen.pieces(r.x as i64, r.y as i64, &spark.pieces(r.w, st));
            return;
        }
        let x_h = (self.x_labels && r.h >= 3 && self.bars.iter().any(|b| !b.label.is_empty())) as u16;
        let body = Rect::new(r.x, r.y, r.w, r.h - x_h);
        let plan = ColPlan::new(range.1, self.max, self.threshold, body.h);
        let gw = plan.gut.fit(self.y_labels, r.w);
        let body_w = (r.w - gw) as usize;
        let n = vals.len();
        let mut gap = self.gap as usize;
        let mut bw = if self.bar_width > 0 { self.bar_width as usize } else { ((body_w + gap) / n).saturating_sub(gap).clamp(1, 8) };
        let span = |bw: usize, gap: usize| n.saturating_mul(bw.saturating_add(gap)).saturating_sub(gap);
        if span(bw, gap) > body_w {
            bw = 1;
        }
        if span(bw, gap) > body_w {
            gap = 0;
        }
        let shown = n.min((body_w + gap) / (bw + gap));
        let first = n - shown;
        let color = |k: usize, v: f64| if self.threshold.is_some_and(|t| v > t) { st.over } else { self.bars[first + k].color.unwrap_or(st.series_color(0)) };
        plan.draw(&mut pen, body, gw, &vals[first..], &color, bw, gap, self.threshold, st);
        if x_h > 0 {
            let mut row = LabelRow::new(body_w as i64);
            let (bx, y) = (r.x as i64 + gw as i64, r.y as i64 + body.h as i64);
            for (k, b) in self.bars[first..].iter().enumerate() {
                let label = truncate(b.label, body_w);
                let lw = width(&label) as i64;
                // centered under the column, leaning left when the widths differ in parity
                let center = (k * (bw + gap)) as i64 + (bw as i64 - 1) / 2;
                if let Some(start) = row.place(center, lw, (lw - 1) / 2) {
                    pen.text(bx + start, y, &label, Style::fg(st.label));
                }
            }
        }
    }
}

/// What a histogram marks under its columns.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Marker {
    #[default]
    None,
    Mean,
    Median,
}

/// A histogram of raw samples, drawn as touching columns.
///
/// - Bins: equal widths over the samples' range (or `range`), by the rule on [`bin`]. With
///   `bins` 0 the count starts from Sturges' rule and grows to fill the width with whole
///   columns; a given count is cut to one column a cell.
/// - Under the columns, one row: the range's ends at the left and right, and the marker (the
///   mean or the median) as [`PlotStyle::marker_glyph`] under its column with its value.
/// - One row high, it becomes a [`Sparkline`] of the counts.
#[derive(Clone, Debug, PartialEq)]
pub struct Histogram<'a> {
    pub samples: &'a [f64],
    pub bins: usize,
    pub range: Bounds,
    pub marker: Marker,
    pub y_labels: bool,
    pub x_labels: bool,
    pub color: Option<Rgb>,
}

impl<'a> Histogram<'a> {
    /// A histogram of `samples` with automatic bins, both label sets and no marker.
    pub fn new(samples: &'a [f64]) -> Histogram<'a> {
        Histogram { samples, bins: 0, range: Bounds::default(), marker: Marker::None, y_labels: true, x_labels: true, color: None }
    }

    /// A fixed number of bins.
    pub fn bins(mut self, n: usize) -> Self {
        self.bins = n;
        self
    }

    /// Fix either end of the sample range.
    pub fn range(mut self, b: Bounds) -> Self {
        self.range = b;
        self
    }

    /// Mark the mean or the median.
    pub fn marker(mut self, m: Marker) -> Self {
        self.marker = m;
        self
    }

    /// Show or hide the count labels and the range row.
    pub fn labels(mut self, y: bool, x: bool) -> Self {
        self.y_labels = y;
        self.x_labels = x;
        self
    }

    /// The columns' color.
    pub fn color(mut self, c: Rgb) -> Self {
        self.color = Some(c);
        self
    }

    /// Bins and cells a bin for a body `body_w` cells wide.
    fn shape(&self, finite: usize, body_w: usize) -> (usize, usize) {
        let body_w = body_w.max(1);
        if self.bins > 0 {
            let bins = self.bins.min(body_w);
            return (bins, body_w / bins);
        }
        let bar_w = (body_w / sturges(finite).max(1)).max(1);
        (body_w / bar_w, bar_w)
    }

    /// Draw into `r` on `ground`.
    pub fn draw(&self, g: &mut Grid, r: Rect, st: &PlotStyle, ground: Rgb, clip: Rect) {
        let r = cap(r);
        let mut pen = Pen::new(g, r, ground, clip);
        if r.is_empty() {
            return;
        }
        let finite = self.samples.iter().filter(|v| v.is_finite()).count();
        if finite == 0 {
            no_data(&mut pen, r, st);
            return;
        }
        let color = self.color.unwrap_or(st.series_color(0));
        if r.h == 1 {
            let (bins, bar_w) = self.shape(finite, r.w as usize);
            let counts: Vec<f64> = bin(self.samples, bins, self.range).counts.iter().flat_map(|c| std::iter::repeat_n(*c as f64, bar_w)).collect();
            let spark = Sparkline::new(&counts).bounds(Bounds::from(0.0)).color(color);
            pen.pieces(r.x as i64, r.y as i64, &spark.pieces(r.w, st));
            return;
        }
        let x_h = (self.x_labels && r.h >= 3) as u16;
        let body = Rect::new(r.x, r.y, r.w, r.h - x_h);
        // the gutter's width depends on the largest count, which depends on the bins, which
        // depend on the width left of the gutter: settle it in a few rounds, the gutter only growing
        let mut gw = 0u16;
        let (mut b, mut plan, mut bar_w);
        let mut rounds = 0;
        loop {
            let (bins, w) = self.shape(finite, (r.w - gw) as usize);
            bar_w = w;
            b = bin(self.samples, bins, self.range);
            plan = ColPlan::new(b.counts.iter().copied().max().unwrap_or(0) as f64, None, None, body.h);
            let want = plan.gut.fit(self.y_labels, r.w);
            rounds += 1;
            if want <= gw || rounds >= 4 {
                break;
            }
            gw = want;
        }
        let vals: Vec<f64> = b.counts.iter().map(|c| *c as f64).collect();
        plan.draw(&mut pen, body, gw, &vals, &|_, _| color, bar_w, 0, None, st);
        if x_h > 0 {
            let used = (b.counts.len() * bar_w).min((r.w - gw) as usize) as i64;
            let mut row = LabelRow::new(used);
            let (bx, y) = (r.x as i64 + gw as i64, r.y as i64 + body.h as i64);
            let mark = match self.marker {
                Marker::None => None,
                Marker::Mean => mean(self.samples),
                Marker::Median => quantile(self.samples, 0.5),
            };
            if let Some(m) = mark.filter(|m| *m >= b.lo && *m <= b.hi) {
                let col = ((frac(m, b.lo, b.hi) * used as f64) as i64).clamp(0, (used - 1).max(0));
                let glyph = one_cell(st.marker_glyph, '▲');
                let text = format!("{glyph}{}", fmt_num(m));
                let lw = width(&text) as i64;
                // the glyph sits on the marked column: the value runs right of it, or left when
                // the row ends first
                if col + lw <= used {
                    if let Some(start) = row.place(col, lw, 0) {
                        pen.text(bx + start, y, &text, Style::fg(st.text));
                    }
                } else {
                    let text = format!("{}{glyph}", fmt_num(m));
                    if let Some(start) = row.place(col, lw, lw - 1) {
                        pen.text(bx + start, y, &text, Style::fg(st.text));
                    }
                }
            }
            for (anchor, v) in [(0, b.lo), (used - 1, b.hi)] {
                let text = fmt_num(v);
                let lw = width(&text) as i64;
                if let Some(start) = row.place(anchor, lw, if anchor == 0 { 0 } else { lw - 1 }) {
                    pen.text(bx + start, y, &text, Style::fg(st.label));
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// scatter

/// Points in braille with a y axis and, optionally, an x axis of round values.
///
/// Each range covers the points, widened to round ticks, unless [`Bounds`] fix an end; a point
/// outside a fixed range, or with a coordinate that is not finite, is not drawn. With
/// [`Marks::Blocks`] each point is a `•` in its cell.
#[derive(Clone, Debug, PartialEq)]
pub struct Scatter<'a> {
    pub points: &'a [(f64, f64)],
    pub x: Bounds,
    pub y: Bounds,
    pub color: Option<Rgb>,
    pub y_labels: bool,
    pub x_labels: bool,
}

impl<'a> Scatter<'a> {
    /// A scatter of `points` with both axes labeled.
    pub fn new(points: &'a [(f64, f64)]) -> Scatter<'a> {
        Scatter { points, x: Bounds::default(), y: Bounds::default(), color: None, y_labels: true, x_labels: true }
    }

    /// Fix either end of the x and y ranges.
    pub fn bounds(mut self, x: Bounds, y: Bounds) -> Self {
        self.x = x;
        self.y = y;
        self
    }

    /// The points' color.
    pub fn color(mut self, c: Rgb) -> Self {
        self.color = Some(c);
        self
    }

    /// Show or hide the y labels and the x axis.
    pub fn labels(mut self, y: bool, x: bool) -> Self {
        self.y_labels = y;
        self.x_labels = x;
        self
    }

    /// Draw into `r` on `ground`.
    pub fn draw(&self, g: &mut Grid, r: Rect, st: &PlotStyle, ground: Rgb, clip: Rect) {
        let r = cap(r);
        let mut pen = Pen::new(g, r, ground, clip);
        if r.is_empty() {
            return;
        }
        let ok = |p: &&(f64, f64)| p.0.is_finite() && p.1.is_finite();
        let (Some(xr), Some(yr)) = (finite_range(self.points.iter().filter(ok).map(|p| p.0)), finite_range(self.points.iter().filter(ok).map(|p| p.1))) else {
            no_data(&mut pen, r, st);
            return;
        };
        let x_h = if self.x_labels && r.h >= 4 { 2 } else { 0 };
        let body_h = r.h - x_h;
        let ys = Scale::new(Some(yr), self.y, &[], y_ticks(body_h), y_fine(body_h));
        let dh = body_h as i64 * 4;
        let ydot = |v: f64| ((1.0 - ys.frac(v)) * (dh - 1) as f64).round() as i64;
        let gut = Gutter::new(&ys, body_h, &|v| (ydot(v) / 4) as usize, None);
        let gw = gut.fit(self.y_labels && r.h >= 2, r.w);
        let body_w = r.w - gw;
        let x_ticks = (body_w as usize / 10).clamp(2, 6);
        let xs = Scale::new(Some(xr), self.x, &[], x_ticks, x_ticks * 2);
        let dw = body_w as i64 * 2;
        let xdot = |v: f64| (xs.frac(v) * (dw - 1) as f64).round() as i64;
        let bx = r.x as i64 + gw as i64;
        let color = self.color.unwrap_or(st.series_color(0));
        let inside = |p: &&(f64, f64)| p.0 >= xs.lo && p.0 <= xs.hi && p.1 >= ys.lo && p.1 <= ys.hi;
        if st.marks == Marks::Blocks {
            for p in self.points.iter().filter(ok).filter(inside) {
                pen.ch(bx + xdot(p.0) / 2, r.y as i64 + ydot(p.1) / 4, '•', color);
            }
        } else if let Ok(bx16) = u16::try_from(bx) {
            let mut canvas = Braille::new(body_w, body_h);
            for p in self.points.iter().filter(ok).filter(inside) {
                canvas.set(xdot(p.0) as i32, ydot(p.1) as i32, color);
            }
            canvas.draw(pen.g, bx16, r.y, ground, pen.clip);
        }
        gut.draw(&mut pen, r.x as i64, r.y as i64, gw, st);
        if x_h > 0 {
            let ticks: Vec<(i64, Cow<str>)> = xs.ticks.iter().zip(&xs.labels).map(|(v, l)| ((xdot(*v) / 2).clamp(0, body_w as i64 - 1), Cow::Borrowed(l.as_str()))).collect();
            x_axis(&mut pen, bx, r.y as i64 + body_h as i64, gw > 0, body_w as i64, &ticks, st);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// heat strip, heatmap, box row

/// One heat cell: the shade (`░▒▓█`, a quarter of the range each) and its color, or the
/// missing mark.
fn heat_cell(v: f64, lo: f64, hi: f64, st: &PlotStyle) -> (char, Rgb) {
    if !v.is_finite() {
        return (one_cell(st.missing_glyph, '·'), st.missing);
    }
    let level = ((frac(v, lo, hi) * 4.0) as usize).min(3);
    (SHADES[level], st.heat.0.mix_lab(st.heat.1, level as f32 / 3.0))
}

/// A one-row heat strip exactly `width` cells wide: each value a shade `░▒▓█` by its quarter of
/// the range, in a color blended from [`PlotStyle::heat`]. Shorter series sit at the right edge;
/// longer ones are resampled by bucket mean. A missing value draws the missing mark.
pub fn heat_pieces(values: &[f64], width: u16, bounds: Bounds, st: &PlotStyle) -> Vec<Piece> {
    let w = width as usize;
    let vals = resample(values, w, Bucket::Mean);
    let mut out: Vec<Piece> = Vec::new();
    if vals.len() < w {
        out.push((" ".repeat(w - vals.len()), Style::default()));
    }
    let (lo, hi) = resolve(bounds, finite_range(vals.iter().copied()));
    for &v in &vals {
        let (c, color) = heat_cell(v, lo, hi, st);
        push_cell(&mut out, c, Style::fg(color));
    }
    out
}

/// Labeled heat strips on one shared scale, a row each: activity by hour for each weekday,
/// load for each host. Column `i` is value `i` in every row (left-aligned); rows longer than
/// the strip are resampled by bucket mean.
#[derive(Clone, Debug, PartialEq)]
pub struct Heatmap<'a> {
    pub rows: &'a [(&'a str, &'a [f64])],
    pub bounds: Bounds,
}

impl<'a> Heatmap<'a> {
    /// A heatmap of `rows` scaled to their joint range.
    pub fn new(rows: &'a [(&'a str, &'a [f64])]) -> Heatmap<'a> {
        Heatmap { rows, bounds: Bounds::default() }
    }

    /// Fix either end of the range.
    pub fn bounds(mut self, b: Bounds) -> Self {
        self.bounds = b;
        self
    }

    /// Draw one strip a row into `r`, as many as fit; returns the rows used.
    pub fn draw(&self, g: &mut Grid, r: Rect, st: &PlotStyle, ground: Rgb, clip: Rect) -> u16 {
        let r = cap(r);
        let mut pen = Pen::new(g, r, ground, clip);
        if r.is_empty() {
            return 0;
        }
        let Some(range) = finite_range(self.rows.iter().flat_map(|(_, v)| v.iter().copied())) else {
            no_data(&mut pen, Rect::new(r.x, r.y, r.w, 1), st);
            return 1;
        };
        let (lo, hi) = resolve(self.bounds, Some(range));
        let w = r.w as usize;
        let mut lw = self.rows.iter().map(|(l, _)| width(l)).max().unwrap_or(0).min(w / 3);
        if lw < 2 {
            lw = 0;
        }
        let sw = w - lw - (lw > 0) as usize;
        let n = self.rows.iter().map(|(_, v)| v.len()).max().unwrap_or(0);
        let shown = self.rows.len().min(r.h as usize);
        for (i, (label, vals)) in self.rows[..shown].iter().enumerate() {
            let y = r.y as i64 + i as i64;
            if lw > 0 {
                pen.text(r.x as i64, y, &truncate(label, lw), Style::fg(st.text));
            }
            let x0 = r.x as i64 + (w - sw) as i64;
            for c in 0..sw.min(n) {
                let v = if n <= sw { vals.get(c).copied() } else { Some(column_value(vals, n, sw, c)) };
                if let Some(v) = v {
                    let (ch, color) = heat_cell(v, lo, hi, st);
                    pen.ch(x0 + c as i64, y, ch, color);
                }
            }
        }
        shown as u16
    }
}

/// A box-plot row exactly `width` cells wide: `├──▒▒▒█▒▒▒▒──┤`, whiskers from the minimum to
/// the maximum, the box from the first to the third quartile, a solid cell on the median.
/// Columns span the samples' range, or `bounds` (so several rows share one scale).
pub fn box_pieces(samples: &[f64], width: u16, bounds: Bounds, st: &PlotStyle) -> Vec<Piece> {
    let w = width as usize;
    let mut v: Vec<f64> = samples.iter().copied().filter(|x| x.is_finite()).map(tame).collect();
    v.sort_by(f64::total_cmp);
    let (Some(&min), Some(&max)) = (v.first(), v.last()) else {
        return vec![(" ".repeat(w), Style::default())];
    };
    if w == 0 {
        return Vec::new();
    }
    let (lo, hi) = resolve(bounds, Some((min, max)));
    let col = |x: f64| ((frac(x, lo, hi) * (w - 1) as f64).round() as usize).min(w - 1);
    let q = |p: f64| col(sorted_quantile(&v, p).unwrap_or(min));
    let (cmin, c1, cmed, c3, cmax) = (col(min), q(0.25), q(0.5), q(0.75), col(max));
    let (axis, body, median) = (Style::fg(st.axis), Style::fg(st.series_color(0)), Style::fg(st.text));
    let mut out: Vec<Piece> = Vec::new();
    for c in 0..w {
        let (ch, style) = if c == cmed {
            ('█', median)
        } else if c >= c1 && c <= c3 {
            ('▒', body)
        } else if c == cmin {
            ('├', axis)
        } else if c == cmax {
            ('┤', axis)
        } else if c > cmin && c < cmax {
            ('─', axis)
        } else {
            (' ', Style::default())
        };
        push_cell(&mut out, ch, style);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::pieces_width;

    const BG: Rgb = EVERFOREST.bg0;
    const LAT: [f64; 16] = [12.0, 14.0, 13.0, 18.0, 25.0, 40.0, 38.0, 22.0, 15.0, 13.0, 12.0, 16.0, 30.0, 45.0, 28.0, 14.0];
    const P50: [f64; 16] = [8.0, 9.0, 8.0, 10.0, 12.0, 15.0, 14.0, 11.0, 9.0, 8.0, 8.0, 9.0, 12.0, 16.0, 12.0, 9.0];

    fn st() -> PlotStyle {
        PlotStyle::default()
    }

    fn blocks() -> PlotStyle {
        PlotStyle { marks: Marks::Blocks, ..st() }
    }

    fn text(ps: &[Piece]) -> String {
        ps.iter().map(|(s, _)| s.as_str()).collect()
    }

    /// Draw into a fresh `w` x `h` grid and return its rows as text.
    fn render(w: u16, h: u16, f: impl FnOnce(&mut Grid, Rect)) -> Vec<String> {
        let mut g = Grid::new(w, h);
        let a = g.area();
        f(&mut g, a);
        g.to_text().lines().map(str::to_string).collect()
    }

    fn days() -> [Bar<'static>; 7] {
        [Bar::new("Mon", 3.0), Bar::new("Tue", 7.0), Bar::new("Wed", 12.0), Bar::new("Thu", 9.0), Bar::new("Fri", 15.0), Bar::new("Sat", 1.0), Bar::new("Sun", 0.0)]
    }

    fn services() -> [Bar<'static>; 4] {
        [Bar::new("checkout", 184.0), Bar::new("search", 97.0), Bar::new("auth", 12.0), Bar::new("cart", 0.0)]
    }

    #[test]
    fn every_glyph_is_one_cell() {
        let s = st();
        let mut b = [0u8; 4];
        let other = [s.missing_glyph, s.threshold_glyph, s.marker_glyph, s.legend_glyph, '┤', '│', '└', '─', '┬', '├', '•', '–'];
        let fixed = EIGHTHS_UP.iter().chain(&EIGHTHS_RIGHT).chain(&SHADES).chain(&other);
        for c in fixed.copied().chain((0..=255u32).map(|i| char::from_u32(0x2800 + i).unwrap())) {
            assert_eq!(width(c.encode_utf8(&mut b)), 1, "{c:?} must be one cell");
        }
        assert_eq!(one_cell('汉', '·'), '·', "a wide glyph from a config falls back");
        assert_eq!((column_glyph(0, 0), column_glyph(3, 0), column_glyph(11, 1), column_glyph(11, 0), column_glyph(11, 2)), (' ', '▃', '▃', '█', ' '));
        assert_eq!((bar_glyph(0, 0), bar_glyph(5, 0), bar_glyph(13, 1), bar_glyph(13, 0), bar_glyph(13, usize::MAX)), (' ', '▋', '▋', '█', ' '));
    }

    #[test]
    fn braille_canvas_sets_lines_and_fills() {
        let c0 = st().axis;
        let mut c = Braille::new(6, 2);
        assert_eq!((c.dot_w(), c.dot_h()), (12, 8));
        c.line(0, 0, 11, 7, c0);
        c.hline(7, 0, 5, c0);
        c.fill_rect(8, 0, 11, 1, c0);
        // outside the canvas: ignored, and a far endpoint costs no more than the visible part
        c.set(-1, 0, c0);
        c.set(12, 8, c0);
        c.line(-2_000_000_000, 3, 2_000_000_000, 3, c0);
        c.line(i32::MIN, i32::MIN, i32::MIN, i32::MAX, c0);
        assert_eq!(render(6, 2, |g, a| c.draw(g, 0, 0, BG, a)), ["⣑⣢⣀⣀⣛⣛", "⣀⣀⣀⠉⠢⢄"]);
        assert!(c.get(0, 0) && c.get(11, 7) && !c.get(0, 1) && !c.get(99, 99));
        c.unset(0, 0);
        assert!(!c.get(0, 0));
        // each dot is its own bit: the four corners of one cell
        let mut one = Braille::new(1, 1);
        for (x, y, glyph) in [(0, 0, '⠁'), (1, 0, '⠉'), (0, 3, '⡉'), (1, 3, '⣉')] {
            one.set(x, y, c0);
            assert_eq!(one.cell(0, 0), Some((glyph, c0)));
        }
        assert_eq!(Braille::new(2, 2).cell(0, 0), None, "a blank cell draws nothing");
        // blank cells leave what is under them
        let rows = render(4, 1, |g, a| {
            g.put(0, 0, "abcd", Style::default());
            let mut c = Braille::new(4, 1);
            c.set(2, 0, c0);
            c.draw(g, 0, 0, BG, a);
        });
        assert_eq!(rows, ["a⠁cd"]);
        // degenerate sizes
        let mut z = Braille::new(0, 5);
        z.line(0, 0, 10, 10, c0);
        z.fill_rect(-5, -5, 5, 5, c0);
        assert_eq!(z.cell(0, 0), None);
        assert_eq!(Braille::new(u16::MAX, u16::MAX).dot_w(), MAX_CELLS.0 as i32 * 2, "a huge canvas is cut, not allocated");
    }

    #[test]
    fn blocks_canvas_stacks_eighths() {
        let c0 = st().axis;
        let mut b = Blocks::new(6, 2);
        assert_eq!(b.levels(), 16);
        for x in 0..6 {
            b.set(x, x * 3 + 1, c0);
        }
        b.set(-1, 4, c0);
        b.set(6, 4, c0);
        assert_eq!(render(6, 2, |g, a| b.draw(g, 0, 0, BG, a)), ["   ▂▅█", "▁▄▇███"]);
        b.fill(0, 99, 99, c0);
        assert_eq!((b.height(0), b.height(5), b.height(6)), (16, 16, 0));
        b.fill(0, 5, -3, c0);
        assert_eq!(b.cell(0, 1), None);
    }

    #[test]
    fn nice_steps_ticks_and_labels() {
        let labels = |lo: f64, hi: f64, n: usize| {
            let step = nice_step(lo, hi, n);
            (step, tick_labels(&ticks_in(lo, hi, step), step))
        };
        assert_eq!(labels(0.0, 11.0, 4), (5.0, vec!["0".to_string(), "5".into(), "10".into()]));
        assert_eq!(labels(12.0, 45.0, 9).0, 5.0);
        assert_eq!(labels(0.0, 26000.0, 3), (20000.0, vec!["0".to_string(), "20k".into()]));
        // two ticks around zero cannot cover -5..95: the fewest ticks win
        assert_eq!(nice_step(-5.0, 95.0, 2), 100.0);
        // decimals come from the step, so the labels share a width; no negative zero
        assert_eq!(tick_labels(&ticks_in(-1.0, 1.0, 0.5), 0.5), ["-1.0", "-0.5", "0.0", "0.5", "1.0"]);
        assert_eq!(tick_labels(&[0.0, 2e6, 4e6], 2e6), ["0", "2M", "4M"]);
        assert_eq!(tick_labels(&[0.0, 5e5, 1.5e6], 5e5), ["0", "500k", "1500k"], "the step picks the suffix, so the labels share one");
        assert_eq!(tick_labels(&[1000.0, 1500.0], 500.0), ["1000", "1500"]);
        assert_eq!(tick_labels(&[0.0, 1e-9], 1e-9), ["0", "1.0e-9"]);
        // 0.1 + 0.2 is not 0.3: the ends still count as ticks
        assert_eq!(ticks_in(0.1 + 0.2, 0.9, 0.3).len(), 3);
        // degenerate input: no ticks, a harmless step
        assert!(ticks_in(1.0, 0.0, 1.0).is_empty() && ticks_in(0.0, 1.0, 0.0).is_empty() && ticks_in(f64::NAN, 1.0, 1.0).is_empty());
        assert_eq!(ticks_in(0.0, 1e9, 1.0).len(), 64, "never more than 64");
        assert_eq!((nice_step(3.0, 3.0, 4), nice_step(f64::NAN, 1.0, 4), nice_step(0.0, f64::INFINITY, 4)), (1.0, 1.0, 1.0));
        assert!(nice_step(-f64::MAX, f64::MAX, 4).is_finite() && nice_step(0.0, 5e-324, 4) > 0.0);
        let nums: Vec<String> = [0.0, 5.0, 12.5, 0.25, 0.0012, 1234.0, 12345.0, 1.5e6, 2.5e9, -97.0, 99.96, 1e20, f64::NAN].iter().map(|v| fmt_num(*v)).collect();
        assert_eq!(nums, ["0", "5", "12.5", "0.25", "0.001", "1234", "12.3k", "1.5M", "2.5G", "-97", "100", "1.0e20", "–"]);
    }

    #[test]
    fn scales_fit_the_data_and_keep_fixed_ends() {
        let ends = |data, b, extra: &[Option<f64>], rows: u16| {
            let s = Scale::new(data, b, extra, y_ticks(rows), y_fine(rows));
            (s.lo, s.hi, s.labels)
        };
        // open ends move out to a fine multiple; labels sit on a coarser one inside
        assert_eq!(ends(Some((12.0, 45.0)), Bounds::default(), &[], 8), (10.0, 45.0, vec!["10".to_string(), "20".into(), "30".into(), "40".into()]));
        // a threshold outside the data widens the range
        assert_eq!(ends(Some((12.0, 45.0)), Bounds::default(), &[Some(80.0), None], 8).1, 80.0);
        // fixed ends stay put, even when they are not round or cut the data
        let (lo, hi, labels) = ends(Some((12.0, 45.0)), Bounds::new(3.0, 33.0), &[Some(80.0)], 8);
        assert_eq!((lo, hi, labels), (3.0, 33.0, vec!["10".to_string(), "20".into(), "30".into()]));
        assert_eq!(ends(Some((12.0, 45.0)), Bounds::from(0.0), &[], 4).0, 0.0);
        // a constant gets room either side, but not below zero
        assert_eq!(ends(Some((250.0, 250.0)), Bounds::default(), &[], 4), (220.0, 280.0, vec!["250".to_string()]));
        assert_eq!(ends(Some((0.0, 0.0)), Bounds::default(), &[], 3), (0.0, 1.0, vec!["0".to_string(), "1".into()]));
        let (lo, hi, _) = ends(Some((-3.0, -3.0)), Bounds::default(), &[], 4);
        assert!(lo < -3.0 && hi > -3.0);
        // inverted and unusable bounds still give a range
        for b in [Bounds::new(10.0, 0.0), Bounds::new(f64::NAN, f64::INFINITY), Bounds::new(5.0, 5.0), Bounds { min: None, max: Some(-1e308) }] {
            let (lo, hi, _) = ends(Some((1.0, 2.0)), b, &[], 4);
            assert!(lo.is_finite() && hi.is_finite() && hi > lo, "{b:?} gave {lo}..{hi}");
        }
        let (lo, hi, _) = ends(Some((-1e300, 1e300)), Bounds::default(), &[], 4);
        assert!(lo.is_finite() && hi.is_finite() && hi > lo);
        assert_eq!(finite_range([f64::NAN, 3.0, f64::INFINITY, -1.0]), Some((-1.0, 3.0)));
        assert_eq!(finite_range([f64::NAN, f64::NEG_INFINITY]), None);
    }

    #[test]
    fn resampling_follows_the_bucket_rule() {
        let v = [1.0, 5.0, 2.0, 9.0, 4.0, 4.0, f64::NAN, 8.0, f64::NAN, f64::NAN];
        // ten values into five buckets of two
        assert_eq!(resample(&v, 5, Bucket::Max)[..4], [5.0, 9.0, 4.0, 8.0]);
        assert_eq!(resample(&v, 5, Bucket::Min)[..4], [1.0, 2.0, 4.0, 8.0]);
        assert_eq!(resample(&v, 5, Bucket::Mean)[..4], [3.0, 5.5, 4.0, 8.0]);
        assert_eq!(resample(&v, 5, Bucket::Last)[..4], [5.0, 9.0, 4.0, 8.0]);
        assert_eq!(resample(&[3.0, 1.0, 7.0, 2.0], 2, Bucket::Last), [1.0, 2.0]);
        assert!(resample(&v, 5, Bucket::Max)[4].is_nan(), "a bucket with no value is missing");
        // uneven: seven into three is 2, 2, 3
        assert_eq!(resample(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0], 3, Bucket::Max), [2.0, 4.0, 7.0]);
        // no longer than asked: unchanged
        assert_eq!(resample(&[1.0, 2.0], 5, Bucket::Mean), [1.0, 2.0]);
        assert!(resample(&v, 0, Bucket::Mean).is_empty() && resample(&[], 4, Bucket::Max).is_empty());
        assert_eq!(resample(&[f64::MAX, f64::MAX], 1, Bucket::Mean), [f64::MAX], "a mean of huge values does not overflow");
        assert_eq!(gaps(&[Some(1.0), None])[0], 1.0);
        assert!(gaps(&[Some(1.0), None])[1].is_nan());
        assert_eq!((mean(&[1.0, f64::NAN, 3.0]), mean(&[f64::NAN]), mean(&[])), (Some(2.0), None, None));
        assert_eq!((quantile(&[4.0, 1.0, 3.0, 2.0], 0.5), quantile(&[5.0, f64::NAN, 1.0], 1.0), quantile(&[], 0.5), quantile(&[7.0], f64::NAN)), (Some(2.5), Some(5.0), None, Some(7.0)));
    }

    #[test]
    fn sparkline_is_one_cell_a_value_at_an_exact_width() {
        let s = st();
        assert_eq!(text(&Sparkline::new(&LAT).pieces(16, &s)), "▁▁▁▂▄▇▇▃▂▁▁▂▅█▄▁");
        // shorter than the cell: the newest value is the last cell; a gap is a dot
        let ps = Sparkline::new(&[1.0, f64::NAN, 3.0, 8.0]).pieces(8, &s);
        assert_eq!(text(&ps), "    ▁·▃█");
        assert_eq!(ps.iter().map(|(_, st)| st.fg).collect::<Vec<_>>(), [None, Some(s.series[0]), Some(s.missing), Some(s.series[0])]);
        // longer: resampled, by the bucket's max unless told otherwise
        assert_eq!(text(&Sparkline::new(&LAT).pieces(8, &s)), "▁▂▇▆▁▁█▄");
        assert_eq!(text(&Sparkline::new(&LAT).bucket(Bucket::Min).pieces(8, &s)), "▁▁▆▅▁▁█▂");
        // over the threshold: the "over" color, the same glyphs
        let ps = Sparkline::new(&[1.0, 9.0, 2.0]).threshold(5.0).color(s.series[2]).pieces(3, &s);
        assert_eq!((text(&ps), ps[1].1.fg, ps[0].1.fg), ("▁█▂".to_string(), Some(s.over), Some(s.series[2])));
        // fixed bounds: values outside ride the edge
        assert_eq!(text(&Sparkline::new(&[0.0, 50.0, 100.0, 500.0, -9.0]).bounds(Bounds::new(0.0, 100.0)).pieces(5, &s)), "▁▅██▁");
        // constant: mid height, or the floor for zeros; fixed bounds still place it
        assert_eq!(text(&Sparkline::new(&[5.0; 4]).pieces(4, &s)), "▄▄▄▄");
        assert_eq!(text(&Sparkline::new(&[0.0; 4]).pieces(4, &s)), "▁▁▁▁");
        assert_eq!(text(&Sparkline::new(&[5.0; 4]).bounds(Bounds::from(0.0)).pieces(4, &s)), "████");
        // nothing to draw is still the width asked for
        assert_eq!(text(&Sparkline::new(&[]).pieces(3, &s)), "   ");
        assert_eq!(text(&Sparkline::new(&[f64::NAN, f64::INFINITY]).pieces(3, &s)), " ··");
        assert!(Sparkline::new(&LAT).pieces(0, &s).is_empty());
        for w in 0..40 {
            assert_eq!(pieces_width(&Sparkline::new(&LAT).threshold(20.0).pieces(w, &s)), w as usize);
        }
        assert_eq!(render(10, 1, |g, a| Sparkline::new(&[1.0, 2.0, 3.0]).draw(g, 2, 0, 5, &s, BG, a)), ["    ▁▅█"]);
    }

    #[test]
    fn line_plot_with_a_threshold() {
        let s = st();
        let mut g = Grid::new(40, 8);
        let a = g.area();
        LinePlot::new(&[Series::new("p99", &LAT)]).threshold(30.0).draw(&mut g, a, &s, BG, a);
        let rows: Vec<&str> = [
            "  │                               ⡜⡄",
            "40┤           ⢠⠋⠒⢤               ⡸ ⢱",
            "  │           ⡎   ⢇             ⢠⠃  ⢇",
            "30┤╌╌╌╌╌╌╌╌╌╌⡜╌╌╌╌⠈⡆╌╌╌╌╌╌╌╌╌╌╌⢀⠇╌╌╌⠘⡄╌╌",
            "  │         ⡰⠁     ⠸⡀          ⡜     ⠱⡀",
            "20┤        ⡔⠁       ⠱⡀        ⢠⠃      ⢱",
            "  │  ⣀⣀ ⢀⠤⠊          ⠈⢆⣀     ⣀⠎        ⢣",
            "10┤⠒⠉  ⠉⠁               ⠉⠉⠑⠒⠉",
        ]
        .to_vec();
        assert_eq!(g.to_text().lines().collect::<Vec<_>>(), rows);
        // the threshold's label and line share its color; the line shows through blank cells only
        assert_eq!(g.get(0, 3).unwrap().style.fg, Some(s.threshold));
        assert_eq!(g.get(3, 3).unwrap().style.fg, Some(s.threshold));
        assert_eq!(g.get(13, 3).unwrap().style.fg, Some(s.series[0]));
        assert_eq!(g.get(0, 1).unwrap().style.fg, Some(s.label));
        assert_eq!(g.get(2, 0).unwrap().style.fg, Some(s.axis));
        // a threshold outside a fixed range cannot show
        let rows = render(20, 4, |g, a| LinePlot::new(&[Series::new("", &LAT)]).y(Bounds::new(0.0, 50.0)).threshold(80.0).draw(g, a, &s, BG, a));
        assert!(!rows.concat().contains('╌'));
    }

    #[test]
    fn line_plot_with_two_series_an_x_axis_and_a_legend() {
        let s = st();
        let mut g = Grid::new(40, 10);
        let a = g.area();
        let series = [Series::new("p99", &LAT), Series::new("p50", &P50).color(Rgb(1, 2, 3))];
        LinePlot::new(&series).x(XAxis::Ends("-15m", "now")).draw(&mut g, a, &s, BG, a);
        let rows: Vec<&str> = [
            "  │                               ⢀",
            "40┤           ⢠⠒⠢⠤⡀              ⢠⠃⢣",
            "  │          ⢠⠃   ⠱⡀            ⡰⠁  ⢣",
            "  │        ⢀⡠⠃     ⠱⡀          ⡜     ⠱⡀",
            "20┤  ⣀⣀ ⢀⡠⠔⠁  ⢀⣀⣀⣀⡀ ⠈⠒⢄⣀     ⣀⠜  ⢀⡠⣀  ⠈⢆",
            "  │⣒⡩⠤⠤⣉⣁⡠⠤⠔⠒⠊⠁   ⠈⠑⠒⠢⠤⠤⣉⣉⣑⣒⣉⠤⠤⠒⠊⠁  ⠉⠒⠢⠤",
            " 0┤",
            "  └┬───────────────────────────────────┬",
            "   -15m                              now",
            "   ● p99  ● p50",
        ]
        .to_vec();
        assert_eq!(g.to_text().lines().collect::<Vec<_>>(), rows);
        assert_eq!((g.get(3, 9).unwrap().style.fg, g.get(10, 9).unwrap().style.fg), (Some(s.series[0]), Some(Rgb(1, 2, 3))));
        // index labels, and a label for each point: a few round ones, as many as fit
        let rows = render(40, 7, |g, a| LinePlot::new(&[Series::new("depth", &LAT)]).fill(true).x(XAxis::Index).draw(g, a, &s, BG, a));
        assert_eq!(rows[5..], ["  └┬───────────┬───────────┬───────────┬", "   0           5          10          15"]);
        let hours: Vec<String> = (0..16).map(|h| format!("{h:02}:00")).collect();
        let hours: Vec<&str> = hours.iter().map(String::as_str).collect();
        let rows = render(40, 6, |g, a| LinePlot::new(&[Series::new("", &LAT)]).x(XAxis::Labels(&hours)).y(Bounds::new(0.0, 50.0)).draw(g, a, &s, BG, a));
        assert_eq!(
            rows,
            [
                "40┤            ⣀⡀                ⢀⠔⢄",
                "  │          ⡠⠊ ⠈⠉⠢⢄           ⢀⠔⠁  ⠣⡀",
                "20┤⣀⣀⣀⣀⣀⣀⡠⠤⠒⠉       ⠑⠢⢄⣀⣀⣀⣀⣀⣀⠤⠔⠁     ⠈⠒⢄",
                " 0┤",
                "  └┬───────────┬───────────┬───────────┬",
                "   00:00     05:00       10:00     15:00",
            ]
        );
    }

    #[test]
    fn area_plot_fills_down_to_zero() {
        let s = st();
        let rows = render(40, 5, |g, a| LinePlot::new(&[Series::new("depth", &LAT)]).fill(true).draw(g, a, &s, BG, a));
        assert_eq!(
            rows,
            [
                "  │                               ⣠⡀",
                "40┤           ⣴⣿⣷⣶⣄             ⢀⣼⣿⣿⣆",
                "20┤        ⣀⣤⣾⣿⣿⣿⣿⣿⣧⣀          ⣴⣿⣿⣿⣿⣿⣷⣄",
                "  │⣤⣤⣤⣤⣤⣤⣶⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣶⣶⣤⣤⣤⣤⣤⣶⣾⣿⣿⣿⣿⣿⣿⣿⣿⣦",
                " 0┤⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿",
            ]
        );
    }

    #[test]
    fn line_plot_in_blocks_is_columns_from_zero() {
        let s = blocks();
        let mut g = Grid::new(40, 6);
        let a = g.area();
        LinePlot::new(&[Series::new("p99", &LAT), Series::new("p50", &P50)]).threshold(30.0).draw(&mut g, a, &s, BG, a);
        let rows: Vec<&str> = [
            "  │                              ▃▃▃",
            "40┤           ▆▆▆▄▄              ███",
            "30┤╌╌╌╌╌╌╌╌╌╌╌█████╌╌╌╌╌╌╌╌╌╌╌╌▅▅███▃▃╌╌",
            "20┤      ▁▁▁███████▅▅          ███████",
            "  │▄▄▅▅▄▄████████████▆▆▆▄▄▄▄▄▇▇███████▅▅",
            " 0┤█████████████████████████████████████",
        ]
        .to_vec();
        assert_eq!(g.to_text().lines().collect::<Vec<_>>(), rows, "the first series only, and no legend");
        assert_eq!((g.get(14, 1).unwrap().style.fg, g.get(3, 5).unwrap().style.fg), (Some(s.over), Some(s.series[0])));
    }

    #[test]
    fn line_plot_handles_gaps_one_point_and_constants() {
        let s = st();
        let gappy = [1.0, 2.0, f64::NAN, 4.0, f64::INFINITY, f64::NAN, 3.0, 5.0, 4.0];
        // a gap breaks the line; the lone 4 between two gaps is a dot
        assert_eq!(
            render(20, 4, |g, a| LinePlot::new(&[Series::new("", &gappy)]).draw(g, a, &s, BG, a)),
            [" │              ⢀⠎⠢⡀", "4┤      ⠈      ⢠⠊  ⠈", "2┤  ⡀          ⠁", " │⡠⠊"]
        );
        assert_eq!(render(12, 3, |g, a| LinePlot::new(&[Series::new("", &[7.0])]).draw(g, a, &s, BG, a)), ["8┤", " │⠄", "6┤"]);
        assert_eq!(
            render(16, 4, |g, a| LinePlot::new(&[Series::new("", &[250.0; 5])]).draw(g, a, &s, BG, a)),
            ["   │", "   │", "250┤⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉", "   │"]
        );
        assert_eq!(render(16, 3, |g, a| LinePlot::new(&[Series::new("", &[0.0; 5])]).draw(g, a, &s, BG, a)), ["1┤", " │", "0┤⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀"]);
        // thousands and fractions label themselves compactly
        assert_eq!(
            render(30, 4, |g, a| LinePlot::new(&[Series::new("", &[1200.0, 18000.0, 26000.0, 9000.0])]).draw(g, a, &s, BG, a)),
            ["   │              ⣀⣀⠤⢄⡀", "20k┤       ⣀⠤⠤⠒⠒⠉⠉    ⠈⠑⠢⢄⡀", "   │   ⣀⠤⠒⠉               ⠈⠑⠢⢄", "  0┤⠤⠒⠉"]
        );
        // a value outside a fixed range rides the edge
        let rows = render(10, 2, |g, a| LinePlot::new(&[Series::new("", &[5.0, 500.0, 500.0])]).y(Bounds::new(0.0, 10.0)).y_labels(false).draw(g, a, &s, BG, a));
        assert!(rows[0].ends_with("⠉⠉⠉⠉⠉"), "{rows:?}");
    }

    #[test]
    fn line_plot_degrades_instead_of_failing() {
        let s = st();
        let one = [Series::new("p99", &LAT)];
        // nothing to plot
        assert_eq!(render(16, 3, |g, a| LinePlot::new(&[]).draw(g, a, &s, BG, a)), ["", "    no data", ""]);
        assert_eq!(render(5, 2, |g, a| LinePlot::new(&[Series::new("", &[f64::NAN])]).draw(g, a, &s, BG, a)), ["no d…", ""]);
        // one row: a sparkline of the first series that has data
        let both = [Series::new("a", &[f64::NAN]), Series::new("b", &LAT)];
        assert_eq!(render(16, 1, |g, a| LinePlot::new(&both).threshold(30.0).x(XAxis::Index).draw(g, a, &s, BG, a)), ["▁▁▁▂▄▇▇▃▂▁▁▂▅█▄▁"]);
        // too narrow for labels, too short for an x axis: the bare curve
        assert_eq!(render(5, 3, |g, a| LinePlot::new(&one).x(XAxis::Index).draw(g, a, &s, BG, a)), ["    ⡀", " ⡜⡇⢰⢇", "⠚⠁⠘⠊⠈"]);
        // the x axis needs four rows, the legend five
        let two = [Series::new("p99", &LAT), Series::new("p50", &P50)];
        let rows = |h: u16| render(30, h, |g, a| LinePlot::new(&two).x(XAxis::Index).draw(g, a, &s, BG, a)).concat();
        assert!(!rows(3).contains('└') && !rows(3).contains('●'));
        assert!(rows(4).contains('└') && !rows(4).contains('●'));
        assert!(rows(5).contains('└') && rows(5).contains('●'));
        // no room at all
        assert_eq!(render(4, 2, |g, _| LinePlot::new(&one).draw(g, Rect::new(1, 1, 0, 0), &s, BG, Rect::new(0, 0, 4, 2))), ["", ""]);
    }

    #[test]
    fn bar_chart_rows_shed_parts_as_they_narrow() {
        let s = st();
        let bars = services();
        let chart = BarChart::new(&bars).threshold(100.0);
        let draw = |w: u16| render(w, 4, |g, a| assert_eq!(chart.draw(g, a, &s, BG, a), 4));
        assert_eq!(draw(30), ["checkout █████████████████ 184", "search   █████████          97", "auth     █▏                 12", "cart                         0"]);
        assert_eq!(draw(16), ["checkout ███ 184", "search   █▋   97", "auth     ▎    12", "cart           0"]);
        // labels shorten, then the values go, then the labels
        assert_eq!(draw(9), ["che… ████", "sea… ██▏", "auth ▎", "cart"]);
        assert_eq!(draw(5), ["█████", "██▋", "▍", ""]);
        for w in 0..40u16 {
            for row in chart.rows(w, &s) {
                assert_eq!(pieces_width(&row), w as usize, "a row is exactly {w} cells");
            }
        }
        // over the threshold: bar and value in the "over" color
        let rows = chart.rows(30, &s);
        assert_eq!((rows[0][1].1.fg, rows[0][2].1.fg, rows[1][1].1.fg, rows[1][0].1.fg), (Some(s.over), Some(s.over), Some(s.series[0]), Some(s.text)));
        // a fixed scale, eighth by eighth; the smallest positive value still shows
        assert_eq!(text(&bar_pieces(50.0, 100.0, 5, s.axis)), "██▌  ");
        assert_eq!(text(&bar_pieces(0.001, 100.0, 5, s.axis)), "▏    ");
        assert_eq!(text(&bar_pieces(900.0, 100.0, 5, s.axis)), "█████");
        for v in [0.0, -4.0, f64::NAN, f64::INFINITY] {
            assert_eq!(text(&bar_pieces(v, 100.0, 5, s.axis)), "     ", "{v} draws no bar");
        }
        assert_eq!(text(&bar_pieces(5.0, f64::NAN, 3, s.axis)), "   ");
        // more bars than rows: the first ones; none at all says so
        assert_eq!(render(12, 2, |g, a| assert_eq!(BarChart::new(&bars).values(false).draw(g, a, &s, BG, a), 2)), ["checkout ███", "search   █▋"]);
        assert_eq!(render(12, 2, |g, a| assert_eq!(BarChart::new(&[]).draw(g, a, &s, BG, a), 1)), ["  no data", ""]);
        assert_eq!(text(&BarChart::new(&[Bar::new("a", 5.0), Bar::new("b", f64::NAN)]).max(10.0).rows(10, &s)[1]), "b        –");
    }

    #[test]
    fn column_chart_has_eighth_tops_and_labels_under() {
        let s = st();
        let bars = days();
        assert_eq!(
            render(32, 6, |g, a| ColumnChart::new(&bars).draw(g, a, &s, BG, a)),
            [
                "  │                ███",
                "10┤        ███     ███",
                "  │    ▃▃▃ ███ ███ ███",
                "  │    ███ ███ ███ ███",
                " 0┤███ ███ ███ ███ ███ ▃▃▃",
                "   Mon Tue Wed Thu Fri Sat Sun",
            ]
        );
        // a threshold: a line behind the columns, and the columns over it change color
        let mut g = Grid::new(32, 6);
        let a = g.area();
        ColumnChart::new(&bars).threshold(10.0).draw(&mut g, a, &s, BG, a);
        assert_eq!(g.to_text().lines().nth(1), Some("10┤╌╌╌╌╌╌╌╌███╌╌╌╌╌███╌╌╌╌╌╌╌╌╌╌"));
        assert_eq!((g.get(11, 4).unwrap().style.fg, g.get(15, 4).unwrap().style.fg, g.get(4, 1).unwrap().style.fg), (Some(s.over), Some(s.series[0]), Some(s.threshold)));
        // given widths; labels that would touch are skipped
        assert_eq!(
            render(16, 4, |g, a| ColumnChart::new(&bars).widths(1, 1).labels(false, true).draw(g, a, &s, BG, a)),
            ["    ▃   █", "  ▃ █ ▆ █", "▅ █ █ █ █ ▂", "Mon  Thu Sat"]
        );
        // more columns than cells: one cell each, no gap, the newest kept
        let many: Vec<Bar> = (0..30).map(|i| Bar::new("", (i % 7) as f64)).collect();
        assert_eq!(render(14, 3, |g, a| ColumnChart::new(&many).draw(g, a, &s, BG, a)), ["5┤ ▄█     ▄█", " │███   ▄███", "0┤███ ▄█████ ▄"]);
        // one row: a sparkline; nothing finite: says so; negatives draw nothing
        assert_eq!(render(14, 1, |g, a| ColumnChart::new(&many).draw(g, a, &s, BG, a)), ["▂▅▇█▃▆█▃▆█▂▅▇█"]);
        assert_eq!(render(9, 2, |g, a| ColumnChart::new(&[Bar::new("a", f64::NAN)]).draw(g, a, &s, BG, a)), [" no data", ""]);
        assert_eq!(render(9, 2, |g, a| ColumnChart::new(&[Bar::new("", -3.0), Bar::new("", -1.0)]).draw(g, a, &s, BG, a)), ["1┤", "0┤"]);
    }

    #[test]
    fn histogram_bins_by_the_documented_rule() {
        let samples = [1.0, 1.0, 2.0, 2.0, 2.0, 3.0, 3.0, 3.0, 3.0, 4.0, 4.0, 5.0, 9.0, f64::NAN];
        // four bins of width 2 over 1..9: [1,3) [3,5) [5,7) [7,9], the top end in the last bin
        assert_eq!(bin(&samples, 4, Bounds::default()), Bins { lo: 1.0, hi: 9.0, counts: vec![5, 6, 1, 1] });
        // a fixed range drops what lies outside it
        assert_eq!(bin(&samples, 2, Bounds::new(2.0, 4.0)).counts, [3, 6]);
        // one distinct value: half a unit either side
        assert_eq!(bin(&[5.0; 3], 2, Bounds::default()), Bins { lo: 4.5, hi: 5.5, counts: vec![0, 3] });
        assert_eq!(bin(&[], 0, Bounds::default()).counts, [0]);
        assert_eq!(bin(&[1e308, -1e308, f64::MAX], 3, Bounds::default()).counts.iter().sum::<u64>(), 3);
        assert_eq!([0, 1, 2, 3, 8, 9, 200].map(sturges), [1, 1, 2, 3, 4, 5, 9]);
    }

    #[test]
    fn histogram_draws_touching_columns_with_its_range_and_marker() {
        let s = st();
        let spread: Vec<f64> = (0..200).map(|i| ((i * 37 % 100) as f64 / 10.0 + (i * 53 % 100) as f64 / 10.0) / 2.0).collect();
        assert_eq!(
            render(34, 7, |g, a| Histogram::new(&spread).marker(Marker::Mean).draw(g, a, &s, BG, a)),
            [
                "40┤            ▁▁▁▃▃▃",
                "  │            ██████▄▄▄",
                "  │         ▇▇▇█████████▂▂▂",
                "20┤      ▆▆▆███████████████▁▁▁",
                "  │   ▄▄▄█████████████████████",
                " 0┤▅▅▅████████████████████████▇▇▇",
                "   0              ▲4.95       9.5",
            ]
        );
        let samples = [1.0, 1.0, 2.0, 2.0, 2.0, 3.0, 3.0, 3.0, 3.0, 4.0, 4.0, 5.0, 9.0];
        assert_eq!(
            render(24, 5, |g, a| Histogram::new(&samples).bins(9).marker(Marker::Median).draw(g, a, &s, BG, a)),
            ["4┤    ██", " │  ████", "2┤████████", "0┤██████████      ██", "  1   ▲3           9"]
        );
        // a marker near the right end keeps its glyph on its column: the value moves left of it
        let skew = [0.0, 10.0, 10.0, 10.0];
        let rows = render(22, 4, |g, a| Histogram::new(&skew).bins(10).marker(Marker::Median).labels(false, true).draw(g, a, &s, BG, a));
        assert_eq!(rows[3], "0                10▲");
        // one distinct value, one row, no samples
        assert_eq!(
            render(24, 4, |g, a| Histogram::new(&[5.0; 3]).marker(Marker::Mean).draw(g, a, &s, BG, a)),
            [" │       ███████", "2┤       ███████", "0┤       ███████", "  4.5       ▲5      5.5"]
        );
        assert_eq!(render(24, 1, |g, a| Histogram::new(&samples).bins(8).draw(g, a, &s, BG, a)), ["▅▅▅▆▆▆███▅▅▅▃▃▃▁▁▁▁▁▁▃▃▃"]);
        assert_eq!(render(12, 2, |g, a| Histogram::new(&[f64::NAN]).draw(g, a, &s, BG, a)), ["  no data", ""]);
    }

    #[test]
    fn scatter_plots_points_on_round_axes() {
        let pts: Vec<(f64, f64)> = (0..40).map(|i| (i as f64, ((i * i) % 17) as f64 + i as f64 / 2.0)).collect();
        assert_eq!(
            render(36, 8, |g, a| Scatter::new(&pts).draw(g, a, &st(), BG, a)),
            [
                "40┤                               ⡀",
                "  │                   ⢀  ⡀ ⠐     ⡀⢀",
                "  │          ⠠      ⠁  ⠈⠁ ⠠ ⠂   ⠠",
                "20┤   ⠐ ⠈⠐⠐⠁ ⡀⠠   ⢀⠈ ⢁    ⠄  ⠁⠒⠈",
                "  │  ⠐ ⠂    ⡀  ⠁⠄⠔",
                " 0┤⡀⠌   ⠁",
                "  └┬───────────────┬───────────────┬",
                "   0              20              40",
            ]
        );
        let sq: Vec<(f64, f64)> = (0..12).map(|i| (i as f64, (i * i) as f64)).collect();
        assert_eq!(
            render(20, 5, |g, a| Scatter::new(&sq).draw(g, a, &blocks(), BG, a)),
            ["   │           •", "100┤       ••••", "  0┤•••••••", "   └┬─────────┬─────", "    0        10"]
        );
        // a point outside a fixed range, or not finite, is not drawn
        let odd = [(0.0, 0.0), (1.0, 1.0), (50.0, 0.5), (f64::NAN, 0.5), (0.5, f64::INFINITY)];
        let rows = render(4, 1, |g, a| Scatter::new(&odd).bounds(Bounds::new(0.0, 1.0), Bounds::new(0.0, 1.0)).labels(false, false).draw(g, a, &st(), BG, a));
        assert_eq!(rows, ["⡀  ⠈"]);
        assert_eq!(render(9, 1, |g, a| Scatter::new(&[(f64::NAN, 1.0)]).draw(g, a, &st(), BG, a)), [" no data"]);
    }

    #[test]
    fn heat_strips_and_box_rows_are_exact_width_pieces() {
        let s = st();
        assert_eq!(text(&heat_pieces(&LAT, 16, Bounds::default(), &s)), "░░░░▒██▒░░░░▓█▒░");
        // a gap is the missing mark; a short series sits at the right
        let ps = heat_pieces(&[0.0, 1.0, f64::NAN, 2.0, 3.0, 4.0], 8, Bounds::default(), &s);
        assert_eq!(text(&ps), "  ░▒·▓██");
        assert_eq!((ps[1].1.fg, ps.last().unwrap().1.fg), (Some(s.heat.0), Some(s.heat.1)), "low to high across the blend");
        assert_eq!(text(&heat_pieces(&LAT, 4, Bounds::new(0.0, 40.0), &s)), "▒█▒▓", "longer: bucket means");
        let rows: [(&str, &[f64]); 2] = [("web", &LAT), ("db", &P50)];
        assert_eq!(render(24, 3, |g, a| assert_eq!(Heatmap::new(&rows).draw(g, a, &s, BG, a), 2)), ["web ░░░▒▒██▒░░░░▓█▓░", "db  ░░░░░░░░░░░░░░░░", ""]);
        assert_eq!(render(8, 1, |g, a| assert_eq!(Heatmap::new(&rows).bounds(Bounds::new(0.0, 16.0)).draw(g, a, &s, BG, a), 1)), ["w… █████"]);
        assert_eq!(render(8, 1, |g, a| assert_eq!(Heatmap::new(&[]).draw(g, a, &s, BG, a), 1)), ["no data"]);
        // box rows: one scale for several rows, or each its own
        assert_eq!(text(&box_pieces(&LAT, 30, Bounds::new(0.0, 50.0), &s)), "       ├▒▒█▒▒▒▒▒▒▒────────┤   ");
        let samples = [1.0, 1.0, 2.0, 2.0, 2.0, 3.0, 3.0, 3.0, 3.0, 4.0, 4.0, 5.0, 9.0, f64::NAN];
        assert_eq!(text(&box_pieces(&samples, 20, Bounds::default(), &s)), "├─▒▒▒█▒▒───────────┤");
        assert_eq!(text(&box_pieces(&[4.0], 5, Bounds::default(), &s)), "  █  ");
        assert_eq!(text(&box_pieces(&[], 5, Bounds::default(), &s)), "     ");
        for w in 0..30 {
            assert_eq!(pieces_width(&box_pieces(&samples, w, Bounds::default(), &s)), w as usize);
            assert_eq!(pieces_width(&heat_pieces(&LAT, w, Bounds::default(), &s)), w as usize);
        }
    }

    /// Every cell of a plot's rectangle is on the ground and nothing outside it is touched.
    fn assert_owns_rect(g: &Grid, r: Rect, clip: Rect, what: &str) {
        let own = cap(r).intersect(clip).intersect(g.area());
        for y in 0..g.h {
            for x in 0..g.w {
                let c = g.get(x, y).unwrap();
                if own.contains(x, y) {
                    assert_eq!(c.style.bg, Some(BG), "{what}: ({x},{y}) inside {r:?} is off the ground");
                } else {
                    assert_eq!(*c, crate::grid::Cell::default(), "{what}: ({x},{y}) outside {r:?} was written");
                }
            }
        }
    }

    /// Run every drawing plot into `r` of a fresh grid; each must own exactly its rectangle and
    /// draw the same thing twice.
    #[allow(clippy::too_many_arguments)]
    fn draw_all(gw: u16, gh: u16, r: Rect, clip: Rect, s: &PlotStyle, a: &[f64], b: &[f64], bounds: Bounds, thr: Option<f64>, n: usize) {
        let labels = ["", "a", "checkout", "日本語", "x\ty", "e\u{301}"];
        let series = [Series::new(labels[n % 6], a), Series::new(labels[(n + 2) % 6], b)];
        let bars: Vec<Bar> = a.iter().enumerate().map(|(i, v)| Bar::new(labels[(i + n) % 6], *v)).collect();
        let pts: Vec<(f64, f64)> = a.iter().zip(b.iter().cycle()).map(|(x, y)| (*x, *y)).collect();
        let xl: Vec<&str> = (0..a.len().min(40)).map(|i| labels[i % 6]).collect();
        let x = [XAxis::None, XAxis::Index, XAxis::Ends("日本", "now"), XAxis::Labels(&xl)][n % 4];
        let rows: Vec<(&str, &[f64])> = vec![(labels[n % 6], a), ("db", b)];
        let plots: Vec<(&str, Box<dyn Fn(&mut Grid) + '_>)> = vec![
            ("line", Box::new(|g: &mut Grid| LinePlot { series: &series[..1 + n % 2], y: bounds, x, threshold: thr, fill: n % 3 == 0, y_labels: n % 5 != 0, legend: true }.draw(g, r, s, BG, clip))),
            ("bars", Box::new(|g: &mut Grid| {
                BarChart { bars: &bars, max: bounds.max, values: n % 2 == 0, threshold: thr }.draw(g, r, s, BG, clip);
            })),
            ("columns", Box::new(|g: &mut Grid| ColumnChart { bars: &bars, max: bounds.max, bar_width: ((n % 4) * (n % 7) * 9000).min(65535) as u16, gap: (n % 3) as u16, y_labels: n % 5 != 1, x_labels: n % 2 == 0, threshold: thr }.draw(g, r, s, BG, clip))),
            ("hist", Box::new(|g: &mut Grid| Histogram { samples: a, bins: [0, 1, 7, usize::MAX][n % 4], range: bounds, marker: [Marker::None, Marker::Mean, Marker::Median][n % 3], y_labels: n % 5 != 2, x_labels: n % 2 == 0, color: None }.draw(g, r, s, BG, clip))),
            ("scatter", Box::new(|g: &mut Grid| Scatter { points: &pts, x: bounds, y: Bounds { min: bounds.max, max: bounds.min }, color: None, y_labels: n % 2 == 0, x_labels: n % 3 != 0 }.draw(g, r, s, BG, clip))),
            ("heatmap", Box::new(|g: &mut Grid| {
                Heatmap { rows: &rows, bounds }.draw(g, r, s, BG, clip);
            })),
            ("spark", Box::new(|g: &mut Grid| Sparkline { values: a, bounds, bucket: Bucket::Mean, color: None, threshold: thr }.draw(g, r.x, r.y, r.w, s, BG, clip))),
        ];
        for (name, plot) in &plots {
            let mut g1 = Grid::new(gw, gh);
            plot(&mut g1);
            let own = if *name == "spark" { Rect::new(r.x, r.y, r.w, 1) } else { r };
            assert_owns_rect(&g1, own, clip, name);
            let mut g2 = Grid::new(gw, gh);
            plot(&mut g2);
            assert_eq!(g1.cells, g2.cells, "{name} drew differently the second time");
        }
    }

    /// xorshift64*: a fixed seed, so a failure reproduces.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }
        fn below(&mut self, n: u64) -> u64 {
            (self.next() >> 11) % n.max(1)
        }
        fn value(&mut self) -> f64 {
            match self.below(24) {
                0 => f64::NAN,
                1 => f64::INFINITY,
                2 => f64::NEG_INFINITY,
                3 => -0.0,
                4 => f64::MAX,
                5 => f64::MIN,
                6 => 5e-324,
                7 => 1e-12,
                8 => 1e15,
                9 | 10 => 0.0,
                11 | 12 => self.below(10) as f64,
                _ => (self.below(200_001) as f64 - 60_000.0) / 7.0,
            }
        }
        fn values(&mut self) -> Vec<f64> {
            let len = match self.below(8) {
                0 => 0,
                1 => 1,
                2 => 2,
                3 => self.below(600),
                _ => self.below(40),
            };
            // some series are all of one kind: all missing, all zero, all the same
            let kind = self.below(10);
            let c = self.value();
            (0..len).map(|_| if kind == 0 { c } else { self.value() }).collect()
        }
        fn opt(&mut self) -> Option<f64> {
            (self.below(3) == 0).then(|| self.value())
        }
        fn size(&mut self, max: u64) -> u16 {
            match self.below(6) {
                0 => 0,
                1 => 1,
                _ => self.below(max) as u16,
            }
        }
    }

    #[test]
    fn fuzz_no_input_panics_and_widths_are_exact() {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        for n in 0..600usize {
            let (a, b) = (rng.values(), rng.values());
            let bounds = Bounds { min: rng.opt(), max: rng.opt() };
            let thr = rng.opt();
            let mut s = st();
            if n % 3 == 0 {
                s.marks = Marks::Blocks;
            }
            if n % 11 == 0 {
                // glyphs a config could set: wide, zero-width, a control character
                (s.missing_glyph, s.threshold_glyph, s.marker_glyph, s.legend_glyph) = ('汉', '\u{301}', '\n', '🙂');
                s.series.clear();
            }
            let (gw, gh) = (rng.size(60), rng.size(16));
            // the rectangle may hang off the grid, and the clip may cut it
            let r = Rect::new(rng.size(12), rng.size(6), rng.size(70), rng.size(20));
            let clip = if n % 4 == 0 { Rect::new(rng.size(20), rng.size(8), rng.size(40), rng.size(10)) } else { Rect::new(0, 0, gw, gh) };
            draw_all(gw, gh, r, clip, &s, &a, &b, bounds, thr, n);

            let w = rng.size(50);
            let exact = |ps: &[Piece], what: &str| assert_eq!(pieces_width(ps), w as usize, "{what} at width {w} (case {n})");
            for bucket in [Bucket::Last, Bucket::Mean, Bucket::Max, Bucket::Min] {
                exact(&Sparkline { values: &a, bounds, bucket, color: None, threshold: thr }.pieces(w, &s), "sparkline");
                assert!(resample(&a, w as usize, bucket).len() <= w as usize);
            }
            exact(&heat_pieces(&a, w, bounds, &s), "heat strip");
            if a.iter().any(|v| v.is_finite()) || w == 0 {
                exact(&box_pieces(&a, w, bounds, &s), "box row");
            }
            exact(&bar_pieces(a.first().copied().unwrap_or(1.0), b.first().copied().unwrap_or(1.0), w, BG), "bar");
            let bars: Vec<Bar> = a.iter().map(|v| Bar::new("日本 label", *v)).collect();
            let chart = BarChart { bars: &bars, max: bounds.max, values: n % 2 == 0, threshold: thr };
            for row in chart.rows(w, &s) {
                exact(&row, "bar row");
            }
            // the scale and reduction helpers take anything
            let (lo, hi) = (rng.value(), rng.value());
            let step = nice_step(lo, hi, rng.below(12) as usize);
            assert!(step > 0.0 && step.is_finite(), "nice_step({lo}, {hi}) = {step}");
            let ticks = ticks_in(lo, hi, step);
            assert_eq!(tick_labels(&ticks, step).len(), ticks.len());
            let _ = (fmt_num(lo), quantile(&a, hi), mean(&a), sturges(a.len()), finite_range(a.iter().copied()));
            let bins = bin(&a, rng.below(40) as usize, bounds);
            assert!(bins.counts.iter().sum::<u64>() <= a.len() as u64);

            let mut c = Braille::new(rng.size(20), rng.size(8));
            let mut k = Blocks::new(rng.size(20), rng.size(8));
            let mut coord = || match rng.below(5) {
                0 => i32::MIN,
                1 => i32::MAX,
                _ => rng.below(120) as i32 - 40,
            };
            for _ in 0..8 {
                let (x0, y0, x1, y1) = (coord(), coord(), coord(), coord());
                c.line(x0, y0, x1, y1, BG);
                c.set(x0, y1, BG);
                c.unset(x1, y0);
                c.vline(x0, y0.max(-500).min(500), y1.max(-500).min(500), BG);
                c.hline(y0, x0.max(-500).min(500), x1.max(-500).min(500), BG);
                c.fill_rect(x0.max(-99).min(99), y0.max(-99).min(99), x1.max(-99).min(99), y1.max(-99).min(99), BG);
                k.set(x0, y0, BG);
                k.fill(x0.max(-500).min(500), x1.max(-500).min(500), y1, BG);
                let _ = (c.get(x1, y1), k.height(x1));
            }
            let mut g = Grid::new(gw, gh);
            c.draw(&mut g, r.x, r.y, BG, clip);
            k.draw(&mut g, r.x, r.y, BG, clip);
        }
    }

    #[test]
    fn a_huge_rectangle_is_cut_not_allocated() {
        let s = st();
        let huge = Rect::new(0, 0, u16::MAX, u16::MAX);
        let far = Rect::new(u16::MAX, u16::MAX, u16::MAX, u16::MAX);
        let bars = days();
        for r in [huge, far] {
            let mut g = Grid::new(30, 6);
            let a = g.area();
            LinePlot::new(&[Series::new("p99", &LAT)]).fill(true).x(XAxis::Index).draw(&mut g, r, &s, BG, a);
            ColumnChart::new(&bars).draw(&mut g, r, &s, BG, a);
            Histogram::new(&LAT).marker(Marker::Mean).draw(&mut g, r, &s, BG, a);
            Scatter::new(&[(0.0, 0.0), (1.0, 1.0)]).draw(&mut g, r, &s, BG, a);
            BarChart::new(&bars).draw(&mut g, r, &s, BG, a);
            Heatmap::new(&[("web", &LAT)]).draw(&mut g, r, &s, BG, a);
            assert_eq!(g.to_text().lines().count(), 6);
        }
    }
}
