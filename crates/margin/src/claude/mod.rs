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
