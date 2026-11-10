//! Input: bytes from the terminal to events.
//!
//! Covers legacy keys, the kitty keyboard protocol (`CSI codepoint;mods u`), SGR mouse, focus,
//! bracketed paste, and the replies a terminal sends to queries (DA1, DECRPM, window sizes).
//! Kitty graphics replies (`ESC _ G … ESC \`) come back as their own event: Zellij writes them into
//! the pane's input (research.md), so they must never be read as keystrokes.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Mods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Tab,
    BackTab,
    Backspace,
    Esc,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    F(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseKind {
    Press(u8),
    Release(u8),
    Drag(u8),
    Move,
    WheelUp,
    WheelDown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Key(Key, Mods),
    Mouse { kind: MouseKind, x: u16, y: u16, mods: Mods },
    Paste(String),
    Focus(bool),
    /// `ESC _ G <payload> ESC \`: a kitty graphics reply (`i=31;OK`, `ENOTSUPPORTED:…`).
    KittyReply(String),
    /// Primary device attributes reply (`CSI ? … c`): the end marker of a capability probe.
    DeviceAttrs(String),
    /// DECRPM: (mode, setting).
    ModeReport(u32, u8),
    /// `CSI 4;h;w t` (window px) or `CSI 6;h;w t` (cell px).
    PixelSize { kind: u8, h: u16, w: u16 },
    /// Anything recognised as a terminal reply but not interesting (OSC/DCS replies).
    Ignored,
}

/// Seconds of quiet after which a partial sequence is given up on (and a lone ESC is the Esc key).
pub const QUIET: f64 = 0.030;
/// Seconds of quiet after which an open paste whose end marker never came is delivered as it
/// stands, so a lost `ESC [ 201 ~` can't hold the keyboard.
pub const PASTE_QUIET: f64 = 2.0;
/// Bytes of paste kept; the rest is dropped up to the end marker.
pub const PASTE_MAX: usize = 1 << 20;
/// Seconds an abandoned sequence's late remainder is still discarded rather than typed.
const SKIP_QUIET: f64 = 1.0;

/// The rest of a sequence `flush` gave up on, still to be discarded if it arrives late.
#[derive(Clone, Copy)]
enum Skip {
    /// CSI/SS3: parameters and intermediates up to the final byte.
    Csi,
    /// APC/DCS/OSC: up to ST (ESC \) or BEL.
    Str,
}

#[derive(Default)]
pub struct Parser {
    buf: Vec<u8>,
    paste: Option<Vec<u8>>,
    /// When the last byte arrived (the caller's clock, seconds).
    last: f64,
    skip: Option<(Skip, f64)>,
}

fn mods_from(n: u32) -> Mods {
    let b = n.saturating_sub(1);
    Mods { shift: b & 1 != 0, alt: b & 2 != 0, ctrl: b & 4 != 0 }
}

impl Parser {
    pub fn new() -> Parser {
        Parser::default()
    }

    /// Feed bytes read at `now` (seconds, any monotonic clock); returns complete events.
    /// Incomplete sequences wait for more bytes; `flush` gives up on them once `deadline` passes,
    /// which is also what turns a lone ESC into the Esc key.
    pub fn feed(&mut self, bytes: &[u8], now: f64) -> Vec<Event> {
        if !bytes.is_empty() {
            self.last = now;
        }
        if self.skip.is_some_and(|(_, since)| now - since > SKIP_QUIET) {
            self.skip = None;
        }
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        loop {
            if let Some(p) = &mut self.paste {
                const END: &[u8] = b"\x1b[201~";
                if let Some(i) = find(&self.buf, END) {
                    let room = PASTE_MAX.saturating_sub(p.len()).min(i);
                    p.extend_from_slice(&self.buf[..room]);
                    let text = String::from_utf8_lossy(p).into_owned();
                    self.paste = None;
                    self.buf.drain(..i + END.len());
                    out.push(Event::Paste(text));
                    continue;
                }
                // hold back a possible partial end marker; `flush` leaves it alone
                let keep = self.buf.len().saturating_sub(END.len() - 1);
                let room = PASTE_MAX.saturating_sub(p.len()).min(keep);
                p.extend_from_slice(&self.buf[..room]);
                self.buf.drain(..keep);
                break;
            }
            if self.buf.is_empty() {
                break;
            }
            if let Some((kind, _)) = self.skip {
                if self.discard(kind) {
                    continue;
                }
                break;
            }
            match self.parse_one() {
                Some((ev, n)) => {
                    self.buf.drain(..n);
                    if let Some(ev) = ev {
                        out.push(ev);
                    }
                }
                None => break,
            }
        }
        out
    }

    /// Give up on what is still incomplete, if its `deadline` has passed at `now`; before that
    /// this does nothing, so a wake for any other reason can't cut a sequence in half.
    pub fn flush(&mut self, now: f64) -> Vec<Event> {
        let mut out = Vec::new();
        if !self.deadline().is_some_and(|d| now >= d) {
            return out;
        }
        if let Some(mut p) = self.paste.take() {
            // the end marker never came: deliver what did
            let room = PASTE_MAX.saturating_sub(p.len()).min(self.buf.len());
            p.extend_from_slice(&self.buf[..room]);
            self.buf.clear();
            out.push(Event::Paste(String::from_utf8_lossy(&p).into_owned()));
            return out;
        }
        self.skip = None;
        if self.buf == b"\x1b" {
            out.push(Event::Key(Key::Esc, Mods::default()));
        } else if self.buf[0] == 0x1b {
            // a stale partial escape sequence: drop it rather than type its bytes, and drop its
            // remainder too if that turns up late
            self.skip = match self.buf[1] {
                b'[' | b'O' => Some((Skip::Csi, now)),
                b'_' | b'P' | b']' => Some((Skip::Str, now)),
                _ => None,
            };
        }
        // (else a stale partial UTF-8 character: dropped)
        self.buf.clear();
        out
    }

    /// When `flush` will act: the quiet period after the last byte, while something is incomplete.
    /// None = nothing to wait for.
    pub fn deadline(&self) -> Option<f64> {
        if self.paste.is_some() {
            Some(self.last + PASTE_QUIET)
        } else if !self.buf.is_empty() {
            Some(self.last + QUIET)
        } else {
            None
        }
    }

    /// Discard an abandoned sequence's remainder from the buffer head. False = wants more bytes.
    fn discard(&mut self, kind: Skip) -> bool {
        let b = &self.buf;
        let mut i = 0;
        match kind {
            Skip::Csi => {
                while i < b.len() && (0x20..=0x3f).contains(&b[i]) {
                    i += 1;
                }
                if i == b.len() {
                    self.buf.clear();
                    return false;
                }
                // anything but a final byte is not this sequence's: parse it afresh
                if (0x40..=0x7e).contains(&b[i]) {
                    i += 1;
                }
            }
            Skip::Str => loop {
                if i == b.len() {
                    self.buf.clear();
                    return false;
                }
                if b[i] == 0x07 {
                    i += 1;
                    break;
                }
                if b[i] == 0x1b {
                    if i + 1 == b.len() {
                        self.buf.drain(..i);
                        return false;
                    }
                    // ST ends it; any other ESC starts something new
                    if b[i + 1] == b'\\' {
                        i += 2;
                    }
                    break;
                }
                i += 1;
            },
        }
        self.buf.drain(..i);
        self.skip = None;
        true
    }

    /// Parse one event from the buffer head. None = incomplete; Some((None, n)) = consumed n bytes
    /// with no event.
    fn parse_one(&mut self) -> Option<(Option<Event>, usize)> {
        let b = &self.buf;
        let c = b[0];
        if c != 0x1b {
            return self.plain_at(0);
        }
        if b.len() == 1 {
            return None;
        }
        match b[1] {
            b'[' => self.csi(),
            b'O' => {
                if b.len() < 3 {
                    return None;
                }
                let k = match b[2] {
                    b'A' => Key::Up,
                    b'B' => Key::Down,
                    b'C' => Key::Right,
                    b'D' => Key::Left,
                    b'H' => Key::Home,
                    b'F' => Key::End,
                    b'P' => Key::F(1),
                    b'Q' => Key::F(2),
                    b'R' => Key::F(3),
                    b'S' => Key::F(4),
                    _ => return Some((None, 3)),
                };
                Some((Some(Event::Key(k, Mods::default())), 3))
            }
            b'_' | b'P' | b']' => {
                // APC, DCS, OSC: run to ST (ESC \) or BEL
                let mut i = 2;
                while i < b.len() {
                    if b[i] == 0x07 {
                        return Some((Some(self.string_event(2, i)), i + 1));
                    }
                    if b[i] == 0x1b && i + 1 < b.len() && b[i + 1] == b'\\' {
                        return Some((Some(self.string_event(2, i)), i + 2));
                    }
                    if b[i] == 0x1b && i + 1 == b.len() {
                        return None;
                    }
                    i += 1;
                }
                None
            }
            0x1b => Some((Some(Event::Key(Key::Esc, Mods::default())), 1)),
            _ => {
                // ESC + key = alt + key
                let (ev, n) = self.plain_at(1)?;
                let ev = ev.map(|e| match e {
                    Event::Key(k, m) => Event::Key(k, Mods { alt: true, ..m }),
                    e => e,
                });
                Some((ev, n + 1))
            }
        }
    }

    fn string_event(&self, start: usize, end: usize) -> Event {
        let kind = self.buf[1];
        let body = String::from_utf8_lossy(&self.buf[start..end]).into_owned();
        if kind == b'_' && body.starts_with('G') {
            Event::KittyReply(body[1..].to_string())
        } else {
            Event::Ignored
        }
    }


    fn plain_at(&self, at: usize) -> Option<(Option<Event>, usize)> {
        let b = &self.buf[at..];
        let c = *b.first()?;
        let none = Mods::default();
        let ctrl = Mods { ctrl: true, ..none };
        let ev = match c {
            b'\r' | b'\n' => Event::Key(Key::Enter, none),
            b'\t' => Event::Key(Key::Tab, none),
            0x7f | 0x08 => Event::Key(Key::Backspace, none),
            0x00 => Event::Key(Key::Char(' '), ctrl),
            0x01..=0x1a => Event::Key(Key::Char((b'a' + c - 1) as char), ctrl),
            0x1b => Event::Key(Key::Esc, none),
            0x1c..=0x1f => Event::Key(Key::Char((b'4' + c - 0x1c) as char), ctrl),
            _ => {
                let len = match c {
                    0x00..=0x7f => 1,
                    0xc2..=0xdf => 2,
                    0xe0..=0xef => 3,
                    0xf0..=0xf4 => 4,
                    // a stray continuation byte or an impossible lead: skip just this byte. Waiting
                    // for "the rest" swallowed the next keystrokes as its continuation
                    _ => return Some((None, 1)),
                };
                // only real continuation bytes may complete it
                if b[1..len.min(b.len())].iter().any(|&x| x & 0xc0 != 0x80) {
                    return Some((None, 1));
                }
                if b.len() < len {
                    return None;
                }
                let s = std::str::from_utf8(&b[..len]).ok();
                return Some((s.and_then(|s| s.chars().next()).map(|ch| Event::Key(Key::Char(ch), none)), len));
            }
        };
        Some((Some(ev), 1))
    }

    fn csi(&mut self) -> Option<(Option<Event>, usize)> {
        let b = &self.buf;
        // find the final byte (0x40..=0x7e) after parameters/intermediates
        let mut i = 2;
        while i < b.len() && !(0x40..=0x7e).contains(&b[i]) {
            if b[i] == 0x1b {
                // an ESC in the parameters: that sequence is broken, a new one starts here
                return Some((None, i));
            }
            i += 1;
        }
        if i >= b.len() {
            return None;
        }
        // SGR mouse needs the params; legacy X10 mouse (CSI M + 3 bytes) is never enabled
        let fin = b[i];
        let params = String::from_utf8_lossy(&b[2..i]).into_owned();
        let n = i + 1;
        if params == "200" && fin == b'~' {
            self.paste = Some(Vec::new());
            return Some((None, n));
        }
        let ev = parse_csi(&params, fin);
        Some((ev, n))
    }
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

fn nums(s: &str) -> Vec<u32> {
    s.split(|c| c == ';' || c == ':').map(|p| p.parse().unwrap_or(0)).collect()
}

fn parse_csi(params: &str, fin: u8) -> Option<Event> {
    let none = Mods::default();
    if let Some(p) = params.strip_prefix('<') {
        if fin == b'M' || fin == b'm' {
            let v = nums(p);
            if v.len() < 3 {
                return None;
            }
            let (cb, x, y) = (v[0], v[1].saturating_sub(1) as u16, v[2].saturating_sub(1) as u16);
            let mods = Mods { shift: cb & 4 != 0, alt: cb & 8 != 0, ctrl: cb & 16 != 0 };
            let btn = (cb & 3) as u8;
            let kind = if cb & 64 != 0 {
                match btn {
                    0 => MouseKind::WheelUp,
                    1 => MouseKind::WheelDown,
                    _ => return None, // horizontal wheel
                }
            } else if cb & 32 != 0 {
                if btn == 3 { MouseKind::Move } else { MouseKind::Drag(btn) }
            } else if fin == b'M' {
                MouseKind::Press(btn)
            } else {
                MouseKind::Release(btn)
            };
            return Some(Event::Mouse { kind, x, y, mods });
        }
    }
    if let Some(p) = params.strip_prefix('?') {
        return match fin {
            b'c' => Some(Event::DeviceAttrs(p.to_string())),
            b'y' => {
                let v = nums(p.trim_end_matches('$'));
                Some(Event::ModeReport(v.first().copied().unwrap_or(0), v.get(1).copied().unwrap_or(0) as u8))
            }
            b'u' => Some(Event::Ignored), // kitty keyboard flags reply
            _ => Some(Event::Ignored),
        };
    }
    if fin == b'y' && params.ends_with('$') {
        return Some(Event::Ignored);
    }
    let v = nums(params);
    let m = v.get(1).map(|&n| mods_from(n)).unwrap_or(none);
    let key = |k: Key| Some(Event::Key(k, m));
    match fin {
        b'A' => key(Key::Up),
        b'B' => key(Key::Down),
        b'C' => key(Key::Right),
        b'D' => key(Key::Left),
        b'H' => key(Key::Home),
        b'F' => key(Key::End),
        b'Z' => Some(Event::Key(Key::BackTab, Mods { shift: true, ..none })),
        b'I' if params.is_empty() => Some(Event::Focus(true)),
        b'O' if params.is_empty() => Some(Event::Focus(false)),
        b'P' => key(Key::F(1)),
        b'Q' => key(Key::F(2)),
        b'S' => key(Key::F(4)),
        b't' => {
            if v.len() >= 3 && (v[0] == 4 || v[0] == 6) {
                Some(Event::PixelSize { kind: v[0] as u8, h: v[1] as u16, w: v[2] as u16 })
            } else {
                Some(Event::Ignored)
            }
        }
        b'~' => match v.first().copied().unwrap_or(0) {
            1 | 7 => key(Key::Home),
            2 => key(Key::Insert),
            3 => key(Key::Delete),
            4 | 8 => key(Key::End),
            5 => key(Key::PageUp),
            6 => key(Key::PageDown),
            11..=15 => key(Key::F((v[0] - 10) as u8)),
            17..=21 => key(Key::F((v[0] - 11) as u8)),
            23 | 24 => key(Key::F((v[0] - 12) as u8)),
            _ => Some(Event::Ignored),
        },
        b'u' => {
            // kitty keyboard: CSI codepoint[:alt] ; mods[:event] u
            let mut parts = params.split(';');
            let cp: u32 = parts.next().and_then(|s| s.split(':').next()).and_then(|s| s.parse().ok())?;
            let mods = parts
                .next()
                .and_then(|s| s.split(':').next())
                .and_then(|s| s.parse().ok())
                .map(mods_from)
                .unwrap_or(none);
            let k = match cp {
                13 => Key::Enter,
                9 => Key::Tab,
                27 => Key::Esc,
                127 | 8 => Key::Backspace,
                _ => Key::Char(char::from_u32(cp)?),
            };
            Some(Event::Key(k, mods))
        }
        _ => Some(Event::Ignored),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn invalid_utf8_keeps_following_keys() {
        let mut p = Parser::new();
        let ev = p.feed(b"\x80jjj", 0.0);
        assert_eq!(ev.iter().filter(|e| matches!(e, Event::Key(Key::Char('j'), _))).count(), 3, "{ev:?}");
        let ev = p.feed(b"\xe2j", 0.0);
        assert!(ev.iter().any(|e| matches!(e, Event::Key(Key::Char('j'), _))), "{ev:?}");
        p.feed(b"\xe2\x94", 0.0);
        assert!(p.deadline().is_some());
        p.flush(QUIET);
        assert!(p.deadline().is_none());
    }

    use super::*;
    fn k(c: char) -> Event {
        Event::Key(Key::Char(c), Mods::default())
    }
    #[test]
    fn plain_and_utf8() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"j\r", 0.0), vec![k('j'), Event::Key(Key::Enter, Mods::default())]);
        assert_eq!(p.feed("é".as_bytes(), 0.0), vec![k('é')]);
        // split utf-8 across reads
        let bytes = "日".as_bytes();
        assert_eq!(p.feed(&bytes[..1], 0.0), vec![]);
        assert_eq!(p.feed(&bytes[1..], 0.0), vec![k('日')]);
        assert_eq!(p.feed(b"\x03", 0.0), vec![Event::Key(Key::Char('c'), Mods { ctrl: true, ..Default::default() })]);
    }
    #[test]
    fn csi_keys_and_kitty() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b[A", 0.0), vec![Event::Key(Key::Up, Mods::default())]);
        assert_eq!(p.feed(b"\x1b[1;5C", 0.0), vec![Event::Key(Key::Right, Mods { ctrl: true, ..Default::default() })]);
        assert_eq!(p.feed(b"\x1b[105;5u", 0.0), vec![Event::Key(Key::Char('i'), Mods { ctrl: true, ..Default::default() })]);
        assert_eq!(p.feed(b"\x1b[27u", 0.0), vec![Event::Key(Key::Esc, Mods::default())]);
        assert_eq!(p.feed(b"\x1b[5~", 0.0), vec![Event::Key(Key::PageUp, Mods::default())]);
    }
    #[test]
    fn esc_alone_needs_flush() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b", 0.0), vec![]);
        // not before the quiet period has really passed, however often the caller asks
        assert_eq!(p.flush(0.0), vec![]);
        assert_eq!(p.flush(QUIET / 2.0), vec![]);
        assert_eq!(p.deadline(), Some(QUIET));
        assert_eq!(p.flush(QUIET), vec![Event::Key(Key::Esc, Mods::default())]);
        assert_eq!(p.deadline(), None);
        assert_eq!(p.feed(b"\x1bx", 1.0), vec![Event::Key(Key::Char('x'), Mods { alt: true, ..Default::default() })]);
    }
    #[test]
    fn kitty_graphics_replies_are_swallowed() {
        let mut p = Parser::new();
        let ev = p.feed(b"\x1b_Gi=31;OK\x1b\\j\x1b_Gi=2;ENOTSUPPORTED:unicode placeholders are not supported\x1b\\", 0.0);
        assert_eq!(
            ev,
            vec![
                Event::KittyReply("i=31;OK".into()),
                k('j'),
                Event::KittyReply("i=2;ENOTSUPPORTED:unicode placeholders are not supported".into())
            ]
        );
        // split across reads
        assert_eq!(p.feed(b"\x1b_Gi=1;O", 0.0), vec![]);
        assert_eq!(p.feed(b"K\x1b\\", 0.0), vec![Event::KittyReply("i=1;OK".into())]);
    }
    #[test]
    fn replies_mouse_focus_paste() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b[?62;22c", 0.0), vec![Event::DeviceAttrs("62;22".into())]);
        assert_eq!(p.feed(b"\x1b[?2026;2$y", 0.0), vec![Event::ModeReport(2026, 2)]);
        assert_eq!(p.feed(b"\x1b[6;20;10t", 0.0), vec![Event::PixelSize { kind: 6, h: 20, w: 10 }]);
        assert_eq!(
            p.feed(b"\x1b[<0;5;3M", 0.0),
            vec![Event::Mouse { kind: MouseKind::Press(0), x: 4, y: 2, mods: Mods::default() }]
        );
        assert_eq!(
            p.feed(b"\x1b[<65;1;1M", 0.0),
            vec![Event::Mouse { kind: MouseKind::WheelDown, x: 0, y: 0, mods: Mods::default() }]
        );
        assert_eq!(p.feed(b"\x1b[I\x1b[O", 0.0), vec![Event::Focus(true), Event::Focus(false)]);
        assert_eq!(p.feed(b"\x1b[200~hi \x1b[A there\x1b[201~q", 0.0), vec![Event::Paste("hi \x1b[A there".into()), k('q')]);
    }
    #[test]
    fn horizontal_wheel_is_ignored() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b[<66;1;1M\x1b[<67;1;1Mj", 0.0), vec![k('j')]);
    }
    #[test]
    fn paste_survives_a_flush_mid_end_marker() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b[200~hello world\x1b[20", 0.0), vec![]);
        // a wake with no bytes, well past the escape-sequence quiet period
        assert_eq!(p.flush(QUIET * 2.0), vec![]);
        assert_eq!(p.feed(b"1~jjj", 0.1), vec![Event::Paste("hello world".into()), k('j'), k('j'), k('j')]);
        assert_eq!(p.feed(b"q\rk", 0.2), vec![k('q'), Event::Key(Key::Enter, Mods::default()), k('k')]);
        assert_eq!(p.deadline(), None);
    }
    #[test]
    fn paste_split_at_every_byte() {
        let full = b"\x1b[200~hello world\x1b[201~";
        for i in 0..=full.len() {
            for flush in [false, true] {
                let mut p = Parser::new();
                let mut ev = p.feed(&full[..i], 0.0);
                if flush {
                    ev.extend(p.flush(QUIET / 2.0));
                }
                ev.extend(p.feed(&full[i..], QUIET / 2.0));
                assert_eq!(ev, vec![Event::Paste("hello world".into())], "split at {i}, flush {flush}");
                assert_eq!(p.deadline(), None, "split at {i}, flush {flush}");
            }
        }
        // byte by byte
        let mut p = Parser::new();
        let ev: Vec<Event> = full.iter().flat_map(|b| p.feed(&[*b], 0.0)).collect();
        assert_eq!(ev, vec![Event::Paste("hello world".into())]);
    }
    #[test]
    fn unterminated_paste_gives_the_keyboard_back() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b[200~lost", 0.0), vec![]);
        assert_eq!(p.deadline(), Some(PASTE_QUIET));
        assert_eq!(p.flush(PASTE_QUIET / 2.0), vec![]);
        assert_eq!(p.flush(PASTE_QUIET), vec![Event::Paste("lost".into())]);
        assert_eq!(p.feed(b"j", PASTE_QUIET + 1.0), vec![k('j')]);
        // the size cap bounds what is kept, not how long the paste stays open
        let mut p = Parser::new();
        p.feed(b"\x1b[200~", 0.0);
        let chunk = vec![b'a'; 1 << 16];
        for _ in 0..20 {
            assert_eq!(p.feed(&chunk, 0.0), vec![]);
        }
        let ev = p.feed(b"\x1b[201~j", 0.0);
        assert!(matches!(&ev[..], [Event::Paste(s), e] if s.len() == PASTE_MAX && *e == k('j')), "{}", ev.len());
    }
    #[test]
    fn split_csi_waits_then_its_late_tail_is_discarded() {
        let ctrl_right = Event::Key(Key::Right, Mods { ctrl: true, ..Default::default() });
        // an early flush changes nothing
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b[1;5", 0.0), vec![]);
        assert_eq!(p.flush(QUIET / 2.0), vec![]);
        assert_eq!(p.feed(b"C", QUIET / 2.0), vec![ctrl_right]);
        // a due one abandons the sequence, and the remainder is not typed
        assert_eq!(p.feed(b"\x1b[1;5", 1.0), vec![]);
        assert_eq!(p.flush(1.0 + QUIET), vec![]);
        assert_eq!(p.deadline(), None);
        assert_eq!(p.feed(b"C", 1.1), vec![]);
        assert_eq!(p.feed(b"C", 1.2), vec![Event::Key(Key::Char('C'), Mods::default())]);
        // the discard lapses, so a key long after is a key
        assert_eq!(p.feed(b"\x1b[1;5", 2.0), vec![]);
        assert_eq!(p.flush(2.0 + QUIET), vec![]);
        assert_eq!(p.feed(b"j", 2.0 + QUIET + SKIP_QUIET + 0.1), vec![k('j')]);
    }
    #[test]
    fn split_kitty_reply_late_tail_is_discarded() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b_Gi=31;O", 0.0), vec![]);
        assert_eq!(p.flush(QUIET), vec![]);
        assert_eq!(p.feed(b"K\x1b\\", 0.1), vec![]);
        assert_eq!(p.feed(b"j", 0.2), vec![k('j')]);
        // the same for an OSC reply, its ST split too, and BEL as the terminator
        assert_eq!(p.feed(b"\x1b]11;rgb:00", 1.0), vec![]);
        assert_eq!(p.flush(1.0 + QUIET), vec![]);
        assert_eq!(p.feed(b"/00/00\x1b", 1.1), vec![]);
        assert_eq!(p.feed(b"\\j", 1.1), vec![k('j')]);
        assert_eq!(p.feed(b"\x1b]11;rgb:00", 3.0), vec![]);
        assert_eq!(p.flush(3.0 + QUIET), vec![]);
        assert_eq!(p.feed(b"/00\x07j", 3.1), vec![k('j')]);
        // a new sequence instead of the tail is parsed as itself
        assert_eq!(p.feed(b"\x1b_Gi=31;O", 5.0), vec![]);
        assert_eq!(p.flush(5.0 + QUIET), vec![]);
        assert_eq!(p.feed(b"\x1b[A", 5.1), vec![Event::Key(Key::Up, Mods::default())]);
    }
    #[test]
    fn esc_inside_csi_restarts_at_the_esc() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b[\x1b[A", 0.0), vec![Event::Key(Key::Up, Mods::default())]);
        assert_eq!(p.feed(b"\x1b[1;5\x1b[Bj", 0.0), vec![Event::Key(Key::Down, Mods::default()), k('j')]);
    }
    #[test]
    fn lone_esc_and_alt_keys() {
        let alt = Mods { alt: true, ..Default::default() };
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b", 0.0), vec![]);
        assert_eq!(p.flush(QUIET), vec![Event::Key(Key::Esc, Mods::default())]);
        // Esc, then a key after the deadline: two keys, not alt+key
        assert_eq!(p.feed(b"j", 0.1), vec![k('j')]);
        assert_eq!(p.feed(b"\x1bj\x1b\r", 0.2), vec![Event::Key(Key::Char('j'), alt), Event::Key(Key::Enter, alt)]);
        // alt+key split across reads inside the quiet period
        assert_eq!(p.feed(b"\x1b", 1.0), vec![]);
        assert_eq!(p.flush(1.0 + QUIET / 2.0), vec![]);
        assert_eq!(p.feed(b"x", 1.0 + QUIET / 2.0), vec![Event::Key(Key::Char('x'), alt)]);
        assert_eq!(p.feed(b"\x1b\x1b", 2.0), vec![Event::Key(Key::Esc, Mods::default())]);
        assert_eq!(p.flush(2.0 + QUIET), vec![Event::Key(Key::Esc, Mods::default())]);
    }
}
