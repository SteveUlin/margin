//! The integration contract (`margin help contract`, skill/topics/contract.md) held against the
//! real binary: every documented field present with its documented type, the handler's stdin,
//! argv, cwd and environment, the exit codes, the cursors' two delivery modes, and the rule that
//! a board cannot carry a field the schema does not know. Each test also checks that the topic
//! names what it asserts, so the document and the tool cannot drift apart silently.

use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_margin");
const TOPIC: &str = include_str!("../../../skill/topics/contract.md");

const BOARD: &str = r#"{"title":"Reviews","actions":[{"key":"u","name":"refresh","label":"refresh all"}],"blocks":[
  {"type":"table","id":"pr","columns":[{"key":"title","title":"Title"}],"actions":[{"key":"e","name":"approve"}],"children":[
    {"type":"tr","id":"pr-7","target":"https://example.com/pr/7","detail":"Fails one run in five.","cells":{"title":"Fix the flaky test"}}]},
  {"type":"todo","id":"t1","text":"Review the queue"}]}"#;

/// A scratch store and a working directory beside it.
struct Tmp {
    root: PathBuf,
}

impl Tmp {
    fn new(name: &str) -> Tmp {
        let root = std::env::temp_dir().join(format!("margin-contract-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("work")).unwrap();
        Tmp { root }
    }
    fn home(&self) -> PathBuf {
        self.root.join("home")
    }
    fn work(&self) -> PathBuf {
        self.root.join("work")
    }
    /// Run margin as a person's shell would: (exit code, stdout, stderr).
    fn run(&self, argv: &[&str], stdin: &str) -> (i32, String, String) {
        let mut c = Command::new(BIN);
        c.args(argv)
            .current_dir(self.work())
            .env("HOME", &self.root)
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("MARGIN_HOME", self.home())
            .env("MARGIN_PROJECT", "p")
            .env("MARGIN_HOST", "plain")
            .env_remove("CLAUDECODE")
            .env_remove("CLAUDE_CODE_ENTRYPOINT")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("ZELLIJ")
            .env_remove("ZELLIJ_SESSION_NAME")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut ch = c.spawn().unwrap();
        ch.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        let out = ch.wait_with_output().unwrap();
        (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
    }
    /// Run and expect exit 0; the stdout.
    fn ok(&self, argv: &[&str], stdin: &str) -> String {
        let (code, out, err) = self.run(argv, stdin);
        assert_eq!(code, 0, "margin {argv:?}: {err}");
        out
    }
    fn json(&self, argv: &[&str]) -> Value {
        let out = self.ok(argv, "");
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("margin {argv:?} printed no JSON ({e}): {out}"))
    }
    fn events(&self, more: &[&str]) -> Vec<Value> {
        let mut argv = vec!["events", "--json"];
        argv.extend(more);
        self.ok(&argv, "").lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }
    fn daemon_running(&self) -> bool {
        self.json(&["daemon", "status", "--json"])["running"] == json!(true)
    }
    /// Stop the daemon the way a service manager would, and wait until it is gone.
    fn stop_daemon(&self) {
        // a daemon that has only just started may not have written its pid yet: ask again
        wait_for("the daemon to stop", || {
            let st = self.json(&["daemon", "status", "--json"]);
            if let Some(pid) = st["pid"].as_u64().filter(|p| *p > 0 && st["running"] == json!(true)) {
                let _ = Command::new("kill").arg(pid.to_string()).status();
            }
            st["running"] != json!(true)
        });
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        // nothing this test started outlives it
        if self.home().join("p/daemon.pid").exists() {
            if let Ok(pid) = std::fs::read_to_string(self.home().join("p/daemon.pid")) {
                let _ = Command::new("kill").arg(pid.trim()).status();
            }
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn wait_for(what: &str, done: impl Fn() -> bool) {
    let t = Instant::now();
    while !done() {
        assert!(t.elapsed() < Duration::from_secs(15), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The JSON type of a value, in the words the topic's tables use.
fn ty(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "list",
        Value::Object(_) => "object",
    }
}

/// `v` has each field of `want` with that type (`a|b`: either), and the topic names the field.
fn has(v: &Value, want: &[(&str, &str)], what: &str) {
    for (k, t) in want {
        let got = v.get(k).unwrap_or_else(|| panic!("{what}: no field {k:?} in {v}"));
        assert!(t.split('|').any(|t| t == "any" || t == ty(got)), "{what}: {k} is a {}, documented as {t}: {v}", ty(got));
        assert!(TOPIC.contains(&format!("`{k}`")) || TOPIC.contains(&format!("\"{k}\"")), "the contract topic does not name {what}'s field {k:?}");
    }
}

fn rfc3339(v: &Value) -> bool {
    let s = v.as_str().unwrap_or("");
    s.len() == 20 && s.ends_with('Z') && s.as_bytes()[10] == b'T'
}

const EVENT: &[(&str, &str)] = &[("v", "number"), ("seq", "number"), ("at", "string"), ("artifact", "string"), ("id", "string"), ("kind", "string"), ("value", "any"), ("by", "string"), ("via", "string"), ("rev", "number")];

/// An event has every documented field, always: for an action, for an emit with no value and
/// about no board, and for a decision. `margin events --json` prints the stored line.
#[test]
fn an_event_has_every_documented_field() {
    let t = Tmp::new("event");
    t.ok(&["new", "reviews"], BOARD);
    assert_eq!(t.ok(&["act", "reviews", "pr-7", "approve"], "").trim(), "1", "act prints the event's seq, and seq starts at 1");
    assert_eq!(t.ok(&["emit", "deploy.finished"], "").trim(), "2");
    t.ok(&["emit", "ci.report", r#"{"ok":true}"#, "--board", "reviews", "--id", "pr-7"], "");
    t.ok(&["--by", "you", "set", "reviews", "t1", "status", "done"], "");
    let evs = t.events(&["--all"]);
    assert_eq!(evs.len(), 3, "{evs:?}");
    for e in &evs {
        has(e, EVENT, "an event");
        assert_eq!(e.as_object().unwrap().len(), EVENT.len(), "a field the contract does not list: {e}");
        assert!(e["v"] == json!(1) && rfc3339(&e["at"]), "{e}");
    }
    assert_eq!(evs.iter().map(|e| e["seq"].as_u64().unwrap()).collect::<Vec<_>>(), [1, 2, 3]);
    // an action: the board's name, the block, a snapshot of it, who and how
    let a = &evs[0];
    assert_eq!((a["artifact"].as_str(), a["id"].as_str(), a["kind"].as_str(), a["by"].as_str(), a["via"].as_str(), a["rev"].as_u64()), (Some("reviews"), Some("pr-7"), Some("reviews.approve"), Some("you"), Some("act"), Some(1)));
    assert_eq!(a["value"], json!({"target": "https://example.com/pr/7", "detail": "Fails one run in five.", "cells": {"title": "Fix the flaky test"}}));
    // an emit about nothing: empty strings, a null value, rev 0
    let e = &evs[1];
    assert_eq!((e["artifact"].as_str(), e["id"].as_str(), &e["value"], e["via"].as_str(), e["rev"].as_u64()), (Some(""), Some(""), &Value::Null, Some("emit"), Some(0)));
    assert_eq!((evs[2]["artifact"].as_str(), evs[2]["id"].as_str(), &evs[2]["value"]), (Some("reviews"), Some("pr-7"), &json!({"ok": true})));
    // as printed is as stored
    let stored = std::fs::read_to_string(t.home().join("p/events.jsonl")).unwrap();
    assert_eq!(stored, t.ok(&["events", "--json", "--all"], ""));
    // a bad name is refused
    assert_eq!(t.run(&["emit", "bad name"], "").0, 1);
}

/// The passive rule from outside: a view event is left out of the default listing, a handler
/// for an action never gets it, one that names it exactly does, and it starts the daemon only
/// when some handler names it. The handler's stdin, argv, cwd and environment are as documented.
#[test]
fn a_view_event_reaches_only_a_handler_that_names_it_and_starts_no_daemon_otherwise() {
    let t = Tmp::new("handler");
    t.ok(&["new", "reviews"], BOARD);
    let out = t.root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let script = r#"cat >> "$0/$MARGIN_HANDLER.stdin"; env | grep '^MARGIN_' | sort > "$0/$MARGIN_HANDLER.env"; pwd > "$0/$MARGIN_HANDLER.cwd"; echo "$@" > "$0/$MARGIN_HANDLER.argv"; echo '{"op":"set","id":"pr-7","fields":{"detail":"more, from the handler"}}'"#;
    let outs = out.display().to_string();
    t.ok(&["handler", "add", "approve", "--on", "reviews.approve", "--on", "reviews.*", "--", "sh", "-c", script, &outs, "one", "two"], "");
    t.stop_daemon();
    // nobody names the view events: they are logged, and nothing is started for them
    for name in ["reviews.shown", "reviews.hidden", "reviews.detail"] {
        t.ok(&["emit", name, r#"{"since":null}"#, "--board", "reviews"], "");
    }
    std::thread::sleep(Duration::from_millis(400));
    assert!(!t.daemon_running(), "a passive event started the daemon with no handler asking for it");
    assert_eq!(t.events(&["--all"]).len(), 3);
    assert!(t.events(&[]).is_empty(), "the default listing leaves the passive kinds out");
    assert_eq!(t.events(&["--name", "reviews.*"]).len(), 3, "a listing by name shows them");
    // a handler that names one exactly: the next such event starts the daemon and reaches it
    t.ok(&["handler", "add", "more", "--on", "reviews.detail", "--board", "reviews", "--owns", "pr", "--", "sh", "-c", script, &outs, "one", "two"], "");
    t.stop_daemon();
    t.ok(&["emit", "reviews.shown", r#"{"since":12}"#, "--board", "reviews"], "");
    std::thread::sleep(Duration::from_millis(400));
    assert!(!t.daemon_running(), "reviews.shown is not what `more` asked for");
    let seq: u64 = t.ok(&["emit", "reviews.detail", r#"{"cells":{"title":"Fix the flaky test"}}"#, "--board", "reviews", "--id", "pr-7"], "").trim().parse().unwrap();
    wait_for("the handler to answer", || t.json(&["show", "reviews", "pr-7", "--json"])["block"]["detail"] == json!("more, from the handler"));
    // its stdin: the event line, with every documented field
    let lines: Vec<Value> = std::fs::read_to_string(out.join("more.stdin")).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 1, "only the event it named: {lines:?}");
    has(&lines[0], EVENT, "a handler's stdin line");
    assert_eq!((lines[0]["seq"].as_u64(), lines[0]["kind"].as_str(), lines[0]["id"].as_str(), lines[0]["artifact"].as_str()), (Some(seq), Some("reviews.detail"), Some("pr-7"), Some("reviews")));
    assert_eq!(lines[0], t.events(&["--all"]).into_iter().find(|e| e["seq"] == json!(seq)).unwrap(), "stdin carries the stored line");
    // its environment, argv and working directory
    let env = std::fs::read_to_string(out.join("more.env")).unwrap();
    let var = |k: &str| env.lines().find_map(|l| l.strip_prefix(&format!("{k}="))).map(String::from);
    assert_eq!(var("MARGIN_HANDLER").as_deref(), Some("more"));
    assert_eq!(var("MARGIN_PROJECT").as_deref(), Some("p"));
    assert_eq!(var("MARGIN_HOME"), Some(t.home().display().to_string()));
    assert_eq!(var("MARGIN_BIN").map(|b| Path::new(&b).exists()), Some(true));
    assert_eq!((var("MARGIN_BOARD").as_deref(), var("MARGIN_OWNS").as_deref()), (Some("reviews"), Some("pr")));
    for k in ["MARGIN_HANDLER", "MARGIN_PROJECT", "MARGIN_HOME", "MARGIN_BIN", "MARGIN_BOARD", "MARGIN_OWNS"] {
        assert!(TOPIC.contains(&format!("`{k}`")), "the contract topic does not name {k}");
    }
    assert_eq!(std::fs::read_to_string(out.join("more.argv")).unwrap().trim(), "one two");
    let cwd = std::fs::read_to_string(out.join("more.cwd")).unwrap();
    assert_eq!(std::fs::canonicalize(cwd.trim()).unwrap(), std::fs::canonicalize(t.work()).unwrap(), "it runs where it was registered");
    // the handler registered for an action, and by a board glob, was given none of the view events
    assert!(!out.join("approve.stdin").exists(), "{:?}", std::fs::read_to_string(out.join("approve.stdin")));
    let ls = t.json(&["handler", "ls", "--json"]);
    has(&ls, &[("v", "number"), ("daemon", "object"), ("handlers", "list")], "handler ls");
    let more = ls["handlers"].as_array().unwrap().iter().find(|h| h["name"] == json!("more")).unwrap();
    has(more, &[("name", "string"), ("on", "list"), ("cmd", "list"), ("paused", "bool"), ("state", "string"), ("cursor", "number"), ("pending", "number"), ("attempts", "number"), ("last_run", "string"), ("last_outcome", "string"), ("last_error", "string")], "a handler");
    assert_eq!((more["cursor"].as_u64(), more["last_outcome"].as_str(), more["board"].as_str(), more["owns"].as_str()), (Some(seq), Some("ok"), Some("reviews"), Some("pr")));
    // an action is not passive: both of approve's globs select it
    t.ok(&["act", "reviews", "pr-7", "approve"], "");
    wait_for("the action's handler", || std::fs::read_to_string(out.join("approve.env")).map(|s| s.contains("MARGIN_HANDLER=approve")).unwrap_or(false));
    let got: Value = serde_json::from_str(std::fs::read_to_string(out.join("approve.stdin")).unwrap().lines().next().unwrap()).unwrap();
    assert_eq!(got["kind"], json!("reviews.approve"));
    assert_eq!(std::fs::read_to_string(out.join("approve.env")).unwrap().lines().filter(|l| l.starts_with("MARGIN_BOARD=") || l.starts_with("MARGIN_OWNS=")).count(), 0, "no board, no MARGIN_BOARD");
    t.ok(&["handler", "rm", "more"], "");
    t.ok(&["handler", "rm", "approve"], "");
    wait_for("the daemon to leave with its last handler", || !t.daemon_running());
}

/// A failed run applies nothing, is retried, and is then passed over with `handler.failed`,
/// whose value has the documented fields.
#[test]
fn a_failing_handler_is_passed_over_with_handler_failed() {
    let t = Tmp::new("failed");
    t.ok(&["new", "reviews"], BOARD);
    t.ok(&["handler", "add", "bad", "--on", "work.go", "--board", "reviews", "--owns", "pr", "--retries", "0", "--", "sh", "-c", r#"echo '{"op":"set","id":"pr-7","fields":{"detail":"half"}}'; echo "cannot do it" >&2; exit 7"#], "");
    let seq: u64 = t.ok(&["emit", "work.go"], "").trim().parse().unwrap();
    wait_for("handler.failed", || !t.events(&["--name", "handler.failed"]).is_empty());
    let f = &t.events(&["--name", "handler.failed"])[0];
    has(f, EVENT, "handler.failed");
    assert_eq!((f["by"].as_str(), f["via"].as_str()), (Some("handler:bad"), Some("handler")));
    has(&f["value"], &[("handler", "string"), ("error", "string"), ("stderr", "string"), ("attempts", "number"), ("skipped", "object")], "handler.failed's value");
    assert_eq!((f["value"]["stderr"].as_str(), &f["value"]["skipped"]), (Some("cannot do it"), &json!({"from": seq, "to": seq, "count": 1})));
    assert_eq!(t.json(&["show", "reviews", "pr-7", "--json"])["block"]["detail"], json!("Fails one run in five."), "a failed run applies nothing");
    t.ok(&["handler", "rm", "bad"], "");
    wait_for("the daemon to leave", || !t.daemon_running());
}

/// `margin show <board> --json`, `margin show <board> <id> --json` and `margin ls --json`.
#[test]
fn a_board_and_a_block_have_the_documented_shape() {
    let t = Tmp::new("board");
    t.ok(&["new", "reviews"], BOARD);
    t.ok(&["set", "reviews", "t1", "status", "done"], "");
    let b = t.json(&["show", "reviews", "--json"]);
    has(&b, &[("doc", "object"), ("state", "object")], "show");
    assert!(b.get("v").is_none(), "the topic says this answer has no v of its own: update it");
    let doc = &b["doc"];
    has(doc, &[("v", "number"), ("id", "string"), ("title", "string"), ("kind", "string"), ("rev", "number"), ("blocks", "list"), ("updated", "string"), ("by", "string"), ("actions", "list")], "a document");
    assert!(doc["v"] == json!(1) && doc["id"] == json!("reviews") && doc["rev"] == json!(1) && rfc3339(&doc["updated"]), "{doc}");
    let table = &doc["blocks"][0];
    has(table, &[("type", "string"), ("id", "string"), ("children", "list"), ("columns", "list"), ("actions", "list")], "a table");
    let row = &table["children"][0];
    has(row, &[("type", "string"), ("id", "string"), ("target", "string"), ("detail", "string"), ("cells", "object")], "a table row");
    assert!(doc["blocks"][1].get("status").is_none(), "a todo's status lives in state.decisions");
    has(&b["state"], &[("decisions", "object")], "state");
    has(&b["state"]["decisions"]["t1"], &[("kind", "string"), ("value", "any"), ("by", "string"), ("at", "string"), ("seq", "number")], "a decision");
    assert_eq!(b["state"]["decisions"]["t1"]["value"], json!("done"));
    // one block
    let one = t.json(&["show", "reviews", "pr-7", "--json"]);
    has(&one, &[("v", "number"), ("artifact", "string"), ("rev", "number"), ("block", "object"), ("decision", "null")], "show <id>");
    assert_eq!((&one["block"], one["artifact"].as_str()), (row, Some("reviews")));
    has(&t.json(&["show", "reviews", "t1", "--json"]), &[("decision", "object")], "show <id> with a decision");
    // the listing
    let ls = t.json(&["ls", "--json"]);
    has(&ls, &[("v", "number"), ("artifacts", "list")], "ls");
    has(&ls["artifacts"][0], &[("id", "string"), ("rev", "number"), ("updated", "string"), ("title", "string"), ("kind", "string")], "ls entry");
    // a write bumps rev by one
    t.ok(&["set", "reviews", "t1", "text", "Review the whole queue"], "");
    assert_eq!(t.json(&["show", "reviews", "--json"])["doc"]["rev"], json!(2));
}

/// `margin data ls --json` and `margin data show --json`; a record comes back as written.
#[test]
fn a_dataset_listing_and_its_records_have_the_documented_shape() {
    let t = Tmp::new("data");
    t.ok(&["data", "add", "queue-depth", "--cap", "100", "--expect-every", "2m"], "");
    let entry = |t: &Tmp| t.json(&["data", "ls", "--json"])["datasets"][0].clone();
    has(&entry(&t), &[("changed", "null")], "a dataset never written");
    t.ok(&["data", "append", "queue-depth", "5"], "");
    t.ok(&["data", "append", "queue-depth", r#"{"depth":7,"at":"2026-11-11T10:00:00Z"}"#], "");
    let ls = t.json(&["data", "ls", "--json"]);
    has(&ls, &[("v", "number"), ("datasets", "list")], "data ls");
    let e = entry(&t);
    has(&e, &[("name", "string"), ("path", "string"), ("external", "bool"), ("owner", "null"), ("cap", "number"), ("expect_every", "number"), ("added", "string"), ("bytes", "number"), ("changed", "string"), ("stale", "bool")], "a dataset entry");
    assert_eq!((e["name"].as_str(), e["cap"].as_u64(), e["expect_every"].as_f64(), e["stale"].as_bool(), e["external"].as_bool()), (Some("queue-depth"), Some(100), Some(120.0), Some(false), Some(false)));
    assert!(Path::new(e["path"].as_str().unwrap()).exists());
    let show = t.json(&["data", "show", "queue-depth", "--json"]);
    has(&show, &[("v", "number"), ("name", "string"), ("path", "string"), ("external", "bool"), ("owner", "null"), ("cap", "number"), ("expect_every", "number"), ("stale", "bool"), ("bytes", "number"), ("bytes_read", "number"), ("records", "list")], "data show");
    assert_eq!(show["records"], json!([5, {"depth": 7, "at": "2026-11-11T10:00:00Z"}]), "records come back as written, oldest first");
    assert_eq!(t.json(&["data", "show", "queue-depth", "--last", "1", "--json"])["records"], json!([{"depth": 7, "at": "2026-11-11T10:00:00Z"}]));
    // a registration without the optional settings says null, not nothing
    t.ok(&["data", "add", "bare"], "");
    has(&t.json(&["data", "show", "bare", "--json"]), &[("cap", "null"), ("expect_every", "null"), ("owner", "null"), ("records", "list")], "a bare dataset");
    // a dataset write is no event
    assert!(t.events(&["--all"]).is_empty());
}

/// An owner writes only its ids, nobody else writes them, and its listing has the documented fields.
#[test]
fn an_owner_writes_its_ids_and_only_those() {
    let t = Tmp::new("owner");
    t.ok(&["new", "reviews"], BOARD);
    t.ok(&["owner", "add", "fetcher", "--board", "reviews", "--owns", "ci", "--expect-every", "10m"], "");
    let row = |id: &str| format!(r#"{{"type":"row","id":"{id}","text":"build"}}"#);
    assert_eq!(t.run(&["apply", "reviews", "--as", "fetcher"], &row("ci-1")).0, 0);
    assert_eq!(t.run(&["apply", "reviews", "--as", "fetcher"], &row("ci_2")).0, 0);
    let (code, out, err) = t.run(&["apply", "reviews", "--as", "fetcher"], &row("city"));
    assert!(code == 1 && out.is_empty() && err.contains("outside its prefix"), "a namespace, not a string prefix: {code} {err}");
    assert_eq!(t.run(&["apply", "reviews"], &row("ci-3")).0, 1, "nobody else writes the owner's ids");
    assert_eq!(t.run(&["apply", "reviews", "--as", "nobody"], &row("ci-3")).0, 3);
    assert_eq!(t.run(&["owner", "add", "second", "--board", "reviews", "--owns", "ci-1"], "").0, 1, "one id has one owner");
    assert_eq!(t.run(&["owner", "add", "late", "--board", "nope", "--owns", "x"], "").0, 3);
    let ls = t.json(&["owner", "ls", "--json"]);
    has(&ls, &[("v", "number"), ("owners", "list")], "owner ls");
    has(&ls["owners"][0], &[("name", "string"), ("board", "string"), ("owns", "string"), ("expect_every", "number"), ("added", "string"), ("last_write", "string"), ("stale", "bool")], "an owner");
    assert_eq!((ls["owners"][0]["stale"].as_bool(), ls["owners"][0]["expect_every"].as_f64()), (Some(false), Some(600.0)));
    // the owner's row still takes the person's action
    assert_eq!(t.run(&["act", "reviews", "ci-1", "refresh"], "").0, 0);
}

/// 0 done, 3 not found, 1 everything else; on failure stdout is empty and the message is on
/// stderr as `margin <command>: …`.
#[test]
fn the_exit_codes_are_as_documented() {
    let t = Tmp::new("exit");
    t.ok(&["new", "reviews"], BOARD);
    let cases: &[(&[&str], &str, i32, &str)] = &[
        // not found: 3
        (&["show", "nope"], "", 3, "no board"),
        (&["show", "nope", "--json"], "", 3, "no board"),
        (&["show", "reviews", "nope"], "", 3, "no component"),
        (&["set", "reviews", "nope", "text", "x"], "", 3, "no component"),
        (&["apply", "nope"], r#"{"op":"rm","id":"x"}"#, 3, "no board"),
        (&["act", "nope", "x", "open"], "", 3, "no board"),
        (&["emit", "x.y", "--board", "nope"], "", 3, "no board"),
        (&["handler", "rm", "nope"], "", 3, "no handler"),
        (&["handler", "logs", "nope"], "", 3, "no handler"),
        (&["owner", "rm", "nope"], "", 3, "no owner"),
        (&["data", "show", "nope"], "", 3, "no dataset"),
        (&["data", "append", "nope", "1"], "", 3, "no dataset"),
        // usage: 1
        (&["bogus"], "", 1, "unknown command"),
        (&["show"], "", 1, "missing"),
        (&["set", "reviews", "t1", "--bad", "x"], "", 1, "unknown flag"),
        (&["help", "nope"], "", 1, "no command or topic"),
        (&["events", "--no-commit"], "", 1, "--cursor"),
        // refused: 1
        (&["new", "reviews"], "", 1, "already exists"),
        (&["add", "reviews", "-", "text", "t1", "again"], "", 1, "already exists"),
        (&["apply", "reviews", "--base-rev", "99"], r#"{"op":"set","id":"t1","fields":{"text":"x"}}"#, 1, "stale base"),
        (&["act", "reviews", "pr-7", "nope"], "", 1, "offers no action"),
        (&["act", "reviews", "t1", "open"], "", 1, "no target"),
        // invalid: 1
        (&["emit", "x.y", "{not json"], "", 1, ""),
        (&["apply", "reviews"], "{not json", 1, ""),
        (&["emit", "bad name"], "", 1, "bad event name"),
    ];
    for (argv, stdin, want, msg) in cases {
        let (code, out, err) = t.run(argv, stdin);
        assert_eq!(code, *want, "margin {argv:?}: {err}");
        assert!(out.is_empty(), "margin {argv:?} failed and still printed: {out}");
        assert!(err.starts_with("margin") && err.contains(msg), "margin {argv:?}: {err}");
    }
    // nothing above wrote anything
    assert_eq!(t.json(&["show", "reviews", "--json"])["doc"]["rev"], json!(1));
    assert!(t.events(&["--all"]).is_empty());
    // 0: done, and a read that finds nothing
    assert_eq!(t.run(&["events", "--json"], "").0, 0);
    assert_eq!(t.run(&["events", "--cursor", "mine", "--json"], "").0, 0);
    assert_eq!(t.run(&["ls", "--json"], "").0, 0);
}

/// A cursor starts at the end of the log, moves as events are printed (at most once), and with
/// `--no-commit` stays until `--commit` moves it (at least once).
#[test]
fn cursors_deliver_at_most_once_or_at_least_once() {
    let t = Tmp::new("cursor");
    t.ok(&["emit", "old.news"], "");
    let seqs = |more: &[&str]| -> Vec<u64> {
        let mut argv = vec!["--cursor", "mine", "--all"];
        argv.extend(more);
        t.events(&argv).iter().map(|e| e["seq"].as_u64().unwrap()).collect()
    };
    assert!(seqs(&[]).is_empty(), "a new cursor hears what happens from now on");
    t.ok(&["emit", "a.one"], "");
    t.ok(&["emit", "a.two"], "");
    // at least once: the read does not move it
    assert_eq!(seqs(&["--no-commit"]), [2, 3]);
    assert_eq!(seqs(&["--no-commit"]), [2, 3]);
    assert_eq!(t.ok(&["events", "--cursor", "mine", "--commit", "2"], ""), "", "--commit prints nothing");
    assert_eq!(seqs(&["--no-commit"]), [3]);
    // at most once: printed is taken
    assert_eq!(seqs(&[]), [3]);
    assert!(seqs(&[]).is_empty());
    // --since replays what is kept, whatever the cursor says
    assert_eq!(seqs(&["--since", "0", "--no-commit"]), [1, 2, 3]);
    assert_eq!(t.run(&["events", "--commit", "2"], "").0, 1, "--commit goes with --cursor");
}

/// No logic in a board: there is no free-form field to put any in. A field the schema does not
/// know is refused on a block, a nested block, a table row, a column, an action, the document
/// and through every way of writing one; nothing is written. A row's cells take its table's
/// column keys only.
#[test]
fn a_board_cannot_carry_a_field_the_schema_does_not_know() {
    let t = Tmp::new("logic");
    t.ok(&["new", "reviews"], BOARD);
    let refused: &[(&[&str], &str)] = &[
        (&["apply", "reviews"], r#"{"type":"row","id":"q1","text":"x","when":"status == fail"}"#),
        (&["apply", "reviews"], r#"{"type":"text","id":"q1","text":"x","command":"ls"}"#),
        (&["apply", "reviews"], r#"{"type":"section","id":"q1","title":"s","children":[{"type":"text","id":"q2","text":"x","template":"{{a}}"}]}"#),
        (&["apply", "reviews"], r#"{"type":"table","id":"q1","columns":[{"key":"a","format":"{a}%"}],"children":[]}"#),
        (&["apply", "reviews"], r#"{"type":"table","id":"q1","columns":[{"key":"a"}],"children":[{"type":"tr","id":"q1-1","cells":{"a":1},"compute":"a*2"}]}"#),
        (&["apply", "reviews"], r#"{"type":"plot","id":"q1","data":"d","expr":"a+b"}"#),
        (&["apply", "reviews"], r#"{"type":"row","id":"q1","text":"x","actions":[{"key":"e","name":"go","cmd":"make"}]}"#),
        (&["apply", "reviews"], r#"{"op":"set","id":"t1","fields":{"script":"x"}}"#),
        (&["apply", "reviews"], r#"{"type":"script","id":"q1","run":"make"}"#),
        (&["apply", "reviews"], r#"{"title":"Reviews","blocks":[],"on_load":"fetch"}"#),
        (&["set", "reviews", "t1", "script", "x"], ""),
        (&["add", "reviews", "-", "text", "q1", "hello", "if=x"], ""),
        (&["new", "other"], r#"{"title":"x","blocks":[],"script":"x"}"#),
        (&["new", "other"], r#"{"title":"x","blocks":[{"type":"text","id":"a","text":"x","eval":"1+1"}]}"#),
    ];
    for (argv, stdin) in refused {
        let (code, out, err) = t.run(argv, stdin);
        assert!(code == 1 && out.is_empty() && err.contains("unknown"), "margin {argv:?} <<< {stdin}: exit {code}: {err}");
    }
    assert_eq!(t.json(&["show", "reviews", "--json"])["doc"]["rev"], json!(1), "nothing was written");
    assert_eq!(t.run(&["show", "other"], "").0, 3);
    // an owner's write goes through the same check
    t.ok(&["owner", "add", "fetcher", "--board", "reviews", "--owns", "ci"], "");
    assert_eq!(t.run(&["apply", "reviews", "--as", "fetcher"], r#"{"type":"row","id":"ci-1","text":"x","when":"x"}"#).0, 1);
    // a row's cells are keyed by its table's columns: no key of one's own there either
    let (code, _, err) = t.run(&["apply", "reviews"], r#"{"op":"set","id":"pr-7","fields":{"cells":{"title":"t","formula":"=a+b"}}}"#);
    assert!(code == 1 && err.contains("names no column"), "{err}");
    // only a dataset's records are free, and they come back as written
    t.ok(&["data", "add", "free"], "");
    t.ok(&["data", "append", "free", r#"{"anything":{"nested":[1,2]}}"#], "");
    assert_eq!(t.json(&["data", "show", "free", "--json"])["records"], json!([{"anything": {"nested": [1, 2]}}]));
}

/// The topic states the version it describes and the promise around it.
#[test]
fn the_topic_states_the_version_and_the_promise() {
    assert!(TOPIC.starts_with("# contract: "), "a topic's first line is `# <name>: <what>`");
    for must in ["contract version **1**", "Ignore what you do not know", "Not part of the contract", "No logic in a board", "<board>.shown", "<board>.hidden", "<board>.detail", "There is no `<board>.focus`", "At most once", "At least once"] {
        assert!(TOPIC.contains(must), "the contract topic lost: {must}");
    }
    // the endings the store reserves are the ones the topic lists
    for name in ["shown", "hidden", "detail"] {
        assert!(TOPIC.contains(&format!("| `<board>.{name}` |")), "{name}");
    }
}
