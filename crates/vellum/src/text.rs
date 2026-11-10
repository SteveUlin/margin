//! Text: NFC normalisation, grapheme clusters with terminal widths, wrapping and truncation.
//!
//! Why NFC: Zellij 0.45 drops combining characters on their way to the host terminal
//! (research.md: `e` + U+0301 arrived as a bare `e`), so decomposed text would lose its
//! accents. Composing first keeps `é` one codepoint that survives.

use std::borrow::Cow;
use unicode_normalization::{is_nfc_quick, IsNormalized, UnicodeNormalization};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

pub fn nfc(s: &str) -> Cow<'_, str> {
    match is_nfc_quick(s.chars()) {
        IsNormalized::Yes => Cow::Borrowed(s),
        _ => Cow::Owned(s.nfc().collect()),
    }
}

/// Terminal cell width of one grapheme cluster: 0, 1 or 2.
///
/// The cluster's first character decides (a base plus combining marks is as wide as its base),
/// except that an emoji variation selector (U+FE0F) or a ZWJ sequence widens it to 2, which is
/// how Ghostty and kitty draw them.
pub fn grapheme_width(g: &str) -> usize {
    let mut chars = g.chars();
    let Some(first) = chars.next() else { return 0 };
    if first.is_control() {
        return 0;
    }
    let mut w = UnicodeWidthChar::width(first).unwrap_or(0);
    if g.contains('\u{FE0F}') || (g.contains('\u{200D}') && w > 0) {
        w = 2;
    }
    w.min(2)
}

/// Iterate (grapheme, width) over NFC-normalised text, skipping zero-width clusters.
pub fn graphemes(s: &str) -> impl Iterator<Item = (&str, usize)> {
    s.graphemes(true).map(|g| (g, grapheme_width(g))).filter(|(_, w)| *w > 0)
}

pub fn width(s: &str) -> usize {
    s.graphemes(true).map(grapheme_width).sum()
}

/// Cut `s` to at most `max` cells, ending in `…` when anything was dropped.
pub fn truncate(s: &str, max: usize) -> Cow<'_, str> {
    if width(s) <= max {
        return Cow::Borrowed(s);
    }
    if max == 0 {
        return Cow::Borrowed("");
    }
    let mut out = String::new();
    let mut w = 0;
    for (g, gw) in graphemes(s) {
        if w + gw > max - 1 {
            break;
        }
        out.push_str(g);
        w += gw;
    }
    out.push('…');
    Cow::Owned(out)
}

/// Greedy word wrap by cell width. Words longer than a line break at grapheme boundaries.
/// Keeps explicit newlines as paragraph breaks.
pub fn wrap(s: &str, max: usize) -> Vec<String> {
    let max = max.max(1);
    let mut lines = Vec::new();
    for para in s.split('\n') {
        let mut line = String::new();
        let mut lw = 0;
        for word in para.split(' ') {
            let ww = width(word);
            let sep = if line.is_empty() { 0 } else { 1 };
            if lw + sep + ww <= max {
                if sep == 1 {
                    line.push(' ');
                }
                line.push_str(word);
                lw += sep + ww;
                continue;
            }
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
                lw = 0;
            }
            if ww <= max {
                line.push_str(word);
                lw = ww;
            } else {
                for (g, gw) in graphemes(word) {
                    if lw + gw > max {
                        lines.push(std::mem::take(&mut line));
                        lw = 0;
                    }
                    line.push_str(g);
                    lw += gw;
                }
            }
        }
        lines.push(line);
    }
    lines
}

/// Greedy word wrap over styled runs: the same rule as [`wrap`], but each word keeps the style of
/// the run it came from, and a word that spans two runs (`**bold**,`) keeps both pieces. Returns
/// lines of (text, style) pieces; adjacent pieces of one style are merged.
pub fn wrap_runs<T: Copy + PartialEq>(runs: &[(String, T)], max: usize) -> Vec<Vec<(String, T)>> {
    let max = max.max(1);
    // split into words, each a list of pieces; a space ends a word, a newline ends a line
    enum Tok<T> {
        Word(Vec<(String, T)>),
        Break,
    }
    let mut toks: Vec<Tok<T>> = Vec::new();
    let mut cur: Vec<(String, T)> = Vec::new();
    for (s, st) in runs {
        let mut piece = String::new();
        for ch in s.chars() {
            if ch == ' ' || ch == '\n' {
                if !piece.is_empty() {
                    cur.push((std::mem::take(&mut piece), *st));
                }
                if !cur.is_empty() {
                    toks.push(Tok::Word(std::mem::take(&mut cur)));
                }
                if ch == '\n' {
                    toks.push(Tok::Break);
                }
            } else {
                piece.push(ch);
            }
        }
        if !piece.is_empty() {
            cur.push((piece, *st));
        }
    }
    if !cur.is_empty() {
        toks.push(Tok::Word(cur));
    }
    fn push<T: Copy + PartialEq>(line: &mut Vec<(String, T)>, s: &str, st: T) {
        match line.last_mut() {
            Some((ls, lst)) if *lst == st => ls.push_str(s),
            _ => line.push((s.to_string(), st)),
        }
    }
    let mut lines: Vec<Vec<(String, T)>> = Vec::new();
    let mut line: Vec<(String, T)> = Vec::new();
    let mut lw = 0;
    for t in toks {
        let word = match t {
            Tok::Break => {
                lines.push(std::mem::take(&mut line));
                lw = 0;
                continue;
            }
            Tok::Word(w) => w,
        };
        let ww: usize = word.iter().map(|(s, _)| width(s)).sum();
        let sep = if lw == 0 { 0 } else { 1 };
        if lw + sep + ww > max && lw > 0 {
            lines.push(std::mem::take(&mut line));
            lw = 0;
        }
        if lw > 0 {
            // the space takes the style of the piece before it, so a bold phrase stays one run (styles
            // here are colours and weight, where a space's style doesn't show)
            let st = line.last().map(|(_, st)| *st).unwrap_or(word[0].1);
            push(&mut line, " ", st);
            lw += 1;
        }
        for (s, st) in word {
            for (g, gw) in graphemes(&s) {
                if lw + gw > max {
                    lines.push(std::mem::take(&mut line));
                    lw = 0;
                }
                push(&mut line, g, st);
                lw += gw;
            }
        }
    }
    lines.push(line);
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wrap_runs_keeps_styles() {
        let runs = vec![("the ".to_string(), 0u8), ("quick brown".to_string(), 1), (" fox".to_string(), 0)];
        let l = wrap_runs(&runs, 9);
        assert_eq!(l, vec![vec![("the ".into(), 0), ("quick".into(), 1)], vec![("brown ".into(), 1), ("fox".into(), 0)]]);
        let words: Vec<String> = wrap_runs(&[("the quick brown fox".to_string(), 0u8)], 9).into_iter().map(|l| l[0].0.clone()).collect();
        assert_eq!(words, wrap("the quick brown fox", 9));
    }
    #[test]
    fn widths() {
        assert_eq!(width("abc"), 3);
        assert_eq!(width("日本"), 4);
        assert_eq!(width("e\u{301}"), 1); // decomposed é is one cell
        assert_eq!(width("👍"), 2);
        assert_eq!(width("❤\u{FE0F}"), 2); // text-default heart made emoji by VS16
        assert_eq!(width("👨\u{200D}👩\u{200D}👧"), 2);
        assert_eq!(width("\u{1b}"), 0);
    }
    #[test]
    fn nfc_composes() {
        assert_eq!(nfc("e\u{301}"), "é");
        assert!(matches!(nfc("plain"), Cow::Borrowed(_)));
    }
    #[test]
    fn wrap_and_truncate() {
        assert_eq!(wrap("the quick brown fox", 9), vec!["the quick", "brown fox"]);
        assert_eq!(wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(wrap("日本語テキスト", 5), vec!["日本", "語テ", "キス", "ト"]);
        assert_eq!(truncate("hello world", 6), "hello…");
        assert_eq!(truncate("日本語", 4), "日…");
    }
}
