//! One writer per file, held against the real binary: a board's layout is `doc.json`, each
//! owner's or handler's blocks are a file of its own under `owned/`, and every reader gets the
//! two composed. An owner's write never touches the layout or its revision; a board written
//! before owners had files moves over on its first write and loses nothing on the way.
//!
//! These tests look inside the store on purpose (the file layout is not part of the contract:
//! `margin help contract`); what an outside tool may rely on is in `contract.rs`.

use serde_json::{json, Value};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_margin");

/// A scratch store and a working directory beside it.
struct Tmp {
    root: PathBuf,
}

impl Tmp {
    fn new(name: &str) -> Tmp {
        let root = std::env::temp_dir().join(format!("margin-owned-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("work")).unwrap();
        Tmp { root }
    }
    fn project(&self) -> PathBuf {
        self.root.join("home/p")
    }
    fn cmd(&self, argv: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(argv)
            .current_dir(self.root.join("work"))
            .env("HOME", &self.root)
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("MARGIN_HOME", self.root.join("home"))
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
        c
    }
    /// Run margin as a person's shell would: (exit code, stdout, stderr).
    fn run(&self, argv: &[&str], stdin: &str) -> (i32, String, String) {
        let mut ch = self.cmd(argv).spawn().unwrap();
        ch.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        let out = ch.wait_with_output().unwrap();
        (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
    }
    fn ok(&self, argv: &[&str], stdin: &str) -> String {
        let (code, out, err) = self.run(argv, stdin);
        assert_eq!(code, 0, "margin {argv:?}: {err}");
        out
    }
    /// `margin show <board> --json`: the document.
    fn doc(&self, board: &str) -> Value {
        let out = self.ok(&["show", board, "--json"], "");
        serde_json::from_str::<Value>(&out).unwrap_or_else(|e| panic!("{e}: {out}"))["doc"].clone()
    }
    fn file(&self, rel: &str) -> String {
        std::fs::read_to_string(self.project().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
    }
    fn json_file(&self, rel: &str) -> Value {
        serde_json::from_str(&self.file(rel)).unwrap()
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        // nothing this test started outlives it
        if let Ok(pid) = std::fs::read_to_string(self.project().join("daemon.pid")) {
            let _ = Command::new("kill").arg(pid.trim()).status();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Every id of a document, depth first.
fn ids(v: &Value) -> Vec<String> {
    fn go(bs: &Value, out: &mut Vec<String>) {
        for b in bs.as_array().into_iter().flatten() {
            out.push(b["id"].as_str().unwrap_or("").to_string());
            go(&b["children"], out);
        }
    }
    let mut out = Vec::new();
    go(&v["blocks"], &mut out);
    out
}

fn children(doc: &Value, id: &str) -> Vec<String> {
    fn find<'a>(bs: &'a Value, id: &str) -> Option<&'a Value> {
        bs.as_array()?.iter().find_map(|b| if b["id"] == json!(id) { Some(b) } else { find(&b["children"], id) })
    }
    find(&doc["blocks"], id).map(|b| b["children"].as_array().into_iter().flatten().map(|c| c["id"].as_str().unwrap().to_string()).collect()).unwrap_or_default()
}

const LAYOUT: &str = r#"{"title":"Runs","actions":[{"key":"X","name":"cancel"}],"blocks":[
  {"type":"text","id":"intro","text":"what is running"},
  {"type":"table","id":"jobs","columns":[{"key":"name","title":"Job"}]},
  {"type":"section","id":"notes","title":"Notes","children":[{"type":"text","id":"n1","text":"the layout's own"}]}]}"#;

fn tr(id: &str, name: &str) -> String {
    json!({"type": "tr", "id": id, "parent": "jobs", "target": format!("https://example.com/{id}"), "cells": {"name": name}}).to_string()
}

/// An owner's write changes its own file and nothing of the layout: `doc.json` keeps its bytes
/// and the board its rev, so a layout write made with `--base-rev` goes through after any number
/// of owner writes, and is stale only once the layout itself has changed.
#[test]
fn an_owner_write_changes_neither_doc_json_nor_the_boards_rev() {
    let t = Tmp::new("rev");
    assert_eq!(t.ok(&["new", "runs"], LAYOUT).trim(), "runs rev 1");
    t.ok(&["owner", "add", "queue", "--board", "runs", "--owns", "run"], "");
    let layout = t.file("runs/doc.json");
    for i in 0..12 {
        assert_eq!(t.ok(&["apply", "runs", "--as", "queue"], &tr(&format!("run-{}", i % 4), &format!("job {i}"))).trim(), "runs rev 1", "the rev it prints is the board's");
    }
    assert_eq!(t.file("runs/doc.json"), layout, "doc.json is byte for byte the layout writer's");
    let doc = t.doc("runs");
    assert_eq!((doc["rev"].as_u64(), doc["owned"]["owner:queue"]["rev"].as_u64(), doc["by"].as_str()), (Some(1), Some(12), Some("owner:queue")));
    assert_eq!(children(&doc, "jobs"), ["run-0", "run-1", "run-2", "run-3"]);
    assert!(t.ok(&["show", "runs"], "").contains("rev 1"), "show says the layout's rev");
    assert!(t.ok(&["ls"], "").contains("rev 1 "));
    assert!(t.ok(&["render", "runs", "--width", "80"], "").contains("job 11"), "the pane's frame has the owner's rows");
    // the assistant read rev 1 before the owner wrote twelve times
    assert_eq!(t.ok(&["set", "runs", "intro", "text=what runs now", "--base-rev", "1"], "").trim(), "runs rev 2");
    let (code, _, err) = t.run(&["set", "runs", "intro", "text=again", "--base-rev", "1"], "");
    assert!(code == 1 && err.contains("stale base") && err.contains("layout is at rev 2"), "{err}");
    // the owner's file holds its rows, each under the layout block it sits in, and nothing else
    let owned = t.json_file("runs/owned/owner.queue.json");
    assert_eq!((owned["by"].as_str(), owned["owns"].as_str(), owned["rev"].as_u64(), owned["blocks"].as_array().unwrap().len()), (Some("owner:queue"), Some("run"), Some(12), 4));
    assert!(owned["blocks"].as_array().unwrap().iter().all(|e| e["parent"] == json!("jobs") && e["block"]["type"] == json!("tr")));
    assert!(!t.file("runs/doc.json").contains("run-0"));
    // every other way to change the layout leaves the owner's file as it is
    let before = t.file("runs/owned/owner.queue.json");
    t.ok(&["add", "runs", "notes", "text", "n2", "another"], "");
    t.ok(&["purpose", "runs", "watch the queue"], "");
    t.ok(&["hints", "runs", "X  cancel"], "");
    t.ok(&["apply", "runs"], r#"{"title":"Runs","blocks":[{"type":"table","id":"jobs","columns":[{"key":"name","title":"Job"}]},{"type":"text","id":"intro","text":"moved below"}]}"#);
    assert_eq!(t.file("runs/owned/owner.queue.json"), before);
    let doc = t.doc("runs");
    assert_eq!((ids(&doc), doc["owned"]["owner:queue"]["rev"].as_u64()), (vec!["jobs".to_string(), "run-0".into(), "run-1".into(), "run-2".into(), "run-3".into(), "intro".into()], Some(12)));
    // still refused inside the prefix, in every form
    for (argv, stdin) in [(vec!["set", "runs", "run-1", "detail=mine"], ""), (vec!["rm", "runs", "run-1"], ""), (vec!["apply", "runs"], r#"{"type":"tr","id":"run-9","parent":"jobs","cells":{"name":"x"}}"#)] {
        let (code, _, err) = t.run(&argv, stdin);
        assert!(code == 1 && err.contains("belongs to owner `queue`"), "{argv:?}: {err}");
    }
}

/// An action on an owner's row carries the row's snapshot, as it did when the row was in
/// doc.json; a decision on it is still refused.
#[test]
fn an_action_on_an_owned_row_carries_its_snapshot() {
    let t = Tmp::new("act");
    t.ok(&["new", "runs"], LAYOUT);
    t.ok(&["owner", "add", "queue", "--board", "runs", "--owns", "run"], "");
    t.ok(&["apply", "runs", "--as", "queue"], &tr("run-7", "sim A"));
    assert_eq!(t.ok(&["act", "runs", "run-7", "cancel"], "").trim(), "1");
    t.ok(&["act", "runs", "run-7", "open"], "");
    let evs: Vec<Value> = t.ok(&["events", "--name", "runs.*", "--json"], "").lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!((evs[0]["kind"].as_str(), evs[0]["id"].as_str(), evs[0]["rev"].as_u64()), (Some("runs.cancel"), Some("run-7"), Some(1)));
    assert_eq!(evs[0]["value"], json!({"cells": {"name": "sim A"}, "target": "https://example.com/run-7"}));
    assert_eq!(evs[1]["kind"], json!("runs.open"));
    let (code, _, err) = t.run(&["select", "mg:pin/runs.run-7"], "");
    assert!(code == 1 && err.contains("is kept current by owner `queue`"), "{err}");
    let one: Value = serde_json::from_str(&t.ok(&["show", "runs", "run-7", "--json"], "")).unwrap();
    assert_eq!((one["rev"].as_u64(), one["block"]["cells"]["name"].as_str()), (Some(1), Some("sim A")));
}

/// Two owners, a handler-less third writer of the layout, each in its own processes, all at
/// once, many times over: every write lands, none overwrites another's, and the board's rev
/// counts the layout's writes alone.
#[test]
fn two_owners_and_a_layout_writer_in_parallel_lose_nothing() {
    let t = Tmp::new("race");
    t.ok(&["new", "runs"], LAYOUT);
    t.ok(&["owner", "add", "queue", "--board", "runs", "--owns", "run"], "");
    t.ok(&["owner", "add", "ci", "--board", "runs", "--owns", "ci"], "");
    const N: usize = 20;
    std::thread::scope(|s| {
        s.spawn(|| {
            for i in 0..N {
                t.ok(&["apply", "runs", "--as", "queue"], &tr(&format!("run-{i}"), "queued"));
            }
        });
        s.spawn(|| {
            for i in 0..N {
                t.ok(&["apply", "runs", "--as", "ci"], &json!({"type": "row", "id": format!("ci-{i}"), "text": "a check", "parent": "notes"}).to_string());
            }
        });
        s.spawn(|| {
            for i in 0..N {
                // read the rev, then write against it: an owner's write in between is not a conflict
                let rev = t.doc("runs")["rev"].as_u64().unwrap().to_string();
                t.ok(&["add", "runs", "-", "text", &format!("l-{i}"), "a layout line", "--base-rev", &rev], "");
            }
        });
    });
    let doc = t.doc("runs");
    let all = ids(&doc);
    for pre in ["run-", "ci-", "l-"] {
        assert_eq!(all.iter().filter(|id| id.starts_with(pre)).count(), N, "{pre}: {all:?}");
    }
    assert_eq!((doc["rev"].as_u64(), doc["owned"]["owner:queue"]["rev"].as_u64(), doc["owned"]["owner:ci"]["rev"].as_u64()), (Some(1 + N as u64), Some(N as u64), Some(N as u64)));
    assert_eq!(children(&doc, "jobs").len(), N);
    assert_eq!(t.ok(&["doctor"], "").lines().filter(|l| l.contains("owned") || l.contains("doc.json")).count(), 0, "nothing out of place");
}

/// A board as a margin before this change left it: one doc.json holding the layout and the
/// owner's rows together.
fn old_board(t: &Tmp) -> String {
    t.ok(&["new", "old"], "");
    let stored = json!({"v": 1, "id": "old", "title": "Old", "kind": "", "rev": 41, "updated": "2026-11-01T10:00:00Z", "by": "owner:queue", "actions": [{"key": "X", "name": "cancel"}], "blocks": [
        {"type": "text", "id": "head", "text": "the layout"},
        {"type": "table", "id": "pr", "columns": [{"key": "title"}], "children": [
            {"type": "tr", "id": "pr-1", "target": "https://example.com/1", "cells": {"title": "One"}},
            {"type": "tr", "id": "pr-2", "cells": {"title": "Two"}}]},
        {"type": "text", "id": "foot", "text": "more layout"},
        {"type": "section", "id": "jj", "title": "left by a runner that is gone", "children": [{"type": "row", "id": "jj-1", "text": "a change"}]}]});
    let text = serde_json::to_string_pretty(&stored).unwrap();
    std::fs::write(t.project().join("old/doc.json"), &text).unwrap();
    std::fs::write(t.project().join("owners.json"), r#"{"owners": [{"name": "queue", "board": "old", "owns": "pr", "added": "2026-10-01T10:00:00Z", "last_write": "2026-11-01T10:00:00Z"}]}"#).unwrap();
    text
}

/// The migration: an old single-file board reads as it always did, and the owner's first write
/// moves its blocks out of doc.json into its own file. The board on screen is the same, the
/// layout keeps its rev, and rows under a prefix nobody claims stay in the layout.
#[test]
fn an_old_single_file_board_migrates_on_the_first_owner_write() {
    let t = Tmp::new("migrate");
    let before = old_board(&t);
    let was = t.doc("old");
    assert_eq!((was["rev"].as_u64(), ids(&was)), (Some(41), ["head", "pr", "pr-1", "pr-2", "foot", "jj", "jj-1"].map(String::from).to_vec()));
    assert_eq!(t.file("old/doc.json"), before, "reading migrates nothing");
    let report = t.ok(&["doctor"], "");
    assert!(report.contains("board old: 3 blocks under the prefix of owner queue are still in doc.json") && report.contains("old/owned/owner.queue.json"), "{report}");
    // the owner writes as it always did
    assert_eq!(t.ok(&["apply", "old", "--as", "queue"], r#"{"type":"tr","id":"pr-3","parent":"pr","cells":{"title":"Three"}}"#).trim(), "old rev 41");
    let layout = t.json_file("old/doc.json");
    assert_eq!((layout["rev"].as_u64(), ids(&layout)), (Some(41), ["head", "foot", "jj", "jj-1"].map(String::from).to_vec()), "the layout: what nobody claims, at the rev it had");
    let owned = t.json_file("old/owned/owner.queue.json");
    assert_eq!((owned["rev"].as_u64(), owned["blocks"][0]["block"]["id"].as_str(), owned["blocks"][0]["after"].as_str(), owned["blocks"][0]["block"]["children"].as_array().unwrap().len()), (Some(2), Some("pr"), Some("head"), 3));
    let now = t.doc("old");
    assert_eq!((now["rev"].as_u64(), ids(&now)), (Some(41), ["head", "pr", "pr-1", "pr-2", "pr-3", "foot", "jj", "jj-1"].map(String::from).to_vec()), "the same board, one row more, the table where it was");
    assert!(!t.ok(&["doctor"], "").contains("still in doc.json"));
    // unclaimed rows are ordinary layout, and a base taken before the move still holds
    assert_eq!(t.ok(&["set", "old", "jj-1", "text=mine", "--base-rev", "41"], "").trim(), "old rev 42");
    assert_eq!(t.ok(&["act", "old", "pr-1", "open"], "").trim().parse::<u64>().is_ok(), true);
    // `doctor --fix` is the same move, for a board nobody writes
    let t2 = Tmp::new("migrate3");
    let before = old_board(&t2);
    let fixed = t2.ok(&["doctor", "--fix"], "");
    assert!(fixed.contains("fixed board old: 3 blocks under the prefix of owner queue"), "{fixed}");
    assert!(t2.file("old/doc.json") != before && t2.project().join("old/owned/owner.queue.json").exists());
    assert_eq!(ids(&t2.doc("old")), ["head", "pr", "pr-1", "pr-2", "foot", "jj", "jj-1"].map(String::from).to_vec());
    assert!(t2.ok(&["owner", "ls"], "").contains("wrote "), "its last write is the one it made, not the move");
}

/// The move writes the owner's file, then doc.json. Killed between the two, the blocks are in
/// both files: a read shows each once, from the owner's file, and the next write finishes it.
#[test]
fn a_crash_between_the_two_writes_shows_every_row_once() {
    let t = Tmp::new("crash");
    let before = old_board(&t);
    t.ok(&["apply", "old", "--as", "queue"], r#"{"op":"set","id":"pr-1","fields":{"cells":{"title":"One, newer"}}}"#);
    // the crash, simulated: doc.json as it was before the move, beside the owner's file
    std::fs::write(t.project().join("old/doc.json"), &before).unwrap();
    let doc = t.doc("old");
    assert_eq!(ids(&doc), ["head", "pr", "pr-1", "pr-2", "foot", "jj", "jj-1"].map(String::from).to_vec(), "no row twice, none missing");
    assert_eq!(doc["blocks"][1]["children"][0]["cells"]["title"], json!("One, newer"), "the owner's file is the truth");
    assert!(t.ok(&["render", "old", "--width", "80"], "").matches("One, newer").count() == 1);
    assert!(t.ok(&["doctor"], "").contains("still in doc.json"));
    // a layout write is a write: it finishes the move, and takes nothing of the owner's with it
    t.ok(&["set", "old", "head", "text=edited"], "");
    assert!(!t.file("old/doc.json").contains("pr-1") && t.file("old/owned/owner.queue.json").contains("One, newer"));
    assert_eq!(ids(&t.doc("old")), ["head", "pr", "pr-1", "pr-2", "foot", "jj", "jj-1"].map(String::from).to_vec());
}

/// `owner rm` and `handler rm` give the blocks back to the layout, where they were, as ordinary
/// content; nothing is left that nobody claims.
#[test]
fn unregistering_returns_the_blocks_to_the_layout() {
    let t = Tmp::new("rm");
    t.ok(&["new", "runs"], LAYOUT);
    t.ok(&["owner", "add", "queue", "--board", "runs", "--owns", "run"], "");
    t.ok(&["handler", "add", "fetch", "--on", "runs.refresh", "--board", "runs", "--owns", "f", "--", "true"], "");
    t.ok(&["apply", "runs", "--as", "queue"], &format!("{}\n{}", tr("run-1", "a"), tr("run-2", "b")));
    let evs = t.root.join("work/evs.jsonl");
    std::fs::write(&evs, "{\"v\":1,\"seq\":7,\"at\":\"2026-11-10T10:00:00Z\",\"artifact\":\"runs\",\"id\":\"\",\"kind\":\"runs.refresh\",\"value\":null,\"by\":\"you\",\"via\":\"emit\"}\n").unwrap();
    t.ok(&["handler", "set", "fetch", "--", "sh", "-c", r#"cat >/dev/null; echo '{"type":"row","id":"f-1","text":"fetched","parent":"notes"}'"#], "");
    let out = t.ok(&["handler", "run", "fetch", "--events", &evs.display().to_string()], "");
    assert!(out.contains("runs: handler:fetch rev 1"), "a handler's run names its own file's rev: {out}");
    let doc = t.doc("runs");
    assert_eq!((doc["rev"].as_u64(), children(&doc, "jobs"), children(&doc, "notes")), (Some(1), vec!["run-1".to_string(), "run-2".into()], vec!["n1".to_string(), "f-1".into()]));
    assert!(t.project().join("runs/owned/handler.fetch.json").exists());
    t.ok(&["owner", "rm", "queue"], "");
    t.ok(&["handler", "rm", "fetch"], "");
    let doc = t.doc("runs");
    assert!(doc.get("owned").is_none(), "nothing is owned: {doc}");
    assert_eq!((children(&doc, "jobs"), children(&doc, "notes")), (vec!["run-1".to_string(), "run-2".into()], vec!["n1".to_string(), "f-1".into()]), "where they were");
    assert!(t.file("runs/doc.json").contains("run-1") && t.file("runs/doc.json").contains("f-1"));
    assert_eq!(std::fs::read_dir(t.project().join("runs/owned")).map(|d| d.count()).unwrap_or(0), 0, "no file is left that nobody claims");
    // ordinary content now
    t.ok(&["set", "runs", "run-1", "detail=mine"], "");
    t.ok(&["rm", "runs", "f-1"], "");
}

/// The layout writer removes the container an owner fills: the owner's rows are kept in its
/// file and not drawn, `margin doctor` says so, a write under the missing container is refused
/// by name, and the rows are back when the container is.
#[test]
fn rows_whose_container_left_the_layout_are_kept_and_reported() {
    let t = Tmp::new("hidden");
    t.ok(&["new", "runs"], LAYOUT);
    t.ok(&["owner", "add", "queue", "--board", "runs", "--owns", "run"], "");
    let (code, _, err) = t.run(&["apply", "runs", "--as", "queue"], &tr("run-1", "a").replace("\"jobs\"", "\"nowhere\""));
    assert!(code == 1 && err.contains("owner queue writes under \"nowhere\"") && err.contains("layout of runs"), "{err}");
    t.ok(&["apply", "runs", "--as", "queue"], &format!("{}\n{}", tr("run-1", "a"), tr("run-2", "b")));
    assert_eq!(t.ok(&["rm", "runs", "jobs"], "").trim(), "runs rev 2");
    assert!(!ids(&t.doc("runs")).iter().any(|i| i.starts_with("run-")), "not drawn");
    assert!(t.file("runs/owned/owner.queue.json").contains("run-1"), "and not lost");
    let report = t.ok(&["doctor"], "");
    assert!(report.contains("note  board runs: 2 blocks of owner queue (run-1 run-2) are not drawn: the container \"jobs\" is no longer in the layout") && report.contains("runs/owned/owner.queue.json"), "{report}");
    let (code, _, err) = t.run(&["apply", "runs", "--as", "queue"], &tr("run-3", "c"));
    assert!(code == 1 && err.contains("\"jobs\""), "{err}");
    t.ok(&["apply", "runs"], r#"{"type":"table","id":"jobs","columns":[{"key":"name","title":"Job"}]}"#);
    assert_eq!(children(&t.doc("runs"), "jobs"), ["run-1", "run-2"]);
    assert!(!t.ok(&["doctor"], "").contains("not drawn"));
}

/// `margin rm <board> --yes` archives the board with its owned files and unregisters the
/// owners and handlers that claimed a prefix on it.
#[test]
fn removing_a_board_unregisters_what_claimed_it() {
    let t = Tmp::new("rmboard");
    t.ok(&["new", "runs"], LAYOUT);
    t.ok(&["new", "keep"], "");
    t.ok(&["owner", "add", "queue", "--board", "runs", "--owns", "run"], "");
    t.ok(&["owner", "add", "other", "--board", "keep", "--owns", "o"], "");
    t.ok(&["handler", "add", "fetch", "--on", "runs.refresh", "--board", "runs", "--owns", "f", "--", "true"], "");
    t.ok(&["apply", "runs", "--as", "queue"], &tr("run-1", "a"));
    t.ok(&["rm", "runs", "--yes"], "");
    let owners: Value = serde_json::from_str(&t.ok(&["owner", "ls", "--json"], "")).unwrap();
    assert_eq!(owners["owners"].as_array().unwrap().iter().map(|o| o["name"].as_str().unwrap()).collect::<Vec<_>>(), ["other"]);
    let handlers: Value = serde_json::from_str(&t.ok(&["handler", "ls", "--json"], "")).unwrap();
    assert!(handlers["handlers"].as_array().unwrap().is_empty(), "{handlers}");
    let kept = std::fs::read_dir(t.project().join(".archive")).unwrap().flatten().next().unwrap().path();
    assert!(kept.join("owned/owner.queue.json").exists() && kept.join("unregistered.json").exists());
    // the name is free again, with nothing owned
    t.ok(&["new", "runs"], LAYOUT);
    assert_eq!(t.ok(&["apply", "runs"], r#"{"type":"row","id":"run-1","text":"anyone's now"}"#).trim(), "runs rev 2");
}

/// Every example still runs end to end: the board is created with its rows in it, its handler
/// is registered on the prefix (the rows move into the handler's file, in place), the script's
/// output applied as the handler changes nothing, and the board on screen is the one the
/// example shipped.
#[test]
fn every_example_keeps_its_board_when_its_handler_takes_its_rows() {
    for (name, owns) in [("reviews", "pr"), ("services", "svc"), ("tracker", "wk")] {
        let t = Tmp::new(&format!("example-{name}"));
        let board = t.ok(&["example", name], "");
        let out = t.ok(&["example", name, "--script"], "");
        assert!(out.contains("jq"), "{name}: the script");
        assert_eq!(t.ok(&["new", name], &board).trim(), format!("{name} rev 1"));
        let shipped = ids(&t.doc(name));
        let frame = t.ok(&["render", name, "--width", "100", "--height", "40"], "");
        let fetch_out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../skill/examples/{name}.fetch.out"));
        let sh = format!("cat >/dev/null; cat {}", fetch_out.display());
        t.ok(&["handler", "add", &format!("{name}-refresh"), "--on", &format!("{name}.refresh"), "--board", name, "--owns", owns, "--", "sh", "-c", &sh], "");
        let file = format!("{name}/owned/handler.{name}-refresh.json");
        assert!(t.project().join(&file).exists(), "{name}: registering takes the rows under the prefix");
        assert!(!ids(&t.json_file(&format!("{name}/doc.json"))).iter().any(|i| i == owns || i.starts_with(&format!("{owns}-"))), "{name}: none of them is left in the layout");
        let doc = t.doc(name);
        assert_eq!((ids(&doc), doc["rev"].as_u64()), (shipped.clone(), Some(1)), "{name}: the same board");
        assert_eq!(t.ok(&["render", name, "--width", "100", "--height", "40"], "").lines().skip(1).collect::<Vec<_>>(), frame.lines().skip(1).collect::<Vec<_>>(), "{name}: the same frame");
        let evs = t.root.join("work/evs.jsonl");
        std::fs::write(&evs, format!("{{\"v\":1,\"seq\":7,\"at\":\"2026-11-10T10:00:00Z\",\"artifact\":\"{name}\",\"id\":\"\",\"kind\":\"{name}.refresh\",\"value\":null,\"by\":\"you\",\"via\":\"emit\"}}\n")).unwrap();
        let before = t.file(&file);
        let ran = t.ok(&["handler", "run", &format!("{name}-refresh"), "--events", &evs.display().to_string()], "");
        assert!(ran.contains(&format!("{name} unchanged")), "{name}: the script prints what the board holds: {ran}");
        assert_eq!((t.file(&file), ids(&t.doc(name))), (before, shipped), "{name}");
        let report = t.ok(&["doctor"], "");
        assert!(!report.contains("owned/") && !report.contains("doc.json"), "{name}: {report}");
    }
}
