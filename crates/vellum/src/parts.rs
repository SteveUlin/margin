//! vellum's parts: drawing primitives that know nothing about Claude. Each takes a grid, a place and
//! its state, draws, and returns where it ended. The look leans toward Charm's: rounded borders, pill
//! labels, gradient focus, a quiet help line, the Lip Gloss status bar.

use crate::anim::{ambient, Motion, Wake};
use crate::color::{cyclic, Palette, Rgb, StatusColors};
use crate::grid::Grid;
use crate::layout::Rect;
use crate::style::{Style, Underline};
use crate::text::width;

/// Nerd Font powerline round caps (JetBrainsMono Nerd Font Mono draws them one cell wide).
pub const CAP_L: &str = "\u{e0b6}";
pub const CAP_R: &str = "\u{e0b4}";
/// Bubble Tea's MiniDot spinner.
pub const SPIN: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const EIGHTHS: [&str; 8] = [" ", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];

/// A rounded label: half-circle caps around a coloured run.
pub fn pill(g: &mut Grid, x: u16, y: u16, text: &str, bg: Rgb, fg: Rgb, ground: Rgb, clip: Rect) -> u16 {
    g.put_clipped(x, y, CAP_L, Style::fg(bg).on(ground), clip);
    let e = g.put_clipped(x + 1, y, text, Style::fg(fg).on(bg).bold(), clip);
    g.put_clipped(e, y, CAP_R, Style::fg(bg).on(ground), clip);
    e + 1
}

/// A soft pill: tinted ground, coloured text (counts, kinds, warrants).
pub fn tag(g: &mut Grid, x: u16, y: u16, text: &str, fg: Rgb, ground: Rgb, clip: Rect) -> u16 {
    pill(g, x, y, text, fg.mix(ground, 0.78), fg, ground, clip)
}

/// A selection token: aqua with a dotted underline. `flash` in 0..1 blends toward an inverse
/// purple block, the acknowledgement that a double-click reached margin.
pub fn token(g: &mut Grid, x: u16, y: u16, tok: &str, p: &Palette, ground: Rgb, flash: f32, clip: Rect) -> u16 {
    if flash > 0.01 {
        let bg = ground.mix(p.purple, flash);
        let fg = p.aqua.mix(p.bg0, flash);
        return g.put_clipped(x, y, tok, Style::fg(fg).on(bg).bold(), clip);
    }
    let st = Style::fg(p.aqua).on(ground).underline(Underline::Dotted, Some(p.aqua.mix(p.bg0, 0.3)));
    g.put_clipped(x, y, tok, st, clip)
}

/// Bubble Tea's help line: bold key, grey description, a dim bullet between pairs.
pub fn help_line(g: &mut Grid, x: u16, y: u16, keys: &[(&str, &str)], p: &Palette, ground: Rgb, clip: Rect) -> u16 {
    let mut x = x;
    for (i, (k, d)) in keys.iter().enumerate() {
        if i > 0 {
            x = g.put_clipped(x, y, " • ", Style::fg(p.bg4).on(ground), clip);
        }
        x = g.put_clipped(x, y, k, Style::fg(p.grey2).on(ground).bold(), clip);
        x = g.put_clipped(x + 1, y, d, Style::fg(p.grey0).on(ground), clip);
    }
    x
}

/// Gradient progress bar with an eighth-block leading edge on a quiet track.
pub fn progress_bar(g: &mut Grid, x: u16, y: u16, w: u16, frac: f32, a: Rgb, b: Rgb, track: Rgb, clip: Rect) {
    let full = frac.clamp(0.0, 1.0) * w as f32;
    for k in 0..w {
        let col = a.mix(b, k as f32 / (w.max(2) - 1) as f32);
        let kf = k as f32;
        if kf + 1.0 <= full {
            g.put_clipped(x + k, y, "█", Style::fg(col).on(track), clip);
        } else if kf < full {
            let e = ((full - kf) * 8.0) as usize;
            g.put_clipped(x + k, y, EIGHTHS[e.min(7)], Style::fg(col).on(track), clip);
        } else {
            g.put_clipped(x + k, y, " ", Style::default().on(track), clip);
        }
    }
}

pub fn spinner(now: f64, m: Motion) -> &'static str {
    if !m.on {
        return SPIN[0];
    }
    SPIN[((now * 10.0) as usize) % SPIN.len()]
}

pub fn clock_text(secs: i64) -> String {
    let s = secs.unsigned_abs();
    let sign = if secs < 0 { "-" } else { "" };
    if s >= 3600 {
        format!("{sign}{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{sign}{:02}:{:02}", s / 60, s % 60)
    }
}

/// Eight-level sparkline; height and colour both carry the value.
pub fn sparkline(g: &mut Grid, x: u16, y: u16, vals: &[f32], a: Rgb, b: Rgb, ground: Rgb, clip: Rect) -> u16 {
    const CH: [&str; 8] = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
    let lo = vals.iter().cloned().fold(f32::MAX, f32::min);
    let hi = vals.iter().cloned().fold(f32::MIN, f32::max);
    for (k, v) in vals.iter().enumerate() {
        let t = if hi > lo { (v - lo) / (hi - lo) } else { 0.5 };
        g.put_clipped(x + k as u16, y, CH[(t * 7.0).round() as usize], Style::fg(a.mix(b, t)).on(ground), clip);
    }
    x + vals.len() as u16
}

// ---------------------------------------------------------------------------------------------
// frames (boxes)

#[derive(Clone, Debug)]
pub enum Ring {
    Plain(Rgb),
    /// A gradient that flows around the border, one lap per `period` seconds. Stops repeat
    /// cyclically, so the ring has no seam.
    Flow { stops: Vec<Rgb>, period: f32 },
    /// A breath between `base` and `peak`, `period` seconds per breath, for `breaths` breaths
    /// after `since`, then resting on `base`. Calm: a raised cosine, never a blink.
    Pulse { base: Rgb, peak: Rgb, period: f32, since: f64, breaths: f32 },
}

impl Ring {
    /// Colour at perimeter position `t` in [0,1).
    pub fn color(&self, t: f32, now: f64, m: Motion) -> Rgb {
        match self {
            Ring::Plain(c) => *c,
            Ring::Flow { stops, period } => cyclic(stops, t - ambient::phase(now, *period, m)),
            Ring::Pulse { base, peak, period, since, breaths } => {
                let el = (now - since) as f32;
                if !m.on || el < 0.0 || el > period * breaths * m.slow {
                    return *base;
                }
                base.mix(*peak, ambient::breath(now - since, *period, m))
            }
        }
    }

    /// Request the next frame if this ring is moving. Flow runs at a low rate: it moves about one
    /// cell per frame, so 12 fps reads smooth and costs little.
    pub fn wake(&self, now: f64, m: Motion, w: &mut Wake) {
        if !m.on {
            return;
        }
        match self {
            Ring::Plain(_) => {}
            Ring::Flow { .. } => w.fps(now, 12.0),
            Ring::Pulse { period, since, breaths, .. } => {
                if ((now - since) as f32) < period * breaths * m.slow {
                    w.fps(now, 20.0);
                }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Corners {
    Round,
    Light,
    Heavy,
    Double,
}

/// Draw a box border. The ring colour is sampled per cell along the perimeter, clockwise from the
/// top-left corner, so a Flow ring visibly travels around the panel.
/// A one-row edge: the ring's colours run along a horizontal rule, left to right. For a panel that
/// gives its side columns to content and keeps only a top edge (title and meta set into it).
pub fn rule(g: &mut Grid, x: u16, y: u16, w: u16, ring: &Ring, ground: Rgb, now: f64, m: Motion, clip: Rect) {
    for k in 0..w {
        let c = ring.color(k as f32 / w.max(1) as f32, now, m);
        g.put_clipped(x + k, y, "─", Style::fg(c).on(ground), clip);
    }
}

pub fn frame(g: &mut Grid, r: Rect, ring: &Ring, corners: Corners, ground: Rgb, now: f64, m: Motion, clip: Rect) {
    if r.w < 2 || r.h < 2 {
        return;
    }
    let (tl, tr, bl, br, hz, vt) = match corners {
        Corners::Round => ("╭", "╮", "╰", "╯", "─", "│"),
        Corners::Light => ("┌", "┐", "└", "┘", "─", "│"),
        Corners::Heavy => ("┏", "┓", "┗", "┛", "━", "┃"),
        Corners::Double => ("╔", "╗", "╚", "╝", "═", "║"),
    };
    let (w, h) = (r.w as u32, r.h as u32);
    let per = (2 * (w + h) - 4) as f32;
    let mut put = |x: u16, y: u16, s: &str, i: u32| {
        let c = ring.color(i as f32 / per, now, m);
        g.put_clipped(x, y, s, Style::fg(c).on(ground), clip);
    };
    let mut i = 0;
    for k in 0..w {
        let s = if k == 0 { tl } else if k == w - 1 { tr } else { hz };
        put(r.x + k as u16, r.y, s, i);
        i += 1;
    }
    for k in 1..h - 1 {
        put(r.right() - 1, r.y + k as u16, vt, i);
        i += 1;
    }
    for k in (0..w).rev() {
        let s = if k == 0 { bl } else if k == w - 1 { br } else { hz };
        put(r.x + k as u16, r.bottom() - 1, s, i);
        i += 1;
    }
    for k in (1..h - 1).rev() {
        put(r.x, r.y + k as u16, vt, i);
        i += 1;
    }
}

// ---------------------------------------------------------------------------------------------
// status bar

/// The Lip Gloss layout example's status bar: a coloured key block on the left, the bar's own text
/// filling the middle, and coloured "nuggets" on the right, all on a #353533 band with one-cell
/// padding per segment. Square segments, no caps: that's the look.
pub struct StatusBar<'a> {
    pub key: &'a str,
    pub key_bg: Option<Rgb>,
    pub text: &'a str,
    pub dim_text: &'a str,
    /// (text, background); drawn right-aligned in order.
    pub nuggets: Vec<(String, Rgb)>,
}

pub fn status_bar(g: &mut Grid, r: Rect, sb: &StatusBar, c: &StatusColors, clip: Rect) {
    let y = r.y;
    g.fill(Rect::new(r.x, y, r.w, 1).intersect(clip), Style::fg(c.bar_fg).on(c.bar_bg));
    let key_bg = sb.key_bg.unwrap_or(c.key_bg);
    let mut x = g.put_clipped(r.x, y, &format!(" {} ", sb.key), Style::fg(c.key_fg).on(key_bg).bold(), clip);
    x += 1; // Lip Gloss MarginRight(1)
    let right_w: u16 = sb.nuggets.iter().map(|(t, _)| width(t) as u16 + 2).sum();
    let avail = r.right().saturating_sub(right_w).saturating_sub(x + 1);
    let text = crate::text::truncate(sb.text, avail as usize);
    x = g.put_clipped(x, y, &text, Style::fg(c.bar_fg).on(c.bar_bg), clip);
    let rest = avail.saturating_sub(width(&text) as u16 + 1);
    if !sb.dim_text.is_empty() && rest > 2 {
        let t = crate::text::truncate(sb.dim_text, rest as usize);
        g.put_clipped(x + 1, y, &t, Style::fg(c.bar_fg.mix(c.bar_bg, 0.45)).on(c.bar_bg), clip);
    }
    let mut rx = r.right().saturating_sub(right_w);
    for (t, bg) in &sb.nuggets {
        rx = g.put_clipped(rx, y, &format!(" {t} "), Style::fg(c.nugget_fg).on(*bg), clip);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::{EVERFOREST, LIPGLOSS_STATUS};

    #[test]
    fn flow_ring_moves_and_motion_off_freezes_it() {
        let ring = Ring::Flow { stops: vec![Rgb(255, 0, 0), Rgb(0, 0, 255)], period: 4.0 };
        let m = Motion::default();
        assert_ne!(ring.color(0.0, 0.0, m), ring.color(0.0, 1.0, m));
        assert_eq!(ring.color(0.0, 0.0, Motion::off()), ring.color(0.0, 1.0, Motion::off()));
        let mut w = Wake::default();
        ring.wake(0.0, Motion::off(), &mut w);
        assert!(w.at.is_none());
    }

    #[test]
    fn pulse_rests_after_its_breaths() {
        let base = Rgb(10, 10, 10);
        let ring = Ring::Pulse { base, peak: Rgb(200, 200, 200), period: 2.0, since: 0.0, breaths: 2.0 };
        let m = Motion::default();
        assert_ne!(ring.color(0.0, 1.0, m), base);
        assert_eq!(ring.color(0.0, 4.5, m), base);
        let mut w = Wake::default();
        ring.wake(4.5, m, &mut w);
        assert!(w.at.is_none(), "a finished pulse costs no frames");
    }

    #[test]
    fn frame_draws_rounded_box() {
        let mut g = Grid::new(6, 3);
        let a = g.area();
        frame(&mut g, a, &Ring::Plain(Rgb(1, 1, 1)), Corners::Round, Rgb(0, 0, 0), 0.0, Motion::default(), a);
        assert_eq!(g.to_text(), "╭────╮\n│    │\n╰────╯\n");
    }

    #[test]
    fn status_bar_layout() {
        let mut g = Grid::new(40, 1);
        let a = g.area();
        let sb = StatusBar {
            key: "MARGIN",
            key_bg: None,
            text: "plan",
            dim_text: "~/project",
            nuggets: vec![("rev 9".into(), LIPGLOSS_STATUS.nugget_a), ("1 event".into(), LIPGLOSS_STATUS.nugget_b)],
        };
        status_bar(&mut g, a, &sb, &LIPGLOSS_STATUS, a);
        assert_eq!(g.to_text(), " MARGIN  plan ~/project  rev 9  1 event\n");
        assert_eq!(g.get(0, 0).unwrap().style.bg, Some(LIPGLOSS_STATUS.key_bg));
        assert_eq!(g.get(39, 0).unwrap().style.bg, Some(LIPGLOSS_STATUS.nugget_b));
        let _ = EVERFOREST;
    }

    #[test]
    fn progress_edges() {
        let mut g = Grid::new(4, 1);
        let a = g.area();
        progress_bar(&mut g, 0, 0, 4, 0.5625, Rgb(0, 0, 0), Rgb(9, 9, 9), Rgb(5, 5, 5), a);
        assert_eq!(g.to_text(), "██▎\n");
    }
}
