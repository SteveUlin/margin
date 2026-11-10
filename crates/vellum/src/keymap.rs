//! Vim-style key sequences: a count prefix (`5j`), multi-key bindings (`gg`, `zc`), and a timeout
//! that drops a half-typed sequence, like vim's `timeoutlen`. Generic over the app's action type:
//! the table lives with the app, the state machine here.
//!
//! The design follows modalkit's (the Rust vim-keybinding library iamb uses) in miniature: an app
//! that needs navigation, not text editing, wants the counts and sequences without a modal
//! editor's types (2026-11-08: a few hundred lines beat bridging a pre-1.0 crate's key events).

use crate::input::Key;

/// One key as the keymap sees it: the key and whether ctrl was held.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Press {
    pub key: Key,
    pub ctrl: bool,
}

impl Press {
    pub fn ch(c: char) -> Press {
        Press { key: Key::Char(c), ctrl: false }
    }
    pub fn ctrl(c: char) -> Press {
        Press { key: Key::Char(c), ctrl: true }
    }
    pub fn key(key: Key) -> Press {
        Press { key, ctrl: false }
    }
}

/// Parse a binding written vim-style: `gg`, `zc`, `<C-d>`, `<Down>`, `<CR>`, `<Space>`.
pub fn keys(spec: &str) -> Vec<Press> {
    let mut out = Vec::new();
    let mut rest = spec;
    while let Some(c) = rest.chars().next() {
        if c == '<' {
            if let Some(end) = rest.find('>') {
                let name = &rest[1..end];
                rest = &rest[end + 1..];
                let p = match name {
                    "CR" | "Enter" => Press::key(Key::Enter),
                    "Esc" => Press::key(Key::Esc),
                    "Space" => Press::ch(' '),
                    "Tab" => Press::key(Key::Tab),
                    "BS" => Press::key(Key::Backspace),
                    "Up" => Press::key(Key::Up),
                    "Down" => Press::key(Key::Down),
                    "Left" => Press::key(Key::Left),
                    "Right" => Press::key(Key::Right),
                    "Home" => Press::key(Key::Home),
                    "End" => Press::key(Key::End),
                    "PageUp" => Press::key(Key::PageUp),
                    "PageDown" => Press::key(Key::PageDown),
                    n if n.starts_with("C-") && n.len() == 3 => Press::ctrl(n.chars().nth(2).unwrap()),
                    _ => continue,
                };
                out.push(p);
                continue;
            }
        }
        out.push(Press::ch(c));
        rest = &rest[c.len_utf8()..];
    }
    out
}

#[derive(Debug, PartialEq)]
pub enum Fed<A> {
    /// A binding matched: the action and its count (1 when none was typed).
    Act(A, u32),
    /// A prefix of some binding, or a count: waiting for more keys.
    Pending,
    /// Nothing matches; the sequence was dropped.
    None,
}

pub struct Keymap<A: Clone> {
    binds: Vec<(Vec<Press>, A)>,
    seq: Vec<Press>,
    count: Option<u32>,
    started: f64,
    /// Seconds a half-typed sequence waits for its next key.
    pub timeout: f64,
}

impl<A: Clone> Keymap<A> {
    pub fn new(binds: &[(&str, A)]) -> Keymap<A> {
        Keymap { binds: binds.iter().map(|(s, a)| (keys(s), a.clone())).collect(), seq: Vec::new(), count: None, started: 0.0, timeout: 1.0 }
    }

    /// Is a sequence or count half-typed (for a status line)?
    pub fn pending(&self) -> String {
        let mut s = self.count.map(|c| c.to_string()).unwrap_or_default();
        for p in &self.seq {
            if let Key::Char(c) = p.key {
                if p.ctrl {
                    s.push('^');
                }
                s.push(c);
            }
        }
        s
    }

    /// Drop a half-typed sequence that has outlived the timeout; true if one was dropped (so the
    /// app can clear its showcmd without waiting for the next key).
    pub fn expire(&mut self, now: f64) -> bool {
        let half = !self.seq.is_empty() || self.count.is_some();
        if half && now - self.started > self.timeout {
            self.reset();
            return true;
        }
        false
    }

    pub fn reset(&mut self) {
        self.seq.clear();
        self.count = None;
    }

    pub fn feed(&mut self, p: Press, now: f64) -> Fed<A> {
        if (!self.seq.is_empty() || self.count.is_some()) && now - self.started > self.timeout {
            self.reset();
        }
        if self.seq.is_empty() && self.count.is_none() {
            self.started = now;
        }
        // a count: 1-9 starts it, 0 continues it (a bare 0 is a key like any other)
        if self.seq.is_empty() && !p.ctrl {
            if let Key::Char(c @ '0'..='9') = p.key {
                if c != '0' || self.count.is_some() {
                    let d = c as u32 - '0' as u32;
                    self.count = Some(self.count.unwrap_or(0).saturating_mul(10).saturating_add(d).min(9999));
                    return Fed::Pending;
                }
            }
        }
        self.seq.push(p);
        let exact = self.binds.iter().find(|(k, _)| *k == self.seq).map(|(_, a)| a.clone());
        let longer = self.binds.iter().any(|(k, _)| k.len() > self.seq.len() && k.starts_with(&self.seq));
        match (exact, longer) {
            // a binding that is also a prefix of a longer one waits (vim does, until the timeout);
            // margin's table avoids that shape, so exact wins at once
            (Some(a), _) => {
                let n = self.count.unwrap_or(1);
                self.reset();
                Fed::Act(a, n)
            }
            (None, true) => Fed::Pending,
            (None, false) => {
                self.reset();
                Fed::None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    enum A {
        Down,
        Top,
        Close,
        Half,
    }

    fn km() -> Keymap<A> {
        Keymap::new(&[("j", A::Down), ("<Down>", A::Down), ("gg", A::Top), ("zc", A::Close), ("<C-d>", A::Half)])
    }

    #[test]
    fn counts_sequences_and_ctrl() {
        let mut k = km();
        assert_eq!(k.feed(Press::ch('j'), 0.0), Fed::Act(A::Down, 1));
        assert_eq!(k.feed(Press::ch('1'), 0.0), Fed::Pending);
        assert_eq!(k.feed(Press::ch('2'), 0.0), Fed::Pending);
        assert_eq!(k.pending(), "12");
        assert_eq!(k.feed(Press::ch('j'), 0.1), Fed::Act(A::Down, 12));
        assert_eq!(k.feed(Press::ch('g'), 0.2), Fed::Pending);
        assert_eq!(k.feed(Press::ch('g'), 0.3), Fed::Act(A::Top, 1));
        assert_eq!(k.feed(Press::ctrl('d'), 0.4), Fed::Act(A::Half, 1));
        assert_eq!(k.feed(Press::key(Key::Down), 0.5), Fed::Act(A::Down, 1));
    }

    #[test]
    fn a_stale_or_wrong_sequence_is_dropped() {
        let mut k = km();
        assert_eq!(k.feed(Press::ch('z'), 0.0), Fed::Pending);
        // too slow: the z is forgotten, and c alone binds nothing
        assert_eq!(k.feed(Press::ch('c'), 5.0), Fed::None);
        assert_eq!(k.feed(Press::ch('z'), 6.0), Fed::Pending);
        assert_eq!(k.feed(Press::ch('q'), 6.1), Fed::None, "zq binds nothing");
        assert_eq!(k.feed(Press::ch('j'), 6.2), Fed::Act(A::Down, 1), "and the next key starts fresh");
    }

    #[test]
    fn specs_parse() {
        assert_eq!(keys("<C-u>"), vec![Press::ctrl('u')]);
        assert_eq!(keys("<Space>"), vec![Press::ch(' ')]);
        assert_eq!(keys("]]"), vec![Press::ch(']'), Press::ch(']')]);
    }
}
