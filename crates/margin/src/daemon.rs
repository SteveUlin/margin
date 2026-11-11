//! `margin daemon`: the dispatcher for one project store. It hands appended events to the
//! registered handlers (handler.rs: one worker thread each, fed from the event log through the
//! handler's cursor), and does nothing else: it has no timer and watches no file. A schedule
//! lives outside margin and reaches a handler as an event (`margin emit` from cron or a systemd
//! timer).
//!
//! One per project, held by `flock` on `<project>/daemon.lock` (a second one exits at once). It
//! re-reads handlers.json every 300 ms and reconciles: a new handler gets a worker thread, a
//! removed or paused one is stopped, a changed one restarts. With no unpaused handler left it
//! exits after a short grace, so nothing runs when nothing is registered (`--stay`, which the
//! installed unit passes, keeps it up). The CLI starts it on demand (`ensure`), or the systemd
//! user unit that `margin daemon install` writes runs it.
//!
//! Hooks never talk to the daemon: they only read files, so a busy or wedged handler can't slow
//! a turn.

use crate::handler::Ctl;
use crate::store::{write_atomic, Store};
use std::collections::HashMap;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

static EXITING: AtomicBool = AtomicBool::new(false);

extern "C" fn on_term(_: libc::c_int) {
    EXITING.store(true, Ordering::Relaxed);
}

pub fn exiting() -> bool {
    EXITING.load(Ordering::Relaxed)
}

/// SIGTERM, SIGINT and SIGHUP raise the `exiting` flag instead of killing the process: the
/// daemon stops its workers first, and `margin events --follow` / `job logs --follow` end with
/// exit 0.
pub fn trap_signals() {
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
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
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
    // A handlers.json that won't parse (a hand edit's trailing comma) is not "every handler was
    // removed": the last registry that parsed stays in force until the file reads again, and
    // the error is logged once, not once per 300 ms.
    let (mut hreg, mut herr) = (crate::handler::Registry::default(), String::new());
    let mut idle_since: Option<f64> = None;
    while !exiting() {
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
            let (st2, h2, c2) = (Store::new(st.home.clone(), st.project.clone()), h.clone(), ctl.clone());
            let join = std::thread::Builder::new()
                .name(format!("handler-{}", h.name))
                .spawn(move || crate::handler::Worker::new(st2, h2, c2, false).run())
                .map_err(|e| e.to_string())?;
            handlers.insert(h.name.clone(), RunningHandler { h: h.clone(), ctl, join });
        }
        if handlers.is_empty() && !stay {
            let now = crate::util::epoch_now();
            // a short grace absorbs `handler rm x` followed at once by `handler add x`
            if now - *idle_since.get_or_insert(now) > 1.5 {
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
            }
        } else {
            idle_since = None;
        }
        std::thread::sleep(Duration::from_millis(300));
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
