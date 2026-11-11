//! Which margin artifact a Claude session keeps: its pane. An artifact belongs to a project, so
//! without a link a fresh session knows no pane is its own, and nothing reminds it to keep that
//! pane current (2026-11-08: a whole session went by with `margin:margin` untouched).
//!
//! A link forms three ways, weakest first:
//! - **auto**: at SessionStart, the margin pane in the chat's own Zellij tab (the one beside you);
//! - **open**: `margin open <art>` links a session that has no link yet (stacking the showcase on
//!   top doesn't take the link over);
//! - **explicit**: `margin link <art>` sets or switches it.
//!
//! A link is keyed by session id and remembers the chat pane, because the hooks docs don't say
//! whether a resumed session keeps its id: a new id in the same chat pane inherits the link.
//!
//! What a link buys: a one-line status on every prompt (`margin: your pane is …`), a nudge when
//! the session has run many tool calls without writing to its pane, and a live strip that shows
//! only its own sessions.

use crate::components::status_of;
use crate::doc::walk;
use crate::store::{write_atomic, Store};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// Tool calls without a pane write before the status line says the pane is stale, and the
/// mid-turn nudge fires (then again every STALE_AGAIN calls).
pub const STALE: u32 = 10;
pub const STALE_AGAIN: u32 = 25;
/// Links untouched this long are ignored (a session that's gone).
const EXPIRE_SECS: f64 = 7.0 * 86400.0;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Link {
    pub session: String,
    pub art: String,
    /// How it formed: auto, open or explicit. An explicit link isn't replaced by open or auto.
    #[serde(default)]
    pub how: String,
    /// The Zellij pane the chat runs in, and its session: the fallback key after a resume.
    #[serde(default)]
    pub chat_pane: String,
    #[serde(default)]
    pub zellij: String,
    /// Last touched (RFC 3339).
    pub since: String,
    /// Tool calls since this session last wrote to margin.
    #[serde(default)]
    pub stale: u32,
    /// The session's transcript, from the hooks: its subagents sit beside it (agents.rs).
    #[serde(default)]
    pub transcript: String,
}

fn dir(st: &Store) -> PathBuf {
    st.dir().join("links")
}

fn key(session: &str) -> String {
    session.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect()
}

fn fresh(l: &Link) -> bool {
    crate::util::parse_rfc3339(&l.since).map(|t| crate::util::epoch_now() - t < EXPIRE_SECS).unwrap_or(false)
}

pub fn all(st: &Store) -> Vec<Link> {
    let mut v: Vec<Link> = fs::read_dir(dir(st))
        .map(|rd| rd.filter_map(|e| e.ok()).filter_map(|e| serde_json::from_slice(&fs::read(e.path()).ok()?).ok()).collect())
        .unwrap_or_default();
    v.retain(fresh);
    v.sort_by(|a: &Link, b: &Link| b.since.cmp(&a.since));
    v
}

pub fn get(st: &Store, session: &str) -> Option<Link> {
    let l: Link = serde_json::from_slice(&fs::read(dir(st).join(format!("{}.json", key(session)))).ok()?).ok()?;
    Some(l).filter(fresh).filter(|l| st.exists(&l.art))
}

pub fn save(st: &Store, l: &Link) {
    let _ = fs::create_dir_all(dir(st));
    if let Ok(j) = serde_json::to_vec_pretty(l) {
        let _ = write_atomic(&dir(st).join(format!("{}.json", key(&l.session))), &j);
    }
}

pub fn remove(st: &Store, session: &str) -> bool {
    fs::remove_file(dir(st).join(format!("{}.json", key(session)))).is_ok()
}

/// Is `art` some live Claude session's pane, and if only ended sessions are linked to it, when
/// did the last one end (local HH:MM)? A session is over once its SessionEnd hook said so
/// (`margin hook end`). (linked to a live session, when the last linked session ended)
pub fn liveness(st: &Store, art: &str) -> (bool, Option<String>) {
    let links: Vec<Link> = all(st).into_iter().filter(|l| l.art == art).collect();
    if links.is_empty() {
        return (false, None);
    }
    let ended: Vec<f64> = links.iter().filter_map(|l| st.activity(&l.session).filter(|a| a.state == "ended").and_then(|a| crate::util::parse_rfc3339(&a.since))).collect();
    if ended.len() < links.len() {
        return (true, None);
    }
    (false, ended.into_iter().max_by(|a, b| a.total_cmp(b)).map(crate::util::local_hm))
}

/// What the Claude sessions behind `art` are doing, newest first: a pane linked to sessions shows
/// only theirs; an unlinked pane shows the project's.
pub fn activity_for(st: &Store, art: &str) -> Vec<crate::store::Activity> {
    let mut acts = st.activities();
    let mine: Vec<String> = all(st).into_iter().filter(|l| l.art == art).map(|l| l.session).collect();
    if !mine.is_empty() {
        acts.retain(|a| mine.contains(&a.session));
    }
    acts
}

/// The chat's Zellij pane and session, from the environment the hooks and Claude's Bash inherit.
pub fn chat_here() -> (String, String) {
    let pane = std::env::var("ZELLIJ_PANE_ID").unwrap_or_default();
    let zs = std::env::var("ZELLIJ_SESSION_NAME").unwrap_or_default();
    (pane, zs)
}

/// Link `session` to `art`. `how` is explicit, open or auto; an explicit link stands until another
/// explicit one replaces it, and open or auto only fill a session that has none.
pub fn set(st: &Store, session: &str, art: &str, how: &str) -> Option<Link> {
    if let Some(cur) = get(st, session) {
        if how != "explicit" && (cur.how == "explicit" || cur.art == art || how == "auto") {
            return None;
        }
    }
    let (chat_pane, zellij) = chat_here();
    let l = Link { session: session.into(), art: art.into(), how: how.into(), chat_pane, zellij, since: crate::util::now_rfc3339(), stale: 0, transcript: String::new() };
    save(st, &l);
    Some(l)
}

/// A new session id in a chat pane that already had a link (a resume that changed the id, or
/// `/clear`) inherits the most recent one.
pub fn inherit(st: &Store, session: &str) -> Option<Link> {
    let (pane, zs) = chat_here();
    if pane.is_empty() {
        return None;
    }
    let prev = all(st).into_iter().find(|l| l.session != session && l.chat_pane == pane && l.zellij == zs && st.exists(&l.art))?;
    let l = Link { session: session.into(), since: crate::util::now_rfc3339(), stale: 0, ..prev };
    save(st, &l);
    Some(l)
}

/// Does this tool call write to margin? Chained writes count (`cargo build && margin set …`).
pub fn is_pane_write(tool: &str, input: &serde_json::Value) -> bool {
    if tool != "Bash" {
        return false;
    }
    let cmd = input["command"].as_str().unwrap_or("");
    const VERBS: &[&str] = &["set", "add", "rm", "move", "apply", "ask", "diff", "image", "title", "link", "open"];
    cmd.split(|c: char| c == ';' || c == '&' || c == '|' || c == '\n' || c == '(')
        .map(|part| part.split_whitespace().collect::<Vec<_>>())
        .any(|w| {
            // the binary by name or path, or a shell variable holding it (`M=…/margin; $M set …`)
            let bin = |t: &str| t.rsplit('/').next() == Some("margin") || (t.starts_with('$') && cmd.contains("margin"));
            w.windows(2).any(|p| bin(p[0]) && VERBS.contains(&p[1]))
        })
}

/// Count a tool call against the link. Returns the new count.
pub fn tick(st: &Store, l: &mut Link, wrote: bool) -> u32 {
    l.stale = if wrote { 0 } else { l.stale.saturating_add(1) };
    l.since = crate::util::now_rfc3339();
    save(st, l);
    l.stale
}

/// The one line a prompt carries: which pane is yours and where the plan stands.
/// `margin: your pane is margin (rev 81) · doing pl8 “Link a Claude…” · next to4 · 3 open`
pub fn status_line(st: &Store, l: &Link) -> String {
    let Ok(doc) = st.load(&l.art) else {
        return format!("margin: your pane is {} (it couldn't be read)", l.art);
    };
    let state = st.state(&l.art);
    let (mut doing, mut next, mut open, mut yours) = (Vec::new(), Vec::new(), 0usize, Vec::new());
    walk(&doc.blocks, &mut |b, _, _| {
        if b.kind == "todo" {
            let st = status_of(&state, b);
            if crate::components::yours(&state, b) && !matches!(st.as_str(), "done" | "skipped") {
                yours.push(b.id.clone());
            }
            match st.as_str() {
                "doing" => doing.push(b),
                "next" => next.push(b.id.clone()),
                "done" | "skipped" => return,
                _ => {}
            }
            open += 1;
        }
    });
    let mut s = format!("margin: your pane is {} (rev {})", l.art, doc.rev);
    match doing.first() {
        Some(b) => s.push_str(&format!(" · doing {} “{}”", b.id, vellum::text::truncate(b.s("text"), 40))),
        None if open > 0 => s.push_str(" · nothing marked doing"),
        None => {}
    }
    if !next.is_empty() {
        s.push_str(&format!(" · next {}", next.join(", ")));
    }
    if !yours.is_empty() {
        s.push_str(&format!(" · sulin's: {} (stop there)", yours.join(", ")));
    }
    s.push_str(&format!(" · {open} open"));
    if l.stale >= STALE {
        s.push_str(&format!(" · not updated in {} tool calls: keep it current as you go", l.stale));
    }
    s
}

/// SessionStart: link a session with no link to the margin pane in its chat's Zellij tab. Of
/// several (a stack), the first opened wins: the primary is opened first, stacks later.
pub fn auto(st: &Store, session: &str) -> Option<Link> {
    let (pane, zs) = chat_here();
    let pid: u64 = pane.trim_start_matches("terminal_").parse().ok()?;
    if zs.is_empty() {
        return None;
    }
    let panes = crate::zj::list_panes(&zs).ok()?;
    let tab = panes.iter().find(|p| !p.is_plugin && p.id == pid)?.tab_id;
    let mut mine: Vec<_> = panes.iter().filter(|p| p.tab_id == tab && !p.exited && p.title.starts_with("margin:")).collect();
    mine.sort_by_key(|p| p.id);
    let art = mine.iter().map(|p| p.title.trim_start_matches("margin:")).find(|a| st.exists(a))?;
    set(st, session, art, "auto")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn store() -> Store {
        let d = std::env::temp_dir().join(format!("margin-link-{}-{}", std::process::id(), rand_suffix()));
        let mut st = Store::open_cwd();
        st.home = d;
        st.project = "p".into();
        st
    }
    fn rand_suffix() -> u128 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    }
    fn plan(st: &Store) {
        let doc: crate::doc::Doc = serde_json::from_value(json!({"id": "plan", "title": "plan", "blocks": [
            {"type": "todo", "id": "t1", "text": "first", "status": "done"},
            {"type": "todo", "id": "t2", "text": "second", "status": "doing"},
            {"type": "todo", "id": "t3", "text": "third"},
            {"type": "todo", "id": "t4", "text": "fourth"}]})).unwrap();
        st.put(doc, None).unwrap();
    }

    #[test]
    fn explicit_stands_and_open_fills_only_an_empty_session() {
        let st = store();
        plan(&st);
        st.put(serde_json::from_value(json!({"id": "show", "title": "show", "blocks": []})).unwrap(), None).unwrap();
        assert!(set(&st, "s1", "plan", "open").is_some());
        // a stack opened later doesn't take the link over
        assert!(set(&st, "s1", "show", "open").is_some(), "open replaces open");
        assert!(set(&st, "s1", "plan", "explicit").is_some());
        assert!(set(&st, "s1", "show", "open").is_none(), "open never replaces explicit");
        assert!(set(&st, "s1", "show", "auto").is_none());
        assert_eq!(get(&st, "s1").unwrap().art, "plan");
        assert!(set(&st, "s1", "show", "explicit").is_some());
        assert_eq!(get(&st, "s1").unwrap().art, "show");
    }

    #[test]
    fn status_line_names_doing_open_and_staleness() {
        let st = store();
        plan(&st);
        let mut l = set(&st, "s1", "plan", "explicit").unwrap();
        let s = status_line(&st, &l);
        assert!(s.starts_with("margin: your pane is plan (rev "), "{s}");
        assert!(s.contains("doing t2 “second”") && s.contains("· 3 open") && !s.contains("not updated"), "{s}");
        for _ in 0..STALE {
            tick(&st, &mut l, false);
        }
        assert!(status_line(&st, &l).contains(&format!("not updated in {STALE} tool calls")));
        tick(&st, &mut l, true);
        assert!(!status_line(&st, &l).contains("not updated"));
    }

    #[test]
    fn pane_writes_are_spotted_inside_chains() {
        let b = |c: &str| is_pane_write("Bash", &json!({"command": c}));
        assert!(b("margin set plan t1 status done"));
        assert!(b("cd ~/src/margin && nix develop -c cargo build && ./target/debug/margin set margin pl8 status doing"));
        assert!(b("M=./target/debug/margin; $M add margin pol todo x \"y\""), "a variable holding the binary");
        assert!(!b("margin show plan"), "reads don't count");
        assert!(!b("grep -n margin src/main.rs"));
        assert!(!is_pane_write("Read", &json!({"file_path": "margin"})));
    }
}
