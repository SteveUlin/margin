//! `margin demo`: a v0 artifact with every component (plan steps, checks, question cards,
//! assumptions, decided rows, a diff block and an image), and `margin demo live`, which plays a
//! Claude turn against it so every animation shows: a step completes and the progress bar
//! springs, a step slides in, a check finishes, a reorder glides, a suggestion lands on a card.
//!
//! The questions teach the current model: a question has no default and nothing is lit until
//! sulin picks. Claude may mark the answer it recommends with `suggest` (a robot after that
//! answer), and every card carries a "talk about this" button.

use crate::doc::{Block, Doc, Op};
use crate::store::Store;
use crate::util::{epoch_now, rfc3339};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::thread::sleep;
use std::time::Duration;

pub fn assets() -> PathBuf {
    std::env::var("MARGIN_ASSETS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/demo"))
}

fn block(v: Value) -> Block {
    serde_json::from_value(v).expect("demo block")
}

pub fn doc() -> Result<Doc, String> {
    let a = assets();
    let a = a.canonicalize().unwrap_or(a);
    let diff = crate::diffblock::compute(
        "d1",
        &a.join("recs.before.ts").to_string_lossy(),
        &a.join("recs.ts").to_string_lossy(),
        Some("apps/web/recs.ts"),
        2,
    )?;
    let mut d = plan(&a)?;
    if let Some(sec) = d.blocks.iter_mut().find(|b| b.id == "changed") {
        sec.children.push(diff);
    }
    Ok(d)
}

/// The showcase without its diff block, which needs difftastic (absent in the nix build's
/// sandbox, where the tests also run).
fn plan(a: &std::path::Path) -> Result<Doc, String> {
    let now = epoch_now();
    serde_json::from_value(json!({
        "id": "plan", "title": "fix recs flake", "kind": "demo",
        "blocks": [
            {"type": "section", "id": "steps", "title": "Steps", "children": [
                {"type": "todo", "id": "t1", "text": "Reproduce the flaky test", "note": "1 in 20 runs", "status": "done"},
                {"type": "todo", "id": "t2", "text": "Find the unseeded draw", "note": "sampleBeta", "status": "done"},
                {"type": "todo", "id": "t3", "text": "Seed RNG per test name", "status": "doing"},
                {"type": "todo", "id": "t4", "text": "Run the suite 200 times"}
            ]},
            {"type": "progress", "id": "p", "of": "steps"},
            {"type": "timer", "id": "clock", "since": rfc3339(now - 842.0), "label": "this turn"},
            {"type": "section", "id": "checks", "title": "Checks", "children": [
                {"type": "status", "id": "c1", "text": "deno test apps/web", "state": "running", "result": "12/20",
                 "spark": [2, 3, 2, 4, 5, 4, 6, 7, 6, 8, 9, 8]},
                {"type": "status", "id": "c2", "text": "deno lint", "state": "pass", "result": "clean"},
                {"type": "status", "id": "c3", "text": "check.py recs", "state": "fail", "result": "2 tells", "target": "https://example.com/recs/check"}
            ]},
            {"type": "section", "id": "inbox", "title": "Open questions", "note": "pick an answer, or talk about it; the robot marks Claude's suggestion", "children": [
                {"type": "question", "id": "Q1", "text": "MCP server in v0, or CLI only?", "options": ["cli", "mcp", "both"], "suggest": "cli",
                 "why": "CLI works today with zero config; MCP adds typed tools"},
                {"type": "question", "id": "Q2", "text": "Keep build logs after a session?", "options": ["keep", "30d", "drop"]}
            ]},
            {"type": "section", "id": "assume", "title": "Assumptions", "note": "strike to correct", "children": [
                {"type": "claim", "id": "A1", "text": "CI runs tests in one process"},
                {"type": "claim", "id": "A2", "text": "Zellij runs over ssh, not locally"}
            ]},
            {"type": "section", "id": "decided", "title": "Rules in force", "children": [
                {"type": "decided", "id": "D1", "text": "Seed from the test name", "by": "claude", "at": rfc3339(now - 600.0)},
                {"type": "decided", "id": "D2", "text": "Rust for toolkit, app and plugin", "by": "you", "at": rfc3339(now - 3600.0)}
            ]},
            {"type": "section", "id": "changed", "title": "Changed this turn", "note": "recs.ts +3 -1", "children": []},
            {"type": "section", "id": "explain", "title": "How the matcher reaches you", "folded": true, "children": [
                {"type": "image", "id": "g1", "path": a.join("graph.png").to_string_lossy(), "alt": "matcher pipeline (graphviz)", "rows": 12},
                {"type": "text", "id": "x1", "text": "Events land in a jsonl log; the matcher tails it and runs an enabled job, whose result comes back as a claude line.", "style": "note"}
            ]},
            {"type": "link", "id": "pr", "text": "PR #412 notes", "target": "https://github.com/example/web/pull/412"}
        ]
    }))
    .map_err(|e| e.to_string())
}

/// Create the demo artifact `plan`, or reset it when it is the demo's own (kind `demo`). A
/// `plan` that is someone's work is refused: the demo is a creating command, and it once
/// replaced whatever `plan` held.
pub fn setup(st: &Store) -> Result<u64, String> {
    if let Ok(d) = st.load("plan") {
        if d.kind != "demo" {
            return Err(format!("plan already exists (rev {}) and is not the demo's; run the demo in another project (--project demo), or remove it with margin rm plan --yes", d.rev));
        }
    }
    // a fresh start: drop old decisions and events for the demo artifact
    let _ = std::fs::remove_file(st.state_path("plan"));
    let _ = std::fs::remove_dir_all(st.art("plan").join("view"));
    st.put(doc()?, None)
}

fn step(st: &Store, ops: Vec<Op>, pause: f64, label: &str) {
    match st.patch("plan", &ops, None) {
        Ok(rev) => eprintln!("rev {rev}: {label}"),
        Err(e) => eprintln!("{label}: {e}"),
    }
    sleep(Duration::from_secs_f64(pause));
}

fn set(id: &str, f: &str, v: Value) -> Op {
    Op::Set { id: id.into(), field: f.into(), value: v }
}

/// Play one Claude turn against the demo plan. `speed` divides every pause.
pub fn live(st: &Store, speed: f64) {
    let p = |s: f64| s / speed.max(0.1);
    sleep(Duration::from_secs_f64(p(2.0)));
    step(st, vec![set("c1", "result", json!("15/20")), set("c1", "spark", json!([2, 3, 2, 4, 5, 4, 6, 7, 6, 8, 9, 8, 9]))], p(2.0), "checks climb");
    step(st, vec![set("t3", "status", json!("done")), set("t4", "status", json!("doing"))], p(3.0), "step 3 done: progress springs");
    step(
        st,
        vec![Op::Add {
            parent: Some("steps".into()),
            block: block(json!({"type": "todo", "id": "t5", "text": "Regression test: seeded runs", "note": "new"})),
            before: None,
            after: Some("t4".into()),
        }],
        p(3.0),
        "a step slides in",
    );
    step(st, vec![set("c1", "state", json!("pass")), set("c1", "result", json!("20/20"))], p(2.5), "check passes");
    step(st, vec![Op::Move { id: "t5".into(), before: Some("t4".into()), after: None, parent: None }], p(3.0), "reorder glides");
    step(
        st,
        vec![Op::Add {
            parent: Some("assume".into()),
            block: block(json!({"type": "claim", "id": "A3", "text": "One chat per tab"})),
            before: None,
            after: None,
        }],
        p(3.0),
        "new assumption",
    );
    step(
        st,
        vec![set("Q2", "suggest", json!("30d")), set("Q2", "why", json!("a month covers a review cycle; older ones are noise"))],
        p(3.0),
        "a suggestion lands: a robot on 30d, nothing picked",
    );
    step(
        st,
        vec![
            set("t4", "status", json!("done")),
            set("t5", "status", json!("done")),
            Op::Add {
                parent: Some("decided".into()),
                block: block(json!({"type": "decided", "id": "D3", "text": "Keep the seeded regression test in the suite", "by": "claude", "at": crate::util::now_rfc3339()})),
                before: Some("D1".into()),
                after: None,
            },
        ],
        p(2.0),
        "all steps done",
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::walk;

    /// The showcase teaches the current question model: no default, no "applies", an optional
    /// `suggest` that is one of the options.
    #[test]
    fn questions_have_no_default_and_suggest_an_option() {
        let d = plan(&assets()).unwrap();
        let (mut questions, mut suggested) = (0, 0);
        walk(&d.blocks, &mut |b, _, _| {
            assert!(!b.s("note").contains("default"), "{}: {}", b.id, b.s("note"));
            if b.kind == "question" {
                questions += 1;
                for f in ["default", "applies", "applied", "takes_at"] {
                    assert!(b.opt(f).is_none(), "{} still has `{f}`", b.id);
                }
                if let Some(s) = b.opt("suggest") {
                    suggested += 1;
                    assert!(b.strs("options").iter().any(|o| o == s), "{}: suggest {s} is not an option", b.id);
                }
            }
        });
        assert!(questions >= 2 && suggested >= 1, "{questions} questions, {suggested} suggested");
    }
}
