//! "Claude is waiting on you", outside the pane. Inside it, the header says so; when the pane's
//! tab isn't the active one, the tab itself gets a `● ` mark (renamed by id: no focus moves), and
//! after a delay an opt-in ntfy push goes out. Never a bell: sulin doesn't want one (2026-11-08).
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

/// Should the phone push go now: waiting (not just idle), out of view, past the delay, once.
pub fn push_due(state: &str, visible: bool, waited: f64, after: f64, pushed: bool) -> bool {
    state == "waiting" && !visible && waited >= after && !pushed
}

#[derive(Default)]
pub struct Attention {
    /// (Zellij session, tab id, the name before the mark)
    marked: Option<(String, u64, String)>,
    waiting_since: Option<f64>,
    pushed: bool,
    last_poll: f64,
    /// The need (session and state) the last look at the host covered: a new one is looked at
    /// once, and the same one never again on the pane's own clock.
    settled: Option<String>,
    /// The push delay has had its one look.
    push_looked: bool,
}

/// What one `plan` asks of the world.
#[derive(Debug, PartialEq)]
pub struct Step {
    pub act: Act,
    /// Send the phone push, with this text.
    pub push: Option<String>,
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
    ///   and at the latest when Claude is busy again, which needs no look at all;
    /// - a `waiting` need with ntfy on gets one more look when the push delay is up.
    ///
    /// An idle Claude with nothing else going on therefore costs no wake-ups (it used to cost one
    /// a second, forever, each spawning two processes).
    pub fn step(&mut self, acts: &[Activity], t: f64, cfg: &Config, host_visible: impl Fn() -> Option<bool>) -> Option<f64> {
        let s = self.plan(acts, t, !cfg.ntfy_url.is_empty(), cfg.ntfy_after as f64, host_visible);
        match s.act {
            Act::Mark => self.mark(),
            Act::Unmark => self.unmark(),
            Act::Keep => {}
        }
        if let Some(text) = &s.push {
            push(cfg, text);
        }
        s.wake
    }

    /// The decision, without the side effects (`step` carries them out).
    pub fn plan(&mut self, acts: &[Activity], t: f64, ntfy: bool, after: f64, host_visible: impl Fn() -> Option<bool>) -> Step {
        let Some(a) = need(acts) else {
            // Claude is busy again (or gone): the mark comes down without asking the host
            let act = if self.marked.is_some() { Act::Unmark } else { Act::Keep };
            (self.waiting_since, self.pushed, self.push_looked, self.settled) = (None, false, false, None);
            return Step { act, push: None, wake: None };
        };
        let key = format!("{}\n{}", a.session, a.state);
        if self.settled.as_deref() != Some(&key) && self.waiting_since.is_some() != (a.state == "waiting") {
            self.waiting_since = (a.state == "waiting").then_some(t);
            (self.pushed, self.push_looked) = (false, false);
        }
        // the one timed thing: the push delay
        let deadline = self.waiting_since.filter(|_| ntfy && !self.pushed && !self.push_looked).map(|s| s + after);
        let due = deadline.is_some_and(|d| t >= d);
        let unsettled = self.settled.as_deref() != Some(&key);
        if !(unsettled || due || self.marked.is_some()) {
            return Step { act: Act::Keep, push: None, wake: deadline };
        }
        if t - self.last_poll < 1.0 {
            // a look that has to happen comes back for it; a marked tab only rides other wake-ups
            let wake = if unsettled || due { Some(self.last_poll + 1.0) } else { deadline };
            return Step { act: Act::Keep, push: None, wake };
        }
        self.last_poll = t;
        let visible = host_visible().unwrap_or(true);
        self.settled = Some(key);
        let act = decide(true, visible, self.marked.is_some());
        let mut push = None;
        if let Some(since) = self.waiting_since {
            if ntfy && push_due(&a.state, visible, t - since, after, self.pushed) {
                self.pushed = true;
                push = Some(a.text.clone());
            }
            if due {
                self.push_looked = true;
            }
        }
        let wake = self.waiting_since.filter(|_| ntfy && !self.pushed && !self.push_looked).map(|s| s + after);
        Step { act, push, wake }
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

/// One ntfy push, in the background so the pane never waits on the network.
fn push(cfg: &Config, text: &str) {
    let (url, token, body) = (cfg.ntfy_url.clone(), cfg.ntfy_token.clone(), format!("Claude is waiting on you: {text}"));
    std::thread::spawn(move || {
        let mut c = std::process::Command::new("curl");
        c.args(["-s", "-m", "10", "-H", "Title: margin", "-d", &body]);
        if !token.is_empty() {
            c.args(["-H", &format!("Authorization: Bearer {token}")]);
        }
        let _ = c.arg(&url).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
    });
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

    #[test]
    fn push_once_after_the_delay_and_only_when_blocked() {
        assert!(!push_due("waiting", false, 30.0, 120.0, false));
        assert!(push_due("waiting", false, 121.0, 120.0, false));
        assert!(!push_due("waiting", false, 500.0, 120.0, true), "once");
        assert!(!push_due("idle", false, 500.0, 120.0, false), "a turn that ended isn't urgent");
        assert!(!push_due("waiting", true, 500.0, 120.0, false), "on screen");
    }

    fn quiet(s: &Step) -> bool {
        s.act == Act::Keep && s.push.is_none() && s.wake.is_none()
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
        assert!(quiet(&a.plan(&idle, 10.0, true, 120.0, || seen(true))));
        for t in [10.5, 11.0, 12.0, 60.0, 3600.0, 86400.0] {
            assert!(quiet(&a.plan(&idle, t, true, 120.0, || seen(true))), "t={t}");
        }
        assert_eq!(looks.get(), 1, "one look at the host for one turn ending");
        // out of view: mark the tab once, then sleep (the caller's mark failed here, as it does
        // outside Zellij, and even then nothing retries)
        looks.set(0);
        let mut a = Attention::default();
        assert_eq!(a.plan(&idle, 10.0, false, 120.0, || seen(false)), Step { act: Act::Mark, push: None, wake: None });
        for t in [10.5, 11.0, 12.0, 3600.0] {
            assert!(quiet(&a.plan(&idle, t, false, 120.0, || seen(false))), "t={t}");
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
        assert_eq!(a.plan(&w, 10.4, false, 0.0, || Some(false)), Step { act: Act::Keep, push: None, wake: Some(11.0) });
        assert_eq!(a.plan(&w, 11.0, false, 0.0, || Some(false)), Step { act: Act::Mark, push: None, wake: None });
        assert!(quiet(&a.plan(&w, 12.0, false, 0.0, || Some(false))));
    }

    /// Waiting with ntfy on: one wake at the push delay, the push goes once, then sleep.
    #[test]
    fn waiting_wakes_once_for_the_push() {
        let mut a = Attention::default();
        let w = [Activity { text: "run it?".into(), ..act("waiting", "") }];
        assert_eq!(a.plan(&w, 10.0, true, 120.0, || Some(false)), Step { act: Act::Mark, push: None, wake: Some(130.0) });
        // woken early by something else: no look, the same deadline
        assert_eq!(a.plan(&w, 50.0, true, 120.0, || panic!("no look")), Step { act: Act::Keep, push: None, wake: Some(130.0) });
        let s = a.plan(&w, 130.0, true, 120.0, || Some(false));
        assert_eq!((s.push.as_deref(), s.wake), (Some("run it?"), None));
        assert!(a.plan(&w, 500.0, true, 120.0, || panic!("no look")).wake.is_none());
        // on screen when the delay is up: no push, and no second deadline
        let mut a = Attention::default();
        assert_eq!(a.plan(&w, 10.0, true, 120.0, || Some(true)).wake, Some(130.0));
        assert!(quiet(&a.plan(&w, 130.0, true, 120.0, || Some(true))));
        assert!(quiet(&a.plan(&w, 900.0, true, 120.0, || panic!("no look"))));
        // ntfy off: no deadline at all
        assert!(Attention::default().plan(&w, 10.0, false, 120.0, || Some(true)).wake.is_none());
    }

    /// A marked tab asks for no wake of its own; it comes down when the pane is awake for another
    /// reason and the tab is in view, or when Claude is busy again.
    #[test]
    fn a_marked_tab_rides_other_wakes() {
        let idle = [act("idle", "")];
        let mut a = Attention::default();
        assert_eq!(a.plan(&idle, 10.0, false, 0.0, || Some(false)).act, Act::Mark);
        a.marked = Some(("s".into(), 1, "tab".into()));
        assert!(quiet(&a.plan(&idle, 10.3, false, 0.0, || panic!("inside the rate limit"))));
        assert!(quiet(&a.plan(&idle, 20.0, false, 0.0, || Some(false))), "still out of view");
        assert_eq!(a.plan(&idle, 30.0, false, 0.0, || Some(true)), Step { act: Act::Unmark, push: None, wake: None });
        assert_eq!(a.plan(&[act("running", "")], 30.1, false, 0.0, || panic!("no look")), Step { act: Act::Unmark, push: None, wake: None });
        a.marked = None; // nothing real to rename back on drop
    }
}
