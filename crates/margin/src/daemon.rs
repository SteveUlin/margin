//! `margin daemon`: the dispatcher for one project store. It hands appended events to the
//! registered handlers (handler.rs: one worker thread each, fed from the event log through the
//! handler's cursor), and does nothing else: it has no timer and watches no file but its own
//! store's. It follows the pinned build (pin.rs): when the pin names another binary than the one
//! running, it lets the runs in flight finish and execs the pin with the same arguments. A schedule lives outside margin and reaches a handler as an event (`margin emit` from cron or a systemd
//! timer).
//!
//! One per project, held by `flock` on `<project>/daemon.lock` (a second one exits at once). It
//! reconciles against handlers.json whenever that file changes (a file watch: watch.rs), not on
//! a timer: a new handler gets a worker thread, a removed or paused one is stopped, a changed
//! one restarts. Between changes it sleeps in `poll(2)`, as each worker does on the event log, so
//! an idle daemon makes no system call. With no unpaused handler left it exits after a short
//! grace, so nothing runs when nothing is registered (`--stay`, which the installed unit passes,
//! keeps it up). The CLI starts it on demand (`ensure`), or the systemd user unit that `margin
//! daemon install` writes runs it.
//!
//! Hooks never talk to the daemon: they only read files, so a busy or wedged handler can't slow
//! a turn.

use crate::handler::Ctl;
use crate::store::{write_atomic, Store};
use crate::watch::{Waker, Watch, Woken};
use std::collections::HashMap;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

static EXITING: AtomicBool = AtomicBool::new(false);
/// The exit pipe: its write end for the signal handler, its read end for whoever waits.
static EXIT_WR: AtomicI32 = AtomicI32::new(-1);
static EXIT_RD: AtomicI32 = AtomicI32::new(-1);

extern "C" fn on_term(_: libc::c_int) {
    EXITING.store(true, Ordering::Relaxed);
    // a thread blocked in poll() is not the one the signal interrupts: the pipe wakes them all
    crate::watch::raise(EXIT_WR.load(Ordering::Relaxed));
}

pub fn exiting() -> bool {
    EXITING.load(Ordering::Relaxed)
}

/// A descriptor that is readable from the moment a trapped signal arrives, and stays so: every
/// blocking wait in the process takes it as an interrupt (`Watch::interrupt`), so a SIGTERM
/// ends them all without any of them waking on a timer to ask. -1 before `trap_signals`.
pub fn exit_fd() -> i32 {
    EXIT_RD.load(Ordering::Relaxed)
}

/// SIGTERM, SIGINT and SIGHUP raise the `exiting` flag instead of killing the process: the
/// daemon stops its workers first, and `margin events --follow` / `job logs --follow` end with
/// exit 0.
pub fn trap_signals() {
    if exit_fd() < 0 {
        // never closed: it lives as long as the process
        let w = std::mem::ManuallyDrop::new(crate::watch::Waker::new());
        EXIT_RD.store(w.fd(), Ordering::Relaxed);
        EXIT_WR.store(w.write_fd(), Ordering::Relaxed);
    }
    unsafe {
        libc::signal(libc::SIGTERM, on_term as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_term as *const () as libc::sighandler_t);
        libc::signal(libc::SIGHUP, on_term as *const () as libc::sighandler_t);
    }
}

fn lock_path(st: &Store) -> std::path::PathBuf {
    st.dir().join("daemon.lock")
}

/// Try the daemon lock without blocking. Some(file) = we hold it (so no daemon is running).
fn try_lock(st: &Store) -> Option<std::fs::File> {
    std::fs::create_dir_all(st.dir()).ok()?;
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(lock_path(st)).ok()?;
    f.try_lock().ok().map(|_| f)
}

/// Where a daemon started on demand writes what it says (one line up, one line down, a registry
/// that stopped parsing).
pub fn log_path(st: &Store) -> std::path::PathBuf {
    st.dir().join("daemon.log")
}

/// The running daemon's pid, if one holds the lock.
pub fn running(st: &Store) -> Option<u32> {
    if try_lock(st).is_some() {
        return None;
    }
    // A daemon takes the lock first and writes its pid next: one that has just started may hold
    // the lock with no pid file yet. Wait a moment for the file before answering "running, pid
    // unknown" (0): a caller that means to signal the daemon needs the number.
    let pid = || std::fs::read_to_string(st.dir().join("daemon.pid")).ok().and_then(|s| s.trim().parse::<u32>().ok());
    for _ in 0..20 {
        if let Some(p) = pid() {
            return Some(p);
        }
        std::thread::sleep(Duration::from_millis(10));
        if try_lock(st).is_some() {
            return None;
        }
    }
    Some(0)
}

/// Start the daemon in the background unless one runs. Detached (setsid), stdio to the daemon log.
pub fn ensure(st: &Store) -> Result<Option<u32>, String> {
    if running(st).is_some() {
        return Ok(None);
    }
    // the pinned build when there is one (pin.rs), else this binary
    let exe = crate::pin::launcher(&st.home).map_err(|e| e.to_string())?;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(log_path(st)).map_err(|e| e.to_string())?;
    let mut c = Command::new(exe);
    c.args(["daemon", "--project", &st.project, "--home"])
        .arg(&st.home)
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(|e| e.to_string())?)
        .stderr(log);
    unsafe {
        c.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let ch = c.spawn().map_err(|e| format!("start daemon: {e}"))?;
    Ok(Some(ch.id()))
}

/// Start the daemon if a handler is registered and none is running: called after an event is
/// appended, so a key in the pane or a `margin emit` from cron is dispatched without anyone
/// having started anything. Cheap when there is nothing to do: one small file read.
pub fn wake(st: &Store) {
    if !crate::handler::Registry::path(&st.dir()).exists() {
        return;
    }
    if crate::handler::Registry::load(&st.dir()).handlers.iter().any(|h| !h.paused) {
        let _ = ensure(st);
    }
}

/// Since when the running daemon has been up: its pid file's time.
pub fn since(st: &Store) -> Option<f64> {
    std::fs::metadata(st.dir().join("daemon.pid")).ok()?.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs_f64())
}

/// The binary process `pid` of this store's daemon runs, and whether that is the pinned build
/// (None: nothing is pinned): for `margin daemon status`.
pub fn binary(st: &Store, pid: u32) -> (Option<std::path::PathBuf>, Option<bool>) {
    (crate::pin::exe_of(pid), crate::pin::runs_pin(&st.home, pid))
}

/// Should a daemon running `own` become the pinned build `target`? When there is a pin and it
/// is another file than the one running: the pin moved, or the daemon was started from a build
/// that is not the pin. `tried` is a target an exec was already made for (it failed, or it led
/// here): never the same one twice, so a pin that cannot be run is not a loop.
fn should_move(own: &std::path::Path, target: Option<&std::path::Path>, tried: Option<&std::path::Path>) -> bool {
    target.is_some_and(|t| t != own && Some(t) != tried)
}

/// The variable a daemon sets when it execs the pinned build: the target it execed for.
const PIN_EXEC: &str = "MARGIN_PIN_EXEC";

/// With no unpaused handler left (and no `--stay`), the daemon exits after this many seconds.
const IDLE_GRACE: f64 = 1.5;

/// Held by a worker thread: tells the main loop when the thread ends without having been told
/// to (a panic), so the handler gets a new worker. A second's pause first, so a worker that
/// dies at once is not restarted in a tight loop.
struct Ended(Arc<Waker>);

impl Drop for Ended {
    fn drop(&mut self) {
        if std::thread::panicking() {
            std::thread::sleep(Duration::from_secs(1));
        }
        self.0.wake();
    }
}

struct RunningHandler {
    h: crate::handler::Handler,
    ctl: Arc<Ctl>,
    join: JoinHandle<()>,
}

/// `stay`: keep running with nothing registered (a systemd unit's daemon: exiting when idle
/// would only make the unit restart it).
pub fn run(st: Store, stay: bool) -> Result<(), String> {
    let Some(lock) = try_lock(&st) else {
        eprintln!("margin daemon: already running for {}", st.project);
        return Ok(());
    };
    let pid_path = st.dir().join("daemon.pid");
    write_atomic(&pid_path, std::process::id().to_string().as_bytes()).map_err(|e| e.to_string())?;
    trap_signals();
    eprintln!("{} margin daemon {} up for {}", crate::util::now_rfc3339(), std::process::id(), st.project);
    let mut handlers: HashMap<String, RunningHandler> = HashMap::new();
    // What ends the main loop's sleep: the registry changing, a signal, a worker thread ending
    // on its own, and (as the wait's deadline) the idle exit. Armed before the first read of the
    // registry, so a change between that read and the wait is not missed.
    let ended = Arc::new(Waker::new());
    let mut watch = Watch::new().dir(&st.dir(), &["handlers.json"]).interrupt(exit_fd()).interrupt(ended.fd());
    // The pin (pin.rs): its link is replaced in `<home>/bin`; before anything is pinned that
    // directory is not there, and its appearing in the home is the news. Nothing is created.
    let bin = crate::pin::dir(&st.home);
    let mut bin_watched = false;
    if !bin.is_dir() && st.home.is_dir() {
        watch.add_dir(&st.home, &[crate::pin::DIR]);
    }
    let canon = |p: std::path::PathBuf| std::fs::canonicalize(&p).unwrap_or(p);
    let own = std::env::current_exe().map(canon).unwrap_or_default();
    // the target an exec was last made for: the one that led here, then any that failed
    let mut tried = std::env::var_os(PIN_EXEC).map(std::path::PathBuf::from);
    std::env::remove_var(PIN_EXEC);
    if let Some(why) = watch.fallback() {
        eprintln!("{} margin daemon: {why}; checking the registry every {} ms instead", crate::util::now_rfc3339(), crate::watch::FALLBACK_TICK.as_millis());
    }
    // A handlers.json that won't parse (a hand edit's trailing comma) is not "every handler was
    // removed": the last registry that parsed stays in force until the file reads again, and
    // the error is logged once, not once per look.
    let (mut hreg, mut herr) = (crate::handler::Registry::default(), String::new());
    let mut idle_since: Option<f64> = None;
    while !exiting() {
        // arm, then read: a pin made between the two is already queued on the watch
        if !bin_watched && bin.is_dir() {
            watch.add_dir(&bin, &["margin"]);
            bin_watched = true;
        }
        // follow the pin: when it names another build than this one, let every worker finish
        // the run it is in (no command is signalled, no new event is taken), then become the
        // pinned build with the same arguments. Handlers resume from their cursors
        let target = crate::pin::target(&st.home).map(canon);
        if should_move(&own, target.as_deref(), tried.as_deref()) {
            let target = target.unwrap();
            eprintln!("{} margin daemon {}: the pin is {}; finishing runs in flight, then becoming it", crate::util::now_rfc3339(), std::process::id(), target.display());
            for w in handlers.values() {
                w.ctl.drain();
            }
            for (_, w) in handlers.drain() {
                let _ = w.join.join();
            }
            if exiting() {
                break;
            }
            // the lock and the watches close on exec; the new image takes the lock again and
            // rewrites the pid file (the pid is the same)
            let err = Command::new(crate::pin::link(&st.home)).args(std::env::args_os().skip(1)).env(PIN_EXEC, &target).exec();
            eprintln!("{} margin daemon {}: cannot run the pinned build {}: {err}; staying on {}", crate::util::now_rfc3339(), std::process::id(), target.display(), own.display());
            tried = Some(target);
            ended.drain();
        }
        // reconcile against handlers.json: stop what was removed, paused or changed
        match crate::handler::Registry::load_strict(&st.dir()) {
            Ok(r) => {
                hreg = r;
                herr.clear();
            }
            Err(e) if e != herr => {
                eprintln!("{} margin daemon: {e}; keeping the last good registry ({} handlers)", crate::util::now_rfc3339(), hreg.handlers.len());
                herr = e;
            }
            Err(_) => {}
        }
        let mut done = Vec::new();
        for (name, w) in &handlers {
            match hreg.get(name) {
                None => {
                    w.ctl.stop("removed");
                    done.push(name.clone());
                }
                Some(h) if h.paused => {
                    w.ctl.stop("paused");
                    done.push(name.clone());
                }
                Some(h) if *h != w.h => {
                    w.ctl.stop("restart");
                    done.push(name.clone());
                }
                _ if w.join.is_finished() => done.push(name.clone()),
                _ => {}
            }
        }
        for n in done {
            if let Some(w) = handlers.remove(&n) {
                let _ = w.join.join();
            }
        }
        // start what's new
        for h in hreg.handlers.iter().filter(|h| !h.paused) {
            if handlers.contains_key(&h.name) {
                continue;
            }
            let ctl = Arc::new(Ctl::default());
            let (st2, h2, c2, e2) = (Store::new(st.home.clone(), st.project.clone()), h.clone(), ctl.clone(), Ended(ended.clone()));
            let join = std::thread::Builder::new()
                .name(format!("handler-{}", h.name))
                .spawn(move || {
                    let _e = e2;
                    crate::handler::Worker::new(st2, h2, c2, false).run()
                })
                .map_err(|e| e.to_string())?;
            handlers.insert(h.name.clone(), RunningHandler { h: h.clone(), ctl, join });
        }
        // how long to sleep: until something changes, or until the idle exit is due
        let mut sleep = None;
        if handlers.is_empty() && !stay {
            let now = crate::util::epoch_now();
            // a short grace absorbs `handler rm x` followed at once by `handler add x`
            let idle = now - *idle_since.get_or_insert(now);
            if idle > IDLE_GRACE {
                // Exit under the project lock: `handler add` registers under the same lock and
                // then checks for a daemon, so either we see its handler here or it sees no
                // daemon and starts a new one. Without this, an add landing as we exit would
                // never run.
                let _l = st.lock().map_err(|e| e.to_string())?;
                // a registry that won't parse is not "no handlers": stay up until it reads again
                if crate::handler::Registry::load_strict(&st.dir()).map(|r| r.handlers.iter().all(|h| h.paused)).unwrap_or(false) {
                    let _ = std::fs::remove_file(&pid_path);
                    drop(lock);
                    eprintln!("{} margin daemon {} down: no handlers left", crate::util::now_rfc3339(), std::process::id());
                    return Ok(());
                }
                idle_since = None;
                // a handler was added as the grace ran out (the next pass starts it), or the
                // registry does not parse (the next look is a grace away, or when it changes)
                sleep = Some(Duration::ZERO);
                if !herr.is_empty() {
                    sleep = Some(Duration::from_secs_f64(IDLE_GRACE));
                }
            } else {
                sleep = Some(Duration::from_secs_f64(IDLE_GRACE - idle + 0.01));
            }
        } else {
            idle_since = None;
        }
        if watch.wait(sleep) == Woken::Interrupted {
            ended.drain();
        }
    }
    // signalled: stop dispatching, stop every running command (its whole process group: TERM,
    // a grace period, KILL), record each as stopped, not failed
    for w in handlers.values() {
        w.ctl.stop("stopped");
    }
    for (_, w) in handlers.drain() {
        let _ = w.join.join();
    }
    let _ = std::fs::remove_file(&pid_path);
    eprintln!("{} margin daemon {} down: signalled", crate::util::now_rfc3339(), std::process::id());
    drop(lock);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// The daemon moves to the pin when one names another build than the one running, whether
    /// the pin moved or the daemon was started from a build that is not the pin; never with no
    /// pin, and never twice for a target an exec was already made for.
    #[test]
    fn the_daemon_moves_to_the_pin_once_per_target() {
        let (own, a, b) = (Path::new("/src/target/debug/margin"), Path::new("/h/bin/margin-a"), Path::new("/h/bin/margin-b"));
        assert!(!should_move(own, None, None), "nothing pinned: unchanged");
        assert!(should_move(own, Some(a), None), "started from a build that is not the pin");
        assert!(!should_move(a, Some(a), Some(a)), "it runs the pin");
        assert!(should_move(a, Some(b), Some(a)), "the pin moved");
        assert!(!should_move(own, Some(a), Some(a)), "that exec failed, or led here: not again");
    }
}
