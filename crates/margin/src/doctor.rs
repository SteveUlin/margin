//! `margin doctor`: what a session, a pane or a daemon left behind when it went without saying
//! so (a crash, a kill -9, a reboot run no hook and no destructor), and what on a board cannot
//! work yet. It lists; with `--fix` it removes the leftovers. Nothing here is needed for
//! correctness: every reader of per-process state checks liveness itself (a rewake mark's pid, a
//! review mark's pid, the daemon's flock). The doctor is how the leftovers stop piling up, and
//! how a person sees them.
//!
//! It never touches a board, an event, a named cursor or a registration.

use crate::store::Store;
use std::path::PathBuf;

pub enum Fix {
    /// A leftover file: `--fix` removes it.
    Remove(PathBuf),
    /// A handler's status that says it is running when nothing is: `--fix` corrects it.
    Settle(String),
    /// Something to know or to do by hand; `--fix` leaves it.
    Note,
}

pub struct Finding {
    pub what: String,
    pub fix: Fix,
}

fn older_than(p: &std::path::Path, days: u32) -> bool {
    let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(days as u64 * 86400);
    days > 0 && std::fs::metadata(p).and_then(|m| m.modified()).map(|t| t < cutoff).unwrap_or(false)
}

fn rel(st: &Store, p: &std::path::Path) -> String {
    p.strip_prefix(st.dir()).map(|r| r.display().to_string()).unwrap_or_else(|_| p.display().to_string())
}

/// Everything the doctor has to say about this project. `days`: how long a session's cursor, a
/// pane's view file or a session's activity entry may go untouched before it counts as left over.
pub fn check(st: &Store, days: u32) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut leftover = |what: String, p: PathBuf| out.push(Finding { what, fix: Fix::Remove(p) });
    // runtime files of sessions that crashed
    for p in crate::wake::dead_marks() {
        leftover(format!("rewake mark of a watcher that is gone: {}", p.display()), p);
    }
    for p in crate::wake::dead_review_marks(st) {
        leftover(format!("review mark of a pane that is gone: {}", rel(st, &p)), p);
    }
    // per-session and per-pane state nobody has touched for a long time
    for p in st.stale_session_cursors(days) {
        leftover(format!("cursor of a session not seen for {days} days: {}", rel(st, &p)), p);
    }
    for e in std::fs::read_dir(st.activity_dir()).into_iter().flatten().flatten() {
        if older_than(&e.path(), days) {
            leftover(format!("activity of a session not seen for {days} days: {}", rel(st, &e.path())), e.path());
        }
    }
    let docs = st.list();
    for d in &docs {
        for e in std::fs::read_dir(st.art(&d.id).join("view")).into_iter().flatten().flatten() {
            if older_than(&e.path(), days) {
                leftover(format!("view of a pane not used for {days} days: {}", rel(st, &e.path())), e.path());
            }
        }
    }
    // a daemon that died hard: its pid file, and handlers recorded as running
    let running = crate::daemon::running(st).is_some();
    let pidf = st.dir().join("daemon.pid");
    if !running && pidf.exists() {
        leftover("pid file of a daemon that is not running: daemon.pid".into(), pidf);
    }
    let reg = crate::handler::Registry::load(&st.dir());
    if !running {
        for h in &reg.handlers {
            if crate::handler::read_status(st, &h.name).map(|s| s.pid != 0 || !s.run_started.is_empty()).unwrap_or(false) {
                out.push(Finding { what: format!("handler {} is recorded as running, and no daemon is: its run was interrupted (its events are still pending)", h.name), fix: Fix::Settle(h.name.clone()) });
            }
        }
    }
    // what cannot work yet: an event a board can raise that no handler takes
    for d in &docs {
        let mut names: Vec<(String, bool)> = Vec::new();
        let mut note = |n: String, open: bool| {
            if !names.iter().any(|(x, _)| *x == n) {
                names.push((n, open));
            }
        };
        for a in d.actions_for(None) {
            note(format!("{}.{}", d.id, a.name), false);
        }
        crate::doc::walk(&d.blocks, &mut |b, _, _| {
            if b.opt("target").is_some() {
                note(format!("{}.open", d.id), true);
            }
            for a in b.f.get("actions").and_then(|v| crate::doc::parse_actions(v, "", &[]).ok()).unwrap_or_default() {
                note(format!("{}.{}", d.id, a.name), false);
            }
        });
        for (n, open) in names {
            if reg.for_kind(&n).iter().all(|h| h.paused) && !st.outside_cursor_passed(1) {
                let how = if open { "margin handler add open --on '*.open' -- margin handle open".to_string() } else { format!("margin handler add {} --on {n} -- <your command>", n.replace('.', "-")) };
                out.push(Finding { what: format!("board {}: nothing handles {n} (a key on its rows would do nothing): {how}", d.id), fix: Fix::Note });
            }
        }
    }
    // files margin no longer writes or reads
    for d in &docs {
        let p = st.art(&d.id).join("history.jsonl");
        if let Ok(m) = std::fs::metadata(&p) {
            out.push(Finding { what: format!("{} ({} KB) is no longer written or read; it can be deleted by hand", rel(st, &p), m.len().div_ceil(1024)), fix: Fix::Note });
        }
    }
    out
}

/// The report, and with `fix` the cleanup. Safe at any time: it removes only what `check`
/// called a leftover, each decided by liveness or age at the moment of the check.
pub fn run(st: &Store, fix: bool, days: u32) -> String {
    let found = check(st, days);
    if found.is_empty() {
        return format!("project {}: nothing stale, nothing unhandled\n", st.project);
    }
    let mut out = String::new();
    let (mut fixable, mut fixed) = (0, 0);
    for f in &found {
        let tag = match &f.fix {
            Fix::Note => "note  ",
            _ if fix => "fixed ",
            _ => "stale ",
        };
        match &f.fix {
            Fix::Remove(p) => {
                fixable += 1;
                if fix && std::fs::remove_file(p).is_ok() {
                    fixed += 1;
                }
            }
            Fix::Settle(name) => {
                fixable += 1;
                if fix {
                    if let Some(mut s) = crate::handler::read_status(st, name) {
                        crate::handler::settle_status(&mut s);
                        s.state = "stopped".into();
                        if crate::store::write_atomic(&crate::handler::status_path(st, name), serde_json::to_string_pretty(&s).unwrap().as_bytes()).is_ok() {
                            fixed += 1;
                        }
                    }
                }
            }
            Fix::Note => {}
        }
        out += &format!("{tag}{}\n", f.what);
    }
    out += &match (fix, fixable) {
        (_, 0) => String::new(),
        (true, _) => format!("{fixed} of {fixable} cleaned up\n"),
        (false, n) => format!("{n} stale item{}: margin doctor --fix removes {}\n", if n == 1 { "" } else { "s" }, if n == 1 { "it" } else { "them" }),
    };
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::temp_store;
    use serde_json::json;

    fn age(p: &std::path::Path, days: u64) {
        let t = std::time::SystemTime::now() - std::time::Duration::from_secs(days * 86400);
        std::fs::File::options().write(true).open(p).unwrap().set_modified(t).unwrap();
    }

    /// What a crash leaves is found by liveness or age and removed by `--fix`; what is live or
    /// recent, and everything durable (boards, events, named cursors, registrations), is left.
    #[test]
    fn the_doctor_finds_and_removes_only_what_is_left_over() {
        let st = temp_store();
        let run = crate::wake::tests::temp_runtime();
        crate::wake::TEST_DIR.with(|d| *d.borrow_mut() = Some(run.clone()));
        let doc: crate::doc::Doc = serde_json::from_value(json!({"id": "ci", "title": "CI", "blocks": [
            {"type": "row", "id": "j1", "text": "build", "target": "https://example.com/1", "actions": [{"key": "e", "name": "retry"}]}]})).unwrap();
        st.put(doc, None).unwrap();
        st.emit("x", "", "", json!(1), "you", "emit").unwrap();
        assert!(run_text(&st, false).contains("nothing handles ci.open") && run_text(&st, false).contains("margin handler add open --on '*.open' -- margin handle open"));
        assert!(run_text(&st, false).contains("nothing handles ci.retry") && run_text(&st, false).contains("margin handler add ci-retry --on ci.retry -- <your command>"));
        // leftovers: a dead watcher's mark, a dead pane's review mark, an old session's cursor,
        // activity and view, a pid file with no daemon, a handler said to be running, an old history file
        let dead = crate::wake::tests::dead_pid();
        crate::wake::watch_begin("gone", dead, 0.0, 10.0);
        crate::wake::watch_begin("alive", std::process::id(), 0.0, crate::util::epoch_now() + 600.0);
        std::fs::create_dir_all(st.dir().join("review")).unwrap();
        std::fs::write(st.dir().join("review").join(format!("{dead}-tty-ci")), "").unwrap();
        std::fs::write(st.dir().join("review").join(format!("{}-tty-ci", std::process::id())), "").unwrap();
        st.set_delivered("old-session", 1);
        st.set_delivered("new-session", 1);
        age(&st.dir().join("delivered/old-session"), 40);
        st.set_cursor("mytool", 1);
        age(&st.cursor_path("mytool"), 400);
        st.set_activity(&crate::store::Activity { session: "old-session".into(), state: "idle".into(), since: "2026-01-01T00:00:00Z".into(), ..Default::default() });
        age(&st.activity_dir().join("old-session.json"), 40);
        st.save_view("ci", "zj-7", &Default::default());
        st.save_view("ci", "zj-8", &Default::default());
        age(&st.view_path("ci", "zj-7"), 40);
        std::fs::write(st.dir().join("daemon.pid"), "999999").unwrap();
        let h = crate::handler::Handler { name: "open".into(), cmd: vec!["true".into()], on: vec!["*.open".into()], board: String::new(), owns: String::new(), paused: false, cwd: "/".into(), path: String::new(), timeout: 60.0, retries: 3, added: String::new() };
        crate::handler::edit_registry(&st, |r| r.add(h)).unwrap();
        std::fs::create_dir_all(crate::handler::dir(&st)).unwrap();
        std::fs::write(crate::handler::status_path(&st, "open"), r#"{"state":"live","since":"2026-11-10T10:00:00Z","pid":999999,"run_started":"2026-11-10T10:00:00Z"}"#).unwrap();
        std::fs::write(st.art("ci").join("history.jsonl"), "x".repeat(3000)).unwrap();
        let report = run_text(&st, false);
        for must in ["rewake mark of a watcher that is gone", "review mark of a pane that is gone", "cursor of a session not seen for 30 days: delivered/old-session", "activity of a session not seen for 30 days", "view of a pane not used for 30 days: ci/view/zj-7.json", "pid file of a daemon that is not running", "handler open is recorded as running", "ci/history.jsonl (3 KB) is no longer written or read", "7 stale items: margin doctor --fix removes them"] {
            assert!(report.contains(must), "{must}:\n{report}");
        }
        assert!(!report.contains("nothing handles ci.open"), "a handler takes *.open now:\n{report}");
        assert!(run.join("gone.rewake").exists(), "listing removes nothing");
        // --fix: the leftovers go, and nothing else
        let report = run_text(&st, true);
        assert!(report.contains("7 of 7 cleaned up"), "{report}");
        assert!(!run.join("gone.rewake").exists() && run.join("alive.rewake").exists());
        assert!(!st.dir().join("review").join(format!("{dead}-tty-ci")).exists() && st.dir().join("review").join(format!("{}-tty-ci", std::process::id())).exists());
        assert!(!st.dir().join("delivered/old-session").exists() && st.dir().join("delivered/new-session").exists());
        assert_eq!(st.cursor("mytool"), Some(1), "a named cursor is never pruned, however old");
        assert!(!st.view_path("ci", "zj-7").exists() && st.view_path("ci", "zj-8").exists());
        assert!(!st.dir().join("daemon.pid").exists() && st.art("ci").join("history.jsonl").exists());
        let s = crate::handler::read_status(&st, "open").unwrap();
        assert_eq!((s.pid, s.run_started.as_str(), s.state.as_str(), s.last_outcome.as_str()), (0, "", "stopped", "interrupted"));
        assert!(st.exists("ci") && st.events().len() == 1 && crate::handler::Registry::load(&st.dir()).get("open").is_some());
        // a second run has only notes left
        let again = run_text(&st, true);
        assert!(!again.contains("stale ") && !again.contains("fixed ") && again.contains("note  "), "{again}");
        crate::wake::watch_end("alive", std::process::id());
        std::fs::remove_dir_all(&st.home).ok();
    }

    fn run_text(st: &Store, fix: bool) -> String {
        run(st, fix, 30)
    }
}
