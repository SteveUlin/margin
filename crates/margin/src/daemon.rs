//! `margin daemon`: the feed supervisor for one project store.
//!
//! One per project, held by `flock` on `<project>/daemon.lock` (a second one exits at once). It
//! re-reads feeds.json every 300 ms and reconciles: a new feed gets a worker thread, a removed or
//! paused one is stopped, a changed one restarts. Workers restart crashed commands with backoff and
//! rate-limit writes (feed.rs). With no unpaused feed left it exits after a short grace, so nothing
//! runs when nothing is registered. The CLI starts it on demand (`ensure`), or a systemd user unit
//! from units/ runs it.
//!
//! Hooks never talk to the daemon: they only read files, so a busy or wedged feed can't slow a turn.

use crate::feed::{Ctl, Feed, Registry, Worker};
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

/// The running daemon's pid, if one holds the lock.
pub fn running(st: &Store) -> Option<u32> {
    if try_lock(st).is_some() {
        return None;
    }
    std::fs::read_to_string(st.dir().join("daemon.pid")).ok().and_then(|s| s.trim().parse().ok()).or(Some(0))
}

/// Start the daemon in the background unless one runs. Detached (setsid), stdio to the daemon log.
pub fn ensure(st: &Store) -> Result<Option<u32>, String> {
    if running(st).is_some() {
        return Ok(None);
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = crate::feed::feeds_dir(st);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("daemon.log")).map_err(|e| e.to_string())?;
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

struct Running {
    feed: Feed,
    ctl: Arc<Ctl>,
    join: JoinHandle<()>,
}

/// The registry the supervisor reconciles against. A feeds.json that won't parse (a hand edit's
/// trailing comma) used to read as empty: every feed was "removed" and the daemon exited. The
/// last good registry stays in force instead, until the file reads again.
#[derive(Default)]
struct Seen {
    reg: Registry,
    err: String,
}

impl Seen {
    /// Re-read feeds.json. Returns a line to log when the error changed, so a broken file costs
    /// one line, not one per 300 ms.
    fn refresh(&mut self, dir: &std::path::Path) -> Option<String> {
        match Registry::load_strict(dir) {
            Ok(r) => {
                self.reg = r;
                (!std::mem::take(&mut self.err).is_empty()).then(|| "feeds.json reads again".to_string())
            }
            Err(e) if e != self.err => {
                self.err = e.clone();
                Some(format!("{e}; keeping the last good registry ({} feeds)", self.reg.feeds.len()))
            }
            Err(_) => None,
        }
    }
}

pub fn run(st: Store) -> Result<(), String> {
    let Some(lock) = try_lock(&st) else {
        eprintln!("margin daemon: already running for {}", st.project);
        return Ok(());
    };
    let pid_path = st.dir().join("daemon.pid");
    write_atomic(&pid_path, std::process::id().to_string().as_bytes()).map_err(|e| e.to_string())?;
    trap_signals();
    eprintln!("{} margin daemon {} up for {}", crate::util::now_rfc3339(), std::process::id(), st.project);
    let mut workers: HashMap<String, Running> = HashMap::new();
    let mut idle_since: Option<f64> = None;
    let stop = |w: &Running, why: &str| w.ctl.stop(why);
    let mut seen = Seen::default();
    while !exiting() {
        if let Some(l) = seen.refresh(&st.dir()) {
            eprintln!("{} margin daemon: {l}", crate::util::now_rfc3339());
        }
        let reg = &seen.reg;
        // stop what was removed, paused or changed
        let mut done = Vec::new();
        for (name, w) in &workers {
            match reg.get(name) {
                None => {
                    stop(w, "removed");
                    done.push(name.clone());
                }
                Some(f) if f.paused => {
                    stop(w, "paused");
                    done.push(name.clone());
                }
                Some(f) if !f.same_job(&w.feed) => {
                    stop(w, "restart");
                    done.push(name.clone());
                }
                _ if w.join.is_finished() => done.push(name.clone()),
                _ => {}
            }
        }
        for n in done {
            if let Some(w) = workers.remove(&n) {
                let _ = w.join.join();
            }
        }
        // start what's new
        for f in reg.feeds.iter().filter(|f| !f.paused) {
            if workers.contains_key(&f.name) {
                continue;
            }
            let ctl = Arc::new(Ctl::default());
            let (st2, f2, c2) = (Store::new(st.home.clone(), st.project.clone()), f.clone(), ctl.clone());
            let join = std::thread::Builder::new()
                .name(format!("feed-{}", f.name))
                .spawn(move || Worker::new(st2, f2, c2, false).run())
                .map_err(|e| e.to_string())?;
            workers.insert(f.name.clone(), Running { feed: f.clone(), ctl, join });
        }
        if workers.is_empty() {
            let now = crate::util::epoch_now();
            // a short grace absorbs `feed rm x` followed at once by `feed add x`
            if now - *idle_since.get_or_insert(now) > 1.5 {
                // Exit under the project lock: `feed add` registers under the same lock and then
                // checks for a daemon, so either we see its feed here or it sees no daemon and
                // starts a new one. Without this, an add landing as we exit would never run.
                let _l = st.lock().map_err(|e| e.to_string())?;
                // a registry that won't parse is not "no feeds": stay up until it reads again
                if Registry::load_strict(&st.dir()).map(|r| r.feeds.iter().all(|f| f.paused)).unwrap_or(false) {
                    let _ = std::fs::remove_file(&pid_path);
                    drop(lock);
                    eprintln!("{} margin daemon {} down: no feeds left", crate::util::now_rfc3339(), std::process::id());
                    return Ok(());
                }
                idle_since = None;
            }
        } else {
            idle_since = None;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    for w in workers.values() {
        stop(w, "stopped");
    }
    for (_, w) in workers.drain() {
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
    use crate::store::tests::temp_store;

    /// A feeds.json that stops parsing must not read as "every feed was removed".
    #[test]
    fn a_corrupt_registry_keeps_the_last_good_one() {
        let st = temp_store();
        let mut seen = Seen::default();
        assert_eq!(seen.refresh(&st.dir()), None, "no file: no feeds, nothing to say");
        let f = Feed {
            name: "jj".into(),
            cmd: vec!["true".into()],
            trigger: crate::feed::Trigger::Stream,
            artifact: "plan".into(),
            owns: "jj".into(),
            paused: false,
            cwd: "/".into(),
            min_gap: 1.0,
            timeout: 60.0,
            added: String::new(),
        };
        crate::feed::edit_registry(&st, |r| r.add(f, false)).unwrap();
        assert_eq!(seen.refresh(&st.dir()), None);
        assert!(seen.reg.get("jj").is_some());
        let p = Registry::path(&st.dir());
        let good = std::fs::read_to_string(&p).unwrap();
        std::fs::write(&p, good.replacen("\n  ]", ",\n  ]", 1)).unwrap();
        // logged once, however often the loop re-reads it; the feed stays registered
        let l = seen.refresh(&st.dir()).expect("the first sight of the error is logged");
        assert!(l.contains("feeds.json") && l.contains("trailing comma") && l.contains("1 feeds"), "{l}");
        for _ in 0..3 {
            assert_eq!(seen.refresh(&st.dir()), None);
        }
        assert!(seen.reg.get("jj").is_some());
        // a different error is news
        std::fs::write(&p, "{").unwrap();
        assert!(seen.refresh(&st.dir()).is_some());
        assert!(seen.reg.get("jj").is_some());
        // fixed: back to following the file
        std::fs::write(&p, r#"{"feeds": []}"#).unwrap();
        assert_eq!(seen.refresh(&st.dir()).as_deref(), Some("feeds.json reads again"));
        assert!(seen.reg.feeds.is_empty());
        std::fs::remove_dir_all(&st.home).ok();
    }
}
