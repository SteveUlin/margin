//! Where panes live. margin's renderer is a plain terminal program (`margin view`); a **host**
//! decides where it runs and how sulin's selections come back. Two hosts:
//!
//! - `ZellijHost` (zj.rs underneath): opens a split or pinned float in Claude's tab without moving
//!   focus, finds panes by title, closes them, tells the renderer whether its tab is on screen, and
//!   routes double-clicked `mg:` tokens through Zellij's `copy_command` (`margin select`).
//! - `PlainHost`: no multiplexer. `margin open` prints the `margin view` command to run in any
//!   terminal; selection comes only from keys and mouse inside the pane, so mouse reporting starts
//!   on (a click on a token acts).
//!
//! Config `host: "zellij" | "plain" | "auto"` (auto = Zellij when `$ZELLIJ_SESSION_NAME` is set or
//! `--session` is given). vellum knows nothing of hosts; only margin does.

use crate::config::Config;
use crate::store::Store;
use crate::zj;

#[derive(Clone, Debug, Default)]
pub struct OpenOpts {
    /// Some(true) float, Some(false) split, None = config default.
    pub float: Option<bool>,
    pub tab_id: Option<u64>,
    pub session: Option<String>,
    /// Stack onto the tab's existing margin pane (Zellij `--stacked`) instead of a new split.
    pub stacked: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Opened {
    /// A pane the host made or found: its handle and whether it's new.
    Pane { handle: String, created: bool },
    /// The host can't place panes: run this command in a terminal.
    Run(String),
}

/// How a selection reaches margin.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Selection {
    /// The multiplexer pipes selected text to `margin select` (Zellij's copy_command); keys and
    /// mouse in the pane work too.
    CopyCommand,
    /// Only keys and clicks inside the pane.
    InPane,
}

pub trait Host {
    fn name(&self) -> &'static str;
    /// Open (or reuse) the pane for `art`.
    fn open(&self, st: &Store, art: &str, o: &OpenOpts, cfg: &Config) -> Result<Opened, String>;
    /// Panes (or processes) currently showing `art`.
    fn find(&self, st: &Store, art: &str) -> Result<Vec<String>, String>;
    fn close(&self, st: &Store, art: &str) -> Result<usize, String>;
    /// For the renderer's `seen`: is the calling pane on screen? None = can't tell (assume yes).
    fn is_visible(&self) -> Option<bool>;
    fn selection(&self) -> Selection;
    /// Start a session wired for margin. Replaces the process on success.
    fn session(&self, name: &str) -> Result<(), String>;
}

pub fn pick(cfg: &Config, session_flag: Option<&str>) -> Box<dyn Host> {
    let in_zellij = session_flag.is_some() || std::env::var("ZELLIJ_SESSION_NAME").map(|s| !s.is_empty()).unwrap_or(false);
    match cfg.host.as_str() {
        "zellij" => Box::new(ZellijHost { session: session_flag.map(String::from) }),
        "plain" => Box::new(PlainHost),
        _ if in_zellij => Box::new(ZellijHost { session: session_flag.map(String::from) }),
        _ => Box::new(PlainHost),
    }
}

pub struct ZellijHost {
    pub session: Option<String>,
}

impl Host for ZellijHost {
    fn name(&self) -> &'static str {
        "zellij"
    }
    fn open(&self, st: &Store, art: &str, o: &OpenOpts, cfg: &Config) -> Result<Opened, String> {
        let o = OpenOpts { session: o.session.clone().or(self.session.clone()), ..o.clone() };
        zj::open(st, art, &o, cfg).map(|(handle, created)| Opened::Pane { handle, created })
    }
    fn find(&self, _st: &Store, art: &str) -> Result<Vec<String>, String> {
        let sess = zj::session(self.session.as_deref()).ok_or("not inside Zellij: pass --session")?;
        Ok(zj::list_panes(&sess)?.into_iter().filter(|p| p.title == zj::title(art) && !p.exited).map(|p| p.handle()).collect())
    }
    fn close(&self, st: &Store, art: &str) -> Result<usize, String> {
        zj::close(st, art, self.session.as_deref())
    }
    fn is_visible(&self) -> Option<bool> {
        let sess = std::env::var("ZELLIJ_SESSION_NAME").ok()?;
        let pane: u64 = std::env::var("ZELLIJ_PANE_ID").ok()?.parse().ok()?;
        zj::pane_tab_active(&sess, pane)
    }
    fn selection(&self) -> Selection {
        Selection::CopyCommand
    }
    fn session(&self, name: &str) -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let layout = crate::demo::assets().join("../../zellij/margin.kdl");
        let mut c = std::process::Command::new("zellij");
        c.args(["-s", name]);
        if layout.exists() {
            c.arg("-n").arg(layout);
        }
        // session-only options (research §8): the global config.kdl stays untouched
        c.args(["options", "--copy-command", &format!("{} select", exe.display()), "--copy-on-select", "true", "--mouse-click-through", "true"]);
        use std::os::unix::process::CommandExt;
        Err(format!("exec zellij: {}", c.exec()))
    }
}

pub struct PlainHost;

/// `margin view <art>` processes of this project, found through /proc (pid list).
fn view_pids(st: &Store, art: &str) -> Vec<i32> {
    let mut out = Vec::new();
    let me = std::process::id() as i32;
    let Ok(rd) = std::fs::read_dir("/proc") else { return out };
    for e in rd.filter_map(|e| e.ok()) {
        let Ok(pid) = e.file_name().to_string_lossy().parse::<i32>() else { continue };
        if pid == me {
            continue;
        }
        let Ok(raw) = std::fs::read(format!("/proc/{pid}/cmdline")) else { continue };
        let args: Vec<String> = raw.split(|b| *b == 0).filter(|a| !a.is_empty()).map(|a| String::from_utf8_lossy(a).into_owned()).collect();
        if args.len() < 3 || !args[0].ends_with("margin") || args[1] != "view" || args[2] != art {
            continue;
        }
        let proj = args.iter().position(|a| a == "--project").and_then(|i| args.get(i + 1).cloned()).unwrap_or_else(|| {
            std::fs::read_link(format!("/proc/{pid}/cwd")).map(|c| crate::store::project_for(&c)).unwrap_or_default()
        });
        if proj == st.project {
            out.push(pid);
        }
    }
    out
}

impl Host for PlainHost {
    fn name(&self) -> &'static str {
        "plain"
    }
    fn open(&self, st: &Store, art: &str, _o: &OpenOpts, _cfg: &Config) -> Result<Opened, String> {
        if !st.exists(art) {
            return Err(format!("no artifact {art:?}; create it with `margin put` first"));
        }
        if let Some(pid) = view_pids(st, art).first() {
            return Ok(Opened::Pane { handle: format!("pid {pid}"), created: false });
        }
        let mut cmd = format!("{} view {art}", crate::util::self_cmd());
        if std::env::var("MARGIN_PROJECT").is_err() {
            cmd += &format!(" --project {}", st.project);
        }
        if std::env::var("MARGIN_HOME").is_ok() {
            cmd += &format!(" --home {}", st.home.display());
        }
        Ok(Opened::Run(cmd))
    }
    fn find(&self, st: &Store, art: &str) -> Result<Vec<String>, String> {
        Ok(view_pids(st, art).into_iter().map(|p| format!("pid {p}")).collect())
    }
    fn close(&self, st: &Store, art: &str) -> Result<usize, String> {
        // SIGTERM: the renderer leaves its loop and restores the terminal (vellum term.rs)
        let pids = view_pids(st, art);
        for p in &pids {
            unsafe { libc::kill(*p, libc::SIGTERM) };
        }
        Ok(pids.len())
    }
    fn is_visible(&self) -> Option<bool> {
        None
    }
    fn selection(&self) -> Selection {
        Selection::InPane
    }
    fn session(&self, _name: &str) -> Result<(), String> {
        Err("host is plain (no multiplexer): run `margin view <artifact>` in any terminal; set host to \"zellij\" for a wired session".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pick_follows_config_then_env() {
        let mut c = Config::default();
        c.host = "plain".into();
        assert_eq!(pick(&c, Some("s")).name(), "plain");
        c.host = "zellij".into();
        assert_eq!(pick(&c, None).name(), "zellij");
        c.host = "auto".into();
        assert_eq!(pick(&c, Some("s")).name(), "zellij");
        assert_eq!(PlainHost.selection(), Selection::InPane);
    }
}
