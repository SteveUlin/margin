//! The return channel into Claude: `margin events`, and the hook that hands Claude sulin's unread
//! actions at the start of its next turn (UserPromptSubmit) or mid-turn after a tool call
//! (PostToolUse; research.md §7 verified `additionalContext` reaches the model on its next call).
//!
//! Lines start with `margin:` so they explain themselves: research found the model ignores
//! unexplained injected text.

use crate::link;
use crate::store::{Activity, Event, Store};
use crate::util::project_store_for;
use serde_json::{json, Value};
use std::io::Read;

pub const MAX_LINES: usize = 8;

fn quote(s: &str) -> String {
    format!("“{}”", vellum::text::truncate(s, 60))
}

/// One readable line per event, with the component's text so Claude needn't look it up.
pub fn describe(st: &Store, e: &Event) -> String {
    let doc = st.load(&e.artifact).ok();
    let b = doc.as_ref().and_then(|d| d.find(&e.id));
    let text = b.map(|b| quote(b.s("text"))).unwrap_or_default();
    let v = e.value.as_str().map(String::from).unwrap_or_else(|| e.value.to_string());
    let at = format!("{}.{}", e.artifact, e.id);
    match e.kind.as_str() {
        "pick" => {
            let def = b.map(|b| b.s("default").to_string()).unwrap_or_default();
            let was = if !def.is_empty() && def != v { format!(" (your default was {def}; switch to {v})") } else { String::new() };
            format!("sulin picked {v} for {at} {text}{was}")
        }
        "own" => format!("sulin took {at} {text} as their own step: don't do it; work on what doesn't depend on it, and wait for them to finish it"),
        "unown" => format!("sulin handed {at} {text} back to you"),
        "tick" if v == "done" && b.map(|b| crate::components::yours(&st.state(&e.artifact), b)).unwrap_or(false) => {
            format!("sulin finished their step {at} {text}: carry on with what was waiting on it")
        }
        "tick" => match v.as_str() {
            "done" => format!("sulin marked {at} {text} done: they did it themselves, so don't redo it"),
            "skipped" if st.state(&e.artifact).decisions.get(&e.id).and_then(|d| d.under.clone()) == Some(json!("doing")) => {
                format!("sulin stopped {at} {text} while it ran: stop working on it now, and say what state you left it in")
            }
            "skipped" => format!("sulin skipped {at} {text}: don't do this step"),
            "next" => format!("sulin wants {at} {text} next: do it before the other open steps"),
            _ => format!("sulin marked {at} {text} {v}"),
        },
        "untick" => format!("sulin cleared their mark on {at} {text}; the step is yours again"),
        "strike" => {
            let st_ = st.state(&e.artifact);
            let r = st_.decisions.get(&e.id).and_then(|d| d.reason.clone()).map(|r| format!(": {}", quote(&r))).unwrap_or_default();
            // the steps that rest on it, so Claude needn't work out what the strike touches
            let deps: Vec<String> = doc
                .as_ref()
                .map(|d| {
                    let mut v = Vec::new();
                    crate::doc::walk(&d.blocks, &mut |x, _, _| {
                        if x.kind == "todo" && x.strs("rests_on").contains(&e.id) && !matches!(crate::components::status_of(&st_, x).as_str(), "done" | "skipped") {
                            v.push(format!("{} {}", x.id, quote(x.s("text"))));
                        }
                    });
                    v
                })
                .unwrap_or_default();
            let dep = if deps.is_empty() { String::new() } else { format!(" Steps resting on it, now flagged: {}; re-plan them before doing them.", deps.join(", ")) };
            format!("sulin struck assumption {at} {text}{r}.{dep} Answer it: drop or re-check the assumption, then `margin set {} {} ack \"…\"`", e.artifact, e.id)
        }
        "unstrike" => format!("sulin withdrew their strike on {at} {text}"),
        "verdict" => format!("sulin {v} {at}{}", b.map(|b| format!(" ({})", b.s("path"))).unwrap_or_default()),
        "hold" => {
            let when = b.map(|b| b.s("applies").to_string()).filter(|a| !a.is_empty()).map(|a| format!(" (it applies at {a})")).unwrap_or_default();
            format!("sulin wants to decide {at} {text} themselves{when}: don't take the default; when you reach that point, stop and wait for their pick")
        }
        "talk" => format!("sulin wants to talk about {at} {text} before picking: raise it in chat now (the tradeoff as you see it, and what would change your suggestion), and treat it as still open"),
        "untalk" => format!("sulin took back \"talk about this\" on {at} {text}: it waits for their pick"),
        "unhold" => format!("sulin released their hold on {at} {text}: take the default as planned unless they pick"),
        "stop" => "sulin pressed stop in the pane: stop what you're doing, say where you are, and wait for them".to_string(),
        "stop-agent" => format!("sulin stopped subagent {v} from the pane: it was told to report what it has and end; don't restart it unless they ask"),
        "unnote" => format!("sulin took back a note on {}.{}", e.artifact, crate::store::note_row(&e.id)),
        "unverdict" => format!("sulin took back their verdict on {at}{}: it waits for one again, so hold off acting on the earlier one", b.map(|b| format!(" ({})", b.s("path"))).unwrap_or_default()),
        "pin" => format!("sulin {} {at} {text}", if e.value == json!(true) { "pinned" } else { "unpinned" }),
        "open" => format!("sulin opened {at}"),
        "proposal" => {
            let what = format!("{}{}", e.value["verb"].as_str().unwrap_or("?"), e.value["value"].as_str().map(|v| format!(" {v}")).unwrap_or_default());
            if e.value["accepted"] == json!(true) {
                format!("sulin accepted your proposal to {what} on {at} {text} (their decision is the line for it)")
            } else {
                format!("sulin declined your proposal to {what} on {at} {text}: leave it as they have it")
            }
        }
        // a named event (`margin emit`, an action, a job's emit): its name, what it is about, who
        k if e.via == "emit" || e.via == "handler" || k.contains('.') => {
            let about = match (e.artifact.is_empty(), e.id.is_empty()) {
                (true, _) => String::new(),
                (false, true) => format!(" {}", e.artifact),
                (false, false) => format!(" {at}"),
            };
            let val = if e.value.is_null() { String::new() } else { format!(" {}", vellum::text::truncate(&v, 100)) };
            format!("{k}{about}{val} (by {}, via {})", if crate::who::is_human(&e.by) { "sulin" } else { &e.by }, e.via)
        }
        k => format!("sulin {k} {at} {v}"),
    }
}

/// Unread, non-passive events sulin made, for this session.
pub fn unread(st: &Store, session: &str) -> Vec<Event> {
    let from = st.delivered(session);
    st.events_since(from).into_iter().filter(crate::store::is_decision).collect()
}

/// Mid-turn, an action younger than this waits: sulin often clicks and takes it back a moment
/// later, and Claude shouldn't act on the first click. A prompt delivers everything (sending a
/// message is his "go").
pub const SETTLE_SECS: f64 = 5.0;

/// A decision and the one that takes it back share a family, so they coalesce.
fn family(kind: &str) -> &str {
    match kind {
        "tick" | "untick" => "tick",
        "strike" | "unstrike" => "strike",
        "verdict" | "unverdict" => "verdict",
        "note" | "unnote" => "note",
        "hold" | "unhold" => "hold",
        "talk" | "untalk" => "talk",
        "own" | "unown" => "own",
        k => k,
    }
}

fn is_undo(kind: &str) -> bool {
    matches!(kind, "untick" | "unstrike" | "unverdict" | "unnote" | "unhold" | "untalk" | "unown")
}

/// What to hand Claude now: the net result per component of sulin's unread actions, and how far
/// the delivered cursor may move. With `settle` > 0, actions younger than it wait for the next
/// look. A run on one component collapses to its last action, and to nothing when it ends where
/// Claude last heard it (ticked then cleared, picked A then B then A).
pub fn pending(all: &[Event], from: u64, now: f64, settle: f64) -> (Vec<Event>, Option<u64>) {
    let mine = |e: &&Event| crate::store::is_decision(e);
    let fresh: Vec<&Event> = all.iter().filter(|e| e.seq > from).filter(mine).collect();
    let age = |e: &Event| now - crate::util::parse_rfc3339(&e.at).unwrap_or(now);
    let ready: Vec<&Event> = if settle > 0.0 { fresh.iter().take_while(|e| age(e) >= settle).copied().collect() } else { fresh };
    let cursor = ready.last().map(|e| e.seq);
    // the last action per component, in the order those last actions happened
    let mut last: Vec<&Event> = Vec::new();
    for e in &ready {
        last.retain(|x| (x.artifact.as_str(), x.id.as_str(), family(&x.kind)) != (e.artifact.as_str(), e.id.as_str(), family(&e.kind)));
        last.push(e);
    }
    let heard = |e: &Event| all.iter().filter(|x| x.seq <= from).filter(mine).filter(|x| x.artifact == e.artifact && x.id == e.id && family(&x.kind) == family(&e.kind)).last();
    let net = last
        .into_iter()
        .filter(|e| match heard(e) {
            // never heard of, or heard it taken back: an undo now is no news
            None => !is_undo(&e.kind),
            Some(h) if is_undo(&h.kind) => !is_undo(&e.kind),
            // ends where Claude last heard it
            Some(h) => !(h.kind == e.kind && h.value == e.value),
        })
        .cloned()
        .collect();
    (net, cursor)
}

/// One note in a review line: `less r3doc-5eea “…”`, or with the quote he picked.
fn note_phrase(st: &Store, e: &Event) -> String {
    let row = crate::store::note_row(&e.id);
    let r = e.value["r"].as_str().unwrap_or("?");
    let q = e.value["quote"].as_str().unwrap_or("").trim();
    if !q.is_empty() {
        return format!("{r} {row} on {}", quote(q));
    }
    let text = st.load(&e.artifact).ok().and_then(|d| d.find(row).map(|b| if b.s("text").is_empty() { b.s("title").to_string() } else { b.s("text").to_string() })).unwrap_or_default();
    format!("{r} {row} {}", quote(&text))
}

pub fn brief(st: &Store, evs: &[Event]) -> String {
    // a review is one line per artifact: his notes are one batch, and read as one
    let (notes, rest): (Vec<&Event>, Vec<&Event>) = evs.iter().partition(|e| e.kind == "note");
    let mut arts: Vec<&str> = notes.iter().map(|e| e.artifact.as_str()).collect();
    arts.dedup();
    let mut review: Vec<String> = arts
        .iter()
        .map(|a| {
            let ps: Vec<String> = notes.iter().filter(|e| e.artifact == *a).map(|e| note_phrase(st, e)).collect();
            format!("- sulin reviewed {a}: {}. Revise those rows in place (a note clears when its row changes); `less` means trim or archive, not wrong; `disagree` means rethink it", ps.join("; "))
        })
        .collect();
    let rest: Vec<Event> = rest.into_iter().cloned().collect();
    let evs = &rest[..];
    let n = evs.len() + notes.len();
    let mut lines = vec![format!(
        "margin: sulin acted in the margin pane since your last look ({} event{}). Their decisions are binding; `margin show <artifact>` for context.",
        n,
        if n == 1 { "" } else { "s" }
    )];
    lines.append(&mut review);
    for e in evs.iter().take(MAX_LINES) {
        lines.push(format!("- {}", describe(st, e)));
    }
    if evs.len() > MAX_LINES {
        lines.push(format!("- … and {} more: `margin events --unread`", evs.len() - MAX_LINES));
    }
    lines.join("\n")
}

/// A short, human label for a tool call: the description Claude gave a command, the file a read
/// or edit touches, the pattern a search looks for.
pub fn label(tool: &str, input: &Value) -> String {
    let s = |k: &str| input[k].as_str().unwrap_or("").to_string();
    let file = |k: &str| s(k).rsplit('/').next().unwrap_or("").to_string();
    let t = match tool {
        "Bash" => {
            let d = s("description");
            if d.is_empty() { s("command") } else { d }
        }
        "Read" | "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => format!("{tool} {}", file(if tool == "NotebookEdit" { "notebook_path" } else { "file_path" })),
        "Grep" | "Glob" => format!("{tool} {}", s("pattern")),
        "Agent" | "Task" => s("description"),
        "WebSearch" => s("query"),
        "WebFetch" => s("url"),
        "Skill" => format!("skill {}", s("skill")),
        _ => String::new(),
    };
    let t = t.lines().next().unwrap_or("").trim().to_string();
    vellum::text::truncate(&t, 120).into_owned()
}

/// Record what this session is doing for the pane's live strip. Cheap: one small atomic write,
/// and nothing reaches the model.
pub fn record(st: &Store, kind: &str, session: &str, v: &Value) {
    let tool = v["tool_name"].as_str().unwrap_or("").to_string();
    let (state, tool, text) = match kind {
        "pre-tool" => ("running", tool.clone(), label(&tool, &v["tool_input"])),
        "post-tool" => ("thinking", String::new(), String::new()),
        "prompt" => ("thinking", String::new(), "reading your message".to_string()),
        "stop" => ("idle", String::new(), String::new()),
        // Claude is blocked on sulin: a permission prompt, or Claude Code's own "needs you"
        // notification (its idle reminder is just the turn having ended: leave that as idle)
        "permission" => ("waiting", tool.clone(), format!("allow {}", label(&tool, &v["tool_input"])).trim_end().to_string()),
        "notify" => {
            let msg = v["message"].as_str().unwrap_or("").to_string();
            let low = msg.to_lowercase();
            if !(low.contains("permission") || low.contains("approv") || low.contains("needs your")) {
                return;
            }
            ("waiting", String::new(), msg)
        }
        _ => return,
    };
    let agent = v["agent_id"].as_str().unwrap_or("").to_string();
    st.set_activity(&Activity { session: session.into(), agent, state: state.into(), tool, text, since: crate::util::now_rfc3339() });
}

/// `margin hook start|prompt|pre-tool|post-tool|stop`: record what Claude is doing for the pane,
/// keep the session's link to its pane (link.rs), and hand Claude what it needs: sulin's unread
/// events (prompt, post-tool), the one-line pane status (start, prompt), and a nudge when the
/// session has gone link::STALE tool calls without writing to its pane (post-tool).
/// Prints nothing (costs nothing) when there's nothing to say.
pub fn run(kind: &str) -> Result<(), String> {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let v: Value = serde_json::from_str(&input).unwrap_or(json!({}));
    let session = v["session_id"]
        .as_str()
        .map(String::from)
        .or_else(|| std::env::var("CLAUDE_CODE_SESSION_ID").ok())
        .unwrap_or_else(|| "default".into());
    let cwd = v["cwd"].as_str().map(std::path::PathBuf::from).unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let st = project_store_for(&cwd);
    if !st.dir().exists() {
        return Ok(());
    }
    record(&st, kind, &session, &v);
    // from its first hook on, a session's unread is measured from its own cursor
    st.first_seen(&session);
    // the session's inbox, so the pane can wake it when sulin finishes (wake.rs); not a subagent's
    if v["agent_id"].as_str().unwrap_or("").is_empty() {
        crate::wake::save_inbox(&session);
        // Claude Code's recap, kept for the pane (recap.rs); it's written as he comes back, so the
        // prompt that follows finds it
        if matches!(kind, "prompt" | "start") {
            if let Some(t) = v["transcript_path"].as_str() {
                crate::recap::save(&st, &session, std::path::Path::new(t));
            }
        }
    }
    // the stop signal: refuse the tool call, and say why, for as long as it's up
    if kind == "pre-tool" && st.stopped_for(&session) {
        let why = "sulin pressed stop in the margin pane. Don't run more tools: say in two or three lines where you are and what you were about to do, then wait for them.";
        println!("{}", json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": crate::who::say(why)}}));
        return Ok(());
    }
    // one subagent stopped from its row in the work overview
    let agent = v["agent_id"].as_str().unwrap_or("");
    if kind == "pre-tool" && !agent.is_empty() && st.agent_stopped(agent) {
        let why = "sulin stopped this subagent from the margin pane. Run no more tools: report what you have so far and end.";
        println!("{}", json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": crate::who::say(why)}}));
        return Ok(());
    }
    // his message ends a stop: from here he's talking
    if kind == "prompt" && agent.is_empty() && (st.stopped_for(&session) || st.dir().join("stop-agents").exists()) {
        st.clear_stop();
    }
    if matches!(kind, "pre-tool" | "stop" | "permission" | "notify") {
        return Ok(());
    }
    let mut lines: Vec<String> = Vec::new();
    // the session's pane: its own link, else one inherited from this chat pane (a resume that
    // changed the id), else, at session start, the margin pane in this chat's tab
    let mut link = link::get(&st, &session).or_else(|| link::inherit(&st, &session));
    if link.is_none() && kind == "start" {
        link = link::auto(&st, &session);
    }
    // the transcript path, so the pane can find this session's subagents (agents.rs)
    if let (Some(l), Some(t), true) = (link.as_mut(), v["transcript_path"].as_str(), v["agent_id"].as_str().unwrap_or("").is_empty()) {
        if l.transcript != t {
            l.transcript = t.to_string();
            link::save(&st, l);
        }
    }
    match (kind, link.as_mut()) {
        ("start", Some(l)) => {
            lines.push(link::status_line(&st, l));
            lines.push(format!("margin: it's linked to this session ({}); keep it current as you work (`margin show {}` for the plan, `margin link <art>` to switch)", l.how, l.art));
            // after a compaction or a resume Claude's picture of the work was rebuilt from a
            // summary: say it back where sulin can strike what's wrong (r6; I-PASS read-back)
            if matches!(v["source"].as_str(), Some("compact") | Some("resume")) {
                lines.push(format!(
                    "margin: read-back: your picture of the work was just rebuilt from a summary. Before acting on it, post 2-3 claims in a section `readback` at the top of {} (ids rb1-rb3): the goal, the live thread and where it stands, your next step. Replace any earlier read-back. sulin strikes what's wrong; re-check before going on if they do.",
                    l.art
                ));
            }
        }
        ("prompt", Some(l)) => lines.push(link::status_line(&st, l)),
        // a subagent shares its parent's session id but owns no pane: its calls neither count
        // against the pane nor get the nudge (2026-11-08: research agents were nudged, and their
        // calls made the parent's pane look stale)
        ("post-tool", Some(_)) if !v["agent_id"].as_str().unwrap_or("").is_empty() => {}
        ("post-tool", Some(l)) => {
            let tool = v["tool_name"].as_str().unwrap_or("");
            let n = link::tick(&st, l, link::is_pane_write(tool, &v["tool_input"]));
            if n == link::STALE || (n > link::STALE && (n - link::STALE) % link::STALE_AGAIN == 0) {
                lines.push(format!("margin: {n} tool calls since you last wrote to your pane ({}); update it as you go (chain the write onto your next real step)", l.art));
            }
        }
        _ => {}
    }
    // sulin's actions are for the main session: a subagent shares its id, and handing them to the
    // subagent marked them read where Claude never saw them (2026-11-08: two skips went missing)
    let subagent = !v["agent_id"].as_str().unwrap_or("").is_empty();
    let (evs, cursor) = if kind == "start" || subagent {
        (vec![], None)
    } else {
        let settle = if kind == "post-tool" { SETTLE_SECS } else { 0.0 };
        pending(&st.events(), st.delivered(&session), crate::util::epoch_now(), settle)
    };
    if !evs.is_empty() {
        lines.insert(0, brief(&st, &evs));
    }
    // actions that netted to nothing still count as handed over
    if let Some(c) = cursor {
        st.set_delivered(&session, c);
    }
    if lines.is_empty() {
        return Ok(());
    }
    // the person's configured name in place of "sulin" (who.rs)
    let text = crate::who::say(&lines.join("\n"));
    match kind {
        "post-tool" | "start" => {
            let event = if kind == "start" { "SessionStart" } else { "PostToolUse" };
            println!("{}", json!({"hookSpecificOutput": {"hookEventName": event, "additionalContext": text}}));
        }
        _ => println!("{text}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(seq: u64, at: f64, id: &str, kind: &str, v: &str) -> Event {
        Event {
            v: 1,
            seq,
            at: crate::util::rfc3339(at),
            artifact: "plan".into(),
            id: id.into(),
            kind: kind.into(),
            value: if v.is_empty() { Value::Null } else { json!(v) },
            by: crate::who::HUMAN.into(),
            via: "pane".into(),
            rev: 1,
        }
    }
    fn ids(v: &[Event]) -> Vec<String> {
        v.iter().map(|e| format!("{}:{}", e.id, e.kind)).collect()
    }

    /// What `run` does with the cursor on a prompt: note the session, hand over, advance.
    fn prompt(st: &Store, session: &str) -> Vec<String> {
        st.first_seen(session);
        let (net, cur) = pending(&st.events(), st.delivered(session), crate::util::epoch_now(), 0.0);
        if let Some(c) = cur {
            st.set_delivered(session, c);
        }
        ids(&net)
    }

    /// B prompted with nothing unread, so it had no cursor file; sulin picked; A read the pick.
    /// B's cursor then fell back to the furthest session's (A's, now past the pick) and B never
    /// heard it.
    #[test]
    fn a_session_hears_what_another_session_read_first() {
        let st = crate::store::tests::temp_store();
        st.put(crate::store::tests::plan(), None).unwrap();
        st.decide("plan", "t1", "tick", json!("done"), "key", None).unwrap();
        assert_eq!(prompt(&st, "A"), ["t1:tick"]);
        assert!(prompt(&st, "B").is_empty(), "a new session gets no backlog");
        st.decide("plan", "Q1", "pick", json!("mcp"), "select", None).unwrap();
        assert_eq!(prompt(&st, "A"), ["Q1:pick"]);
        assert_eq!(prompt(&st, "B"), ["Q1:pick"], "B was here before the pick: it's B's to hear too");
        assert!(prompt(&st, "B").is_empty() && prompt(&st, "A").is_empty());
        // a session that arrives now starts where the furthest one is
        assert!(prompt(&st, "C").is_empty());
        // and first_seen never moves a cursor that exists
        st.set_delivered("B", 1);
        st.first_seen("B");
        assert_eq!(st.delivered("B"), 1);
        std::fs::remove_dir_all(&st.home).ok();
    }

    #[test]
    fn a_click_taken_back_never_reaches_claude() {
        let all = [ev(1, 100.0, "t1", "tick", "next"), ev(2, 101.0, "t1", "untick", ""), ev(3, 102.0, "c1", "strike", "")];
        let (net, cur) = pending(&all, 0, 200.0, 0.0);
        assert_eq!(ids(&net), ["c1:strike"]);
        assert_eq!(cur, Some(3), "the cursor still moves past them");
    }

    #[test]
    fn a_run_collapses_to_its_last_action() {
        let all = [ev(1, 100.0, "Q1", "pick", "a"), ev(2, 101.0, "Q1", "pick", "b"), ev(3, 102.0, "t1", "tick", "next"), ev(4, 103.0, "t1", "tick", "skipped")];
        let (net, _) = pending(&all, 0, 200.0, 0.0);
        assert_eq!(net.iter().map(|e| e.value.as_str().unwrap()).collect::<Vec<_>>(), ["b", "skipped"]);
    }

    #[test]
    fn ending_where_claude_last_heard_is_no_news() {
        // Claude heard pick a (seq 1); then b, then a again
        let all = [ev(1, 10.0, "Q1", "pick", "a"), ev(2, 100.0, "Q1", "pick", "b"), ev(3, 101.0, "Q1", "pick", "a")];
        let (net, cur) = pending(&all, 1, 200.0, 0.0);
        assert!(net.is_empty(), "{:?}", ids(&net));
        assert_eq!(cur, Some(3));
        // but clearing a mark Claude heard is news
        let all = [ev(1, 10.0, "t1", "tick", "next"), ev(2, 100.0, "t1", "untick", "")];
        assert_eq!(ids(&pending(&all, 1, 200.0, 0.0).0), ["t1:untick"]);
    }

    #[test]
    fn notes_coalesce_per_quote() {
        let mut a = ev(1, 100.0, "r1#note#00aa", "note", "");
        a.value = json!({"r": "less", "quote": ""});
        let mut b = ev(2, 101.0, "r1#note#00aa", "note", "");
        b.value = json!({"r": "disagree", "quote": ""});
        let c = ev(3, 102.0, "r2#note#00aa", "note", "");
        let d = ev(4, 103.0, "r2#note#00aa", "unnote", "");
        let (net, _) = pending(&[a, b, c, d], 0, 200.0, 0.0);
        assert_eq!(net.len(), 1, "a note changed twice is one; a note taken back is none");
        assert_eq!(net[0].value["r"], "disagree");
    }

    #[test]
    fn mid_turn_young_actions_wait_out_the_undo_window() {
        let all = [ev(1, 100.0, "t1", "tick", "next"), ev(2, 198.0, "t2", "tick", "next")];
        let (net, cur) = pending(&all, 0, 200.0, SETTLE_SECS);
        assert_eq!(ids(&net), ["t1:tick"]);
        assert_eq!(cur, Some(1), "t2 waits for the next look");
        // a prompt is sulin's go: everything
        assert_eq!(pending(&all, 0, 200.0, 0.0).0.len(), 2);
    }
}
