//! `margin doctor`: what a session, a pane or a daemon left behind when it went without saying
//! so (a crash, a kill -9, a reboot run no hook and no destructor), and what on a board cannot
//! work yet. It lists; with `--fix` it removes the leftovers. Nothing here is needed for
//! correctness: every reader of per-process state checks liveness itself (a rewake mark's pid, a
//! review mark's pid, the daemon's flock). The doctor is how the leftovers stop piling up, and
//! how a person sees them.
//!
//! It never touches an event, a named cursor or a registration, and of a board only where its
//! blocks are kept, never what they say: `--fix` finishes a move of an owner's rows out of
//! doc.json into the owner's own file (what the board's next write would do anyway), and gives
//! the rows of an owned file nobody claims back to the layout.
//!
//! It reports what a board's owners wrote that is not drawn: rows whose container left the
//! layout (kept in the owner's file until the container is back or the owner writes them
//! elsewhere).
//!
//! It also reads what the removed feed runner left in a store (`feeds.json`, `feeds/*.log`,
//! `feeds/*.status.json`): margin no longer runs a command on a timer, a file watch or as a
//! stream, so each old feed is listed with the commands that replace it, and `--fix` moves the
//! runner's files into `.archive/`. Nothing else in margin reads those files.

use crate::store::Store;
use std::path::PathBuf;

pub enum Fix {
    /// A leftover file: `--fix` removes it.
    Remove(PathBuf),
    /// A handler's status that says it is running when nothing is: `--fix` corrects it.
    Settle(String),
    /// Files of the removed feed runner: `--fix` moves them into this directory of `.archive/`
    /// (never deletes), keeping each file's path below the project.
    Archive(Vec<PathBuf>),
    /// Blocks under an owner's prefix still in a board's doc.json: `--fix` moves them into the
    /// owner's file (`Store::settle`), as the board's next write would.
    Move(String),
    /// An owned file nobody claims, as (board, kind, name): `--fix` puts its blocks into the
    /// layout (`Store::release`).
    Release(String, String, String),
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

/// One word of a command line, quoted when a shell would split or expand it.
fn sh_word(w: &str) -> String {
    if !w.is_empty() && w.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+,".contains(c)) {
        w.to_string()
    } else {
        format!("'{}'", w.replace('\'', "'\\''"))
    }
}

fn sh_line(words: &[String]) -> String {
    words.iter().map(|w| sh_word(w)).collect::<Vec<_>>().join(" ")
}

/// A registered command word that names one of the shipped scripts where they used to live
/// (`…/feeds/jj`): the same script in `providers/` beside it, when the old path is gone and
/// the new one is there. Anything else is returned as it was.
fn moved_script(word: &str) -> String {
    if let Some((dir, name)) = word.rsplit_once("/feeds/") {
        let new = format!("{dir}/providers/{name}");
        if !std::path::Path::new(word).exists() && std::path::Path::new(&new).is_file() {
            return new;
        }
    }
    word.to_string()
}

/// The schedule line that replaces `--every N`: a cron line when N is whole minutes that cron
/// can say, else a loop (cron's finest step is a minute).
fn schedule(every: f64, emit: &str) -> String {
    let m = every / 60.0;
    if every >= 60.0 && m.fract() == 0.0 && m <= 59.0 && 60.0 % m == 0.0 {
        format!("cron: {} * * * * {emit}", if m == 1.0 { "*".to_string() } else { format!("*/{m}") })
    } else if every >= 3600.0 && (every / 3600.0).fract() == 0.0 && 24.0 % (every / 3600.0) == 0.0 {
        let h = every / 3600.0;
        format!("cron: 0 {} * * * {emit}", if h == 1.0 { "*".to_string() } else { format!("*/{h}") })
    } else {
        format!("a loop or a systemd timer (OnUnitActiveSec={0}): while sleep {0}; do {emit}; done", crate::owner::fmt_secs(every))
    }
}

/// What the removed feed runner left in this store: a note per feed in `feeds.json` with the
/// commands that replace it, and one fixable finding for the files (`--fix` archives them).
/// The file is read as loose JSON: a feeds.json that does not parse is reported and archived
/// like any other, and never stops a command (nothing else reads it).
fn old_feeds(st: &Store, out: &mut Vec<Finding>) {
    let reg = st.dir().join("feeds.json");
    let mut files: Vec<PathBuf> = Vec::new();
    if reg.is_file() {
        files.push(reg.clone());
    }
    // a board may be named `feeds`: only the runner's own files in that directory are its
    for e in std::fs::read_dir(st.dir().join("feeds")).into_iter().flatten().flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if e.path().is_file() && (n.ends_with(".status.json") || n.ends_with(".log") || n.ends_with(".log.1")) {
            files.push(e.path());
        }
    }
    files.sort();
    if files.is_empty() {
        return;
    }
    let text = std::fs::read_to_string(&reg).unwrap_or_default();
    let parsed: Option<serde_json::Value> = serde_json::from_str(&text).ok();
    if reg.is_file() && parsed.is_none() {
        out.push(Finding { what: "feeds.json does not parse; margin no longer reads it (the feed runner was removed), so nothing depends on it".into(), fix: Fix::Note });
    }
    let claims = |board: &str| st.claims(board);
    for f in parsed.as_ref().and_then(|v| v["feeds"].as_array()).into_iter().flatten() {
        let s = |k: &str| f[k].as_str().unwrap_or("").to_string();
        let (name, board, owns, cwd) = (s("name"), s("artifact"), s("owns"), s("cwd"));
        if name.is_empty() {
            continue;
        }
        let cmd: Vec<String> = f["cmd"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(moved_script)).collect()).unwrap_or_default();
        let cmd = if cmd.is_empty() { "<its command>".to_string() } else { sh_line(&cmd) };
        let kind = f["trigger"]["kind"].as_str().unwrap_or("");
        let emit = format!("margin --project {} emit {name}.refresh", sh_word(&st.project));
        let handler = format!("margin handler add {name} --on {name}.refresh --board {board} --owns {} -- {cmd}", sh_word(&owns));
        let from = if cwd.is_empty() { String::new() } else { format!("cd {} && ", sh_word(&cwd)) };
        let (was, now) = match kind {
            "interval" => {
                let every = f["trigger"]["every"].as_f64().unwrap_or(60.0);
                (format!("every {}", crate::owner::fmt_secs(every)), format!("    {from}{handler}\n    {}", schedule(every, &emit)))
            }
            "watch" => {
                let paths: Vec<String> = f["trigger"]["paths"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
                (
                    format!("watch {}", paths.join(" ")),
                    format!("    {from}{handler}\n    then emit when those paths change, from whatever changes them or a watcher of your own:\n    {from}inotifywait -qm -e close_write,move,create,delete {} | while read -r _; do {emit}; done", sh_line(&paths)),
                )
            }
            _ => (
                "stream".to_string(),
                format!("    margin owner add {name} --board {board} --owns {}\n    {from}{cmd} | while read -r line; do margin --project {} apply {board} --as {name} <<<\"$line\"; done", sh_word(&owns), sh_word(&st.project)),
            ),
        };
        let span = if owns == "*" {
            format!("every id in {board}")
        } else if owns.ends_with(['-', '_']) {
            format!("the ids starting with {owns} in {board}")
        } else {
            format!("the ids {owns} and {owns}-… in {board}")
        };
        let owned = if !st.exists(&board) {
            format!("its board {board} is gone")
        } else {
            match claims(&board).into_iter().find(|c| crate::owner::overlaps(&c.owns, &owns)) {
                Some(c) => format!("{span} now belong to {} {}", c.kind, c.feed),
                None => format!("{span} are no longer owned by anyone: anybody may write them until a handler or an owner claims the prefix"),
            }
        };
        // the health line the runner wrote beside the feed's rows: nobody updates it now
        let sid = if owns == "*" { format!("{name}-feed") } else if owns.ends_with(['-', '_']) { format!("{owns}feed") } else { format!("{owns}-feed") };
        let health = match st.load(&board).ok().filter(|d| d.find(&sid).map(|b| b.kind == "feed").unwrap_or(false)) {
            Some(_) => format!("\n    its old health line stays as a dim line until removed: margin rm {board} {sid}"),
            None => String::new(),
        };
        let paused = if f["paused"].as_bool().unwrap_or(false) { ", paused" } else { "" };
        out.push(Finding { what: format!("old feed {name} ({was}{paused}) is no longer run (margin has no timer, file watch or stream runner); {owned}. What replaces it:\n{now}{health}"), fix: Fix::Note });
    }
    let n = files.len();
    out.push(Finding { what: format!("{n} file{} of the removed feed runner (feeds.json, feeds/*.log, feeds/*.status.json): --fix moves {} into .archive/", if n == 1 { "" } else { "s" }, if n == 1 { "it" } else { "them" }), fix: Fix::Archive(files) });
}

/// Move the removed feed runner's files into `.archive/feed-runner-<date>/`, each at its path
/// below the project. Returns where they went.
fn archive_files(st: &Store, files: &[PathBuf]) -> Result<PathBuf, String> {
    let _l = st.lock().map_err(|e| e.to_string())?;
    let dir = st.dir().join(".archive");
    let base = format!("feed-runner-{}", &crate::util::now_rfc3339()[..10]);
    let dest = (1..).map(|n| dir.join(if n == 1 { base.clone() } else { format!("{base}-{n}") })).find(|p| !p.exists()).unwrap();
    for f in files {
        let to = dest.join(f.strip_prefix(st.dir()).map_err(|e| e.to_string())?);
        std::fs::create_dir_all(to.parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::rename(f, &to).map_err(|e| format!("archive {}: {e}", f.display()))?;
    }
    // the runner's directory goes when nothing else is in it (a board named `feeds` keeps it)
    let _ = std::fs::remove_dir(st.dir().join("feeds"));
    Ok(dest)
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
    // one writer per file: what of a board's owned files is not where it should be
    for d in &docs {
        let i = st.inspect(&d.id);
        let n = |n: usize, what: &str| format!("{n} {what}{}", if n == 1 { "" } else { "s" });
        for (by, parent, ids) in &i.hidden {
            let who = by.replacen(':', " ", 1);
            let shown: Vec<&str> = ids.iter().take(4).map(|s| s.as_str()).collect();
            let more = if ids.len() > shown.len() { format!(" and {} more", ids.len() - shown.len()) } else { String::new() };
            out.push(Finding {
                what: format!(
                    "board {}: {} of {who} ({}{more}) {} not drawn: the container {parent:?} is no longer in the layout. They are kept in {}/owned/{}.json; add {parent:?} back to the layout, or have {who} write them under another parent",
                    d.id,
                    n(ids.len(), "block"),
                    shown.join(" "),
                    if ids.len() == 1 { "is" } else { "are" },
                    d.id,
                    by.replacen(':', ".", 1)
                ),
                fix: Fix::Note,
            });
        }
        for (c, k) in &i.legacy {
            out.push(Finding { what: format!("board {}: {} under the prefix of {} {} {} still in doc.json (written before owners had files of their own); the board's next write moves them into {}/owned/{}.{}.json", d.id, n(*k, "block"), c.kind, c.feed, if *k == 1 { "is" } else { "are" }, d.id, c.kind, c.feed), fix: Fix::Move(d.id.clone()) });
        }
        for (kind, name, k) in &i.unclaimed {
            out.push(Finding { what: format!("board {}: owned/{kind}.{name}.json holds {} and no {kind} {name} is registered on the board, so they are not drawn: --fix puts them into the layout (or register it again: its rows are still there)", d.id, n(*k, "block")), fix: Fix::Release(d.id.clone(), kind.clone(), name.clone()) });
        }
        for e in &i.corrupt {
            out.push(Finding { what: format!("{e}: its blocks are not drawn and its owner's writes are refused; fix the file or move it aside"), fix: Fix::Note });
        }
    }
    // files margin no longer writes or reads
    old_feeds(st, &mut out);
    for d in &docs {
        let p = st.art(&d.id).join("history.jsonl");
        if let Ok(m) = std::fs::metadata(&p) {
            out.push(Finding { what: format!("{} ({} KB) is no longer written or read; it can be deleted by hand", rel(st, &p), m.len().div_ceil(1024)), fix: Fix::Note });
        }
    }
    out
}

/// The report, and with `fix` the cleanup. Safe at any time: it removes only what `check`
/// called a leftover, each decided by liveness or age at the moment of the check; the removed
/// feed runner's files are moved into `.archive/`, not deleted.
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
            Fix::Archive(files) => {
                fixable += 1;
                if fix {
                    match archive_files(st, files) {
                        Ok(dest) => {
                            fixed += 1;
                            out += &format!("{tag}{}\n      now in {}\n", f.what, rel(st, &dest));
                            continue;
                        }
                        Err(e) => {
                            out += &format!("stale {} (not moved: {e})\n", f.what);
                            continue;
                        }
                    }
                }
            }
            Fix::Move(board) => {
                fixable += 1;
                if fix {
                    match st.settle(board) {
                        Ok(()) => fixed += 1,
                        Err(e) => {
                            out += &format!("stale {} (not moved: {e})\n", f.what);
                            continue;
                        }
                    }
                }
            }
            Fix::Release(board, kind, name) => {
                fixable += 1;
                if fix {
                    match st.release(board, kind, name) {
                        Ok(_) => fixed += 1,
                        Err(e) => {
                            out += &format!("stale {} (not moved: {e})\n", f.what);
                            continue;
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

    /// A store the feed runner once ran in: each old feed is named with the commands that
    /// replace it and with who owns its rows now (nobody, until claimed); `--fix` moves the
    /// runner's files into `.archive/` and touches no board; a store without them says nothing.
    #[test]
    fn old_feeds_are_reported_with_their_replacements_and_archived() {
        let st = temp_store();
        let run = crate::wake::tests::temp_runtime();
        crate::wake::TEST_DIR.with(|d| *d.borrow_mut() = Some(run.clone()));
        assert!(!run_text(&st, false).contains("feed"), "a store that never had one");
        let doc: crate::doc::Doc = serde_json::from_value(json!({"id": "showcase", "title": "S", "blocks": [
            {"type": "section", "id": "jj", "title": "jj", "children": [{"type": "row", "id": "jj-1", "text": "a change"}]}]})).unwrap();
        st.put(doc, None).unwrap();
        // the health line the runner wrote is a block type the schema no longer has: it is in
        // the stored document, as it is in a store made before the runner was removed
        let path = st.art("showcase").join("doc.json");
        let mut stored: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        stored["blocks"].as_array_mut().unwrap().insert(0, json!({"type": "feed", "id": "jj-feed", "name": "jj", "state": "stopped", "trigger": "watch .jj/repo"}));
        std::fs::write(&path, stored.to_string()).unwrap();
        st.create(crate::doc::Doc::empty("agents")).unwrap();
        st.create(crate::doc::Doc::empty("feeds")).unwrap();
        std::fs::write(st.dir().join("feeds.json"), json!({"feeds": [
            {"name": "jj", "cmd": ["/opt/providers/jj", "--limit", "5 6"], "trigger": {"kind": "watch", "paths": [".jj/repo/op_heads"]}, "artifact": "showcase", "owns": "jj", "cwd": "/work/repo", "paused": true},
            {"name": "agents", "cmd": ["/opt/providers/agents"], "trigger": {"kind": "stream"}, "artifact": "agents", "owns": "ag", "cwd": "/work"},
            {"name": "ci", "cmd": ["ci-rows"], "trigger": {"kind": "interval", "every": 300.0}, "artifact": "gone", "owns": "*", "cwd": "/work"}]}).to_string()).unwrap();
        for f in ["jj.log", "jj.log.1", "jj.status.json", "daemon.log"] {
            std::fs::write(st.dir().join("feeds").join(f), "x").unwrap();
        }
        // the store still works with the file there: a write lands in the old prefix, unowned
        st.patch("showcase", &[crate::doc::Op::Set { id: "jj-1".into(), field: "text".into(), value: json!("mine now") }], None).unwrap();
        st.add_owner(crate::owner::Owner { name: "agents".into(), board: "agents".into(), owns: "ag".into(), expect_every: None, added: String::new(), last_write: String::new() }).unwrap();
        let report = run_text(&st, false);
        for must in [
            "note  old feed jj (watch .jj/repo/op_heads, paused) is no longer run (margin has no timer, file watch or stream runner); the ids jj and jj-… in showcase are no longer owned by anyone",
            "the ids jj and jj-… in showcase are no longer owned by anyone",
            "    cd /work/repo && margin handler add jj --on jj.refresh --board showcase --owns jj -- /opt/providers/jj --limit '5 6'",
            "inotifywait -qm -e close_write,move,create,delete .jj/repo/op_heads | while read -r _; do margin --project proj emit jj.refresh; done",
            "its old health line stays as a dim line until removed: margin rm showcase jj-feed",
            "note  old feed agents (stream) is no longer run",
            "the ids ag and ag-… in agents now belong to owner agents",
            "    margin owner add agents --board agents --owns ag",
            "    cd /work && /opt/providers/agents | while read -r line; do margin --project proj apply agents --as agents <<<\"$line\"; done",
            "old feed ci (every 5m) is no longer run",
            "its board gone is gone",
            "margin handler add ci --on ci.refresh --board gone --owns '*' -- ci-rows",
            "    cron: */5 * * * * margin --project proj emit ci.refresh",
            "stale 5 files of the removed feed runner",
            "1 stale item: margin doctor --fix removes it",
        ] {
            assert!(report.contains(must), "{must}:\n{report}");
        }
        assert!(st.dir().join("feeds.json").exists(), "listing moves nothing");
        let fixed = run_text(&st, true);
        assert!(fixed.contains("fixed 5 files of the removed feed runner") && fixed.contains("now in .archive/feed-runner-") && fixed.contains("1 of 1 cleaned up"), "{fixed}");
        let kept = std::fs::read_dir(st.dir().join(".archive")).unwrap().next().unwrap().unwrap().path();
        for f in ["feeds.json", "feeds/jj.log", "feeds/jj.log.1", "feeds/jj.status.json", "feeds/daemon.log"] {
            assert!(kept.join(f).is_file(), "{f} is in the archive");
        }
        assert!(!st.dir().join("feeds.json").exists() && st.exists("feeds") && st.load("showcase").unwrap().find("jj-feed").is_some(), "the boards are as they were, the one named feeds too");
        assert!(!run_text(&st, true).contains("feed runner"), "nothing is left to say");
        // a feeds.json that does not parse stops nothing and is archived like any other
        std::fs::write(st.dir().join("feeds.json"), "{").unwrap();
        st.patch("showcase", &[crate::doc::Op::Set { id: "jj-1".into(), field: "text".into(), value: json!("still writable") }], None).unwrap();
        assert!(run_text(&st, false).contains("feeds.json does not parse") && run_text(&st, true).contains("fixed 1 file of the removed feed runner"));
        // a shipped script registered at its old place is named at its new one, when that is where it is
        let dir = st.home.join("checkout");
        std::fs::create_dir_all(dir.join("providers")).unwrap();
        std::fs::write(dir.join("providers/jj"), "").unwrap();
        assert_eq!(moved_script(&dir.join("feeds/jj").display().to_string()), dir.join("providers/jj").display().to_string());
        assert_eq!(moved_script("/nowhere/feeds/jj"), "/nowhere/feeds/jj");
        assert_eq!(schedule(60.0, "E"), "cron: * * * * * E");
        assert_eq!(schedule(7200.0, "E"), "cron: 0 */2 * * * E");
        assert_eq!(schedule(10.0, "E"), "a loop or a systemd timer (OnUnitActiveSec=10s): while sleep 10s; do E; done");
        std::fs::remove_dir_all(&st.home).ok();
    }

    fn run_text(st: &Store, fix: bool) -> String {
        run(st, fix, 30)
    }
}
