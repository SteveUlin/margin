//! The cell grid: the only path to the terminal.
//!
//! Components draw into a `Grid`; `diff` compares it with the previous frame and emits only the
//! changed cells, with minimal SGR transitions, cursor moves and OSC 8 open/close, wrapped in
//! synchronized output (DEC mode 2026) so Zellij and Ghostty present the frame atomically.
//! Every width, colour-bleed or link-bleed bug gets fixed here, once.

use crate::color::Rgb;
use crate::layout::Rect;
use crate::style::{sgr_transition, Style};
use crate::text::{graphemes, nfc};
use std::fmt::Write;

/// A cell's symbol. Most cells hold one `char`, so they never allocate.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Sym {
    Char(char),
    Str(Box<str>),
}

impl Sym {
    pub fn as_str<'a>(&'a self, buf: &'a mut [u8; 4]) -> &'a str {
        match self {
            Sym::Char(c) => c.encode_utf8(buf),
            Sym::Str(s) => s,
        }
    }
    fn from(g: &str) -> Sym {
        let mut it = g.chars();
        match (it.next(), it.next()) {
            (Some(c), None) => Sym::Char(c),
            _ => Sym::Str(g.into()),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Cell {
    pub sym: Sym,
    /// 1 or 2 for a lead cell; 0 marks the right half of a wide grapheme.
    pub width: u8,
    pub style: Style,
}

impl Default for Cell {
    fn default() -> Self {
        Cell { sym: Sym::Char(' '), width: 1, style: Style::default() }
    }
}

#[derive(Clone, Debug)]
pub struct Grid {
    pub w: u16,
    pub h: u16,
    pub cells: Vec<Cell>,
    /// OSC 8 targets; `Style::link` indexes this (1-based).
    pub links: Vec<String>,
}

impl Grid {
    pub fn new(w: u16, h: u16) -> Grid {
        Grid { w, h, cells: vec![Cell::default(); w as usize * h as usize], links: Vec::new() }
    }

    pub fn area(&self) -> Rect {
        Rect::new(0, 0, self.w, self.h)
    }

    fn idx(&self, x: u16, y: u16) -> usize {
        y as usize * self.w as usize + x as usize
    }

    pub fn get(&self, x: u16, y: u16) -> Option<&Cell> {
        (x < self.w && y < self.h).then(|| &self.cells[self.idx(x, y)])
    }

    pub fn get_mut(&mut self, x: u16, y: u16) -> Option<&mut Cell> {
        if x < self.w && y < self.h {
            let i = self.idx(x, y);
            Some(&mut self.cells[i])
        } else {
            None
        }
    }

    pub fn link(&mut self, url: &str) -> u16 {
        // the URL goes inside an OSC 8 sequence verbatim: a control byte (ESC, BEL) in it would
        // end that sequence early and let the rest act as terminal commands, so drop them
        let clean: String = url.chars().filter(|c| !c.is_control()).collect();
        let url = clean.as_str();
        if let Some(i) = self.links.iter().position(|l| l == url) {
            return i as u16 + 1;
        }
        self.links.push(url.to_string());
        self.links.len() as u16
    }

    /// Clear one cell, repairing any wide grapheme it was half of.
    fn clear_at(&mut self, x: u16, y: u16) {
        let i = self.idx(x, y);
        let style = self.cells[i].style;
        if self.cells[i].width == 0 && x > 0 {
            let j = self.idx(x - 1, y);
            self.cells[j].sym = Sym::Char(' ');
            self.cells[j].width = 1;
        }
        if self.cells[i].width == 2 && x + 1 < self.w {
            let j = self.idx(x + 1, y);
            self.cells[j] = Cell { sym: Sym::Char(' '), width: 1, style: self.cells[j].style };
        }
        self.cells[i] = Cell { sym: Sym::Char(' '), width: 1, style };
    }

    /// Put text at (x, y) clipped to `clip`; returns the x after the last drawn cell.
    /// A wide grapheme that would straddle the clip edge becomes a space, as terminals do.
    pub fn put_clipped(&mut self, x: u16, y: u16, s: &str, style: Style, clip: Rect) -> u16 {
        let s = nfc(s);
        let mut cx = x as i32;
        if y < clip.y || y >= clip.bottom() || y >= self.h {
            return x + crate::text::width(&s) as u16;
        }
        for (g, gw) in graphemes(&s) {
            let right = clip.right().min(self.w) as i32;
            if cx >= right {
                cx += gw as i32;
                continue;
            }
            if cx < clip.x as i32 {
                cx += gw as i32;
                continue;
            }
            let ux = cx as u16;
            if gw == 2 && cx + 1 >= right {
                self.clear_at(ux, y);
                let i = self.idx(ux, y);
                self.cells[i].style = style;
                cx += 2;
                continue;
            }
            self.clear_at(ux, y);
            if gw == 2 {
                self.clear_at(ux + 1, y);
            }
            let i = self.idx(ux, y);
            self.cells[i] = Cell { sym: Sym::from(g), width: gw as u8, style };
            if gw == 2 {
                self.cells[i + 1] = Cell { sym: Sym::Char(' '), width: 0, style };
            }
            cx += gw as i32;
        }
        cx.max(0) as u16
    }

    pub fn put(&mut self, x: u16, y: u16, s: &str, style: Style) -> u16 {
        let a = self.area();
        self.put_clipped(x, y, s, style, a)
    }

    pub fn fill(&mut self, r: Rect, style: Style) {
        let r = r.intersect(self.area());
        for y in r.y..r.bottom() {
            for x in r.x..r.right() {
                self.clear_at(x, y);
                let i = self.idx(x, y);
                self.cells[i].style = style;
            }
        }
    }

    /// Change the background under a rectangle, keeping the text.
    pub fn tint(&mut self, r: Rect, bg: Rgb) {
        let r = r.intersect(self.area());
        for y in r.y..r.bottom() {
            for x in r.x..r.right() {
                let i = self.idx(x, y);
                self.cells[i].style.bg = Some(bg);
            }
        }
    }

    /// Copy `src`'s rows [sy, sy+h) into self at (dx, dy), clipped to `clip`.
    pub fn blit(&mut self, src: &Grid, sy: u16, h: u16, dx: u16, dy: i32, clip: Rect) {
        let clip = clip.intersect(self.area());
        for row in 0..h {
            let ty = dy + row as i32;
            if ty < clip.y as i32 || ty >= clip.bottom() as i32 {
                continue;
            }
            let syy = sy + row;
            if syy >= src.h {
                break;
            }
            for x in 0..src.w {
                let tx = dx + x;
                if tx < clip.x || tx >= clip.right() {
                    continue;
                }
                let mut c = src.cells[src.idx(x, syy)].clone();
                if c.style.link != 0 {
                    let url = src.links[c.style.link as usize - 1].clone();
                    c.style.link = self.link(&url);
                }
                // a wide lead whose right half falls outside the clip becomes a space
                if c.width == 2 && tx + 1 >= clip.right() {
                    c = Cell { sym: Sym::Char(' '), width: 1, style: c.style };
                }
                if c.width == 0 && tx == clip.x {
                    c = Cell { sym: Sym::Char(' '), width: 1, style: c.style };
                }
                let i = self.idx(tx, ty as u16);
                self.cells[i] = c;
            }
        }
    }

    /// Plain text of the grid, one line per row, trailing spaces trimmed (snapshot tests).
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        let mut buf = [0u8; 4];
        for y in 0..self.h {
            let mut line = String::new();
            for x in 0..self.w {
                let c = &self.cells[self.idx(x, y)];
                if c.width == 0 {
                    continue;
                }
                line.push_str(c.sym.as_str(&mut buf));
            }
            out.push_str(line.trim_end());
            out.push('\n');
        }
        out
    }

    /// The whole grid as ANSI lines, for `margin render --ansi` (no cursor addressing).
    pub fn to_ansi(&self) -> String {
        let mut out = String::new();
        let mut buf = [0u8; 4];
        for y in 0..self.h {
            let mut cur = Style::default();
            let mut link = 0u16;
            for x in 0..self.w {
                let c = &self.cells[self.idx(x, y)];
                if c.width == 0 {
                    continue;
                }
                emit_style(&mut out, &mut cur, &c.style);
                emit_link(&mut out, &mut link, c.style.link, &self.links);
                out.push_str(c.sym.as_str(&mut buf));
            }
            emit_link(&mut out, &mut link, 0, &self.links);
            out.push_str("\x1b[0m\n");
        }
        out
    }
}

fn emit_style(out: &mut String, cur: &mut Style, next: &Style) {
    let mut p = String::new();
    if sgr_transition(cur, next, &mut p) {
        out.push_str("\x1b[");
        out.push_str(&p);
        out.push('m');
    }
    *cur = *next;
}

fn emit_link(out: &mut String, cur: &mut u16, next: u16, links: &[String]) {
    if *cur == next {
        return;
    }
    if next == 0 {
        out.push_str("\x1b]8;;\x1b\\");
    } else {
        let _ = write!(out, "\x1b]8;;{}\x1b\\", links[next as usize - 1]);
    }
    *cur = next;
}

/// Emit the escape sequences that turn `prev` (what the terminal shows) into `next`.
/// With `prev = None` (first frame, resize) the whole screen is redrawn.
pub fn diff(prev: Option<&Grid>, next: &Grid, out: &mut String) -> usize {
    let full = match prev {
        Some(p) => p.w != next.w || p.h != next.h,
        None => true,
    };
    let mut cur = Style::default();
    let mut link = 0u16;
    let mut written = 0usize;
    // cursor position as the terminal sees it; None = unknown
    let mut at: Option<(u16, u16)> = None;
    out.push_str("\x1b[0m");
    if full {
        out.push_str("\x1b[H\x1b[2J");
        at = Some((0, 0));
    }
    let mut buf = [0u8; 4];
    for y in 0..next.h {
        for x in 0..next.w {
            let i = next.idx(x, y);
            let c = &next.cells[i];
            if c.width == 0 {
                continue;
            }
            let changed = full || {
                let p = prev.unwrap();
                p.cells[i] != *c
                    // a wide cell's right half changed underneath (e.g. old wide glyph replaced)
                    || (c.width == 2 && x + 1 < next.w && p.cells[i + 1] != next.cells[i + 1])
                    || link_target(p, i) != link_target(next, i)
            };
            if !changed {
                continue;
            }
            if at != Some((x, y)) {
                let _ = write!(out, "\x1b[{};{}H", y + 1, x + 1);
            }
            emit_style(out, &mut cur, &c.style);
            emit_link(out, &mut link, c.style.link, &next.links);
            out.push_str(c.sym.as_str(&mut buf));
            written += 1;
            let nx = x + c.width as u16;
            // after writing the last column the cursor's position is terminal-specific (pending wrap)
            at = if nx >= next.w { None } else { Some((nx, y)) };
        }
    }
    emit_link(out, &mut link, 0, &next.links);
    out.push_str("\x1b[0m");
    written
}

fn link_target(g: &Grid, i: usize) -> Option<&str> {
    let l = g.cells[i].style.link;
    (l != 0).then(|| g.links[l as usize - 1].as_str())
}

/// Wrap a frame in synchronized output so the terminal shows it all at once.
pub fn synchronized(body: &str) -> String {
    format!("\x1b[?2026h{body}\x1b[?2026l")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn link_drops_control_bytes() {
        let mut g = Grid::new(4, 1);
        let id = g.link("https://x\x1b]0;PWNED\x07/");
        let mut st = crate::style::Style::default();
        st.link = id;
        g.put(0, 0, "ab", st);
        let mut out = String::new();
        diff(None, &g, &mut out);
        assert!(!out.contains("\x1b]0;") && !out.contains('\x07'), "{out:?}");
    }
    #[test]
    fn put_wide_and_overwrite() {
        let mut g = Grid::new(6, 1);
        g.put(0, 0, "a日b", Style::default());
        assert_eq!(g.to_text(), "a日b\n");
        assert_eq!(g.cells[2].width, 0);
        // overwriting the right half of a wide glyph clears the left half too
        g.put(2, 0, "x", Style::default());
        assert_eq!(g.to_text(), "a xb\n");
        // a wide glyph at the clip edge becomes a space
        let mut g = Grid::new(3, 1);
        g.put(2, 0, "日", Style::default());
        assert_eq!(g.to_text(), "\n");
    }
    #[test]
    fn nfc_on_put() {
        let mut g = Grid::new(4, 1);
        g.put(0, 0, "e\u{301}x", Style::default());
        assert_eq!(g.to_text(), "éx\n");
    }
    #[test]
    fn diff_emits_only_changes() {
        let mut a = Grid::new(10, 2);
        a.put(0, 0, "hello", Style::default());
        let mut b = a.clone();
        let mut out = String::new();
        assert_eq!(diff(Some(&a), &b, &mut out), 0);
        b.put(1, 1, "yo", Style::fg(Rgb(1, 2, 3)));
        out.clear();
        assert_eq!(diff(Some(&a), &b, &mut out), 2);
        assert!(out.contains("\x1b[2;2H"));
        assert!(out.contains("38;2;1;2;3"));
        // consecutive cells don't repeat the cursor move
        assert_eq!(out.matches('H').count(), 1);
    }
    #[test]
    fn diff_full_on_resize_and_links_close() {
        let mut g = Grid::new(4, 1);
        let l = g.link("https://x.test");
        g.put(0, 0, "ab", Style { link: l, ..Default::default() });
        let mut out = String::new();
        diff(None, &g, &mut out);
        assert!(out.contains("\x1b[2J"));
        assert!(out.contains("\x1b]8;;https://x.test\x1b\\ab\x1b]8;;\x1b\\"));
    }
    #[test]
    fn blit_clips() {
        let mut src = Grid::new(4, 3);
        src.put(0, 0, "aaaa", Style::default());
        src.put(0, 1, "bbbb", Style::default());
        src.put(0, 2, "cccc", Style::default());
        let mut dst = Grid::new(4, 3);
        dst.blit(&src, 1, 2, 0, 0, dst.area());
        assert_eq!(dst.to_text(), "bbbb\ncccc\n\n");
        let mut dst = Grid::new(4, 3);
        dst.blit(&src, 0, 3, 0, -2, dst.area());
        assert_eq!(dst.to_text(), "cccc\n\n\n");
    }
}
