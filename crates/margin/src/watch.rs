//! Waiting for a file to change, without a timer.
//!
//! Everything that reacts to the store sleeps here: the pane (`Inotify`, a watch per directory it
//! shows), and the event log's consumers (`Watch`: a handler's worker, the daemon's main loop,
//! `margin events --follow`, the rewake watcher). Each blocks in `poll(2)` on an inotify
//! descriptor plus whatever else may end the wait (a `Waker`, the daemon's exit pipe, a pid fd),
//! so an idle process makes no system call at all and an append is seen as it lands.
//!
//! The rules a caller of `Watch` relies on:
//! - **arm, then read.** Build the watch before the first look at the file. Whatever is appended
//!   after that look is already queued on the descriptor, and the next `wait` returns at once.
//! - **a wake is a hint.** `Changed` means "look again", not "there is something": a burst of
//!   appends is one wake, and a write of no interest (another name passed the filter, the log
//!   rotated) is one too. Callers re-read and compare.
//! - **directories, not files.** A watch on a file follows its inode and is lost when the file is
//!   replaced by a rename (every atomic write) or rotated away; a watch on its directory,
//!   filtered by name, survives both.
//! - **no watcher, no silence.** Where inotify cannot be had (no descriptors left, a network
//!   filesystem that reports nobody else's writes, another platform) `wait` says `Changed`
//!   every 250 ms instead, and `fallback()` says why so the caller can log it once.
//!   `MARGIN_WATCH=poll` forces that, for a filesystem this file does not know to distrust.

use std::path::Path;
use std::time::{Duration, Instant};

/// How often a `Watch` without inotify says "look again".
pub const FALLBACK_TICK: Duration = Duration::from_millis(250);

#[cfg(target_os = "linux")]
mod sys {
    use std::ffi::CString;
    use std::path::Path;

    /// What the pane reloads on.
    pub const PANE: u32 = libc::IN_CLOSE_WRITE | libc::IN_MOVED_TO | libc::IN_MODIFY | libc::IN_CREATE;
    /// What a `Watch` wakes on: a write, and a name arriving or leaving (a file replaced by
    /// rename, the log rotated away, a mark removed). Not the close after a write: the write
    /// already woke, and the close would wake a second time for nothing.
    pub const CHANGES: u32 = libc::IN_MODIFY | libc::IN_CREATE | libc::IN_MOVED_TO | libc::IN_MOVED_FROM | libc::IN_DELETE;
    pub const OVERFLOW: u32 = libc::IN_Q_OVERFLOW;

    pub fn init() -> i32 {
        unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) }
    }
    pub fn add(fd: i32, p: &Path, mask: u32) -> i32 {
        match CString::new(p.to_string_lossy().as_bytes()) {
            Ok(c) => unsafe { libc::inotify_add_watch(fd, c.as_ptr(), mask) },
            Err(_) => -1,
        }
    }
    /// A filesystem whose other writers inotify never reports: a watch there is accepted and
    /// then silent for everything not written through this very mount.
    pub fn remote(p: &Path) -> Option<&'static str> {
        let c = CString::new(p.to_string_lossy().as_bytes()).ok()?;
        let mut s: libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statfs(c.as_ptr(), &mut s) } != 0 {
            return None;
        }
        match s.f_type as u32 {
            0x6969 => Some("NFS"),
            0xFF53_4D42 | 0xFE53_4D42 | 0x517B => Some("SMB"),
            0x6573_5546 => Some("FUSE"),
            0x0102_1997 => Some("9p"),
            _ => None,
        }
    }
    /// A descriptor that reads as ready when process `pid` ends (Linux 5.3); -1 when there is none.
    pub fn pidfd(pid: u32) -> i32 {
        unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) as i32 }
    }
    pub fn pipe(fds: &mut [i32; 2]) -> i32 {
        unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) }
    }
}

/// Not Linux: no inotify, so every `Watch` is the 250 ms fallback and the pane's watch never
/// fires. Enough for the crate to build; the pane is not expected to be useful there yet.
#[cfg(not(target_os = "linux"))]
mod sys {
    use std::path::Path;
    pub const PANE: u32 = 0;
    pub const CHANGES: u32 = 0;
    pub const OVERFLOW: u32 = 0;
    pub fn init() -> i32 {
        -1
    }
    pub fn add(_: i32, _: &Path, _: u32) -> i32 {
        -1
    }
    pub fn remote(_: &Path) -> Option<&'static str> {
        None
    }
    pub fn pidfd(_: u32) -> i32 {
        -1
    }
    pub fn pipe(fds: &mut [i32; 2]) -> i32 {
        let r = unsafe { libc::pipe(fds.as_mut_ptr()) };
        if r == 0 {
            for fd in fds.iter() {
                unsafe {
                    libc::fcntl(*fd, libc::F_SETFL, libc::fcntl(*fd, libc::F_GETFL) | libc::O_NONBLOCK);
                    libc::fcntl(*fd, libc::F_SETFD, libc::FD_CLOEXEC);
                }
            }
        }
        r
    }
}

/// Read every event queued on an inotify descriptor (which must be non-blocking): `f(wd, mask,
/// name)` for each.
fn read_events(fd: i32, mut f: impl FnMut(i32, u32, &str)) {
    let mut buf = [0u8; 4096];
    loop {
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut _, buf.len()) };
        if n <= 0 {
            break;
        }
        // struct inotify_event { int wd; u32 mask; u32 cookie; u32 len; char name[len]; }
        let (mut at, n) = (0usize, n as usize);
        while at + 16 <= n {
            let wd = i32::from_ne_bytes(buf[at..at + 4].try_into().unwrap());
            let mask = u32::from_ne_bytes(buf[at + 4..at + 8].try_into().unwrap());
            let len = u32::from_ne_bytes(buf[at + 12..at + 16].try_into().unwrap()) as usize;
            let raw = &buf[(at + 16).min(n)..(at + 16 + len).min(n)];
            let name = String::from_utf8_lossy(raw.split(|b| *b == 0).next().unwrap_or(&[]));
            f(wd, mask, &name);
            at += 16 + len;
        }
    }
}

// --- the pane's watch ---

/// The pane's watches: the directories of the boards it shows, its own view directory, the
/// project directory and the datasets. The pane polls `fd` beside its terminal (vellum's
/// `Terminal::poll`) and asks `drain` what fired.
pub struct Inotify {
    pub fd: i32,
    /// The watch on the pane's own view directory: a write there is this pane saving its view, or
    /// `margin tab` changing the tab set, and neither is a reason to reload the documents.
    view_wd: i32,
    /// The project directories' watches (the pane's own, and one per other project a tab comes
    /// from). A project's `.lock` is opened by every command that takes the project lock,
    /// whatever it then writes: the write itself is the news, not the lock (an append to a
    /// dataset ten times a second would otherwise reload every tab each time).
    proj_wds: Vec<i32>,
    /// The watches whose changes mean "reload".
    other: Vec<i32>,
    /// The watches on datasets: the project's `data/` directory (any name), and the directory of
    /// each registered external file (that file's name only).
    data: Vec<(i32, Option<String>)>,
}

/// What fired since the last look.
#[derive(Default, Debug, PartialEq)]
pub struct Fired {
    pub views: bool,
    /// A dataset's file changed: no document did.
    pub data: bool,
    pub other: bool,
}

impl Inotify {
    /// `paths`: directories to reload on. `proj`: the project directory (one of them).
    /// `data_dir` and `files`: where the datasets are (data.rs).
    pub fn new(paths: &[std::path::PathBuf], view_dir: &Path, proj: &Path, data_dir: &Path, files: &[std::path::PathBuf]) -> Inotify {
        let fd = sys::init();
        let add = |p: &Path, make: bool| -> i32 {
            if make {
                let _ = std::fs::create_dir_all(p);
            }
            sys::add(fd, p, sys::PANE)
        };
        let other: Vec<i32> = paths.iter().map(|p| add(p, true)).collect();
        let view_wd = add(view_dir, true);
        // the same directory watched twice is one watch: the same descriptor comes back
        let proj_wd = add(proj, true);
        let mut data = vec![(add(data_dir, true), None)];
        for f in files {
            // an external file's directory, never created here; a file replaced by rename is
            // still seen, which a watch on the file itself would lose
            if let (Some(dir), Some(name)) = (f.parent(), f.file_name()) {
                data.push((add(dir, false), Some(name.to_string_lossy().into_owned())));
            }
        }
        Inotify { fd, view_wd, proj_wds: vec![proj_wd], other, data }
    }
    /// Another project a tab comes from: its directory (already among the paths given to `new`)
    /// has its `.lock` ignored too, and its `data/` directory is a dataset watch.
    pub fn project(&mut self, proj: &Path, data_dir: &Path) {
        self.proj_wds.push(sys::add(self.fd, proj, sys::PANE));
        let _ = std::fs::create_dir_all(data_dir);
        self.data.push((sys::add(self.fd, data_dir, sys::PANE), None));
    }
    /// Reload on changes in `p` too, if it is there: nothing is created (a tab whose project or
    /// board is missing waits here for it to appear).
    pub fn also(&mut self, p: &Path) {
        let wd = sys::add(self.fd, p, sys::PANE);
        if wd >= 0 {
            self.other.push(wd);
        }
    }
    /// Read what fired.
    pub fn drain(&self) -> Fired {
        let mut out = Fired::default();
        read_events(self.fd, |wd, _, name| {
            let is_data = self.data.iter().any(|(d, only)| *d == wd && only.as_deref().map(|o| o == name).unwrap_or(true));
            if wd == self.view_wd {
                out.views = true;
            } else if is_data {
                out.data = true;
            } else if self.other.contains(&wd) && !(self.proj_wds.contains(&wd) && name == ".lock") {
                out.other = true;
            }
        });
        out
    }
}

impl Drop for Inotify {
    fn drop(&mut self) {
        if self.fd >= 0 {
            unsafe { libc::close(self.fd) };
        }
    }
}

// --- waking a waiter from another thread or a signal handler ---

/// A pipe to end a `wait` from outside: `wake()` makes `fd()` readable, and it stays readable
/// until `drain()`. Raising it is one `write(2)`, so a signal handler may do it. A stop flag
/// that is never lowered needs no drain: every later wait returns at once, which is the point.
pub struct Waker {
    rd: i32,
    wr: i32,
}

impl Waker {
    pub fn new() -> Waker {
        let mut fds = [-1i32; 2];
        if sys::pipe(&mut fds) != 0 {
            // out of descriptors: the waits this would have ended fall back on their own clocks
            return Waker { rd: -1, wr: -1 };
        }
        Waker { rd: fds[0], wr: fds[1] }
    }
    /// The end to wait on (`Watch::interrupt`).
    pub fn fd(&self) -> i32 {
        self.rd
    }
    /// The end a signal handler writes to.
    pub fn write_fd(&self) -> i32 {
        self.wr
    }
    pub fn wake(&self) {
        raise(self.wr);
    }
    pub fn drain(&self) {
        let mut buf = [0u8; 64];
        while self.rd >= 0 && unsafe { libc::read(self.rd, buf.as_mut_ptr() as *mut _, buf.len()) } > 0 {}
    }
}

/// Make the read end of a `Waker` readable, given its write end. Async-signal-safe.
pub fn raise(wr: i32) {
    if wr >= 0 {
        // a full pipe is already as readable as it gets
        unsafe { libc::write(wr, [1u8].as_ptr() as *const _, 1) };
    }
}

impl Default for Waker {
    fn default() -> Waker {
        Waker::new()
    }
}

impl Drop for Waker {
    fn drop(&mut self) {
        for fd in [self.rd, self.wr] {
            if fd >= 0 {
                unsafe { libc::close(fd) };
            }
        }
    }
}

// --- waiting on a child ---

/// A descriptor that turns readable when a child ends, so the wait for it needs no tick.
pub struct PidFd(i32);

impl PidFd {
    pub fn open(pid: u32) -> PidFd {
        PidFd(sys::pidfd(pid))
    }
    /// Wait until the process has ended (maybe: the caller asks `try_wait`), one of `interrupts`
    /// is readable, or `max` has passed. Without a pid fd (an old kernel) it is a 20 ms nap.
    pub fn wait(&self, interrupts: &[i32], max: Duration) {
        if self.0 < 0 {
            std::thread::sleep(max.min(Duration::from_millis(20)));
            return;
        }
        let mut fds: Vec<libc::pollfd> = std::iter::once(self.0).chain(interrupts.iter().copied()).map(|fd| libc::pollfd { fd, events: libc::POLLIN, revents: 0 }).collect();
        unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, millis(max)) };
    }
}

impl Drop for PidFd {
    fn drop(&mut self) {
        if self.0 >= 0 {
            unsafe { libc::close(self.0) };
        }
    }
}

/// A duration as a `poll` timeout: rounded up, so a wait never ends before its deadline and the
/// caller does not spin through the last fraction of a millisecond.
fn millis(d: Duration) -> i32 {
    d.as_micros().div_ceil(1000).min(i32::MAX as u128) as i32
}

// --- the watch ---

/// Why a `wait` returned.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Woken {
    /// A watched name was written, created, renamed or removed (or the fallback's tick passed):
    /// look again.
    Changed,
    /// The deadline passed with nothing to see.
    Timeout,
    /// An interrupt descriptor is ready, or a signal arrived: ask the stop flag.
    Interrupted,
}

/// A blocking wait on files, by directory and name. See the module's rules.
pub struct Watch {
    fd: i32,
    /// Watch descriptor, and the names that count in that directory (none: every name).
    dirs: Vec<(i32, Vec<String>)>,
    /// What else ends a wait: (fd, poll events). Events 0 is "hung up or broken" only.
    interrupts: Vec<(i32, i16)>,
    fallback: Option<String>,
}

impl Watch {
    pub fn new() -> Watch {
        let mut w = Watch { fd: sys::init(), dirs: vec![], interrupts: vec![], fallback: None };
        if w.fd < 0 {
            w.fallback = Some(format!("no inotify: {}", std::io::Error::last_os_error()));
        }
        if std::env::var("MARGIN_WATCH").map(|v| v == "poll").unwrap_or(false) {
            w.poll_only("MARGIN_WATCH=poll");
        }
        w
    }

    /// Give up on inotify for this watch: `wait` says `Changed` every `FALLBACK_TICK`.
    pub fn poll_only(&mut self, why: &str) {
        if self.fd >= 0 {
            unsafe { libc::close(self.fd) };
            self.fd = -1;
        }
        self.fallback.get_or_insert_with(|| why.to_string());
    }

    /// Also wake for `names` in `dir` (no names: anything in it). The directory is created.
    pub fn add_dir(&mut self, dir: &Path, names: &[&str]) {
        let _ = std::fs::create_dir_all(dir);
        if self.fd < 0 {
            return;
        }
        if let Some(fs) = sys::remote(dir) {
            return self.poll_only(&format!("{} is on {fs}, where a file watch misses other machines' writes", dir.display()));
        }
        let wd = sys::add(self.fd, dir, sys::CHANGES);
        if wd < 0 {
            return self.poll_only(&format!("cannot watch {}: {}", dir.display(), std::io::Error::last_os_error()));
        }
        let names: Vec<String> = names.iter().map(|s| s.to_string()).collect();
        // the same directory again is the same watch: its names add up
        match self.dirs.iter_mut().find(|(d, _)| *d == wd) {
            Some((_, have)) if have.is_empty() || names.is_empty() => have.clear(),
            Some((_, have)) => have.extend(names),
            None => self.dirs.push((wd, names)),
        }
    }
    pub fn dir(mut self, dir: &Path, names: &[&str]) -> Watch {
        self.add_dir(dir, names);
        self
    }
    /// End a wait when `fd` is readable: a `Waker`, the daemon's exit pipe. A negative fd is none.
    pub fn interrupt(mut self, fd: i32) -> Watch {
        self.interrupts.push((fd, libc::POLLIN));
        self
    }
    /// End a wait when the other end of `fd` has gone (stdout, when it is a pipe).
    pub fn hangup(mut self, fd: i32) -> Watch {
        self.interrupts.push((fd, 0));
        self
    }
    /// Why this watch is a 250 ms poll, when it is: for the caller's log, once.
    pub fn fallback(&self) -> Option<&str> {
        self.fallback.as_deref()
    }

    /// Block until something watched changed, `deadline` passed (None: no deadline), or an
    /// interrupt is ready. Everything queued is read first, so a burst is one `Changed`.
    pub fn wait(&mut self, deadline: Option<Duration>) -> Woken {
        let end = deadline.map(|d| Instant::now() + d);
        // the fallback's tick is its own, shorter deadline
        let tick = (self.fd < 0).then(|| Instant::now() + FALLBACK_TICK);
        loop {
            let now = Instant::now();
            let until = match (end, tick) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            let timeout = until.map(|t| millis(t.saturating_duration_since(now))).unwrap_or(-1);
            let mut fds: Vec<libc::pollfd> = vec![libc::pollfd { fd: self.fd, events: libc::POLLIN, revents: 0 }];
            fds.extend(self.interrupts.iter().map(|&(fd, events)| libc::pollfd { fd, events, revents: 0 }));
            let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, timeout) };
            if n < 0 {
                if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                    // nothing poll can do with these descriptors: never a busy loop
                    std::thread::sleep(FALLBACK_TICK);
                }
                return Woken::Interrupted;
            }
            if fds[1..].iter().any(|p| p.fd >= 0 && p.revents != 0) {
                return Woken::Interrupted;
            }
            if fds[0].revents != 0 && self.drain() {
                return Woken::Changed;
            }
            let now = Instant::now();
            if end.map(|t| now >= t).unwrap_or(false) {
                return Woken::Timeout;
            }
            if tick.map(|t| now >= t).unwrap_or(false) {
                return Woken::Changed;
            }
        }
    }

    /// Read what is queued; true when any of it is for a watched name.
    fn drain(&mut self) -> bool {
        let mut hit = false;
        let dirs = &self.dirs;
        read_events(self.fd, |wd, mask, name| {
            // the queue overflowed and dropped events: assume one of them was ours
            hit |= mask & sys::OVERFLOW != 0 || dirs.iter().any(|(d, names)| *d == wd && (names.is_empty() || names.iter().any(|n| n == name)));
        });
        hit
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        if self.fd >= 0 {
            unsafe { libc::close(self.fd) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::temp_store;
    use serde_json::Value;

    const SOON: Option<Duration> = Some(Duration::from_millis(40));
    /// Longer than any wake should take; a wait that runs this long has missed its event.
    const LONG: Option<Duration> = Some(Duration::from_secs(5));

    fn emit(st: &crate::store::Store) -> u64 {
        st.emit("ping", "", "", Value::Null, "you", "emit").unwrap().seq
    }

    /// The log's watch: an append wakes it, a burst of appends is one wake, a write to another
    /// file in the project directory is none, and with nothing happening it times out.
    #[test]
    fn a_watch_wakes_on_an_append_once_per_burst_and_times_out_when_idle() {
        let st = temp_store();
        let mut w = st.watch_events();
        assert_eq!(w.fallback(), None);
        assert_eq!(w.wait(SOON), Woken::Timeout, "nothing was written");
        // armed first: an append before the wait starts is not missed
        emit(&st);
        assert_eq!(w.wait(LONG), Woken::Changed);
        for _ in 0..20 {
            emit(&st);
        }
        assert_eq!(w.wait(LONG), Woken::Changed);
        assert_eq!(w.wait(SOON), Woken::Timeout, "the burst was one wake");
        // the lock, a cursor's file, the registry: not the log
        st.set_cursor("mine", 3);
        std::fs::write(st.dir().join("handlers.json"), "{}").unwrap();
        assert_eq!(w.wait(SOON), Woken::Timeout);
        // an append from another thread while this one is already blocked
        let st2 = crate::store::Store::new(st.home.clone(), st.project.clone());
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            emit(&st2);
            Instant::now()
        });
        assert_eq!(w.wait(LONG), Woken::Changed);
        let woke = Instant::now();
        assert!(woke.duration_since(t.join().unwrap()) < Duration::from_millis(100), "woken by the append, not by a clock");
        std::fs::remove_dir_all(&st.home).ok();
    }

    /// Rotation renames the live file away and the next append creates a new one: a watch on the
    /// directory sees both, where a watch on the file would follow the old inode into silence.
    #[test]
    fn a_watch_survives_the_log_rotating() {
        let st = temp_store();
        emit(&st);
        let mut w = st.watch_events();
        std::fs::rename(st.events_path(), st.dir().join("events.1.jsonl")).unwrap();
        assert_eq!(w.wait(LONG), Woken::Changed, "the live file went");
        assert_eq!(w.wait(SOON), Woken::Timeout);
        let seq = emit(&st);
        assert_eq!(w.wait(LONG), Woken::Changed, "the new live file");
        assert_eq!(st.events_since(seq - 1).len(), 1);
        emit(&st);
        assert_eq!(w.wait(LONG), Woken::Changed, "and it is still watched after");
        std::fs::remove_dir_all(&st.home).ok();
    }

    /// A waker ends a wait from another thread and keeps ending it until drained; a deadline of
    /// none is no deadline.
    #[test]
    fn a_waker_interrupts_a_wait_with_no_deadline() {
        let st = temp_store();
        let k = std::sync::Arc::new(Waker::new());
        let mut w = st.watch_events().interrupt(k.fd()).interrupt(-1);
        assert_eq!(w.wait(SOON), Woken::Timeout);
        let k2 = k.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            k2.wake();
        });
        assert_eq!(w.wait(None), Woken::Interrupted);
        t.join().unwrap();
        assert_eq!(w.wait(None), Woken::Interrupted, "raised until drained");
        k.drain();
        assert_eq!(w.wait(SOON), Woken::Timeout);
        std::fs::remove_dir_all(&st.home).ok();
    }

    /// Without inotify the watch says "look again" every tick, says why, and still honors a
    /// shorter deadline and an interrupt.
    #[test]
    fn without_inotify_a_watch_ticks_and_says_why() {
        let st = temp_store();
        let k = Waker::new();
        let mut w = st.watch_events().interrupt(k.fd());
        w.poll_only("a test");
        assert_eq!(w.fallback(), Some("a test"));
        assert_eq!(w.wait(SOON), Woken::Timeout, "a deadline before the tick");
        let t0 = Instant::now();
        assert_eq!(w.wait(None), Woken::Changed);
        let took = t0.elapsed();
        assert!(took >= FALLBACK_TICK && took < FALLBACK_TICK * 3, "{took:?}");
        k.wake();
        assert_eq!(w.wait(None), Woken::Interrupted);
        std::fs::remove_dir_all(&st.home).ok();
    }

    /// A pid fd ends the wait for a child when the child ends.
    #[test]
    fn a_pid_fd_wakes_when_the_child_ends() {
        let mut ch = std::process::Command::new("sh").args(["-c", "sleep 0.05"]).spawn().unwrap();
        let p = PidFd::open(ch.id());
        let t0 = Instant::now();
        while ch.try_wait().unwrap().is_none() && t0.elapsed() < Duration::from_secs(5) {
            p.wait(&[], Duration::from_secs(5));
        }
        assert!(t0.elapsed() < Duration::from_secs(2), "{:?}", t0.elapsed());
    }
}
