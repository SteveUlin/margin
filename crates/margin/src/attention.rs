//! "Claude is waiting on you", outside the pane. Inside it, the header says so; when the pane's
//! tab isn't the active one, the tab itself gets a `● ` mark (renamed by id: no focus moves).
//! Never a bell: sulin doesn't want one (2026-11-08). Telling a phone is a policy, so it lives
//! outside margin (`margin help integrate` has the recipe).
//!
//! The pane owns this because it already watches the activity files and knows whether it's on
//! screen; the mark goes when Claude is busy again, when the tab comes into view, or when the
//! pane exits (Drop).

use crate::config::Config;
use crate::store::Activity;

pub const MARK: &str = "● ";

/// What the newest main-session activity asks of sulin: `waiting` (blocked on a permission or a
/// question Claude Code raised) or `idle` (the turn ended: his move).
pub fn need(acts: &[Activity]) -> Option<&Activity> {
    acts.iter().find(|a| a.agent.is_empty()).filter(|a| a.state == "waiting" || a.state == "idle")
}

#[derive(Debug, PartialEq)]
pub enum Act {
    Mark,
    Unmark,
    Keep,
}

/// The tab mark: on while Claude needs sulin and the pane is out of view, off otherwise.
pub fn decide(needs: bool, visible: bool, marked: bool) -> Act {
    match (needs && !visible, marked) {
        (true, false) => Act::Mark,
        (false, true) => Act::Unmark,
        _ => Act::Keep,
    }
}

#[derive(Default)]
pub struct Attention {
    /// (Zellij session, tab id, the name before the mark)
    marked: Option<(String, u64, String)>,
    last_poll: f64,
    /// The need (session and state) the last look at the host covered: a new one is looked at
    /// once, and the same one never again on the pane's own clock.
    settled: Option<String>,
}

/// What one `plan` asks of the world.
#[derive(Debug, PartialEq)]
pub struct Step {
    pub act: Act,
    /// Come back at this time; None = nothing pending, sleep until input or a file change.
    pub wake: Option<f64>,
}

impl Attention {
    /// Called every frame. Asks the host (two `zellij action` calls) only when the answer can
    /// change something, at most once a second, and never keeps the pane awake to ask again:
    ///
    /// - a need that just appeared gets one look (mark the tab if the pane is out of view);
    /// - a marked tab is looked at again whenever the pane is awake anyway (a key, a click, a
    ///   focus, a file change), so the mark comes down once sulin is on the tab and touches it,
    ///   and at the latest when Claude is busy again, which needs no look at all.
    ///
    /// An idle Claude with nothing else going on therefore costs no wake-ups (it used to cost one
    /// a second, forever, each spawning two processes).
    pub fn step(&mut self, acts: &[Activity], t: f64, cfg: &Config, host_visible: impl Fn() -> Option<bool>) -> Option<f64> {
        let _ = cfg;
        let s = self.plan(acts, t, host_visible);
        match s.act {
            Act::Mark => self.mark(),
            Act::Unmark => self.unmark(),
            Act::Keep => {}
        }
        s.wake
    }

    /// The decision, without the side effects (`step` carries them out).
    pub fn plan(&mut self, acts: &[Activity], t: f64, host_visible: impl Fn() -> Option<bool>) -> Step {
        let Some(a) = need(acts) else {
            // Claude is busy again (or gone): the mark comes down without asking the host
            let act = if self.marked.is_some() { Act::Unmark } else { Act::Keep };
            self.settled = None;
            return Step { act, wake: None };
        };
        let key = format!("{}\n{}", a.session, a.state);
        let unsettled = self.settled.as_deref() != Some(&key);
        if !(unsettled || self.marked.is_some()) {
            return Step { act: Act::Keep, wake: None };
        }
        if t - self.last_poll < 1.0 {
            // a look that has to happen comes back for it; a marked tab only rides other wake-ups
            return Step { act: Act::Keep, wake: unsettled.then_some(self.last_poll + 1.0) };
        }
        self.last_poll = t;
        let visible = host_visible().unwrap_or(true);
        self.settled = Some(key);
        Step { act: decide(true, visible, self.marked.is_some()), wake: None }
    }

    fn mark(&mut self) {
        let (Ok(sess), Some(pane)) = (std::env::var("ZELLIJ_SESSION_NAME"), std::env::var("ZELLIJ_PANE_ID").ok().and_then(|p| p.parse::<u64>().ok())) else {
            return;
        };
        let Some((tab, name)) = crate::zj::tab_of(&sess, pane) else { return };
        // another margin pane in this tab may have marked it already: keep the real name
        let orig = name.strip_prefix(MARK).unwrap_or(&name).to_string();
        if !name.starts_with(MARK) && crate::zj::rename_tab(&sess, tab, &format!("{MARK}{orig}")).is_err() {
            return;
        }
        self.marked = Some((sess, tab, orig));
    }

    fn unmark(&mut self) {
        if let Some((sess, tab, orig)) = self.marked.take() {
            let _ = crate::zj::rename_tab(&sess, tab, &orig);
        }
    }
}

impl Drop for Attention {
    fn drop(&mut self) {
        self.unmark();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn act(state: &str, agent: &str) -> Activity {
        Activity { session: "s".into(), agent: agent.into(), state: state.into(), tool: String::new(), text: String::new(), since: String::new() }
    }

    #[test]
    fn marks_only_out_of_view_and_takes_it_down() {
        assert_eq!(decide(true, false, false), Act::Mark);
        assert_eq!(decide(true, true, false), Act::Keep, "on screen: the header says it");
        assert_eq!(decide(true, true, true), Act::Unmark, "he switched to the tab");
        assert_eq!(decide(false, false, true), Act::Unmark, "Claude is busy again");
        assert_eq!(decide(false, false, false), Act::Keep);
    }

    #[test]
    fn needs_come_from_the_main_session_not_subagents() {
        assert!(need(&[act("waiting", "")]).is_some());
        assert!(need(&[act("idle", "")]).is_some());
        assert!(need(&[act("running", "")]).is_none());
        // newest first: a subagent waiting doesn't count, the main session running wins
        assert!(need(&[act("waiting", "sub"), act("running", "")]).is_none());
    }

    fn quiet(s: &Step) -> bool {
        s.act == Act::Keep && s.wake.is_none()
    }

    /// The regression for "the pane wakes every second once a turn ends": an idle Claude gets one
    /// look at the host, and after it nothing asks for a wake-up or another look.
    #[test]
    fn idle_asks_for_no_wake() {
        let looks = std::cell::Cell::new(0);
        let seen = |v: bool| { looks.set(looks.get() + 1); Some(v) };
        let idle = [act("idle", "")];
        // on screen: the header says "your turn", nothing to mark
        let mut a = Attention::default();
        assert!(quiet(&a.plan(&idle, 10.0, || seen(true))));
        for t in [10.5, 11.0, 12.0, 60.0, 3600.0, 86400.0] {
            assert!(quiet(&a.plan(&idle, t, || seen(true))), "t={t}");
        }
        assert_eq!(looks.get(), 1, "one look at the host for one turn ending");
        // out of view: mark the tab once, then sleep (the caller's mark failed here, as it does
        // outside Zellij, and even then nothing retries)
        looks.set(0);
        let mut a = Attention::default();
        assert_eq!(a.plan(&idle, 10.0, || seen(false)), Step { act: Act::Mark, wake: None });
        for t in [10.5, 11.0, 12.0, 3600.0] {
            assert!(quiet(&a.plan(&idle, t, || seen(false))), "t={t}");
        }
        assert_eq!(looks.get(), 1);
        // nothing needed at all: no look, no wake
        assert!(quiet(&a.plan(&[act("running", "")], 4000.0, true, 120.0, || seen(false))));
        assert_eq!(looks.get(), 1);
    }

    /// A need that appears inside the one-second limit on host looks comes back for its look
    /// once, instead of being dropped or polled.
    #[test]
    fn a_new_need_inside_the_rate_limit_gets_one_wake() {
        let mut a = Attention::default();
        assert!(quiet(&a.plan(&[act("idle", "")], 10.0, false, 0.0, || Some(true))));
        assert!(quiet(&a.plan(&[act("running", "")], 10.2, false, 0.0, || Some(true))));
        let w = [act("waiting", "")];
        assert_eq!(a.plan(&w, 10.4, || Some(false)), Step { act: Act::Keep, wake: Some(11.0) });
        assert_eq!(a.plan(&w, 11.0, || Some(false)), Step { act: Act::Mark, wake: None });
        assert!(quiet(&a.plan(&w, 12.0, || Some(false))));
    }

    /// A marked tab asks for no wake of its own; it comes down when the pane is awake for another
    /// reason and the tab is in view, or when Claude is busy again.
    #[test]
    fn a_marked_tab_rides_other_wakes() {
        let idle = [act("idle", "")];
        let mut a = Attention::default();
        assert_eq!(a.plan(&idle, 10.0, || Some(false)).act, Act::Mark);
        a.marked = Some(("s".into(), 1, "tab".into()));
        assert!(quiet(&a.plan(&idle, 10.3, || panic!("inside the rate limit"))));
        assert!(quiet(&a.plan(&idle, 20.0, || Some(false))), "still out of view");
        assert_eq!(a.plan(&idle, 30.0, || Some(true)), Step { act: Act::Unmark, wake: None });
        assert_eq!(a.plan(&[act("running", "")], 30.1, false, 0.0, || panic!("no look")), Step { act: Act::Unmark, wake: None });
        a.marked = None; // nothing real to rename back on drop
    }
}
