//! Waking an idle Claude session when sulin finishes in the pane (r5). Claude Code gives every
//! session a local inbox, a Unix socket that peers write to (the same one `SendMessage` uses); a
//! message there starts a turn. margin writes one labelled line into it, and only to wake: the
//! decisions themselves still travel through the hooks, which Claude reads as sulin's own. A
//! message on the socket arrives framed as another session, never as him (tested 2026-11-08).
//!
//! That message is the fallback. Claude Code holds a peer's message for approval in some
//! permission modes, so it may not wake at all; the `Stop` hook `margin hook rewake` (rewake.rs,
//! `asyncRewake`) waits in the background instead and hands Claude the decisions themselves. The
//! pane sends the inbox message only when no such watcher is alive (`pane_wake`).
//!
//! The socket path and token reach margin through the hooks' environment. The hook keeps them in
//! `$XDG_RUNTIME_DIR/margin/<session>.json`, mode 0600: memory-backed, gone at logout, never in the
//! state directory and never printed.

use crate::store::Event;
use serde_json::{json, Value};
use std::io::Write;
use std::path::PathBuf;

/// Quiet this long after sulin's last action before margin calls him done (his undo window).
pub const QUIET: f64 = 5.0;
/// Quiet this long and margin wakes Claude even with a question set half answered, or review
/// mode still on: he has stepped away, and what he did is worth acting on.
pub const GIVE_UP: f64 = 60.0;

#[cfg(test)]
thread_local! {
    /// A test's own runtime directory (tests run on threads, and the environment is shared).
    pub static TEST_DIR: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

fn dir() -> PathBuf {
    #[cfg(test)]
    if let Some(d) = TEST_DIR.with(|d| d.borrow().clone()) {
        return d;
    }
    let base = std::env::var("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(|_| std::env::temp_dir());
    base.join("margin")
}

fn key(session: &str) -> String {
    session.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect()
}

/// Called by the hook: keep this session's inbox address for the pane. Writes only on a change.
pub fn save_inbox(session: &str) {
    let (Ok(sock), Ok(tok)) = (std::env::var("CLAUDE_CODE_MESSAGING_SOCKET"), std::env::var("CLAUDE_CODE_MESSAGING_TOKEN")) else { return };
    let p = dir().join(format!("{}.json", key(session)));
    let v = json!({"socket": sock, "token": tok});
    if std::fs::read(&p).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).as_ref() == Some(&v) {
        return;
    }
    let _ = std::fs::create_dir_all(dir());
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let _ = std::fs::set_permissions(dir(), std::fs::Permissions::from_mode(0o700));
        let tmp = p.with_extension("tmp");
        if let Ok(mut f) = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp) {
            let _ = f.write_all(v.to_string().as_bytes());
            let _ = std::fs::rename(&tmp, &p);
        }
    }
}

/// Write one message into a session's inbox. The text should start with `margin:` so it explains
/// itself.
pub fn send(session: &str, text: &str) -> Result<(), String> {
    let p = dir().join(format!("{}.json", key(session)));
    let v: Value = serde_json::from_slice(&std::fs::read(&p).map_err(|e| format!("no inbox for {session}: {e}"))?).map_err(|e| e.to_string())?;
    let (sock, tok) = (v["socket"].as_str().unwrap_or(""), v["token"].as_str().unwrap_or(""));
    #[cfg(unix)]
    {
        let mut s = std::os::unix::net::UnixStream::connect(sock).map_err(|e| format!("inbox {sock}: {e}"))?;
        let _ = s.set_write_timeout(Some(std::time::Duration::from_secs(2)));
        let lines = format!("{}\n{}\n", json!({"type": "auth", "token": tok}), json!({"type": "user", "message": {"role": "user", "content": text}}));
        s.write_all(lines.as_bytes()).map_err(|e| e.to_string())?;
        let _ = s.shutdown(std::net::Shutdown::Write);
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (sock, tok, text);
        Err("no Unix sockets here".into())
    }
}

fn private_dir() {
    let _ = std::fs::create_dir_all(dir());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir(), std::fs::Permissions::from_mode(0o700));
    }
}

fn open_private(p: &std::path::Path, truncate: bool) -> std::io::Result<std::fs::File> {
    let mut o = std::fs::OpenOptions::new();
    o.read(true).write(true).create(true).truncate(truncate);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(p)
}

pub fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // EPERM: it exists, under another user
        pid > 0 && (unsafe { libc::kill(pid as i32, 0) } == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM))
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

/// A session ended: remove what only a live session needs. Its inbox address, the rewake
/// watcher's mark (the watcher sees its mark gone at its next look and exits) and the record of
/// what a wake-up covered. Returns how many files went. The store is not touched.
pub fn end_session(session: &str) -> usize {
    [dir().join(format!("{}.json", key(session))), watch_path(session), woke_path(session)].iter().filter(|p| std::fs::remove_file(p).is_ok()).count()
}

/// The runtime files of sessions that are gone without saying so (a crash runs no hook): rewake
/// marks whose watcher's pid is dead. For `margin doctor`.
pub fn dead_marks() -> Vec<PathBuf> {
    std::fs::read_dir(dir())
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_name().to_string_lossy().ends_with(".rewake"))
                .filter(|e| std::fs::read_to_string(e.path()).ok().and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()).and_then(|v| v["pid"].as_u64()).map(|p| !pid_alive(p as u32)).unwrap_or(true))
                .map(|e| e.path())
                .collect()
        })
        .unwrap_or_default()
}

/// Review marks of panes that died in review mode (`<store>/review/<pid>-…`, pid dead).
pub fn dead_review_marks(st: &crate::store::Store) -> Vec<PathBuf> {
    std::fs::read_dir(review_dir(st))
        .map(|rd| rd.flatten().filter(|e| !e.file_name().to_string_lossy().split('-').next().and_then(|p| p.parse::<u32>().ok()).map(pid_alive).unwrap_or(false)).map(|e| e.path()).collect())
        .unwrap_or_default()
}

// --- the rewake watcher's mark (rewake.rs): `<session>.rewake`, {pid, start, until} ---

fn watch_path(session: &str) -> PathBuf {
    dir().join(format!("{}.rewake", key(session)))
}

/// A watcher takes the session over: the newest turn's end is the one that counts, and an older
/// watcher sees the mark is no longer its own and leaves.
pub fn watch_begin(session: &str, pid: u32, now: f64, until: f64) {
    private_dir();
    let p = watch_path(session);
    let tmp = p.with_extension(format!("rewake.tmp{pid}"));
    if let Ok(mut f) = open_private(&tmp, true) {
        let _ = f.write_all(json!({"pid": pid, "start": now, "until": until}).to_string().as_bytes());
        let _ = std::fs::rename(&tmp, &p);
    }
}

fn watch_mark(session: &str) -> Option<(u32, f64)> {
    let v: Value = serde_json::from_slice(&std::fs::read(watch_path(session)).ok()?).ok()?;
    Some((v["pid"].as_u64()? as u32, v["until"].as_f64().unwrap_or(0.0)))
}

/// Is the mark still this watcher's?
pub fn watch_owned(session: &str, pid: u32) -> bool {
    watch_mark(session).map(|(p, _)| p == pid).unwrap_or(false)
}

/// Remove the mark, if it is this watcher's.
pub fn watch_end(session: &str, pid: u32) {
    if watch_owned(session, pid) {
        let _ = std::fs::remove_file(watch_path(session));
    }
}

/// Is a rewake watcher waiting for this session: a mark whose process is alive and whose own
/// deadline hasn't passed (a killed watcher leaves its mark; the deadline bounds a reused pid).
pub fn watcher_live(session: &str, now: f64) -> bool {
    watch_mark(session).map(|(pid, until)| pid_alive(pid) && now < until).unwrap_or(false)
}

// --- what a wake-up already covered: `<session>.woke`, the highest action seq ---

fn woke_path(session: &str) -> PathBuf {
    dir().join(format!("{}.woke", key(session)))
}

/// The highest action seq a wake-up for this session covered. In a file, not in the pane: every
/// pane on the project and every restart of one (a rebuild re-execs them) share it.
pub fn woke(session: &str) -> u64 {
    std::fs::read_to_string(woke_path(session)).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0)
}

pub fn forget(session: &str) {
    let _ = std::fs::remove_file(woke_path(session));
}

/// Take the wake-up for actions up to `seq`: true for the one caller that may send it. Under a
/// file lock, so two panes looking in the same second don't both win.
pub fn claim(session: &str, seq: u64) -> bool {
    use std::io::{Read, Seek};
    private_dir();
    let Ok(mut f) = open_private(&woke_path(session), false) else { return true };
    let _ = f.lock();
    let mut s = String::new();
    let _ = f.read_to_string(&mut s);
    if s.trim().parse::<u64>().unwrap_or(0) >= seq {
        return false;
    }
    let _ = f.set_len(0);
    let _ = f.rewind();
    let _ = f.write_all(seq.to_string().as_bytes());
    true
}

/// The newest action a batch holds: what a wake-up for it covers.
pub fn top(net: &[Event]) -> u64 {
    net.iter().map(|e| e.seq).max().unwrap_or(0)
}

#[derive(Debug, PartialEq)]
pub enum Pane {
    /// A live rewake watcher will wake Claude with the decisions themselves: send nothing.
    Watcher,
    /// The inbox message went out (or failed: `Err`), once for this batch.
    Sent(Result<(), String>),
    /// A wake-up already covered this batch.
    Covered,
    /// `wake` is "hook" (or "off"): the pane never writes to the inbox.
    Never,
}

/// The pane's part once sulin is done with a batch whose newest action is `top`. `mode` is the
/// config's `wake`: "auto" leaves it to a live watcher and falls back to the inbox without one
/// (hook not installed, watcher past its deadline, a Claude Code without asyncRewake); "inbox"
/// always sends; "hook" and "off" never do.
pub fn pane_wake(session: &str, top: u64, mode: &str, now: f64, send: impl FnOnce() -> Result<(), String>) -> Pane {
    match mode {
        "hook" | "off" => Pane::Never,
        "inbox" => {
            if claim(session, top) { Pane::Sent(send()) } else { Pane::Covered }
        }
        _ if woke(session) >= top => Pane::Covered,
        _ if watcher_live(session, now) => Pane::Watcher,
        _ => {
            if claim(session, top) { Pane::Sent(send()) } else { Pane::Covered }
        }
    }
}

// --- review mode, said where the watcher can read it: `<store>/review/<pid>-<pane>-<art>` ---

fn review_dir(st: &crate::store::Store) -> PathBuf {
    st.dir().join("review")
}

/// A pane turned review mode on or off for one of its tabs.
pub fn review_mark(st: &crate::store::Store, pane: &str, art: &str, on: bool) {
    let p = review_dir(st).join(format!("{}-{}-{}", std::process::id(), key(pane), key(art)));
    if on {
        let _ = std::fs::create_dir_all(review_dir(st));
        let _ = std::fs::write(&p, b"");
    } else {
        let _ = std::fs::remove_file(&p);
    }
}

/// A pane starting: review mode is off in all its tabs (a rebuild re-execs under the same pid).
pub fn review_reset(st: &crate::store::Store) {
    let mine = format!("{}-", std::process::id());
    for e in std::fs::read_dir(review_dir(st)).into_iter().flatten().flatten() {
        if e.file_name().to_string_lossy().starts_with(&mine) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Is some running pane on this project in review mode?
pub fn reviewing(st: &crate::store::Store) -> bool {
    std::fs::read_dir(review_dir(st)).into_iter().flatten().flatten().any(|e| {
        let alive = e.file_name().to_string_lossy().split('-').next().and_then(|p| p.parse::<u32>().ok()).map(pid_alive).unwrap_or(false);
        if !alive {
            let _ = std::fs::remove_file(e.path()); // a pane that died in review mode
        }
        alive
    })
}

/// Has sulin started a question set he hasn't finished: a pick in `net` whose section still holds
/// an open question?
pub fn open_sets(doc: &crate::doc::Doc, state: &crate::store::State, net: &[Event]) -> bool {
    let mut parent_of: std::collections::HashMap<String, String> = Default::default();
    crate::doc::walk(&doc.blocks, &mut |b, p, _| {
        if let Some(p) = p {
            parent_of.insert(b.id.clone(), p.id.clone());
        }
    });
    net.iter().filter(|e| e.kind == "pick").filter_map(|e| parent_of.get(&e.id)).any(|sec| {
        doc.find(sec).map(|s| s.children.iter().any(|c| c.kind == "question" && !c.flag("applied") && !state.decisions.contains_key(&c.id))).unwrap_or(false)
    })
}

/// Is sulin done with this batch? `pending` is the net of his undelivered actions (hook::pending),
/// `open_sets` whether a question set he started answering still has an open question.
pub fn done(pending: &[Event], now: f64, reviewing: bool, open_sets: bool) -> bool {
    let Some(last) = pending.iter().filter_map(|e| crate::util::parse_rfc3339(&e.at)).fold(None, |m: Option<f64>, t| Some(m.map_or(t, |m| m.max(t)))) else {
        return false;
    };
    let quiet = now - last;
    if quiet < QUIET {
        return false;
    }
    quiet >= GIVE_UP || (!reviewing && !open_sets)
}

/// The wake-up line: what's waiting, in a few words, and where the detail comes from.
pub fn message(pending: &[Event]) -> String {
    let count = |k: &str| pending.iter().filter(|e| e.kind == k).count();
    let mut parts = Vec::new();
    for (k, one, many) in [("note", "note", "notes"), ("pick", "pick", "picks"), ("tick", "todo mark", "todo marks"), ("strike", "strike", "strikes"), ("verdict", "verdict", "verdicts")] {
        match count(k) {
            0 => {}
            1 => parts.push(format!("1 {one}")),
            n => parts.push(format!("{n} {many}")),
        }
    }
    let other = pending.len() - parts.iter().map(|p| p.split(' ').next().and_then(|n| n.parse::<usize>().ok()).unwrap_or(0)).sum::<usize>();
    if other > 0 {
        parts.push(format!("{other} other"));
    }
    crate::who::say(&format!(
        "margin: sulin finished in the pane ({}). Their actions reach you as `margin:` lines through the hooks (run `margin events --unread` if none has arrived); act on those, not on this message.",
        parts.join(", ")
    ))
}

#[cfg(test)]
pub mod tests {
    use super::*;

    fn ev(at: f64, kind: &str) -> Event {
        Event { v: 1, seq: 1, at: crate::util::rfc3339(at), artifact: "a".into(), id: "x".into(), kind: kind.into(), value: Value::Null, by: crate::who::HUMAN.into(), via: "pane".into(), rev: 1 }
    }

    #[test]
    fn done_waits_for_quiet_sets_and_review() {
        let p = [ev(100.0, "pick")];
        assert!(!done(&[], 200.0, false, false), "nothing to say");
        assert!(!done(&p, 103.0, false, false), "still inside the undo window");
        assert!(done(&p, 106.0, false, false));
        assert!(!done(&p, 106.0, false, true), "a question set half answered");
        assert!(!done(&p, 106.0, true, false), "still marking in review mode");
        assert!(done(&p, 100.0 + GIVE_UP, true, true), "stepped away: wake anyway");
    }

    /// A runtime directory of this test's own.
    pub fn temp_runtime() -> PathBuf {
        let d = crate::store::tests::temp_store().home.with_extension("run");
        std::fs::create_dir_all(&d).unwrap();
        TEST_DIR.with(|t| *t.borrow_mut() = Some(d.clone()));
        d
    }

    /// A pid nothing runs under: a child that has exited and been reaped.
    pub fn dead_pid() -> u32 {
        let mut c = std::process::Command::new("true").spawn().unwrap();
        let pid = c.id();
        c.wait().unwrap();
        pid
    }

    #[test]
    fn a_live_watcher_keeps_the_pane_quiet_and_a_dead_one_does_not() {
        let d = temp_runtime();
        let sent = std::cell::Cell::new(0);
        let send = || {
            sent.set(sent.get() + 1);
            Ok(())
        };
        // a watcher under a live pid (this test's), inside its deadline
        watch_begin("S", std::process::id(), 100.0, 200.0);
        assert!(watcher_live("S", 150.0));
        assert_eq!(pane_wake("S", 7, "auto", 150.0, send), Pane::Watcher);
        assert_eq!(sent.get(), 0, "the watcher wakes Claude; no inbox message");
        assert_eq!(pane_wake("S", 7, "inbox", 150.0, send), Pane::Sent(Ok(())), "inbox: always the message");
        assert_eq!(pane_wake("S", 9, "hook", 150.0, send), Pane::Never);
        // past its own deadline the mark counts for nothing
        assert_eq!(pane_wake("S", 8, "auto", 250.0, send), Pane::Sent(Ok(())));
        // a mark whose process is gone
        watch_begin("T", dead_pid(), 100.0, 200.0);
        assert!(!watcher_live("T", 150.0));
        assert_eq!(pane_wake("T", 7, "auto", 150.0, send), Pane::Sent(Ok(())));
        // no mark at all: the old hook config
        assert_eq!(pane_wake("U", 7, "auto", 150.0, send), Pane::Sent(Ok(())));
        assert_eq!(sent.get(), 4);
        // the mark is private, and only its owner removes it
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(d.join("S.rewake")).unwrap().permissions().mode() & 0o777, 0o600);
        }
        watch_end("S", 1);
        assert!(watch_owned("S", std::process::id()));
        watch_end("S", std::process::id());
        assert!(!d.join("S.rewake").exists());
        std::fs::remove_dir_all(&d).ok();
    }

    /// The duplicate wake-up: a second message with nothing new behind it. The pane compared the
    /// cursor (which moves past actions that net to nothing) with a seq it kept in memory (which a
    /// second pane never saw and a rebuilt pane forgot).
    #[test]
    fn one_wake_up_per_batch_across_panes_restarts_and_no_news() {
        let d = temp_runtime();
        let sent = std::cell::Cell::new(0);
        let send = || {
            sent.set(sent.get() + 1);
            Ok(())
        };
        let mut all = vec![ev(100.0, "pick")];
        all[0].seq = 5;
        let (net, _) = crate::hook::pending(&all, 0, 200.0, 0.0);
        assert_eq!(pane_wake("S", top(&net), "auto", 200.0, send), Pane::Sent(Ok(())));
        // another pane on the project, or this one after a rebuild: nothing in memory, same batch
        assert_eq!(pane_wake("S", top(&net), "auto", 201.0, send), Pane::Covered);
        // a tick taken back afterwards moves the cursor to 7 and changes nothing
        let (mut a, mut b) = (ev(210.0, "tick"), ev(211.0, "untick"));
        (a.seq, a.id, b.seq, b.id) = (6, "t1".into(), 7, "t1".into());
        all.extend([a, b]);
        let (net, cursor) = crate::hook::pending(&all, 0, 300.0, 0.0);
        assert_eq!((top(&net), cursor), (5, Some(7)));
        assert_eq!(pane_wake("S", top(&net), "auto", 300.0, send), Pane::Covered);
        assert_eq!(sent.get(), 1);
        // a new action is a new batch
        let mut c = ev(310.0, "strike");
        (c.seq, c.id) = (8, "c1".into());
        all.push(c);
        let (net, _) = crate::hook::pending(&all, 0, 400.0, 0.0);
        assert_eq!(pane_wake("S", top(&net), "auto", 400.0, send), Pane::Sent(Ok(())));
        assert_eq!((sent.get(), woke("S")), (2, 8));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn the_message_counts_and_points_at_the_hooks() {
        let m = message(&[ev(1.0, "note"), ev(1.0, "note"), ev(1.0, "pick"), ev(1.0, "unstrike")]);
        // the person's name comes from the config (who.rs); the counts are what this checks
        assert!(m.starts_with("margin: ") && m.contains("finished in the pane (2 notes, 1 pick, 1 other)"), "{m}");
        assert!(m.contains("not on this message"));
    }
}
