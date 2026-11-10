//! The terminal: raw mode, alternate screen, modes on and off, size, SIGWINCH, and a poll loop over
//! stdin plus any extra fds the app watches (inotify). Restores everything on drop and on panic.

use std::io::{self, Write};
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, Default)]
pub struct Modes {
    /// SGR mouse reporting. Off by default: with it on, Zellij hands drags to the app and
    /// double-click selection of `mg:` tokens stops working inside the pane.
    pub mouse: bool,
    /// Kitty keyboard protocol, flag 1 (disambiguate): ctrl-i vs tab, esc vs alt.
    pub kitty_keys: bool,
    /// Focus in/out reports (mode 1004): the gradient ring only flows while the pane has focus.
    pub focus: bool,
    pub paste: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Size {
    pub cols: u16,
    pub rows: u16,
    pub px_w: u16,
    pub px_h: u16,
}

static WINCH_FD: AtomicI32 = AtomicI32::new(-1);
static TERMINATED: AtomicBool = AtomicBool::new(false);
static SAVED: Mutex<Option<libc::termios>> = Mutex::new(None);

extern "C" fn on_winch(_: libc::c_int) {
    let fd = WINCH_FD.load(Ordering::Relaxed);
    if fd >= 0 {
        unsafe { libc::write(fd, b"w".as_ptr() as *const _, 1) };
    }
}

/// SIGTERM / SIGHUP: note it and wake `poll` through the same self-pipe, so the app leaves its
/// loop and `Drop` restores the terminal instead of the process dying in raw mode.
extern "C" fn on_term(_: libc::c_int) {
    TERMINATED.store(true, Ordering::Relaxed);
    on_winch(0);
}

/// True once SIGTERM or SIGHUP arrived: the app should quit and drop its `Terminal`.
pub fn terminated() -> bool {
    TERMINATED.load(Ordering::Relaxed)
}

pub struct Terminal {
    pub modes: Modes,
    winch_rx: RawFd,
    out: io::Stdout,
}

const IN: RawFd = 0;

pub fn size() -> Size {
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(1, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_col > 0 {
            return Size { cols: ws.ws_col, rows: ws.ws_row, px_w: ws.ws_xpixel, px_h: ws.ws_ypixel };
        }
    }
    Size { cols: 80, rows: 24, px_w: 0, px_h: 0 }
}

fn mode_seq(m: &Modes, on: bool) -> String {
    let mut s = String::new();
    let hl = if on { 'h' } else { 'l' };
    if m.focus {
        s += &format!("\x1b[?1004{hl}");
    }
    if m.paste {
        s += &format!("\x1b[?2004{hl}");
    }
    if m.mouse {
        s += &format!("\x1b[?1000{hl}\x1b[?1002{hl}\x1b[?1003{hl}\x1b[?1006{hl}");
    }
    if m.kitty_keys {
        s += if on { "\x1b[>1u" } else { "\x1b[<u" };
    }
    s
}

pub fn restore_now() {
    if let Some(t) = SAVED.lock().ok().and_then(|g| *g) {
        unsafe { libc::tcsetattr(IN, libc::TCSAFLUSH, &t) };
    }
    // modes off, cursor on, leave alt screen; safe to send even if some were never on
    let _ = io::stdout().write_all(
        b"\x1b[?1004l\x1b[?2004l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[<u\x1b[0m\x1b[?25h\x1b[?1049l",
    );
    let _ = io::stdout().flush();
}

impl Terminal {
    pub fn enter(modes: Modes) -> io::Result<Terminal> {
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(IN, &mut t) != 0 {
                return Err(io::Error::last_os_error());
            }
            *SAVED.lock().unwrap() = Some(t);
            let mut raw = t;
            libc::cfmakeraw(&mut raw);
            raw.c_cc[libc::VMIN] = 1;
            raw.c_cc[libc::VTIME] = 0;
            libc::tcsetattr(IN, libc::TCSAFLUSH, &raw);

            let mut fds = [0; 2];
            libc::pipe2(fds.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC);
            WINCH_FD.store(fds[1], Ordering::Relaxed);
            libc::signal(libc::SIGWINCH, on_winch as *const () as libc::sighandler_t);
            libc::signal(libc::SIGTERM, on_term as *const () as libc::sighandler_t);
            libc::signal(libc::SIGHUP, on_term as *const () as libc::sighandler_t);

            let prev = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                restore_now();
                prev(info);
            }));

            let mut term = Terminal { modes, winch_rx: fds[0], out: io::stdout() };
            let s = format!("\x1b[?1049h\x1b[?25l\x1b[H\x1b[2J{}", mode_seq(&modes, true));
            term.write(&s)?;
            Ok(term)
        }
    }

    pub fn write(&mut self, s: &str) -> io::Result<()> {
        self.out.write_all(s.as_bytes())?;
        self.out.flush()
    }

    pub fn set_mouse(&mut self, on: bool) -> io::Result<()> {
        if self.modes.mouse == on {
            return Ok(());
        }
        let m = Modes { mouse: true, ..Default::default() };
        self.write(&mode_seq(&m, on))?;
        self.modes.mouse = on;
        Ok(())
    }

    /// Wait for stdin, a resize, or one of `extra` fds, up to `timeout_ms` (-1 = forever).
    /// Returns (stdin bytes, resized, indices of ready extra fds).
    pub fn poll(&mut self, extra: &[RawFd], timeout_ms: i32) -> io::Result<(Vec<u8>, bool, Vec<usize>)> {
        let mut pfds: Vec<libc::pollfd> = vec![
            libc::pollfd { fd: IN, events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: self.winch_rx, events: libc::POLLIN, revents: 0 },
        ];
        for &fd in extra {
            pfds.push(libc::pollfd { fd, events: libc::POLLIN, revents: 0 });
        }
        let n = unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as _, timeout_ms) };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                return Ok((Vec::new(), false, Vec::new()));
            }
            return Err(e);
        }
        let mut bytes = Vec::new();
        if pfds[0].revents & libc::POLLIN != 0 {
            let mut buf = [0u8; 8192];
            let r = unsafe { libc::read(IN, buf.as_mut_ptr() as *mut _, buf.len()) };
            if r > 0 {
                bytes.extend_from_slice(&buf[..r as usize]);
            } else if r == 0 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "stdin closed"));
            }
        }
        if pfds[0].revents & (libc::POLLHUP | libc::POLLERR) != 0 && bytes.is_empty() {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "tty hung up"));
        }
        let mut resized = false;
        if pfds[1].revents & libc::POLLIN != 0 {
            let mut buf = [0u8; 64];
            unsafe { libc::read(self.winch_rx, buf.as_mut_ptr() as *mut _, buf.len()) };
            resized = true;
        }
        let ready = (0..extra.len()).filter(|&i| pfds[i + 2].revents & libc::POLLIN != 0).collect();
        Ok((bytes, resized, ready))
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.write(&mode_seq(&self.modes, false));
        restore_now();
        WINCH_FD.store(-1, Ordering::Relaxed);
    }
}
