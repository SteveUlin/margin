//! The Claude Code adapter: everything in margin that knows what a Claude session is.
//!
//! The core (boards, the event log and its cursors, handlers, owners, datasets, the pane) names no
//! consumer. This module is one: it reads the person's pane events on behalf of a Claude Code
//! session and tells the pane who is working on a board. The dependency runs one way. Code here
//! uses the core freely; the core reaches this module only through `crate::consumer`, the seam
//! `install` plugs into, and through `main.rs`, which dispatches `margin claude …` to it.
//!
//! - `hook`: the Claude Code hooks (`margin claude hook <event>`): hand a session the person's
//!   decisions, record what the session is doing, refuse a tool call while the stop flag is up
//! - `rewake`, `wake`: wake an idle session once the person is done in the pane
//! - `link`: which board a session keeps
//! - `agents`, `recap`: a session's subagents and its recap, read from its transcript
//! - `attention`: the mark on the terminal tab while a session waits on the person
//! - `init`: `margin claude init`, which installs the skill and the hooks
//!
//! The manners of this adapter are its own, not rules of the core: it marks the tab instead of
//! ringing a bell, it wakes a session only after the person has gone quiet, and the panes it asks
//! the host for open without taking focus.

pub mod agents;
pub mod attention;
pub mod hook;
pub mod init;
pub mod link;
pub mod recap;
pub mod rewake;
pub mod wake;

use crate::consumer::{Consumer, PaneWatch, Presence, Tick};
use crate::store::Store;

/// The name the pane writes for this consumer ("Claude is current", "stop Claude").
pub const NAME: &str = "Claude";
/// The `by` of a write made from inside a Claude Code session.
pub const AUTHOR: &str = "claude";

/// The skill a Claude session loads: the adapter's own document, a template like the topics
/// (`help::render` fills in the person's name and this binary's path).
pub const SKILL: &str = include_str!("../../../../skill/SKILL.md");

/// The skill rendered for this machine (`margin claude skill`).
pub fn skill() -> String {
    crate::help::render(SKILL, &crate::help::Ctx::here())
}

/// Is this process running inside Claude Code? Its Bash tool sets these.
pub fn here() -> bool {
    std::env::var_os("CLAUDECODE").is_some() || std::env::var_os("CLAUDE_CODE_ENTRYPOINT").is_some()
}

/// The Claude session this process runs in, when it runs in one.
pub fn session_here() -> Option<String> {
    std::env::var("CLAUDE_CODE_SESSION_ID").ok().filter(|s| !s.is_empty())
}

/// `margin open <board>` from inside a session that keeps no board yet makes this board its own
/// (a pane stacked on later does not take the link over). Outside a session: nothing.
pub fn opened(st: &Store, board: &str, stacked: bool) {
    if let Some(sess) = session_here() {
        if !stacked && link::get(st, &sess).is_none() {
            link::set(st, &sess, board, "open");
        }
    }
}

struct Adapter;

/// Plug the adapter into the core's seam. `main` calls it once; tests that draw a linked board
/// call it too (a second call is ignored).
pub fn install() {
    crate::consumer::install(&Adapter);
}

impl Consumer for Adapter {
    fn on_board(&self, st: &Store, board: &str) -> Option<Presence> {
        let sessions: Vec<String> = link::all(st).into_iter().filter(|l| l.art == board).map(|l| l.session).collect();
        // no session keeps this board: the pane draws nothing of Claude, whatever other sessions
        // in the project are doing
        if sessions.is_empty() {
            return None;
        }
        let (linked, ended) = link::liveness(st, board);
        Some(Presence {
            // named only while a live session keeps the board: one that ended leaves the quiet
            // line saying when, and no name
            name: if linked { NAME.into() } else { String::new() },
            linked,
            ended,
            activity: link::activity_for(st, board),
            recap: recap::for_pane(st, board),
            workers_title: "Agents".into(),
            workers: agents::workers(&agents::for_pane(st, board)),
            undelivered: if linked { undelivered(st, board, &sessions) } else { None },
        })
    }

    fn pane(&self) -> Box<dyn PaneWatch> {
        Box::new(Watch { attn: attention::Attention::default(), checked: -10.0, look: (0, String::new()), owed: false })
    }

    fn pane_started(&self, st: &Store) {
        wake::review_reset(st);
    }

    fn review(&self, st: &Store, pane: &str, board: &str, on: bool) {
        // said in the store, where the rewake watcher reads it (wake.rs)
        wake::review_mark(st, pane, board, on);
    }
}

/// The person's decisions on `board` that a live session keeping it has not been handed (the
/// session's cursor under `delivered/` is behind them; the hooks move it), and whether that
/// session has worked on since. The session furthest behind answers for the board.
fn undelivered(st: &Store, board: &str, sessions: &[String]) -> Option<crate::consumer::Undelivered> {
    let doc = st.load(board).ok();
    sessions
        .iter()
        .filter(|s| st.activity(s).map(|a| a.state != "ended").unwrap_or(true))
        .filter_map(|s| {
            let evs: Vec<_> = hook::unread(st, s).into_iter().filter(|e| e.artifact == board).collect();
            let oldest = crate::util::parse_rfc3339(&evs.first()?.at)?;
            Some(crate::consumer::Undelivered { count: evs.len(), oldest, active_since: active_since(st.activity(s).as_ref(), doc.as_ref(), oldest) })
        })
        .max_by_key(|u| u.count)
}

/// Has a session worked on past decisions made at `oldest` without being handed them? From
/// what is recorded, by two signals, each counted only once the decisions had settled (a hook
/// holds back a decision younger than `hook::SETTLE_SECS`):
/// - the session's own activity record, which its hooks write. `thinking` is written by the
///   prompt and post-tool hooks, the two that hand decisions over: one after the decisions
///   settled means a hook ran and delivered nothing. `idle` (the turn ended) and `running` or
///   `waiting` (one tool call in flight; the hook after it delivers) are the normal waits;
/// - the board itself: a revision written from inside a session after the decisions settled.
///   This one needs no hook, so it is what catches a session whose hooks do not run at all.
///
/// `Some(false)` when the record says the session is idle or mid-call, `None` when there is no
/// record or it says nothing either way.
fn active_since(act: Option<&crate::store::Activity>, doc: Option<&crate::doc::Doc>, oldest: f64) -> Option<bool> {
    let settled = oldest + hook::SETTLE_SECS;
    let after = |t: &str| crate::util::parse_rfc3339(t).is_some_and(|t| t > settled);
    if doc.is_some_and(|d| d.by == AUTHOR && after(&d.updated)) {
        return Some(true);
    }
    match act.map(|a| (a.state.as_str(), after(&a.since))) {
        Some(("thinking", true)) => Some(true),
        Some(("idle" | "running" | "waiting", _)) => Some(false),
        _ => None,
    }
}

/// What one pane keeps for its Claude sessions.
struct Watch {
    /// The mark on this pane's terminal tab while a session waits on the person out of view; Drop
    /// takes it down. A mark, never a bell: that is this adapter's choice, not the core's.
    attn: attention::Attention,
    /// When the pane last checked whether a wake-up is due (what one covered is in `wake::woke`).
    checked: f64,
    /// What that check covered, and whether actions were still waiting out their quiet period.
    look: (u64, String),
    owed: bool,
}

impl PaneWatch for Watch {
    fn step(&mut self, k: &Tick) -> Option<f64> {
        let mut at: Option<f64> = None;
        let mut come_back = |x: f64| at = Some(at.map_or(x, |a| a.min(x)));
        // the tab mark: a wake only for a look that is owed (attention.rs); an idle session with
        // nothing else pending asks for none
        if let Some(x) = self.attn.step(k.activity, k.t, k.cfg, || (k.visible)()) {
            come_back(x);
        }
        // wake an idle session once the person is done in the pane; at most one look a second
        let idle = attention::need(k.activity).filter(|a| k.cfg.wake != "off" && a.state == "idle").map(|a| a.session.clone());
        let look = (k.last_seq, idle.clone().unwrap_or_default());
        if k.t - self.checked < 1.0 {
            // an action (or the turn's end) landed inside that second, or actions are waiting
            // out their quiet period: come back for the look. Nothing new and nothing waiting
            // asks for nothing, so an idle pane sleeps
            if idle.is_some() && (self.owed || look != self.look) {
                come_back(self.checked + 1.0);
            }
            return at;
        }
        self.checked = k.t;
        self.look = look;
        self.owed = false;
        let Some(sess) = idle else { return at };
        let st = k.store;
        let all = st.events();
        let (net, _) = hook::pending(&all, st.delivered(&sess), k.wall, 0.0);
        // a wake-up covers the batch's newest action. Not the cursor: that moves past
        // actions that net to nothing, and a tick taken back sent a second wake-up
        let top = wake::top(&net);
        // a store that started over hands out low seqs again: an old mark covers nothing
        let mut woke = wake::woke(&sess);
        if woke > all.last().map(|e| e.seq).unwrap_or(0) {
            wake::forget(&sess);
            woke = 0;
        }
        let mut owed = top > woke;
        // review mode or a half-answered question set in any tab holds the wake-up back
        let open = k.boards.iter().any(|(doc, state)| wake::open_sets(doc, state, &net));
        if owed && wake::done(&net, k.wall, k.review, open) {
            let send = || wake::send(&sess, &wake::message(&net));
            match wake::pane_wake(&sess, top, &k.cfg.wake, k.wall, send) {
                // the rewake hook's watcher will wake the session; keep looking in case it dies
                wake::Pane::Watcher => {}
                wake::Pane::Sent(Err(e)) => {
                    crate::select::log_line(&format!("wake {sess}: {e}"));
                    owed = false; // claimed: no retry every second
                }
                _ => owed = false,
            }
        }
        if owed {
            self.owed = true;
            come_back(k.t + 1.0);
        }
        at
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn board(st: &Store) {
        let doc: crate::doc::Doc = serde_json::from_value(json!({"id": "ci", "title": "CI", "blocks": [{"type": "row", "id": "j1", "text": "build"}]})).unwrap();
        st.put(doc, None).unwrap();
    }

    /// What the adapter tells the seam about decisions a session has not been handed: none once
    /// its cursor is past them; while they wait, whether the session worked on without them.
    #[test]
    fn undelivered_decisions_are_counted_per_board_until_the_cursor_passes_them() {
        let st = crate::store::tests::temp_store();
        let doc: crate::doc::Doc = serde_json::from_value(json!({"id": "ci", "title": "CI", "blocks": [{"type": "question", "id": "q1", "text": "Which?", "options": ["x", "y"]}]})).unwrap();
        st.put(doc, None).unwrap();
        link::set(&st, "mine", "ci", "explicit");
        st.first_seen("mine");
        assert_eq!(Adapter.on_board(&st, "ci").unwrap().undelivered, None, "nothing decided");
        let e = st.decide("ci", "q1", "pick", json!("x"), "key", None).unwrap();
        let u = Adapter.on_board(&st, "ci").unwrap().undelivered.expect("one waits");
        assert_eq!((u.count, u.active_since), (1, None), "no activity recorded: nothing says why");
        let act = |state: &str, since: f64| crate::store::Activity { session: "mine".into(), state: state.into(), since: crate::util::rfc3339(since), ..Default::default() };
        st.set_activity(&act("idle", u.oldest + 30.0));
        assert_eq!(Adapter.on_board(&st, "ci").unwrap().undelivered.unwrap().active_since, Some(false), "the turn ended: an idle session is normal");
        st.set_activity(&act("thinking", u.oldest + 30.0));
        assert_eq!(Adapter.on_board(&st, "ci").unwrap().undelivered.unwrap().active_since, Some(true), "a delivering hook ran and delivered nothing");
        // the hook hands it over: the cursor passes it and nothing waits
        st.set_delivered("mine", e.seq);
        assert_eq!(Adapter.on_board(&st, "ci").unwrap().undelivered, None);
        // the signals, one at a time
        let d = |by: &str, at: f64| crate::doc::Doc { by: by.into(), updated: crate::util::rfc3339(at), ..crate::doc::Doc::empty("ci") };
        let t = 1_000_000.0;
        assert_eq!(active_since(Some(&act("running", t + 30.0)), None, t), Some(false), "one call in flight: the hook after it delivers");
        assert_eq!(active_since(Some(&act("thinking", t + 2.0)), None, t), None, "before the decision settled: no hook would have handed it over yet");
        assert_eq!(active_since(None, Some(&d(AUTHOR, t + 30.0)), t), Some(true), "it wrote the board since, with no hook recording anything");
        assert_eq!(active_since(Some(&act("idle", t - 9.0)), Some(&d(AUTHOR, t + 30.0)), t), Some(true));
        assert_eq!(active_since(None, Some(&d("you", t + 30.0)), t), None, "the person's own write says nothing of the session");
        std::fs::remove_dir_all(&st.home).ok();
    }

    /// Nobody keeps a board until a session links to it: the project's other sessions, however
    /// busy, put nothing on it. Linked, the presence carries the name the pane writes and only
    /// that session's activity.
    #[test]
    fn a_board_has_a_presence_only_once_a_session_keeps_it() {
        let st = crate::store::tests::temp_store();
        board(&st);
        let act = |s: &str| crate::store::Activity { session: s.into(), state: "running".into(), text: "Build".into(), since: crate::util::now_rfc3339(), ..Default::default() };
        st.set_activity(&act("other"));
        assert!(Adapter.on_board(&st, "ci").is_none(), "a session elsewhere in the project is not on this board");
        link::set(&st, "mine", "ci", "explicit");
        st.set_activity(&act("mine"));
        let p = Adapter.on_board(&st, "ci").unwrap();
        assert_eq!((p.name.as_str(), p.linked, p.workers_title.as_str()), (NAME, true, "Agents"));
        assert_eq!(p.activity.iter().map(|a| a.session.as_str()).collect::<Vec<_>>(), ["mine"]);
        // through the seam, once installed
        install();
        assert_eq!(crate::consumer::on_board(&st, "ci").name, NAME);
        link::remove(&st, "mine");
        assert_eq!(crate::consumer::on_board(&st, "ci"), Presence::default());
        std::fs::remove_dir_all(&st.home).ok();
    }

    /// With no session's activity the pane's callback asks for nothing and touches nothing.
    #[test]
    fn a_pane_with_no_session_asks_for_no_wake() {
        let st = crate::store::tests::temp_store();
        let cfg = crate::config::Config::default();
        let mut w = Adapter.pane();
        for t in [0.0, 0.5, 2.0, 60.0] {
            let k = Tick { activity: &[], t, wall: crate::util::epoch_now(), cfg: &cfg, store: &st, last_seq: 0, review: false, boards: &[], visible: &|| panic!("the host is not asked") };
            assert_eq!(w.step(&k), None, "t={t}");
        }
    }
}
