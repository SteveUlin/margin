//! `margin select`: the target of Zellij's `copy_command`. Zellij pipes every copied selection to
//! its stdin (research.md §3), and a double-click selects a whole `mg:` token, so selection becomes
//! the input that works locally and over ssh alike.
//!
//! 1. A line matching the token grammar is applied: state.json + an event.
//! 2. An `http(s)://` line opens through the configured opener.
//! 3. Anything else goes to the clipboard, because setting `copy_command` replaces Zellij's own
//!    OSC 52 write and the copy must still work.

use crate::config::Config;
use crate::store::{default_home, projects, Event, Store};
use serde_json::{json, Value};
use std::io::Write;
use std::process::{Command, Stdio};

#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub verb: String,
    pub artifact: Option<String>,
    pub id: String,
    pub value: Option<String>,
}

/// The token verbs. `tick` still parses, as an old spelling of `todo` (one verb per action).
pub const VERBS: &[&str] = &["todo", "pick", "strike", "approve", "reject", "ask", "open", "pin", "fold", "accept", "decline", "hold", "talk", "toasttest"];

/// The verbs a proposal can carry: the decisions that are sulin's to make.
pub const PROPOSABLE: &[&str] = &["tick", "pick", "strike", "approve", "reject"];

/// The open proposal on a block: its `proposal` field, unless sulin already answered that one.
pub fn open_proposal<'a>(b: &'a crate::doc::Block, state: &crate::store::State) -> Option<&'a Value> {
    let pr = b.f.get("proposal").filter(|v| v.is_object())?;
    match state.decisions.get(&format!("{}#proposal", b.id)) {
        Some(d) if d.value["at"] == pr["at"] => None,
        _ => Some(pr),
    }
}

/// `mg:<verb>/[<artifact>.]<id>[=<value>]`
pub fn parse_token(s: &str) -> Option<Token> {
    let s = s.trim();
    let rest = s.strip_prefix("mg:")?;
    let (verb, path) = rest.split_once('/')?;
    let verb = if verb == "tick" { "todo" } else { verb };
    if !VERBS.contains(&verb) {
        return None;
    }
    let (path, value) = match path.split_once('=') {
        Some((p, v)) => (p, Some(v.to_string()).filter(|v| !v.is_empty())),
        None => (path, None),
    };
    let (artifact, id) = match path.split_once('.') {
        Some((a, i)) => (Some(a.to_string()), i.to_string()),
        None => (None, path.to_string()),
    };
    if !crate::util::valid_id(&id) || artifact.as_deref().map(|a| !crate::util::valid_id(a)).unwrap_or(false) {
        return None;
    }
    Some(Token { verb: verb.into(), artifact, id, value })
}

/// The short token the renderer prints inside a pane that shows one artifact.
pub fn short(verb: &str, id: &str, value: Option<&str>) -> String {
    match value {
        Some(v) => format!("mg:{verb}/{id}={v}"),
        None => format!("mg:{verb}/{id}"),
    }
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    Applied(Event),
    Opened(String),
    Copied(&'static str),
    Ignored(String),
}

/// Find the store and artifact a token refers to. With an explicit artifact, look in the current
/// project first, then every project. Without one, prefer artifacts open in this Zellij session
/// (`margin open` records them), then any artifact holding that id, most recently updated first.
pub fn resolve(tok: &Token) -> Option<(Store, String)> {
    let home = default_home();
    let mut cands: Vec<String> = Vec::new();
    if let Ok(p) = std::env::var("MARGIN_PROJECT") {
        cands.push(p);
    }
    if let Ok(sess) = std::env::var("ZELLIJ_SESSION_NAME") {
        if let Ok(s) = std::fs::read_to_string(home.join("zellij").join(format!("{sess}.json"))) {
            if let Ok(v) = serde_json::from_str::<Value>(&s) {
                if let Some(arr) = v["projects"].as_array() {
                    cands.extend(arr.iter().filter_map(|x| x.as_str().map(String::from)));
                }
            }
        }
    }
    cands.push(Store::open_cwd().project);
    for p in projects(&home) {
        if !cands.contains(&p) {
            cands.push(p);
        }
    }
    let mut best: Option<(String, String, String)> = None; // (updated, project, artifact)
    for p in cands {
        let st = Store::new(home.clone(), p.clone());
        let docs = match &tok.artifact {
            Some(a) => st.load(a).ok().into_iter().collect::<Vec<_>>(),
            None => st.list(),
        };
        for d in docs {
            if d.find(&tok.id).is_some() || (tok.verb == "open" && d.id == tok.id) {
                if best.as_ref().map(|b| d.updated > b.0).unwrap_or(true) {
                    best = Some((d.updated.clone(), p.clone(), d.id.clone()));
                }
            }
        }
        // an explicit project hint wins as soon as it matches
        if best.is_some() && tok.artifact.is_some() {
            break;
        }
    }
    best.map(|(_, p, a)| (Store::new(home.clone(), p), a))
}

pub fn apply_token(tok: &Token, via: &str, cfg: &Config) -> Result<Outcome, String> {
    let (st, art) = resolve(tok).ok_or_else(|| format!("no artifact holds {:?}", tok.id))?;
    apply_in(&st, &art, tok, via, cfg)
}

/// Apply a token to a known artifact. The pane calls this for its own clicks: it already knows
/// its store, and resolving would pick whichever project last opened an artifact of that name.
pub fn apply_in(st: &Store, art: &str, tok: &Token, via: &str, cfg: &Config) -> Result<Outcome, String> {
    let art = art.to_string();
    let doc = st.load(&art)?;
    let state = st.state(&art);
    let block = doc.find(&tok.id);
    let cur = state.decisions.get(&tok.id);
    // a verb acts only on its own kind: `mg:ask/d1` against an artifact whose d1 is a todo once
    // replaced that todo's status with a verdict
    let want = match tok.verb.as_str() {
        "todo" | "tick" => Some("todo"),
        "pick" | "hold" | "talk" => Some("question"),
        "strike" => Some("claim"),
        "approve" | "reject" | "ask" => Some("diff"),
        _ => None,
    };
    if let (Some(k), Some(b)) = (want, block) {
        if b.kind != k {
            return Err(format!("{} is a {}, and {} acts on a {k}", tok.id, b.kind, tok.verb));
        }
    }
    let ev = match tok.verb.as_str() {
        "todo" => {
            // a bare token does what a click on the circle does: next → skipped → Claude's again
            let v = match tok.value.clone() {
                Some(v) => v,
                None => {
                    let b = block.ok_or("no such todo")?;
                    let st_now = crate::components::status_of(&state, b);
                    let _ = &cur;
                    match crate::components::cycle_for(&state, b) {
                        Some(v) => v.to_string(),
                        None => return Err(format!("{} is {st_now}: that's Claude's to change (stop a running step with =skipped)", tok.id)),
                    }
                }
            };
            match v.as_str() {
                // drop sulin's mark: the todo shows Claude's status again
                "clear" => st.decide(&art, &tok.id, "untick", Value::Null, via, None)?,
                s if crate::doc::STATUSES.contains(&s) => st.decide(&art, &tok.id, "tick", json!(v), via, None)?,
                _ => return Err(format!("a todo takes {} or clear, not {v:?}", crate::doc::STATUSES.join(" "))),
            }
        }
        "pick" => {
            let b = block.ok_or("no such question")?;
            let opts = b.strs("options");
            let v = tok.value.clone().ok_or("pick needs =<option>")?;
            // accept the option text or its 1-based number
            let v = match v.parse::<usize>() {
                Ok(n) if n >= 1 && n <= opts.len() && !opts.contains(&v) => opts[n - 1].clone(),
                _ => v,
            };
            if !opts.contains(&v) {
                return Err(format!("{v:?} isn't an option of {} ({})", tok.id, opts.join(", ")));
            }
            st.decide(&art, &tok.id, "pick", json!(v), via, None)?
        }
        // a test toast, from a link row (`target: mg:toasttest/<anything>`): a view action, so it
        // reaches Claude as a passive event only
        "toasttest" => {
            crate::toast::test(st, &art)?;
            st.log(&art, "", "toasttest", Value::Null, via)?
        }
        // "wait for me": Claude stops where the default would be taken instead of taking it;
        // again to let the default stand
        "hold" => {
            if cur.map(|d| d.kind == "hold").unwrap_or(false) {
                st.decide(&art, &tok.id, "unhold", Value::Null, via, None)?
            } else {
                st.decide(&art, &tok.id, "hold", json!(true), via, None)?
            }
        }
        // "talk about this": Claude raises the question in chat; again to take it back. A pick
        // replaces it
        "talk" => {
            if cur.map(|d| d.kind == "talk").unwrap_or(false) {
                st.decide(&art, &tok.id, "untalk", Value::Null, via, None)?
            } else {
                st.decide(&art, &tok.id, "talk", json!(true), via, None)?
            }
        }
        "strike" => {
            if cur.map(|d| d.kind == "strike").unwrap_or(false) && tok.value.is_none() {
                st.decide(&art, &tok.id, "unstrike", json!(false), via, None)?
            } else {
                let reason = tok.value.as_ref().map(|r| r.replace(['-', '_'], " "));
                st.decide(&art, &tok.id, "strike", json!(true), via, reason)?
            }
        }
        // `=clear` takes sulin's verdict back: the diff waits for one again
        "approve" | "reject" | "ask" if tok.value.as_deref() == Some("clear") => st.decide(&art, &tok.id, "unverdict", Value::Null, via, None)?,
        "approve" | "reject" | "ask" => {
            let v = if tok.verb == "approve" { "approved" } else if tok.verb == "reject" { "rejected" } else { "asked" };
            st.decide(&art, &tok.id, "verdict", json!(v), via, None)?
        }
        // a view change, not a decision: logged as a passive event that panes on this artifact
        // pick up and Claude is never handed
        "fold" => {
            let b = block.ok_or("no such section")?;
            if b.kind != "section" {
                return Err(format!("{} is a {}, not a section", tok.id, b.kind));
            }
            st.log(&art, &tok.id, "fold", Value::Null, via)?
        }
        "pin" => {
            let pinned = cur.map(|d| d.kind == "pin" && d.value == json!(true)).unwrap_or(false);
            st.decide(&art, &tok.id, "pin", json!(!pinned), via, None)?
        }
        // a block with a target: the pane and `select` run nothing themselves. They append
        // the event `<board>.open` (the block's id, a snapshot with its target), and the handler
        // registered for it opens it: `margin handler add open --on '*.open' -- margin handle open`
        "open" if block.is_some() => {
            let by = if via == "handler" { "handler:open" } else { crate::who::HUMAN };
            st.act(&art, &tok.id, "open", None, by, via)?
        }
        "open" => {
            // an artifact's name, not a block: show its pane wherever the host puts panes (a
            // view of the store, not an action on a row)
            crate::host::pick(cfg, None).open(st, &art, &crate::host::OpenOpts::default(), cfg)?;
            return Ok(Outcome::Opened(art));
        }
        "accept" | "decline" => {
            let b = block.ok_or_else(|| format!("no component {:?}", tok.id))?;
            let pr = open_proposal(b, &state).ok_or_else(|| format!("{} has no open proposal", tok.id))?.clone();
            let accepted = tok.verb == "accept";
            if accepted {
                // the change first: if it no longer applies (an option Claude since removed), the
                // proposal stays open and he sees why
                let t = Token { verb: match pr["verb"].as_str().unwrap_or("") { "tick" => "todo".to_string(), v => v.to_string() }, artifact: Some(art.clone()), id: tok.id.clone(), value: pr["value"].as_str().map(String::from) };
                if !PROPOSABLE.contains(&t.verb.as_str()) {
                    return Err(format!("a proposal can't carry {:?}", t.verb));
                }
                apply_in(st, &art, &t, via, cfg)?;
            }
            st.resolve_proposal(&art, &tok.id, &pr, accepted, via)?
        }
        v => return Err(format!("unknown verb {v}")),
    };
    Ok(Outcome::Applied(ev))
}

pub fn dispatch(text: &str, via: &str, cfg: &Config) -> Outcome {
    let t = text.trim();
    log_line(&format!("select {:?}", truncate_for_log(t)));
    if let Some(tok) = parse_token(t) {
        return match apply_token(&tok, via, cfg) {
            Ok(o) => o,
            Err(e) => {
                log_line(&format!("  error: {e}"));
                Outcome::Ignored(e)
            }
        };
    }
    if (t.starts_with("http://") || t.starts_with("https://")) && !t.contains(char::is_whitespace) {
        return match open_url(t, cfg) {
            Ok(()) => Outcome::Opened(t.to_string()),
            Err(e) => Outcome::Ignored(e),
        };
    }
    Outcome::Copied(copy(text, cfg))
}

fn truncate_for_log(s: &str) -> String {
    vellum::text::truncate(s, 80).into_owned()
}

pub fn log_line(s: &str) {
    let p = default_home().join("select.log");
    let _ = std::fs::create_dir_all(default_home());
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
        let _ = writeln!(f, "{} {s}", crate::util::now_rfc3339());
    }
}

fn on_path(cmd: &str) -> bool {
    std::env::var("PATH").unwrap_or_default().split(':').any(|d| std::path::Path::new(d).join(cmd).is_file())
}

fn over_ssh() -> bool {
    std::env::var("SSH_CONNECTION").map(|s| !s.is_empty()).unwrap_or(false)
}

/// Open a URL where sulin's eyes are: over ssh through superbrothers/opener (its daemon on the
/// laptop listens on a forwarded socket), else xdg-open on this machine.
pub fn open_url(url: &str, cfg: &Config) -> Result<(), String> {
    let cmd = match cfg.opener.as_str() {
        "auto" => {
            if over_ssh() && on_path("opener") {
                "opener".to_string()
            } else {
                "xdg-open".to_string()
            }
        }
        c => c.to_string(),
    };
    log_line(&format!("  open {url} with {cmd}"));
    let mut parts = cmd.split_whitespace();
    let prog = parts.next().ok_or("empty opener")?;
    Command::new(prog)
        .args(parts)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("{prog}: {e}"))
}

/// Put text on the clipboard. Returns which path it took.
pub fn copy(text: &str, cfg: &Config) -> &'static str {
    let mode = match cfg.clipboard.as_str() {
        "auto" => {
            if !over_ssh() && std::env::var("WAYLAND_DISPLAY").is_ok() && on_path("wl-copy") {
                "wl-copy"
            } else {
                "osc52"
            }
        }
        "wl-copy" => "wl-copy",
        "none" => "none",
        _ => "osc52",
    };
    match mode {
        "wl-copy" => {
            if let Ok(mut c) = Command::new("wl-copy").stdin(Stdio::piped()).spawn() {
                if let Some(mut i) = c.stdin.take() {
                    let _ = i.write_all(text.as_bytes());
                }
                let _ = c.wait();
            }
            "wl-copy"
        }
        "osc52" => {
            let seq = format!("\x1b]52;c;{}\x07", vellum::image::base64(text.as_bytes()));
            let ttys = client_ttys();
            log_line(&format!("  osc52 to {ttys:?}"));
            for t in ttys {
                if let Ok(mut f) = std::fs::OpenOptions::new().write(true).open(&t) {
                    let _ = f.write_all(seq.as_bytes());
                }
            }
            "osc52"
        }
        _ => "none",
    }
}

/// The ttys of the Zellij clients attached to `$ZELLIJ_SESSION_NAME`. The session's server listens
/// on a unix socket named after the session; each attached client holds the peer end, so `ss -xp`
/// maps socket peers to client pids, and each client's stdin is its tty (research.md §2).
pub fn client_ttys() -> Vec<String> {
    let Ok(sess) = std::env::var("ZELLIJ_SESSION_NAME") else { return vec![] };
    let Ok(out) = Command::new("ss").args(["-xpH"]).output() else { return vec![] };
    let text = String::from_utf8_lossy(&out.stdout);
    // columns: Netid State Recv-Q Send-Q Local-Address Port Peer-Address Port Process
    let rows: Vec<Vec<&str>> = text.lines().map(|l| l.split_whitespace().collect()).collect();
    let suffix = format!("/{sess}");
    let peers: Vec<&str> = rows
        .iter()
        .filter(|r| r.len() >= 8 && r[4].ends_with(&suffix) && r[4].contains("zellij"))
        .map(|r| r[7])
        .collect();
    let mut ttys = Vec::new();
    for r in rows.iter().filter(|r| r.len() >= 9 && peers.contains(&r[5])) {
        let procs = r[8..].join(" ");
        for pid in procs.split("pid=").skip(1).filter_map(|s| s.split(',').next()) {
            if let Ok(t) = std::fs::read_link(format!("/proc/{pid}/fd/0")) {
                let t = t.to_string_lossy().into_owned();
                if t.starts_with("/dev/pts/") || t.starts_with("/dev/tty") {
                    if !ttys.contains(&t) {
                        ttys.push(t);
                    }
                }
            }
        }
    }
    ttys
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn grammar() {
        assert_eq!(
            parse_token("mg:pick/plan.Q1=mcp"),
            Some(Token { verb: "pick".into(), artifact: Some("plan".into()), id: "Q1".into(), value: Some("mcp".into()) })
        );
        assert_eq!(
            parse_token(" mg:todo/t3\n"),
            Some(Token { verb: "todo".into(), artifact: None, id: "t3".into(), value: None })
        );
        assert_eq!(parse_token("mg:strike/A2=work-laptop-is-1.1").unwrap().value.as_deref(), Some("work-laptop-is-1.1"));
        assert_eq!(parse_token("mg:tick/t3").unwrap().verb, "todo", "tick: the old spelling of todo");
        assert_eq!(parse_token("mg:explode/t3"), None);
        assert_eq!(parse_token("mg:todo/"), None);
        assert_eq!(parse_token("mg:todo/a b"), None);
        assert_eq!(parse_token("hello"), None);
        assert_eq!(short("pick", "Q1", Some("mcp")), "mg:pick/Q1=mcp");
    }

    /// Every verb a button or link emits must parse: the hold button and the toast test once
    /// didn't, so a click did nothing.
    #[test]
    fn every_button_verb_parses() {
        for t in ["mg:hold/Q1", "mg:talk/Q1", "mg:toasttest/go", "mg:todo/t1=next", "mg:strike/A1", "mg:approve/d1", "mg:ask/d1=clear", "mg:pick/Q1=cli", "mg:fold/s"] {
            assert!(parse_token(t).is_some(), "{t} must parse");
        }
    }
}
