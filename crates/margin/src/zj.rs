//! Pane management through `zellij action`, no plugin (design §6). This is `ZellijHost`'s
//! implementation (host.rs); nothing else calls it directly.
//!
//! `margin open` finds the home tab from `$ZELLIJ_PANE_ID` (the pane Claude Code runs in), reuses a
//! pane titled `margin:<artifact>` in that tab if one exists, and otherwise creates one with
//! `--tab-id T --no-focus`: a pinned float by default (visible with the floating layer hidden, and
//! keystrokes still reach the chat), or a split in the biggest free space. Neither steals focus
//! (research.md §1). Updates never touch Zellij: the renderer watches the files.

use crate::config::Config;
use crate::store::{default_home, write_atomic, Store};
use serde_json::{json, Value};
use std::process::Command;

pub use crate::host::OpenOpts;

#[derive(Clone, Debug)]
pub struct Pane {
    pub id: u64,
    pub is_plugin: bool,
    pub title: String,
    pub tab_id: u64,
    pub is_floating: bool,
    pub is_focused: bool,
    pub exited: bool,
}

impl Pane {
    pub fn handle(&self) -> String {
        format!("{}_{}", if self.is_plugin { "plugin" } else { "terminal" }, self.id)
    }
}

#[derive(Clone, Debug)]
pub struct Tab {
    pub tab_id: u64,
    pub name: String,
    pub active: bool,
}

pub fn session(opt: Option<&str>) -> Option<String> {
    opt.map(String::from).or_else(|| std::env::var("ZELLIJ_SESSION_NAME").ok()).filter(|s| !s.is_empty())
}

fn action(sess: &str, args: &[&str]) -> Result<String, String> {
    action_as(sess, None, args)
}

/// A Zellij action run "from" pane `as_pane`: `--near-current-pane` then means that pane, not
/// wherever sulin's focus is, so margin can place a pane without moving his focus.
fn action_as(sess: &str, as_pane: Option<u64>, args: &[&str]) -> Result<String, String> {
    let mut c = Command::new("zellij");
    c.args(["-s", sess, "action"]).args(args);
    if let Some(id) = as_pane {
        c.env("ZELLIJ_PANE_ID", id.to_string());
    }
    let out = c
        .output()
        .map_err(|e| format!("zellij: {e}"))?;
    if !out.status.success() && !matches!(out.status.code(), Some(2)) {
        return Err(format!("zellij action {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn list_panes(sess: &str) -> Result<Vec<Pane>, String> {
    let v: Value = serde_json::from_str(&action(sess, &["list-panes", "-a", "-j"])?).map_err(|e| e.to_string())?;
    Ok(v.as_array()
        .map(|a| {
            a.iter()
                .map(|p| Pane {
                    id: p["id"].as_u64().unwrap_or(0),
                    is_plugin: p["is_plugin"].as_bool().unwrap_or(false),
                    title: p["title"].as_str().unwrap_or("").to_string(),
                    tab_id: p["tab_id"].as_u64().unwrap_or(0),
                    is_floating: p["is_floating"].as_bool().unwrap_or(false),
                    is_focused: p["is_focused"].as_bool().unwrap_or(false),
                    exited: p["exited"].as_bool().unwrap_or(false),
                })
                .collect()
        })
        .unwrap_or_default())
}

pub fn list_tabs(sess: &str) -> Result<Vec<Tab>, String> {
    let v: Value = serde_json::from_str(&action(sess, &["list-tabs", "-j"])?).map_err(|e| e.to_string())?;
    Ok(v.as_array()
        .map(|a| {
            a.iter()
                .map(|t| Tab {
                    tab_id: t["tab_id"].as_u64().unwrap_or(0),
                    name: t["name"].as_str().unwrap_or("").to_string(),
                    active: t["active"].as_bool().unwrap_or(false),
                })
                .collect()
        })
        .unwrap_or_default())
}

/// The tab holding `$ZELLIJ_PANE_ID` (a terminal pane id).
pub fn home_tab(sess: &str) -> Result<u64, String> {
    let pid: u64 = std::env::var("ZELLIJ_PANE_ID")
        .ok()
        .and_then(|s| s.trim_start_matches("terminal_").parse().ok())
        .ok_or("not inside a Zellij pane: pass --tab-id")?;
    list_panes(sess)?
        .into_iter()
        .find(|p| !p.is_plugin && p.id == pid)
        .map(|p| p.tab_id)
        .ok_or_else(|| format!("pane {pid} not found in session {sess}"))
}

pub fn title(art: &str) -> String {
    format!("margin:{art}")
}

/// Remember that this Zellij session shows artifacts from this project, so `margin select` (which
/// runs as a child of the Zellij server, cwd unknown) resolves short tokens to the right project.
fn remember(sess: &str, st: &Store) {
    let p = default_home().join("zellij").join(format!("{sess}.json"));
    let mut v: Value = std::fs::read_to_string(&p).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(json!({}));
    let mut ps: Vec<String> = v["projects"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    ps.retain(|x| x != &st.project);
    ps.insert(0, st.project.clone());
    v["projects"] = json!(ps);
    let _ = write_atomic(&p, serde_json::to_string_pretty(&v).unwrap().as_bytes());
}

/// Open (or reuse) the pane for an artifact. Returns the pane handle and whether it was created.
pub fn open(st: &Store, art: &str, o: &OpenOpts, cfg: &Config) -> Result<(String, bool), String> {
    if !st.exists(art) {
        return Err(st.missing(art));
    }
    let sess = session(o.session.as_deref()).ok_or("not inside Zellij ($ZELLIJ_SESSION_NAME unset): pass --session")?;
    let tab = match o.tab_id {
        Some(t) => t,
        None => home_tab(&sess)?,
    };
    remember(&sess, st);
    let t = title(art);
    if let Some(p) = list_panes(&sess)?.into_iter().find(|p| p.title == t && p.tab_id == tab && !p.exited) {
        return Ok((p.handle(), false));
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = exe.to_string_lossy().into_owned();
    let tab_s = tab.to_string();
    let float = o.float.unwrap_or(cfg.pane == "float");
    // stacking onto the tab's margin pane: run the action as that pane with --near-current-pane,
    // so Zellij stacks there. Never by focusing it: focus is sulin's (it pulled him out of his work)
    let stack_on = if o.stacked && !float {
        list_panes(&sess)?.into_iter().find(|p| !p.is_plugin && p.tab_id == tab && !p.exited && !p.is_floating && p.title.starts_with("margin:")).map(|p| p.id)
    } else {
        None
    };
    // --near-current-pane already places it in the margin pane's tab; with --tab-id as well,
    // Zellij created nothing and printed no handle
    let mut args: Vec<&str> = if stack_on.is_some() {
        vec!["new-pane", "--stacked", "--near-current-pane", "--no-focus", "-n", &t]
    } else {
        vec!["new-pane", "--tab-id", &tab_s, "--no-focus", "-n", &t]
    };
    if float {
        args.extend([
            "--floating", "--pinned", "true", "-x", &cfg.float_x, "-y", &cfg.float_y, "--width", &cfg.float_width,
            "--height", &cfg.float_height,
        ]);
    }
    let home = st.home.to_string_lossy().into_owned();
    args.extend(["--", &exe, "view", art, "--project", &st.project, "--home", &home]);
    let out = action_as(&sess, stack_on, &args)?;
    let handle = out.trim().to_string();
    if handle.is_empty() {
        return Err(format!("zellij opened no pane for {art} (new-pane printed nothing)"));
    }
    let _ = st.log(art, "", "open", json!({"pane": handle, "tab": tab, "float": float}), "cli");
    Ok((handle, true))
}

pub fn close(st: &Store, art: &str, session_opt: Option<&str>) -> Result<usize, String> {
    let sess = session(session_opt).ok_or("not inside Zellij: pass --session")?;
    let t = title(art);
    let mut n = 0;
    for p in list_panes(&sess)?.into_iter().filter(|p| p.title == t) {
        action(&sess, &["close-pane", "-p", &p.handle()])?;
        n += 1;
    }
    if n > 0 {
        let _ = st.log(art, "", "close", Value::Null, "cli");
    }
    Ok(n)
}

/// For the renderer's `seen` check: is the tab that holds this pane the active one?
pub fn pane_tab_active(sess: &str, pane_id: u64) -> Option<bool> {
    let tab = list_panes(sess).ok()?.into_iter().find(|p| !p.is_plugin && p.id == pane_id)?.tab_id;
    Some(list_tabs(sess).ok()?.into_iter().any(|t| t.tab_id == tab && t.active))
}

/// Rename a tab by id without focusing it (the waiting mark).
pub fn rename_tab(sess: &str, tab: u64, name: &str) -> Result<(), String> {
    action(sess, &["rename-tab", "--tab-id", &tab.to_string(), name]).map(|_| ())
}

/// The tab holding terminal pane `pane`: (tab id, tab name).
pub fn tab_of(sess: &str, pane: u64) -> Option<(u64, String)> {
    let tab = list_panes(sess).ok()?.into_iter().find(|p| !p.is_plugin && p.id == pane)?.tab_id;
    let name = list_tabs(sess).ok()?.into_iter().find(|t| t.tab_id == tab)?.name;
    Some((tab, name))
}
