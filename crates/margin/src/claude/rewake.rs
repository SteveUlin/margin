//! `margin claude hook rewake`: the `Stop` hook that waits. Claude Code runs a command hook marked
//! `"asyncRewake": true` in the background and, when it exits with code 2, wakes the session (idle
//! or not) with the hook's stderr as a system reminder. So this process starts as a turn ends,
//! waits until sulin is done in the pane, prints his decisions as the `margin:` lines the prompt
//! hook would have printed, moves the session's delivered cursor past them, and exits 2.
//!
//! "Done" is the pane's own rule (wake::done over hook::pending): quiet for wake::QUIET, with no
//! question set half answered and no pane in review mode, or quiet for wake::GIVE_UP regardless.
//! A click taken back inside the window nets to nothing and wakes nobody.
//!
//! It exits 0, silently, when it has nothing to wake for: a newer turn started (the session's
//! activity says thinking or running), a later watcher took the session over, the session has no
//! pane, the config's `wake` is "off" or "inbox", or its own deadline passed (a little under the
//! hook's `timeout`, so Claude Code never has to kill it).
//!
//! It waits without a timer (watch.rs): blocked until one of the files its answer depends on
//! changes (the event log, the session's activity and cursor, the review marks, its own mark, the
//! boards a pending decision is on), or until the next moment the clock alone changes the answer:
//! the end of the quiet period, the give-up time, its deadline. Only while a batch is past its
//! quiet period and held back (review mode, a half-answered set) does it look again every
//! `HELD_TICK`: what releases it then may be a pane dying, which writes no file.
//!
//! While it lives it holds `$XDG_RUNTIME_DIR/margin/<session>.rewake` (wake.rs); the pane reads
//! that mark and leaves the wake-up to it.

use crate::store::{Event, Store};
use super::{hook, link, wake};
use serde_json::{json, Value};
use std::io::Read;
use std::time::SystemTime;

/// Its own deadline, under the hook timeout `init` writes (3600 s).
pub const DEADLINE: f64 = 3480.0;
/// How often a batch that is held back (review mode, an open question set) is looked at again.
const HELD_TICK: f64 = 2.0;

#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// Exit 2 with this on stderr.
    Wake(String),
    /// Exit 0; the reason is for tests and the log.
    Silent(&'static str),
}

pub struct Watch {
    st: Store,
    session: String,
    pid: u32,
    deadline: f64,
    /// The events file and the session's cursor as last parsed: (len, mtime, delivered).
    seen: Option<(u64, Option<SystemTime>, u64)>,
    net: Vec<Event>,
    cursor: Option<u64>,
}

impl Watch {
    /// Take the session over, or say why there is nothing to watch.
    pub fn start(st: Store, session: &str, mode: &str, now: f64, timeout: f64) -> Result<Watch, &'static str> {
        if !st.dir().exists() {
            return Err("no store for this project");
        }
        if matches!(mode, "off" | "inbox") {
            return Err("wake is off or inbox");
        }
        if link::get(&st, session).or_else(|| link::inherit(&st, session)).is_none() {
            return Err("the session has no pane");
        }
        // the turn ended: the Stop hook beside this one says so too, but the two run at once, and
        // a "thinking" still on record would read as a newer turn
        hook::record(&st, "stop", session, &json!({}));
        st.first_seen(session);
        let pid = std::process::id();
        wake::watch_begin(session, pid, now, now + timeout);
        Ok(Watch { st, session: session.into(), pid, deadline: now + timeout, seen: None, net: vec![], cursor: None })
    }

    /// One look. The events file is parsed only when it (or the session's cursor) changed; a batch
    /// waiting out its quiet period costs a clock comparison.
    pub fn step(&mut self, now: f64) -> Option<Outcome> {
        if !wake::watch_owned(&self.session, self.pid) {
            return Some(Outcome::Silent("a later watcher took over"));
        }
        if self.st.activity(&self.session).map(|a| matches!(a.state.as_str(), "thinking" | "running")).unwrap_or(false) {
            return Some(Outcome::Silent("a newer turn started"));
        }
        if now >= self.deadline {
            return Some(Outcome::Silent("deadline"));
        }
        let meta = std::fs::metadata(self.st.events_path()).ok();
        let sig = (meta.as_ref().map(|m| m.len()).unwrap_or(0), meta.and_then(|m| m.modified().ok()), self.st.delivered(&self.session));
        if self.seen != Some(sig) {
            self.seen = Some(sig);
            (self.net, self.cursor) = hook::pending(&self.st.events(), sig.2, now, 0.0);
        }
        if self.net.is_empty() {
            return None;
        }
        let mut arts: Vec<&str> = self.net.iter().map(|e| e.artifact.as_str()).collect();
        arts.sort();
        arts.dedup();
        let open = arts.iter().any(|a| self.st.load(a).map(|d| wake::open_sets(&d, &self.st.state(a), &self.net)).unwrap_or(false));
        if !wake::done(&self.net, now, wake::reviewing(&self.st), open) {
            return None;
        }
        let text = crate::who::say(&hook::brief(&self.st, &self.net));
        if let Some(c) = self.cursor {
            self.st.set_delivered(&self.session, c);
        }
        // this batch has its wake-up: the pane's inbox message stays unsent
        wake::claim(&self.session, wake::top(&self.net));
        Some(Outcome::Wake(text))
    }

    /// When to look again if no watched file changes first: the next moment the clock alone can
    /// change what `step` says. Call after a `step` that returned None.
    pub fn next_look(&self, now: f64) -> f64 {
        let mut t = self.deadline;
        if let Some(last) = self.net.iter().filter_map(|e| crate::util::parse_rfc3339(&e.at)).reduce(f64::max) {
            if now >= last + wake::QUIET {
                // quiet, and still not done: held by review mode or an open set
                t = t.min(now + HELD_TICK);
            }
            for at in [last + wake::QUIET, last + wake::GIVE_UP] {
                if at > now {
                    t = t.min(at);
                }
            }
        }
        t
    }

    /// The files `step` reads, as a watch to sleep on. Built before the first `step`.
    /// `runtime`: where wake.rs keeps `<session>.rewake`, which a later watcher replaces.
    fn files(&self, runtime: &std::path::Path) -> crate::watch::Watch {
        let dir = self.st.dir();
        let mut w = crate::watch::Watch::new().dir(&dir, &["events.jsonl"]).dir(&self.st.activity_dir(), &[]).dir(&dir.join("delivered"), &[]).dir(&dir.join("review"), &[]);
        // made by `watch_begin`, with its own permissions: watched, never created here
        if runtime.is_dir() {
            w.add_dir(runtime, &[]);
        }
        w
    }

    pub fn end(&self) {
        wake::watch_end(&self.session, self.pid);
    }
}

/// The process: read the hook's JSON, wait, and return the exit code (2 wakes Claude).
pub fn run(home: Option<&str>, project: Option<&str>, timeout: f64) -> i32 {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let v: Value = serde_json::from_str(&input).unwrap_or(json!({}));
    // a subagent's stop is not the session's
    if !v["agent_id"].as_str().unwrap_or("").is_empty() {
        return 0;
    }
    let session = v["session_id"].as_str().map(String::from).or_else(|| std::env::var("CLAUDE_CODE_SESSION_ID").ok()).unwrap_or_else(|| "default".into());
    let cwd = v["cwd"].as_str().map(std::path::PathBuf::from).unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let mut st = crate::util::project_store_for(&cwd);
    if let Some(h) = home {
        st.home = h.into();
    }
    if let Some(p) = project {
        st.project = p.into();
    }
    let cfg = crate::config::Config::load();
    let Ok(mut w) = Watch::start(st, &session, &cfg.wake, crate::util::epoch_now(), timeout) else { return 0 };
    wake::save_inbox(&session);
    let mut files = w.files(&std::env::var("XDG_RUNTIME_DIR").map(std::path::PathBuf::from).unwrap_or_else(|_| std::env::temp_dir()).join("margin"));
    let mut boards: Vec<String> = vec![];
    loop {
        match w.step(crate::util::epoch_now()) {
            Some(Outcome::Wake(text)) => {
                w.end();
                eprintln!("{text}");
                return 2;
            }
            Some(Outcome::Silent(_)) => {
                w.end();
                return 0;
            }
            None => {
                // a pending decision's board: an answer applied or a question added there
                // changes whether its set is still open
                for a in w.net.iter().map(|e| &e.artifact).filter(|a| !a.is_empty()) {
                    if !boards.contains(a) {
                        files.add_dir(&w.st.art(a), &["doc.json", "state.json"]);
                        boards.push(a.clone());
                    }
                }
                let now = crate::util::epoch_now();
                files.wait(Some(std::time::Duration::try_from_secs_f64(w.next_look(now) - now).unwrap_or_default()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::{plan, temp_store};
    use crate::util::epoch_now;

    /// A store with the plan, a session linked to it, and a runtime directory of the test's own.
    fn setup() -> (Store, std::path::PathBuf) {
        let run = wake::tests::temp_runtime();
        let st = temp_store();
        st.put(plan(), None).unwrap();
        link::set(&st, "S", "plan", "explicit").unwrap();
        (st, run)
    }
    fn watch(st: &Store) -> Watch {
        Watch::start(Store::new(st.home.clone(), st.project.clone()), "S", "auto", epoch_now(), 600.0).unwrap()
    }
    fn cleanup(st: &Store, run: &std::path::Path) {
        std::fs::remove_dir_all(&st.home).ok();
        std::fs::remove_dir_all(run).ok();
    }

    #[test]
    fn a_pick_then_quiet_wakes_with_the_line_and_moves_the_cursor() {
        let (st, run) = setup();
        let mut w = watch(&st);
        let now = epoch_now();
        assert!(wake::watcher_live("S", now), "the pane can see the watcher");
        assert_eq!(w.step(now), None, "nothing happened yet");
        let e = st.decide("plan", "Q1", "pick", json!("mcp"), "select", None).unwrap();
        assert_eq!(w.step(now + 1.0), None, "inside his undo window");
        let Some(Outcome::Wake(text)) = w.step(now + wake::QUIET + 1.0) else { panic!("no wake") };
        assert!(text.starts_with("margin: ") && text.contains("picked mcp for plan.Q1") && text.contains("your default was cli"), "{text}");
        assert_eq!(st.delivered("S"), e.seq, "handed over: the prompt hook won't say it again");
        assert!(hook::unread(&st, "S").is_empty());
        assert_eq!(wake::woke("S"), e.seq, "and the pane's inbox message for it is spoken for");
        w.end();
        assert!(!wake::watcher_live("S", now));
        cleanup(&st, &run);
    }

    /// What the process sleeps until when no file changes: its deadline while nothing waits,
    /// then the end of the quiet period, then (held back) a short look-again up to the give-up.
    #[test]
    fn the_next_look_is_the_next_moment_the_clock_matters() {
        let (st, run) = setup();
        let mut w = watch(&st);
        let mut files = w.files(&run);
        let now = epoch_now();
        assert_eq!(w.step(now), None);
        assert_eq!(w.next_look(now), w.deadline, "nothing pending: no clock but the deadline");
        assert_eq!(files.wait(Some(std::time::Duration::from_millis(30))), crate::watch::Woken::Timeout);
        let e = st.decide("plan", "Q1", "pick", json!("mcp"), "select", None).unwrap();
        assert_eq!(files.wait(Some(std::time::Duration::from_secs(5))), crate::watch::Woken::Changed, "his pick wakes the watcher");
        let at = crate::util::parse_rfc3339(&e.at).unwrap();
        assert_eq!(w.step(at + 1.0), None);
        assert_eq!(w.next_look(at + 1.0), at + wake::QUIET, "the end of his undo window");
        // quiet, but a pane is in review mode: held, looked at again soon, never past the give-up
        wake::review_mark(&st, "tty", "plan", true);
        assert_eq!(w.step(at + wake::QUIET), None);
        assert_eq!(w.next_look(at + wake::QUIET), at + wake::QUIET + HELD_TICK);
        assert_eq!(w.next_look(at + wake::GIVE_UP - 0.5), at + wake::GIVE_UP);
        assert!(matches!(w.step(at + wake::GIVE_UP), Some(Outcome::Wake(_))));
        cleanup(&st, &run);
    }

    #[test]
    fn a_click_taken_back_inside_the_window_wakes_nobody() {
        let (st, run) = setup();
        let mut w = watch(&st);
        let now = epoch_now();
        st.decide("plan", "t1", "tick", json!("next"), "key", None).unwrap();
        assert_eq!(w.step(now + 1.0), None);
        st.decide("plan", "t1", "untick", Value::Null, "key", None).unwrap();
        for dt in [2.0, wake::QUIET + 2.0, wake::GIVE_UP + 2.0] {
            assert_eq!(w.step(now + dt), None, "+{dt}s");
        }
        cleanup(&st, &run);
    }

    #[test]
    fn a_half_answered_set_and_review_mode_hold_it_until_he_steps_away() {
        let (st, run) = setup();
        let two = json!({"id": "qs", "title": "qs", "blocks": [{"type": "section", "id": "set", "title": "Set", "children": [
            {"type": "question", "id": "A", "text": "a?", "options": ["x", "y"]}, {"type": "question", "id": "B", "text": "b?", "options": ["x", "y"]}]}]});
        st.put(serde_json::from_value(two).unwrap(), None).unwrap();
        let mut w = watch(&st);
        let now = epoch_now();
        st.decide("qs", "A", "pick", json!("x"), "select", None).unwrap();
        assert_eq!(w.step(now + wake::QUIET + 1.0), None, "B is still open");
        st.decide("qs", "B", "pick", json!("y"), "select", None).unwrap();
        // a pane (this process) in review mode
        wake::review_mark(&st, "tty", "qs", true);
        assert_eq!(w.step(now + wake::QUIET + 2.0), None, "still marking");
        wake::review_mark(&st, "tty", "qs", false);
        assert!(matches!(w.step(now + wake::QUIET + 2.0), Some(Outcome::Wake(_))));
        cleanup(&st, &run);
    }

    #[test]
    fn a_newer_turn_a_later_watcher_and_the_deadline_end_it_silently() {
        let (st, run) = setup();
        let now = epoch_now();
        // a newer turn: his prompt, with a pick waiting
        let mut w = watch(&st);
        st.decide("plan", "Q1", "pick", json!("mcp"), "select", None).unwrap();
        hook::record(&st, "prompt", "S", &json!({}));
        assert_eq!(w.step(now + wake::QUIET + 1.0), Some(Outcome::Silent("a newer turn started")));
        assert_eq!(hook::unread(&st, "S").len(), 1, "the pick is the prompt hook's to hand over");
        // the next turn's end starts a watcher that takes the session over
        let mut w2 = watch(&st);
        assert_eq!(w2.step(now), None);
        wake::watch_begin("S", wake::tests::dead_pid(), now, now + 600.0);
        assert_eq!(w2.step(now + wake::QUIET + 1.0), Some(Outcome::Silent("a later watcher took over")));
        w2.end();
        assert!(run.join("S.rewake").exists(), "the mark is the later watcher's to remove");
        // its own deadline
        let mut w3 = watch(&st);
        assert_eq!(w3.step(now + 601.0), Some(Outcome::Silent("deadline")));
        cleanup(&st, &run);
    }

    #[test]
    fn nothing_to_watch_without_a_pane_or_with_the_hook_turned_off() {
        let (st, run) = setup();
        let again = || Store::new(st.home.clone(), st.project.clone());
        assert_eq!(Watch::start(again(), "S", "off", 0.0, 600.0).err(), Some("wake is off or inbox"));
        assert_eq!(Watch::start(again(), "S", "inbox", 0.0, 600.0).err(), Some("wake is off or inbox"));
        // (a session in a Zellij pane that had a link inherits it; outside one there is none)
        if link::chat_here().0.is_empty() {
            assert_eq!(Watch::start(again(), "nobody", "auto", 0.0, 600.0).err(), Some("the session has no pane"));
        }
        assert!(!run.join("S.rewake").exists() && !run.join("nobody.rewake").exists());
        cleanup(&st, &run);
    }
}
