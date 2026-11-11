//! The CLI from the outside: the global flags on either side of the command, and every command
//! acting on the store `--home`/`--project` name, never on the one the environment would give.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_margin");

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("margin-cli-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Run margin with an environment that points at a decoy store: anything a command writes there
/// is a command that ignored its flags.
fn margin(decoy: &Path, argv: &[String], stdin: &str) -> (bool, String) {
    let mut c = Command::new(BIN);
    c.args(argv)
        .current_dir(decoy)
        .env("HOME", decoy)
        .env("XDG_STATE_HOME", decoy.join("state"))
        .env("XDG_CONFIG_HOME", decoy.join("config"))
        .env("MARGIN_HOST", "plain")
        .env("CLAUDE_CODE_SESSION_ID", "walk")
        .env_remove("MARGIN_HOME")
        .env_remove("MARGIN_PROJECT")
        .env_remove("ZELLIJ")
        .env_remove("ZELLIJ_SESSION_NAME")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut ch = c.spawn().unwrap();
    ch.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    // drain both pipes while it runs: a pipe may hold as little as a page, and `margin skill`
    // prints more than that
    let drain = |mut r: Box<dyn std::io::Read + Send>| {
        std::thread::spawn(move || {
            let mut b = Vec::new();
            let _ = r.read_to_end(&mut b);
            String::from_utf8_lossy(&b).into_owned()
        })
    };
    let out = drain(Box::new(ch.stdout.take().unwrap()));
    let err = drain(Box::new(ch.stderr.take().unwrap()));
    let start = Instant::now();
    let status = loop {
        if let Some(s) = ch.try_wait().unwrap() {
            break s;
        }
        if start.elapsed() > Duration::from_secs(30) {
            let _ = ch.kill();
            panic!("margin {argv:?} did not finish");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    (status.success(), format!("{}{}", out.join().unwrap(), err.join().unwrap()))
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

/// The command names in main.rs's table, read from its source: a command added there without a
/// line in the walk below fails the test.
fn command_table() -> Vec<String> {
    let src = include_str!("../src/main.rs");
    src.split("Cmd { name: \"").skip(1).map(|c| c.split('"').next().unwrap().to_string()).collect()
}

const DOC: &str = r#"{"title":"walk","actions":[{"key":"p","name":"poke"}],"blocks":[{"type":"section","id":"s","title":"S","children":[
  {"type":"todo","id":"t1","text":"one"},{"type":"todo","id":"t3","text":"three"},
  {"type":"question","id":"q1","text":"Which?","options":["x","y"]}]}]}"#;

/// Every command, once with the global flags before it and once after, against a temp store.
/// The third field is stdin; `true` in the fourth means the command must succeed (the others
/// need a tool or a terminal this test doesn't have, and are only held to the decoy check).
#[test]
fn every_command_honors_home_and_project_on_either_side() {
    let decoy = temp("decoy");
    let home = temp("home");
    let hook_in = format!(r#"{{"cwd":"{}","session_id":"walk"}}"#, decoy.display());
    let claude_dir = decoy.join("claude").display().to_string();
    let walk: Vec<(&str, Vec<String>, &str, bool)> = vec![
        ("new", s(&["a", "--title", "walk"]), "", true),
        ("new", s(&["b"]), r#"[{"type":"todo","id":"x","text":"x"}]"#, true),
        ("apply", s(&["a"]), DOC, true),
        ("apply", s(&["b"]), r#"{"blocks":[{"type":"todo","id":"y","text":"y"}]}"#, true),
        ("schema", s(&[]), "", true),
        ("add", s(&["a", "s", "todo", "t2", "two"]), "", true),
        ("set", s(&["a", "t2", "note=n"]), "", true),
        ("rm", s(&["a", "t2"]), "", true),
        ("move", s(&["a", "t1", "--after", "t3"]), "", true),
        ("title", s(&["a", "Walk"]), "", true),
        ("hints", s(&["a", "k  what it does"]), "", true),
        ("purpose", s(&["a", "walk the table"]), "", true),
        ("apply", s(&["a"]), r#"{"op":"set","id":"t3","fields":{"note":"n"}}"#, true),
        ("ask", s(&["a", "q2", "Again?", "x", "y"]), "", true),
        ("propose", s(&["a", "t1", "tick", "done"]), "", true),
        ("diff", s(&["a", "d1", "--old", "/nonexistent-a", "--new", "/nonexistent-b"]), "", false),
        ("image", s(&["a", "i1", "/nonexistent.png"]), "", false),
        ("show", s(&["a"]), "", true),
        ("ls", s(&[]), "", true),
        ("gc", s(&[]), "", true),
        ("events", s(&["--all"]), "", true),
        ("emit", s(&["walk.done", r#"{"n":1}"#, "--board", "a"]), "", true),
        ("act", s(&["a", "t1", "poke"]), "", true),
        ("hook", s(&["prompt"]), &hook_in, true),
        ("doc", s(&["a", "notes"]), "# Notes\n\nA line.\n", true),
        ("toast", s(&["a", "hello"]), "", true),
        ("stop", s(&["a"]), "", true),
        ("link", s(&["a"]), "", true),
        ("unlink", s(&[]), "", true),
        ("open", s(&["a"]), "", true),
        ("tab", s(&["a", "--pane", "tty"]), "", true),
        ("close", s(&["a"]), "", true),
        ("panes", s(&["a"]), "", true),
        ("render", s(&["a"]), "", true),
        ("select", s(&["mg:todo/a.t3=next"]), "", true),
        ("session", s(&[]), "", false),
        ("daemon", s(&[]), "", true),
        ("demo", s(&[]), "", true),
        ("feed", s(&["ls"]), "", true),
        ("feed", s(&["add", "f", "--every", "1h", "--artifact", "a", "--owns", "fd", "--", "true"]), "", true),
        ("feed", s(&["rm", "f"]), "", true),
        ("handler", s(&["add", "h", "--on", "walk.*", "--", "true"]), "", true),
        ("handler", s(&["ls"]), "", true),
        ("handler", s(&["rm", "h"]), "", true),
        ("handle", s(&["open"]), "", true),
        ("owner", s(&["add", "w", "--board", "a", "--owns", "ow"]), "", true),
        ("owner", s(&["ls"]), "", true),
        ("owner", s(&["rm", "w"]), "", true),
        ("data", s(&["add", "nums", "--cap", "100"]), "", true),
        ("data", s(&["append", "nums", "3.5"]), "", true),
        ("data", s(&["put", "nums"]), "[1, 2, {\"v\": 3}]", true),
        ("data", s(&["show", "nums"]), "", true),
        ("data", s(&["ls"]), "", true),
        ("data", s(&["rm", "nums"]), "", true),
        ("daemon", s(&["status"]), "", true),
        ("doctor", s(&[]), "", true),
        ("init", s(&["--dry-run", "--claude-dir", &claude_dir]), "", true),
        ("example", s(&[]), "", true),
        ("skill", s(&[]), "", true),
        ("rm", s(&["b", "--yes"]), "", true),
    ];
    // `view` is the live pane: it needs a terminal
    let skipped = ["view"];
    for name in command_table() {
        assert!(walk.iter().any(|w| w.0 == name) || skipped.contains(&name.as_str()), "margin {name} is not in the walk");
    }
    for (side, project) in [("before", "pb"), ("after", "pa")] {
        let globals = s(&["--home", &home.display().to_string(), "--project", project]);
        for (cmd, args, stdin, must) in &walk {
            let cut = args.iter().position(|a| a == "--").unwrap_or(args.len());
            let mut argv: Vec<String> = Vec::new();
            if side == "before" {
                argv.extend(globals.iter().cloned());
            }
            argv.push(cmd.to_string());
            argv.extend(args[..cut].iter().cloned());
            if side == "after" {
                argv.extend(globals.iter().cloned());
            }
            argv.extend(args[cut..].iter().cloned());
            let (ok, out) = margin(&decoy, &argv, stdin);
            assert!(ok || !must, "flags {side}: margin {argv:?} failed: {out}");
            assert!(!out.contains("unknown command") && !out.contains("unknown flag"), "flags {side}: margin {argv:?}: {out}");
            assert!(!decoy.join("state").exists(), "flags {side}: margin {cmd} wrote to the environment's store, not --home");
        }
        // what the walk did is in the store the flags named
        let dir = home.join(project);
        assert!(dir.join("a/doc.json").exists() && !dir.join("b").exists() && dir.join(".archive").exists(), "flags {side}");
        let state = std::fs::read_to_string(dir.join("a/state.json")).unwrap();
        assert!(state.contains("\"t3\"") && state.contains("next"), "flags {side}: select's decision landed: {state}");
        assert!(dir.join("plan/doc.json").exists(), "flags {side}: demo");
        // the daemon `feed add` started leaves once its feed is gone
        let t = Instant::now();
        while dir.join("daemon.pid").exists() && t.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(!dir.join("daemon.pid").exists(), "flags {side}: the daemon is still up");
    }
    let (ok, out) = margin(&decoy, &s(&["--home"]), "");
    assert!(!ok && out.contains("--home needs a value"), "{out}");
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&decoy);
}

/// `margin ls | head -1`: a reader that leaves early ends the command without a panic message.
#[test]
fn a_closed_stdout_ends_the_command_quietly() {
    let decoy = temp("pipe-decoy");
    let home = temp("pipe-home");
    let globals = s(&["--home", &home.display().to_string(), "--project", "p"]);
    // one artifact through the CLI, then enough copies of it that the listing is past a pipe's 64 KB
    let mut argv = globals.clone();
    argv.extend(s(&["new", "art0"]));
    let (ok, out) = margin(&decoy, &argv, r#"{"title":"an artifact with a title long enough to fill a pipe","blocks":[]}"#);
    assert!(ok, "{out}");
    let src = std::fs::read_to_string(home.join("p/art0/doc.json")).unwrap();
    for j in 1..1500 {
        let d = home.join("p").join(format!("art{j}"));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("doc.json"), src.replace("\"art0\"", &format!("\"art{j}\""))).unwrap();
    }
    let mut c = Command::new(BIN);
    c.args(&globals).arg("ls").env("XDG_CONFIG_HOME", decoy.join("config")).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut ch = c.spawn().unwrap();
    // close the read end before the command has written everything
    drop(ch.stdout.take());
    let o = ch.wait_with_output().unwrap();
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!err.contains("panicked") && !err.contains("Broken pipe"), "{err}");
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(o.status.signal(), Some(13), "ended by SIGPIPE, like any Unix tool: {:?} {err}", o.status);
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&decoy);
}

/// Run margin against `home` (project `p`); returns (succeeded, stdout + stderr).
fn at(decoy: &Path, home: &Path, argv: &[&str], stdin: &str) -> (bool, String) {
    let mut v = s(&["--home", &home.display().to_string(), "--project", "p"]);
    v.extend(s(argv));
    margin(decoy, &v, stdin)
}

/// The artifacts a store's project holds, by directory (anything a command created shows here).
fn artifacts(home: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(home.join("p"))
        .map(|rd| rd.flatten().filter(|e| e.path().join("doc.json").exists()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

/// Creating is `margin new` and nothing else: it makes an artifact once, and every command that
/// writes to an artifact fails on a name that has none, with the one message that names the fix
/// and the artifact that was probably meant. A mistyped name creates nothing.
#[test]
fn only_new_creates_and_every_write_refuses_a_missing_artifact() {
    let decoy = temp("new-decoy");
    let home = temp("new-home");
    let run = |argv: &[&str], stdin: &str| at(&decoy, &home, argv, stdin);
    // nothing exists yet: no near name to suggest
    let (ok, out) = run(&["add", "plan", "-", "todo", "t1", "one"], "");
    assert!(!ok && out.trim() == "margin add: no board \"plan\" in project p: create it with margin new plan", "{out}");
    // new: empty (stdin closed at once), then refused a second time, with or without a document
    let (ok, out) = run(&["new", "plan", "--title", "Plan", "--purpose", "walk it"], "");
    assert!(ok && out.trim() == "plan rev 1", "{out}");
    for stdin in ["", DOC] {
        let (ok, out) = run(&["new", "plan"], stdin);
        assert!(!ok && out.trim() == "margin new: plan already exists (rev 1); change it with margin apply/add/set, or remove it with margin rm plan --yes", "{out}");
    }
    let (ok, out) = run(&["show", "plan"], "");
    assert!(ok && out.contains("plan \"Plan\" rev 1") && out.contains("purpose: walk it"), "{out}");
    // a whole-document apply to it is an update; so is put
    let (ok, out) = run(&["apply", "plan"], DOC);
    assert!(ok && out.trim() == "plan rev 2", "{out}");
    let (ok, out) = run(&["apply", "plan"], r#"{"blocks":[]}"#);
    assert!(ok, "{out}");
    let png = decoy.join("x.png");
    let want = "no board \"paln\" in project p: create it with margin new paln (did you mean \"plan\"?)";
    let png_s = png.display().to_string();
    let walk: Vec<(Vec<&str>, &str)> = vec![
        (vec!["apply", "paln"], DOC),
        (vec!["apply", "paln"], r#"{"type":"todo","id":"t9","text":"nine"}"#),
        (vec!["apply", "paln"], r#"{"op":"set","id":"t1","fields":{"note":"n"}}"#),
        (vec!["add", "paln", "s", "todo", "t9", "nine"], ""),
        (vec!["set", "paln", "t1", "note=n"], ""),
        (vec!["rm", "paln", "t1"], ""),
        (vec!["move", "paln", "t1", "--after", "t3"], ""),
        (vec!["ask", "paln", "q9", "Again?", "x", "y"], ""),
        (vec!["doc", "paln", "notes"], "# Notes\n\nA line.\n"),
        (vec!["title", "paln", "Plan"], ""),
        (vec!["purpose", "paln", "why"], ""),
        (vec!["hints", "paln", "k  key"], ""),
        (vec!["propose", "paln", "t1", "tick", "done"], ""),
        (vec!["diff", "paln", "d1", "--old", "/nonexistent-a", "--new", "/nonexistent-b"], ""),
        (vec!["image", "paln", "i1", &png_s], ""),
        (vec!["toast", "paln", "hello"], ""),
        (vec!["feed", "add", "f", "--every", "1h", "--artifact", "paln", "--", "true"], ""),
    ];
    for (argv, stdin) in &walk {
        let (ok, out) = run(argv, stdin);
        assert!(!ok && out.trim() == format!("margin {}: {want}", argv[0]), "margin {argv:?}: {out}");
        assert_eq!(artifacts(&home), ["plan"], "margin {argv:?} created something");
    }
    let (_, out) = run(&["feed", "ls"], "");
    assert!(out.contains("no feeds in project p"), "no feed registered: {out}");
    let (_, out) = run(&["show", "plan"], "");
    assert!(out.contains("rev 3") || out.contains("rev 2"), "plan is as the last apply left it: {out}");
    // a name further than two edits away gets no suggestion
    let (ok, out) = run(&["add", "scratch", "-", "todo", "t1", "x"], "");
    assert!(!ok && out.trim().ends_with("create it with margin new scratch"), "{out}");
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&decoy);
}

/// A registered feed whose artifact is gone fails its run with the same message, in the
/// foreground and in `feed ls`' terms, and never writes the artifact back into being.
#[test]
fn a_feed_run_against_a_missing_artifact_fails_without_creating_it() {
    let decoy = temp("feedrun-decoy");
    let home = temp("feedrun-home");
    let run = |argv: &[&str], stdin: &str| at(&decoy, &home, argv, stdin);
    assert!(run(&["new", "board"], "").0);
    // registered paused-free but with a long interval; the daemon it starts leaves with the feed
    let (ok, out) = run(&["feed", "add", "f", "--every", "1h", "--board", "board", "--owns", "fd", "--", "printf", r#"{"type":"text","id":"fd","text":"hi"}\n"#], "");
    assert!(ok, "{out}");
    let (ok, out) = run(&["feed", "run", "f"], "");
    assert!(ok && (out.contains("board rev") || out.contains("board unchanged")), "{out}");
    // the artifact goes (by hand: `margin rm board --yes` would unregister the feed with it)
    std::fs::rename(home.join("p/board"), home.join("gone")).unwrap();
    let (ok, out) = run(&["feed", "run", "f"], "");
    assert!(!ok && out.contains("margin feed: no board \"board\" in project p: create it with margin new board"), "{out}");
    assert!(artifacts(&home).is_empty(), "the run created {:?}", artifacts(&home));
    let (ok, out) = run(&["feed", "rm", "f"], "");
    assert!(ok, "{out}");
    let t = Instant::now();
    while home.join("p/daemon.pid").exists() && t.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(artifacts(&home).is_empty() && !home.join("p/daemon.pid").exists());
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&decoy);
}

/// Each bundled example, by the steps `margin example <name>` prints: piped into `margin new`
/// it creates the board, its feed's stored output applies as an update that changes nothing,
/// the feed registers on it, and a second pipe into `new` is refused.
#[test]
fn the_examples_run_end_to_end_through_new() {
    let decoy = temp("ex-decoy");
    let home = temp("ex-home");
    let run = |argv: &[&str], stdin: &str| at(&decoy, &home, argv, stdin);
    let (ok, list) = run(&["example"], "");
    assert!(ok && list.contains("margin example reviews | margin new reviews"), "{list}");
    for name in ["reviews", "services", "tracker"] {
        // stdout alone is what a pipe carries; the steps go to stderr
        let o = Command::new(BIN).args(["example", name]).env("XDG_CONFIG_HOME", decoy.join("config")).output().unwrap();
        let (json, steps) = (String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned());
        assert!(steps.contains(&format!("margin example {name} | margin new {name}")) && !steps.contains("| margin apply {name}\n#        margin render"), "{steps}");
        // step 3 before step 1: an update of a board that was never created is refused
        let feed_out = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../skill/examples/{name}.feed.out"))).unwrap();
        let (ok, out) = run(&["apply", name], &feed_out);
        assert!(!ok && out.contains(&format!("create it with margin new {name}")), "{out}");
        let (ok, out) = run(&["new", name], &json);
        assert!(ok && out.trim() == format!("{name} rev 1"), "{name}: {out}");
        let (ok, out) = run(&["render", name, "--width", "100", "--height", "40"], "");
        assert!(ok && !out.trim().is_empty(), "{name}: {out}");
        // the services board's plot reads a dataset: the steps register it and append to it
        // from a loop. Run as written (the loop's body three times), the plot draws
        if name == "services" {
            assert!(out.contains("no dataset \"latency\" (margin data add latency)"), "{out}");
            let lines: Vec<&str> = steps.lines().map(|l| l.trim_start_matches('#').trim()).collect();
            let at = lines.iter().position(|l| l.starts_with("margin data add latency")).expect("the dataset step");
            let words: Vec<&str> = lines[at].split_whitespace().skip(1).collect();
            assert!(run(&words, "").0, "{}", lines[at]);
            assert!(lines[at + 1].starts_with("while sleep") && lines[at + 3] == "done", "{:?}", &lines[at..at + 4]);
            let script = format!("margin() {{ {BIN:?} --home {:?} --project p \"$@\"; }}\nfor i in 1 2 3; do\n{}\ndone\n", home.display().to_string(), lines[at + 2]);
            let o = Command::new("bash").arg("-c").arg(&script).current_dir(&decoy).output().unwrap();
            assert!(o.status.success(), "{script}\n{}", String::from_utf8_lossy(&o.stderr));
            let (_, shown) = run(&["data", "show", "latency", "--json"], "");
            let v: serde_json::Value = serde_json::from_str(shown.trim()).unwrap_or_else(|e| panic!("{e}: {shown}"));
            assert!(v["records"].as_array().unwrap().len() == 3 && v["records"][2]["p95"].as_u64().unwrap() >= 120 && v["records"][2]["t"].as_u64().unwrap() > 1_700_000_000, "{shown}");
            let (ok, out) = run(&["render", name, "--width", "100", "--height", "40"], "");
            assert!(ok && !out.contains("no dataset") && out.contains("● p50") && out.contains("● p95") && out.contains("now"), "{out}");
            assert!(run(&["show", name, "--json"], "").1.contains("\"rev\": 1,"), "appends make no revision");
        }
        let (ok, out) = run(&["apply", name], &feed_out);
        assert!(ok && out.trim() == format!("{name} rev 1"), "{name}: the feed's output changes nothing: {out}");
        let (ok, out) = run(&["new", name], &json);
        assert!(!ok && out.contains(&format!("{name} already exists (rev 1)")), "{name}: {out}");
        // the feed line of step 4, as written in the steps (paused at once: nothing needs to run)
        let line = steps.lines().find(|l| l.contains("margin feed add")).unwrap().trim_start_matches(['#', ' ']).to_string();
        let words: Vec<&str> = line.split_whitespace().skip(1).collect();
        let (ok, out) = run(&words, "");
        assert!(ok && out.contains(&format!("→ {name}")), "{name}: {line}: {out}");
        assert!(run(&["feed", "rm", name], "").0);
    }
    assert_eq!(artifacts(&home), ["reviews", "services", "tracker"]);
    let t = Instant::now();
    while home.join("p/daemon.pid").exists() && t.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&decoy);
}

/// The skill's worked example (the fenced block under "The turn loop"), run as written against
/// a temp store: every line succeeds, in order, starting from a store with nothing in it.
#[test]
fn the_skills_worked_example_runs_end_to_end() {
    let decoy = temp("skill-decoy");
    let home = temp("skill-home");
    let (ok, skill) = at(&decoy, &home, &["skill"], "");
    assert!(ok);
    let block = skill.split("```fish\n").nth(1).expect("the turn loop's block").split("```").next().unwrap().to_string();
    assert!(block.contains("margin new plan <<'EOF'") && !block.contains("margin apply plan"), "the example creates with new: {block}");
    // `margin` and `cargo` as the block calls them: this build, and a test run that passes
    let bin = decoy.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(BIN, bin.join("margin")).unwrap();
    std::fs::write(bin.join("cargo"), "#!/bin/sh\nexit 0\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(bin.join("cargo"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    let o = Command::new("sh")
        .args(["-e", "-c", &block])
        .current_dir(&decoy)
        .env("PATH", path)
        .env("HOME", &decoy)
        .env("XDG_CONFIG_HOME", decoy.join("config"))
        .env("MARGIN_HOME", &home)
        .env("MARGIN_PROJECT", "p")
        .env("MARGIN_HOST", "plain")
        .env_remove("ZELLIJ")
        .env_remove("ZELLIJ_SESSION_NAME")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let out = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    assert!(o.status.success(), "the skill's example failed: {out}");
    assert_eq!(artifacts(&home), ["plan"]);
    for must in ["plan \"fix the flake\"", "todo t1 [done]", "todo t2 [doing]", "todo t3 ", "question Q1 ", "claim A1 ", "decided D3 "] {
        assert!(out.contains(must), "{must}: {out}");
    }
    // run again from the top, the create is refused and nothing after it runs
    let o = Command::new("sh").args(["-e", "-c", &block]).current_dir(&decoy).env("PATH", format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default())).env("XDG_CONFIG_HOME", decoy.join("config")).env("MARGIN_HOME", &home).env("MARGIN_PROJECT", "p").env("MARGIN_HOST", "plain").stdin(Stdio::null()).output().unwrap();
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("plan already exists"));
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&decoy);
}

/// A command for margin with nothing of Claude Code (or of this machine's margin setup) in its
/// environment: PATH, a decoy HOME, and the store on the command line. What cron or a shell with
/// no AI in it would run.
fn bare(decoy: &Path, home: &Path, argv: &[&str]) -> Command {
    let mut c = Command::new(BIN);
    c.args(["--home", &home.display().to_string(), "--project", "p"])
        .args(argv)
        .current_dir(decoy)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", decoy)
        .env("MARGIN_HOST", "plain")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c
}

/// Run one to its end: (succeeded, stdout, stderr).
fn plain(decoy: &Path, home: &Path, argv: &[&str], stdin: &str) -> (bool, String, String) {
    let mut ch = bare(decoy, home, argv).spawn().unwrap();
    ch.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let o = ch.wait_with_output().unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned())
}

fn json_lines(out: &str) -> Vec<serde_json::Value> {
    out.lines().map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}"))).collect()
}

/// `margin emit`: a named event from anywhere, with the value as JSON (an argument or stdin),
/// the seq on stdout, and a refusal (exit 1, nothing logged) for a value that is not JSON, a bad
/// name or a board that isn't there.
#[test]
fn emit_appends_a_named_event_and_prints_its_seq() {
    let (decoy, home) = (temp("emit-decoy"), temp("emit-home"));
    let run = |argv: &[&str], stdin: &str| plain(&decoy, &home, argv, stdin);
    assert!(run(&["new", "ci"], "").0);
    let (ok, out, _) = run(&["emit", "deploy.finished", r#"{"sha":"abc","ok":true}"#], "");
    assert!(ok && out == "1\n", "{out}");
    let (ok, out, _) = run(&["emit", "ci.report", "-", "--board", "ci", "--id", "x"], "[1, 2]\n");
    assert!(ok && out == "2\n", "{out}");
    let (ok, out, _) = run(&["emit", "bare"], "");
    assert!(ok && out == "3\n", "{out}");
    for (argv, stdin, why) in [
        (vec!["emit", "x", "not json"], "", "the value is not JSON"),
        (vec!["emit", "x", "-"], "{", "stdin is not one JSON value"),
        (vec!["emit", "two words"], "", "bad event name"),
        (vec!["emit", "x", "1", "--board", "nope"], "", "no board \"nope\""),
        (vec!["emit", "job.failed"], "", "the runner's own"),
        (vec!["emit"], "", "missing <name>"),
    ] {
        let (ok, out, err) = run(&argv, stdin);
        assert!(!ok && out.is_empty() && err.starts_with("margin emit: ") && err.contains(why), "{argv:?}: {out}{err}");
    }
    let (ok, out, _) = run(&["events", "--name", "*", "--json"], "");
    let evs = json_lines(&out);
    assert!(ok && evs.len() == 3, "{out}");
    assert_eq!(evs[0], serde_json::json!({"v": 1, "seq": 1, "at": evs[0]["at"], "artifact": "", "id": "", "kind": "deploy.finished", "value": {"sha": "abc", "ok": true}, "by": "you", "via": "emit", "rev": 0}));
    assert_eq!((&evs[1]["artifact"], &evs[1]["id"], &evs[1]["value"], &evs[1]["rev"]), (&serde_json::json!("ci"), &serde_json::json!("x"), &serde_json::json!([1, 2]), &serde_json::json!(1)));
    assert!(evs[2]["value"].is_null());
    // one board's events: --board, and --artifact (the older spelling) still means the same
    let (ok, new, _) = run(&["events", "--name", "*", "--json", "--board", "ci"], "");
    let (ok2, old, _) = run(&["events", "--name", "*", "--json", "--artifact", "ci"], "");
    assert!(ok && ok2 && new == old && json_lines(&new).len() == 1 && json_lines(&new)[0]["artifact"] == "ci", "{new}{old}");
    // the human listing names the event, what it is about and who sent it
    let (_, out, _) = run(&["events"], "");
    assert!(out.contains("deploy.finished") && out.contains("ci.report ci.x") && out.contains("via emit"), "{out}");
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&decoy);
}

/// Two `margin emit` processes at once never share a seq and leave no gap.
#[test]
fn concurrent_emits_from_two_processes_get_consecutive_seqs() {
    let (decoy, home) = (temp("race-decoy"), temp("race-home"));
    let mut seqs = Vec::new();
    for _ in 0..10 {
        let kids: Vec<_> = (0..2).map(|i| bare(&decoy, &home, &["emit", "race", &i.to_string()]).spawn().unwrap()).collect();
        for k in kids {
            let o = k.wait_with_output().unwrap();
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
            seqs.push(String::from_utf8_lossy(&o.stdout).trim().parse::<u64>().unwrap());
        }
    }
    seqs.sort();
    assert_eq!(seqs, (1..=20).collect::<Vec<u64>>());
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&decoy);
}

/// `margin events`: --name matches the kind by glob; a cursor starts at the end of the log and
/// moves as events are printed; --no-commit leaves it; --commit moves it; --since 0 replays.
#[test]
fn events_reads_by_name_and_through_a_named_cursor() {
    let (decoy, home) = (temp("cursor-decoy"), temp("cursor-home"));
    let run = |argv: &[&str]| plain(&decoy, &home, argv, "");
    let kinds = |out: &str| json_lines(out).iter().map(|e| format!("{}:{}", e["seq"], e["kind"].as_str().unwrap())).collect::<Vec<_>>();
    for n in ["a.one", "b.one", "a.two"] {
        assert!(run(&["emit", n]).0);
    }
    assert_eq!(kinds(&run(&["events", "--name", "a.*", "--json"]).1), ["1:a.one", "3:a.two"]);
    assert_eq!(kinds(&run(&["events", "--name", "*.one", "--since", "1", "--json"]).1), ["2:b.one"]);
    assert_eq!(kinds(&run(&["events", "--kind", "b.one", "--json"]).1), ["2:b.one"]);
    // a new cursor starts at the end: nothing of the history
    let (ok, out, _) = run(&["events", "--cursor", "mine", "--json"]);
    assert!(ok && out.is_empty(), "{out}");
    assert_eq!(std::fs::read_to_string(home.join("p/cursors/mine")).unwrap(), "3");
    for n in ["a.three", "b.two"] {
        assert!(run(&["emit", n]).0);
    }
    // --no-commit: the same events again and again
    for _ in 0..2 {
        assert_eq!(kinds(&run(&["events", "--cursor", "mine", "--no-commit", "--json"]).1), ["4:a.three", "5:b.two"]);
    }
    // the explicit commit of at-least-once delivery: one event handled, one left
    assert!(run(&["events", "--cursor", "mine", "--commit", "4"]).0);
    assert_eq!(kinds(&run(&["events", "--cursor", "mine", "--no-commit", "--json"]).1), ["5:b.two"]);
    // a committing read takes what is left, once
    assert_eq!(kinds(&run(&["events", "--cursor", "mine", "--json"]).1), ["5:b.two"]);
    assert!(run(&["events", "--cursor", "mine", "--json"]).1.is_empty());
    // a filtered read moves the cursor past what it left out too
    assert!(run(&["emit", "b.three"]).0 && run(&["emit", "a.four"]).0);
    assert_eq!(kinds(&run(&["events", "--cursor", "mine", "--name", "b.*", "--json"]).1), ["6:b.three"]);
    assert_eq!(std::fs::read_to_string(home.join("p/cursors/mine")).unwrap(), "7");
    // --since 0 replays through the cursor
    assert_eq!(kinds(&run(&["events", "--cursor", "mine", "--since", "0", "--json"]).1).len(), 7);
    // a second cursor is independent, and starts at today's end
    assert!(run(&["events", "--cursor", "other", "--json"]).1.is_empty());
    assert_eq!(std::fs::read_to_string(home.join("p/cursors/other")).unwrap(), "7");
    for (argv, why) in [
        (vec!["events", "--cursor", "a/b"], "bad cursor name"),
        (vec!["events", "--commit", "3"], "--commit moves a cursor"),
        (vec!["events", "--cursor", "mine", "--commit", "x"], "bad --commit"),
        (vec!["events", "--no-commit"], "--no-commit goes with --cursor"),
        (vec!["events", "--since", "soon"], "bad --since"),
        (vec!["events", "--unread", "--follow"], "--unread"),
    ] {
        let (ok, _, err) = run(&argv);
        assert!(!ok && err.contains(why), "{argv:?}: {err}");
    }
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&decoy);
}

/// `--follow` prints an event emitted after it started, commits its cursor as it prints, and
/// ends with exit 0 on SIGINT.
#[test]
fn events_follow_sees_an_event_emitted_after_it_started() {
    use std::io::{BufRead, BufReader};
    let (decoy, home) = (temp("follow-decoy"), temp("follow-home"));
    assert!(plain(&decoy, &home, &["emit", "before"], "").0);
    let mut ch = bare(&decoy, &home, &["events", "--follow", "--cursor", "f", "--name", "late.*", "--json"]).spawn().unwrap();
    // it is following once its cursor exists (created at the end: seq 1)
    let t = Instant::now();
    while !home.join("p/cursors/f").exists() && t.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(plain(&decoy, &home, &["emit", "other"], "").0);
    assert!(plain(&decoy, &home, &["emit", "late.one", r#"{"n":1}"#], "").0);
    let mut lines = BufReader::new(ch.stdout.take().unwrap()).lines();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for l in lines.by_ref().map_while(Result::ok) {
            if tx.send(l).is_err() {
                break;
            }
        }
    });
    let first: serde_json::Value = serde_json::from_str(&rx.recv_timeout(Duration::from_secs(10)).expect("the followed event")).unwrap();
    assert_eq!((first["seq"].as_u64(), first["kind"].as_str(), &first["value"]["n"]), (Some(3), Some("late.one"), &serde_json::json!(1)));
    assert!(plain(&decoy, &home, &["emit", "late.two"], "").0);
    let second: serde_json::Value = serde_json::from_str(&rx.recv_timeout(Duration::from_secs(10)).expect("the second event")).unwrap();
    assert_eq!(second["seq"].as_u64(), Some(4));
    let t = Instant::now();
    while std::fs::read_to_string(home.join("p/cursors/f")).unwrap_or_default() != "4" && t.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(std::fs::read_to_string(home.join("p/cursors/f")).unwrap(), "4", "committed as it printed");
    unsafe { libc_kill(ch.id() as i32, 2) };
    let st = ch.wait().unwrap();
    assert_eq!(st.code(), Some(0), "ctrl-c ends a follow cleanly: {st:?}");
    // a follower whose reader has gone ends too, without an event to trip over
    let mut ch = bare(&decoy, &home, &["events", "--follow", "--json"]).spawn().unwrap();
    drop(ch.stdout.take());
    let t = Instant::now();
    let done = loop {
        if let Some(s) = ch.try_wait().unwrap() {
            break Some(s);
        }
        if t.elapsed() > Duration::from_secs(10) {
            let _ = ch.kill();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(done.and_then(|s| s.code()), Some(0), "a closed stdout ends a follow");
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&decoy);
}

extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}

/// `margin act` appends the action's event and prints its seq; a refusal exits 1 with the
/// reason. `margin select` exits 1 on a refusal too (it used to exit 0 and only log).
#[test]
fn act_and_select_exit_nonzero_when_they_refuse() {
    let (decoy, home) = (temp("act-decoy"), temp("act-home"));
    let run = |argv: &[&str], stdin: &str| plain(&decoy, &home, argv, stdin);
    let doc = r#"{"title":"Reviews","blocks":[{"type":"table","id":"pr","columns":[{"key":"title"}],"children":[
      {"type":"tr","id":"pr-1","target":"https://example.com/1","cells":{"title":"One"}}]},{"type":"todo","id":"t1","text":"a step"}]}"#;
    assert!(run(&["new", "reviews"], doc).0);
    assert!(run(&["feed", "add", "reviews", "--every", "1h", "--owns", "pr", "--", "true"], "").0);
    let (ok, out, err) = run(&["act", "reviews", "pr-1", "open"], "");
    assert!(ok && out == "1\n", "{out}{err}");
    let (_, out, _) = run(&["events", "--name", "reviews.*", "--json"], "");
    let e = &json_lines(&out)[0];
    assert_eq!((e["kind"].as_str(), e["id"].as_str(), e["by"].as_str(), e["via"].as_str()), (Some("reviews.open"), Some("pr-1"), Some("you"), Some("act")));
    assert_eq!(e["value"], serde_json::json!({"cells": {"title": "One"}, "target": "https://example.com/1"}));
    for (argv, why) in [(vec!["act", "reviews", "pr-9", "open"], "no component \"pr-9\""), (vec!["act", "review", "pr-1", "open"], "did you mean \"reviews\""), (vec!["act", "reviews", "pr-1"], "missing <action>")] {
        let (ok, out, err) = run(&argv, "");
        assert!(!ok && out.is_empty() && err.starts_with("margin act: ") && err.contains(why), "{argv:?}: {err}");
    }
    // select: a content-changing verb on a feed-owned row is still refused, and now says so
    let (ok, _, err) = run(&["select", "mg:pin/reviews.pr-1"], "");
    assert!(!ok && err.contains("is kept current by feed `reviews`"), "{err}");
    let (ok, _, err) = run(&["select", "mg:pick/reviews.t1=x"], "");
    assert!(!ok && err.contains("t1 is a todo"), "{err}");
    let (ok, _, err) = run(&["select", "mg:todo/reviews.t1=next"], "");
    assert!(ok, "{err}");
    assert!(run(&["feed", "rm", "reviews"], "").0);
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&decoy);
}

/// Run one to its end and give its exit code: (code, stdout, stderr).
fn coded(decoy: &Path, home: &Path, argv: &[&str], stdin: &str) -> (i32, String, String) {
    let mut ch = bare(decoy, home, argv).spawn().unwrap();
    ch.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let o = ch.wait_with_output().unwrap();
    (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned())
}

fn wait_for(what: &str, secs: u64, f: impl Fn() -> bool) {
    let t = Instant::now();
    while !f() {
        assert!(t.elapsed() < Duration::from_secs(secs), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(30));
    }
}

/// The queries an outside tool reads margin through: JSON on stdout, and exit code 3 for
/// "not there" apart from 1 for "refused".
#[test]
fn queries_are_json_and_not_found_is_exit_3() {
    let (decoy, home) = (temp("query-decoy"), temp("query-home"));
    let run = |argv: &[&str], stdin: &str| coded(&decoy, &home, argv, stdin);
    let json = |out: &str| -> serde_json::Value { serde_json::from_str(out).unwrap_or_else(|e| panic!("{e}: {out}")) };
    assert_eq!(run(&["new", "plan"], DOC).0, 0);
    assert_eq!(run(&["select", "mg:pick/plan.q1=y"], "").0, 0);
    let (code, out, _) = run(&["show", "plan", "--json"], "");
    let v = json(&out);
    assert_eq!((code, v["doc"]["id"].as_str(), v["doc"]["rev"].as_u64(), v["state"]["decisions"]["q1"]["value"].as_str()), (0, Some("plan"), Some(1), Some("y")));
    let (code, out, _) = run(&["show", "plan", "q1", "--json"], "");
    let v = json(&out);
    assert_eq!((code, v["artifact"].as_str(), v["block"]["type"].as_str(), v["block"]["options"][1].as_str(), v["decision"]["value"].as_str(), v["decision"]["by"].as_str()), (0, Some("plan"), Some("question"), Some("y"), Some("y"), Some("you")));
    assert!(json(&run(&["show", "plan", "t1"], "").1)["decision"].is_null());
    let v = json(&run(&["ls", "--json"], "").1);
    assert_eq!((v["v"].as_u64(), v["artifacts"][0]["id"].as_str()), (Some(1), Some("plan")));
    assert_eq!(json(&run(&["handler", "ls", "--json"], "").1), serde_json::json!({"v": 1, "daemon": {"running": false, "pid": 0}, "handlers": []}));
    assert_eq!(json(&run(&["owner", "ls", "--json"], "").1), serde_json::json!({"v": 1, "owners": []}));
    assert_eq!(json_lines(&run(&["events", "--json", "--since", "0"], "").1)[0]["kind"], "pick");
    // not found: 3, with the message on stderr and nothing on stdout
    for argv in [
        vec!["show", "nope", "--json"],
        vec!["show", "plan", "zz", "--json"],
        vec!["act", "plan", "zz", "poke"],
        vec!["set", "plan", "zz", "text=x"],
        vec!["apply", "nope"],
        vec!["handler", "rm", "ghost"],
        vec!["handler", "set", "ghost", "--on", "x"],
        vec!["handler", "run", "ghost"],
        vec!["handler", "logs", "ghost"],
        vec!["owner", "rm", "ghost"],
        vec!["owner", "set", "ghost", "--owns", "x"],
        vec!["apply", "plan", "--as", "ghost"],
        vec!["events", "--cursor", "handler:ghost", "--no-commit"],
    ] {
        let (code, out, err) = run(&argv, r#"{"type":"todo","id":"t9","text":"x"}"#);
        assert!(code == 3 && out.is_empty() && err.starts_with("margin "), "{argv:?}: {code} {out}{err}");
    }
    // refused or invalid: 1
    for argv in [vec!["new", "plan"], vec!["emit", "bad name"], vec!["select", "mg:pick/plan.t1=x"], vec!["owner", "add", "o", "--board", "plan"], vec!["handler", "add", "h", "--", "true"]] {
        let (code, _, err) = run(&argv, "");
        assert!(code == 1 && !err.is_empty(), "{argv:?}: {code} {err}");
    }
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&decoy);
}

/// Datasets from the command line: registered once, appended to from anywhere, read by a board
/// that binds to them by name. New numbers change what the board draws and nothing about the
/// board: its revision stays where it was. A file the provider already writes is read in place.
#[test]
fn a_dataset_feeds_a_board_without_touching_its_revision() {
    let (decoy, home) = (temp("data-decoy"), temp("data-home"));
    let run = |argv: &[&str], stdin: &str| coded(&decoy, &home, argv, stdin);
    let json = |out: &str| -> serde_json::Value { serde_json::from_str(out).unwrap_or_else(|e| panic!("{e}: {out}")) };
    // create and change are separate; a write to an unregistered name is exit 3 with the fix
    let (code, _, err) = run(&["data", "append", "requests", "1"], "");
    assert!(code == 3 && err.contains("no dataset \"requests\" in project p: create it with margin data add requests"), "{code} {err}");
    assert_eq!(run(&["data", "set", "requests", "--cap", "5"], "").0, 3);
    assert_eq!(run(&["data", "add", "requests", "--cap", "50", "--expect-every", "1h"], "").0, 0);
    let (code, _, err) = run(&["data", "add", "requests"], "");
    assert!(code == 1 && err.contains("exists: change it with margin data set requests"), "{err}");
    let (code, _, err) = run(&["data", "append", "request", "1"], "");
    assert!(code == 3 && err.contains("(did you mean \"requests\"?)"), "{err}");
    assert_eq!(run(&["data", "set", "requests", "--cap", "40"], "").0, 0);
    assert_eq!(run(&["data", "add", "prs"], "").0, 0);
    assert_eq!(run(&["data", "put", "prs"], r#"[{"id": 12, "title": "Cache the layout"}, {"id": 14, "title": "Fix the flaky test", "target": "https://example.com/14"}]"#).0, 0);
    for (i, rps) in [70, 71, 90].iter().enumerate() {
        assert_eq!(run(&["data", "append", "requests", &format!(r#"{{"t": {}, "rps": {rps}}}"#, 1000 + i)], "").0, 0);
    }
    assert_eq!(run(&["data", "append", "requests", "-"], "{\"t\": 1003, \"rps\": 60}\n{\"t\": 1004, \"rps\": 61}\n").0, 0);
    for bad in [vec!["data", "append", "requests", "words"], vec!["data", "append", "requests"], vec!["data", "put", "requests"], vec!["data", "add", "bad name"], vec!["data", "show", "requests", "--cap", "3"]] {
        let stdin = if bad[1] == "put" { "[\"text\"]" } else { "" };
        let (code, out, err) = run(&bad, stdin);
        assert!(code == 1 && out.is_empty() && !err.is_empty(), "{bad:?}: {code} {out}{err}");
    }
    let (_, out, _) = run(&["data", "show", "requests", "--last", "2"], "");
    assert_eq!(out, "{\"t\":1003,\"rps\":60}\n{\"t\":1004,\"rps\":61}\n");
    let v = json(&run(&["data", "show", "requests", "--last", "1", "--json"], "").1);
    assert_eq!((v["records"][0]["rps"].as_u64(), v["cap"].as_u64(), v["external"].as_bool(), v["stale"].as_bool()), (Some(61), Some(40), Some(false), Some(false)));
    let v = json(&run(&["data", "ls", "--json"], "").1);
    assert_eq!((v["datasets"][0]["name"].as_str(), v["datasets"][1]["name"].as_str(), v["datasets"][1]["expect_every"].as_f64()), (Some("prs"), Some("requests"), Some(3600.0)));
    // a board bound to both
    let board = r#"{"title": "Board", "actions": [{"key": "p", "name": "poke"}], "blocks": [
        {"type": "plot", "id": "load", "kind": "spark", "data": "requests", "y": ["rps"], "last": 3},
        {"type": "table", "id": "pr", "rows": "prs", "key": "id", "columns": [{"key": "id", "title": "PR"}, {"key": "title", "title": "Title"}]},
        {"type": "plot", "id": "gone", "data": "absent"}]}"#;
    assert_eq!(run(&["new", "board"], board).0, 0);
    let frame = |w: &str| run(&["render", "board", "--width", w, "--height", "14"], "").1;
    let one = frame("60");
    assert!(one.contains("rps") && one.contains("█▁▁") && one.contains("12 Cache the layout") && one.contains("14 Fix the flaky test"), "{one}");
    assert!(one.contains("no dataset \"absent\" (margin data add absent)"), "{one}");
    // new numbers, one record gone: another frame, the same revision
    assert_eq!(run(&["data", "append", "requests", r#"{"t": 1005, "rps": 99}"#], "").0, 0);
    assert_eq!(run(&["data", "put", "prs"], r#"{"id": 14, "title": "Fix the flaky test", "target": "https://example.com/14"}"#).0, 0);
    let two = frame("60");
    assert!(two.contains("▁▁█") && !two.contains("Cache the layout") && two.contains("14 Fix the flaky test"), "{two}");
    let v = json(&run(&["show", "board", "--json"], "").1);
    assert_eq!(v["doc"]["rev"].as_u64(), Some(1), "a dataset changing is not the board changing");
    assert!(v["doc"]["blocks"][1].get("children").is_none(), "the rows are never written into the document");
    // a row from a dataset takes actions, and the event carries the record
    let (code, _, err) = run(&["act", "board", "pr-14", "open"], "");
    assert_eq!(code, 0, "{err}");
    let ev = json_lines(&run(&["events", "--json", "--name", "board.open", "--since", "0"], "").1);
    assert_eq!((ev[0]["id"].as_str(), ev[0]["value"]["cells"]["title"].as_str(), ev[0]["value"]["target"].as_str()), (Some("pr-14"), Some("Fix the flaky test"), Some("https://example.com/14")));
    assert_eq!(run(&["act", "board", "pr-12", "open"], "").0, 3, "the record that went is no row");
    // a file its provider writes, registered by a relative path: read where it is, never written
    std::fs::write(decoy.join("metrics.csv"), "t,rps\n1,5\n2,50\n3,5\n").unwrap();
    assert_eq!(run(&["data", "add", "metrics", "--file", "metrics.csv"], "").0, 0);
    let v = json(&run(&["data", "show", "metrics", "--json"], "").1);
    assert_eq!((v["path"].as_str().map(|p| std::path::Path::new(p).is_absolute()), v["external"].as_bool(), v["records"][1]["rps"].as_f64()), (Some(true), Some(true), Some(50.0)));
    let (code, _, err) = run(&["data", "append", "metrics", "1"], "");
    assert!(code == 1 && err.contains("margin reads it and never writes it"), "{err}");
    assert_eq!(std::fs::read_to_string(decoy.join("metrics.csv")).unwrap(), "t,rps\n1,5\n2,50\n3,5\n");
    assert_eq!(run(&["set", "board", "load", "data=metrics"], "").0, 0);
    assert!(frame("60").contains("▁█▁"), "{}", frame("60"));
    // an owner's dataset takes writes only as that owner
    assert_eq!(run(&["owner", "add", "probe", "--board", "board", "--owns", "probe"], "").0, 0);
    assert_eq!(run(&["data", "set", "requests", "--owner", "probe"], "").0, 0);
    let (code, _, err) = run(&["data", "append", "requests", "1"], "");
    assert!(code == 1 && err.contains("margin data append requests --as probe"), "{err}");
    assert_eq!(run(&["data", "append", "requests", "1", "--as", "probe"], "").0, 0);
    // overdue: ls says so, and the bound plot is marked
    assert_eq!(run(&["data", "set", "prs", "--expect-every", "1s"], "").0, 0);
    wait_for("prs to be overdue", 10, || run(&["data", "ls"], "").1.contains("STALE: last updated"));
    assert!(frame("90").contains("◷ stale · last updated"), "{}", frame("90"));
    let (code, out, _) = run(&["data", "rm", "prs"], "");
    assert!(code == 0 && out.contains("its records are kept in"), "{out}");
    assert!(frame("60").contains("no dataset \"prs\" (margin data add prs)"));
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&decoy);
}

/// Owners from the command line: add and set are separate, the owner writes with `apply --as`
/// and nobody else does, `owner ls` shows the last write and says when it is overdue, and the
/// person's action on an owned row still reaches the log.
#[test]
fn an_outside_owner_registers_writes_and_goes_stale() {
    let (decoy, home) = (temp("owner-decoy"), temp("owner-home"));
    let run = |argv: &[&str], stdin: &str| coded(&decoy, &home, argv, stdin);
    assert_eq!(run(&["new", "runs"], r#"{"title":"Runs","actions":[{"key":"X","name":"cancel"}],"blocks":[{"type":"section","id":"run","title":"Running"},{"type":"text","id":"note","text":"mine"}]}"#).0, 0);
    let (code, out, _) = run(&["owner", "add", "queue", "--board", "runs", "--owns", "run"], "");
    assert!(code == 0 && out.trim() == "owner queue: owns \"run\" in runs; it writes with: margin apply runs --as queue", "{out}");
    let (code, _, err) = run(&["owner", "add", "queue", "--board", "runs", "--owns", "x"], "");
    assert!(code == 1 && err.contains("exists: change it with margin owner set queue"), "{err}");
    assert_eq!(run(&["owner", "add", "typo", "--board", "rnus", "--owns", "x"], "").0, 3, "the board must exist");
    let (code, _, err) = run(&["owner", "add", "two", "--board", "runs", "--owns", "run-a"], "");
    assert!(code == 1 && err.contains("overlaps owner queue"), "{err}");
    // the owner writes; anyone else is refused; outside its prefix it is refused
    let row = r#"{"type":"row","id":"run-1","text":"sim A","parent":"run"}"#;
    let (code, out, err) = run(&["apply", "runs", "--as", "queue"], row);
    assert!(code == 0 && out.trim() == "runs rev 2", "{out}{err}");
    let (code, _, err) = run(&["set", "runs", "run-1", "text=mine"], "");
    assert!(code == 1 && err.contains("belongs to owner `queue`") && err.contains("margin apply runs --as queue"), "{err}");
    let (code, _, err) = run(&["apply", "runs", "--as", "queue"], r#"{"op":"set","id":"note","fields":{"text":"theirs"}}"#);
    assert!(code == 1 && err.contains("owner queue may only write the ids run and run-…"), "{err}");
    let (code, _, err) = run(&["apply", "runs", "--as", "queue"], r#"{"title":"x","blocks":[]}"#);
    assert!(code == 1 && err.contains("not a whole document"), "{err}");
    assert_eq!(run(&["set", "runs", "note", "text=still mine"], "").0, 0);
    let v: serde_json::Value = serde_json::from_str(&run(&["show", "runs", "--json"], "").1).unwrap();
    assert_eq!((v["doc"]["rev"].as_u64(), v["doc"]["blocks"][0]["children"][0]["text"].as_str()), (Some(3), Some("sim A")));
    // an action on the owned row is an event, snapshot and all
    let (code, out, _) = run(&["act", "runs", "run-1", "cancel"], "");
    assert_eq!(code, 0, "{out}");
    let e = &json_lines(&run(&["events", "--name", "runs.cancel", "--json"], "").1)[0];
    assert_eq!((e["id"].as_str(), e["value"]["text"].as_str()), (Some("run-1"), Some("sim A")));
    // staleness: fresh after a write, overdue once the expectation has passed
    let (_, out, _) = run(&["owner", "ls"], "");
    assert!(out.contains("queue") && out.contains("runs/run") && out.contains("wrote ") && !out.contains("STALE"), "{out}");
    let (code, out, _) = run(&["owner", "set", "queue", "--expect-every", "1s"], "");
    assert!(code == 0 && out.contains("expected every 1s"), "{out}");
    std::thread::sleep(Duration::from_millis(2100));
    let (_, out, _) = run(&["owner", "ls"], "");
    assert!(out.contains("STALE: last updated ") && out.contains("expected every 1s"), "{out}");
    let v: serde_json::Value = serde_json::from_str(&run(&["owner", "ls", "--json"], "").1).unwrap();
    assert_eq!((v["owners"][0]["name"].as_str(), v["owners"][0]["stale"].as_bool(), v["owners"][0]["expect_every"].as_f64()), (Some("queue"), Some(true), Some(1.0)));
    // the board draws it: a mark on the owner's rows and the reason on the first
    let (_, out, _) = run(&["render", "runs", "--width", "100"], "");
    assert!(out.contains("stale") && out.contains("expected every 1s"), "{out}");
    // the same write again changes nothing and still counts as a write
    let (code, out, _) = run(&["apply", "runs", "--as", "queue"], row);
    assert!(code == 0 && out.trim() == "runs rev 3", "{out}");
    assert!(!run(&["owner", "ls"], "").1.contains("STALE"));
    assert!(!run(&["render", "runs", "--width", "100"], "").1.contains("stale"));
    let (code, out, _) = run(&["owner", "rm", "queue"], "");
    assert!(code == 0 && out.contains("its rows stay in runs, unowned"), "{out}");
    assert_eq!(run(&["set", "runs", "run-1", "text=mine now"], "").0, 0);
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&decoy);
}

/// The handler registry from the command line: add creates and refuses a second time, set
/// changes only what it is given and refuses a missing name, `run --events --dry-run` shows what
/// a run would commit and writes nothing, and rm forgets the cursor.
#[test]
fn handlers_register_change_and_dry_run() {
    let (decoy, home) = (temp("handler-decoy"), temp("handler-home"));
    let run = |argv: &[&str], stdin: &str| coded(&decoy, &home, argv, stdin);
    assert_eq!(run(&["new", "board"], "").0, 0);
    let script = decoy.join("h.sh");
    std::fs::write(&script, "#!/bin/sh\nn=$(wc -l)\necho \"{\\\"type\\\":\\\"text\\\",\\\"id\\\":\\\"h-1\\\",\\\"text\\\":\\\"saw $n\\\"}\"\necho '{\"emit\":\"board.done\",\"value\":1}'\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    // registered paused-free; the daemon it starts is stopped at the end of this test by `rm`
    let (code, out, err) = run(&["handler", "add", "h", "--on", "board.*", "--on", "x", "--board", "board", "--owns", "h", "--retries", "0", "--", "./h.sh"], "");
    assert!(code == 0 && out.starts_with("handler h: on board.* x → ./h.sh (writes board, owns \"h\")"), "{out}{err}");
    let (code, _, err) = run(&["handler", "add", "h", "--on", "y", "--", "true"], "");
    assert!(code == 1 && err.contains("handler \"h\" exists: change it with margin handler set h"), "{err}");
    for (argv, why) in [
        (vec!["handler", "add", "n", "--", "true"], "needs the events"),
        (vec!["handler", "add", "n", "--on", "x"], "no command"),
        (vec!["handler", "add", "n", "--on", "x", "--board", "board", "--", "true"], "--board needs the id prefix"),
        (vec!["handler", "add", "n", "--on", "x", "--board", "board", "--owns", "h-x", "--", "true"], "overlaps handler h"),
        (vec!["handler", "add", "n", "--on", "x", "--timeout", "soon", "--", "true"], "bad --timeout"),
        (vec!["handler", "frob"], "unknown handler command"),
    ] {
        let (code, _, err) = run(&argv, "");
        assert!(code == 1 && err.contains(why), "{argv:?}: {err}");
    }
    assert_eq!(run(&["handler", "add", "n", "--on", "x", "--board", "baord", "--owns", "n", "--", "true"], "").0, 3);
    // set: only what is passed changes
    let (code, out, _) = run(&["handler", "set", "h", "--on", "board.go", "--timeout", "5s"], "");
    assert!(code == 0 && out.starts_with("handler h: on board.go → ./h.sh"), "{out}");
    let v: serde_json::Value = serde_json::from_str(&run(&["handler", "ls", "--json"], "").1).unwrap();
    let h = &v["handlers"][0];
    assert_eq!((h["name"].as_str(), &h["on"], &h["cmd"], h["board"].as_str(), h["owns"].as_str(), h["pending"].as_u64(), h["cursor"].as_u64()), (Some("h"), &serde_json::json!(["board.go"]), &serde_json::json!(["./h.sh"]), Some("board"), Some("h"), Some(0), Some(0)));
    let (_, out, _) = run(&["handler", "ls"], "");
    assert!(out.contains("daemon: running") && out.contains("on board.go") && out.contains("board/h") && out.contains("waiting 0"), "{out}");
    // a dry run on events from a file: prints, writes nothing
    let evs = decoy.join("evs.jsonl");
    std::fs::write(&evs, "{\"v\":1,\"seq\":41,\"at\":\"2026-11-10T10:00:00Z\",\"artifact\":\"board\",\"id\":\"\",\"kind\":\"board.go\",\"value\":null,\"by\":\"you\",\"via\":\"emit\"}\n{\"v\":1,\"seq\":42,\"at\":\"2026-11-10T10:00:01Z\",\"artifact\":\"board\",\"id\":\"\",\"kind\":\"board.go\",\"value\":null,\"by\":\"you\",\"via\":\"emit\"}\n").unwrap();
    let (code, out, err) = run(&["handler", "run", "h", "--events", &evs.display().to_string(), "--dry-run"], "");
    assert!(code == 0, "{out}{err}");
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].starts_with("given  {") && lines[1].starts_with("given  {"), "{out}");
    assert_eq!(&lines[2..], [r#"would  {"type":"text","id":"h-1","text":"saw 2"}"#, r#"would  {"emit":"board.done","value":1}"#], "{out}");
    let (_, out, _) = run(&["show", "board"], "");
    assert!(out.contains("rev 1") && !out.contains("h-1"), "{out}");
    assert!(run(&["events", "--name", "board.done", "--json"], "").1.is_empty());
    // the same run for real: it commits, as the handler, and leaves the cursor (events came from a file)
    let (code, out, err) = run(&["handler", "run", "h", "--events", &evs.display().to_string()], "");
    assert!(code == 0 && out.contains("board rev 2") && out.contains("emitted board.done"), "{out}{err}");
    let e = &json_lines(&run(&["events", "--name", "board.done", "--json"], "").1)[0];
    assert_eq!((e["by"].as_str(), e["via"].as_str(), e["artifact"].as_str()), (Some("handler:h"), Some("handler"), Some("board")));
    assert_eq!(std::fs::read_to_string(home.join("p/cursors/handler.h")).unwrap(), "0");
    // what waits for it is readable through its cursor, and only readable
    let (code, out, _) = run(&["events", "--cursor", "handler:h", "--no-commit", "--name", "*", "--json"], "");
    assert!(code == 0 && json_lines(&out).len() == 1, "{out}");
    let (code, _, err) = run(&["events", "--cursor", "handler:h"], "");
    assert!(code == 1 && err.contains("read it with --no-commit"), "{err}");
    // pause, resume, rm
    assert!(run(&["handler", "pause", "h"], "").1.contains("paused handler h"));
    assert!(run(&["handler", "ls"], "").1.contains("paused"));
    assert!(run(&["handler", "resume", "h"], "").1.contains("resumed handler h"));
    let (code, out, _) = run(&["handler", "rm", "h"], "");
    assert!(code == 0 && out.contains("removed handler h; what it wrote stays in board, unowned"), "{out}");
    assert!(!home.join("p/cursors/handler.h").exists());
    wait_for("the daemon to leave with its last handler", 10, || !home.join("p/daemon.pid").exists());
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&decoy);
}

/// `margin daemon install` writes a per-project systemd user unit where it is told to, prints
/// the line that enables it, and runs nothing; uninstall removes it; status says how the daemon
/// comes back after a reboot.
#[test]
fn daemon_install_writes_a_unit_and_status_says_how_it_comes_back() {
    let (decoy, home) = (temp("unit-decoy"), temp("unit-home"));
    let units = decoy.join("units");
    let run = |argv: &[&str]| coded(&decoy, &home, argv, "");
    let (code, out, _) = run(&["daemon", "status", "--unit-dir", &units.display().to_string()]);
    assert!(code == 0 && out.contains("daemon: not running for project p") && out.contains("after a reboot: no unit (margin daemon install)") && out.contains("handlers: none"), "{out}");
    let (code, out, err) = run(&["daemon", "install", "--unit-dir", &units.display().to_string()]);
    assert!(code == 0, "{out}{err}");
    let unit = std::fs::read_to_string(units.join("margin-p.service")).unwrap();
    assert!(unit.contains(&format!("ExecStart={BIN} daemon --stay --project p --home {}", home.display())), "{unit}");
    assert!(unit.contains("Restart=on-failure") && unit.contains("WantedBy=default.target") && unit.contains("Environment=PATH="), "{unit}");
    assert!(out.contains("wrote ") && (out.contains("systemctl --user daemon-reload && systemctl --user enable --now margin-p.service") || out.contains("systemd is not running here")), "{out}");
    let (_, out, _) = run(&["daemon", "status", "--unit-dir", &units.display().to_string()]);
    assert!(out.contains("the unit margin-p.service is installed but not enabled"), "{out}");
    // enabled is the symlink systemctl makes
    std::fs::create_dir_all(units.join("default.target.wants")).unwrap();
    std::fs::write(units.join("default.target.wants/margin-p.service"), "").unwrap();
    let v: serde_json::Value = serde_json::from_str(&run(&["daemon", "status", "--json", "--unit-dir", &units.display().to_string()]).1).unwrap();
    assert_eq!(v["unit"], serde_json::json!({"name": "margin-p.service", "installed": true, "enabled": true}));
    assert_eq!((v["running"].as_bool(), v["project"].as_str()), (Some(false), Some("p")));
    let (code, out, _) = run(&["daemon", "uninstall", "--unit-dir", &units.display().to_string()]);
    assert!(code == 0 && out.contains("removed ") && out.contains("systemctl --user disable --now margin-p.service") && !units.join("margin-p.service").exists(), "{out}");
    assert!(run(&["daemon", "uninstall", "--unit-dir", &units.display().to_string()]).1.contains("nothing to remove"));
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&decoy);
}

/// The proof, with the real binary, a temp store and nothing of Claude in the environment: the
/// reviews example is created; its rows are fetched by a handler when `reviews.refresh` is
/// emitted; an `open` on a row is an event the daemon hands to the registered `open` handler,
/// which opens that row's URL exactly once. The pane's enter key does what `margin act` does
/// here. A daemon stopped (cleanly, then by kill -9) loses nothing and repeats nothing.
#[test]
fn a_row_is_opened_by_the_registered_handler_through_the_daemon() {
    let has = |c: &str| Command::new(c).arg("--version").output().map(|o| o.status.success()).unwrap_or(false);
    if !has("bash") || !has("jq") {
        eprintln!("skipped: bash or jq is not on PATH");
        return;
    }
    let (decoy, home) = (temp("proof-decoy"), temp("proof-home"));
    let opened = decoy.join("opened.txt");
    // every command carries MARGIN_DRY_OPEN: whichever of them starts the daemon hands it on,
    // and `margin handle open` then records the URL instead of starting a browser
    let run = |argv: &[&str], stdin: &str| {
        let mut ch = bare(&decoy, &home, argv).env("MARGIN_DRY_OPEN", &opened).spawn().unwrap();
        ch.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        let o = ch.wait_with_output().unwrap();
        (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned())
    };
    let ok = |argv: &[&str], stdin: &str| {
        let (code, out, err) = run(argv, stdin);
        assert_eq!(code, 0, "margin {argv:?}: {out}{err}");
        out
    };
    let opens = || std::fs::read_to_string(&opened).unwrap_or_default().lines().map(String::from).collect::<Vec<_>>();
    let cursor = |name: &str| std::fs::read_to_string(home.join(format!("p/cursors/handler.{name}"))).ok().and_then(|s| s.trim().parse::<u64>().ok());
    let daemon_pid = || std::fs::read_to_string(home.join("p/daemon.pid")).ok().and_then(|s| s.trim().parse::<i32>().ok());

    // the example's own steps: the board, the script and its data, the two handlers
    let (_, board, steps) = run(&["example", "reviews"], "");
    assert_eq!(ok(&["new", "reviews"], &board).trim(), "reviews rev 1");
    std::fs::write(decoy.join("reviews.feed.sh"), ok(&["example", "reviews", "--feed"], "")).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(decoy.join("reviews.feed.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let data = ok(&["example", "reviews", "--data"], "");
    std::fs::write(decoy.join("reviews.data.json"), &data).unwrap();
    for needle in ["margin handler add reviews-refresh --on reviews.refresh --board reviews --owns pr -- ./reviews.feed.sh reviews.data.json", "margin handler add open --on '*.open' -- margin handle open"] {
        let line = steps.lines().find(|l| l.contains(needle)).unwrap_or_else(|| panic!("the steps don't print: {needle}\n{steps}"));
        let words: Vec<String> = line.trim_start_matches(['#', ' ']).split_whitespace().skip(1).map(|w| w.trim_matches('\'').to_string()).collect();
        ok(&words.iter().map(String::as_str).collect::<Vec<_>>(), "");
    }
    wait_for("the daemon", 10, || daemon_pid().is_some());
    assert!(ok(&["handler", "ls"], "").contains("daemon: running"));

    // a fetch: the data gains a pull request, something outside emits the refresh event, and
    // the handler's output puts the row on the board
    let mut prs: serde_json::Value = serde_json::from_str(&data).unwrap();
    prs.as_array_mut().unwrap().push(serde_json::json!({"number": 9001, "title": "A new one", "author": "ada", "url": "https://example.com/pulls/9001", "created_at": "2026-11-11T09:00:00Z", "body": "", "ci": []}));
    std::fs::write(decoy.join("reviews.data.json"), prs.to_string()).unwrap();
    let refresh: u64 = ok(&["emit", "reviews.refresh"], "").trim().parse().unwrap();
    wait_for("the refresh handler", 15, || cursor("reviews-refresh") == Some(refresh));
    let show = ok(&["show", "reviews", "--json"], "");
    assert!(show.contains("pr-9001") && show.contains("\"by\": \"handler:reviews-refresh\""), "{show}");
    assert!(opens().is_empty(), "a refresh opens nothing");

    // open the review under the cursor: an event, dispatched to the handler, opened once
    let first: u64 = ok(&["act", "reviews", "pr-4812", "open"], "").trim().parse().unwrap();
    wait_for("the open handler", 15, || cursor("open") == Some(first));
    assert_eq!(opens(), ["https://example.com/pulls/4812"]);
    let e = &json_lines(&ok(&["events", "--name", "reviews.open", "--json"], ""))[0];
    assert_eq!((e["id"].as_str(), e["by"].as_str(), e["via"].as_str(), e["value"]["target"].as_str()), (Some("pr-4812"), Some("you"), Some("act"), Some("https://example.com/pulls/4812")));
    // a second act is a second event: it opens again
    let second: u64 = ok(&["act", "reviews", "pr-4812", "open"], "").trim().parse().unwrap();
    wait_for("the second open", 15, || cursor("open") == Some(second));
    assert_eq!(opens(), ["https://example.com/pulls/4812", "https://example.com/pulls/4812"]);
    // the selection route (a click, a Zellij double-click) is the same event
    let (code, _, err) = run(&["select", "mg:open/reviews.pr-4807"], "");
    assert_eq!(code, 0, "{err}");
    wait_for("the selected open", 15, || opens().len() == 3);
    assert_eq!(opens()[2], "https://example.com/pulls/4807");

    // a clean stop: SIGTERM. The handlers are recorded as stopped, not failed, and nothing reopens
    let pid = daemon_pid().unwrap();
    unsafe { libc_kill(pid, 15) };
    wait_for("the daemon to stop", 10, || daemon_pid().is_none());
    assert!(ok(&["daemon", "status", "--unit-dir", &decoy.join("units").display().to_string()], "").contains("daemon: not running"));
    let ls = ok(&["handler", "ls", "--json"], "");
    assert!(ls.contains("\"state\":\"stopped\"") && !ls.contains("\"last_outcome\":\"failed\""), "{ls}");
    // an event appended while it is down starts it again (a handler is registered), and is
    // delivered once; what was handled before the stop is not handled again
    let third: u64 = ok(&["act", "reviews", "pr-4795", "open"], "").trim().parse().unwrap();
    wait_for("the daemon to come back for the event", 15, || cursor("open") == Some(third));
    assert_eq!(opens().len(), 4, "{:?}", opens());
    assert_eq!(opens()[3], "https://example.com/pulls/4795");

    // an unclean stop: kill -9. No stale lock blocks the next daemon; the cursor is the memory
    let pid = daemon_pid().unwrap();
    unsafe { libc_kill(pid, 9) };
    wait_for("the killed daemon to be gone", 10, || unsafe { libc_kill(pid, 0) } != 0);
    let fourth: u64 = ok(&["act", "reviews", "pr-4780", "open"], "").trim().parse().unwrap();
    wait_for("a new daemon after the kill", 15, || cursor("open") == Some(fourth));
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(opens(), ["https://example.com/pulls/4812", "https://example.com/pulls/4812", "https://example.com/pulls/4807", "https://example.com/pulls/4795", "https://example.com/pulls/4780"], "each event opened exactly once, across a stop and a kill");
    let status = ok(&["daemon", "status", "--unit-dir", &decoy.join("units").display().to_string()], "");
    assert!(status.contains("daemon: running for project p") && status.contains("open") && status.contains("waiting 0") && status.contains("ran "), "{status}");

    // a row with nothing to open, and an action nobody declared, are refused where they are asked
    let (code, _, err) = run(&["act", "reviews", "note1", "open"], "");
    assert!(code == 1 && err.contains("has no target to open"), "{err}");
    for h in ["open", "reviews-refresh"] {
        ok(&["handler", "rm", h], "");
    }
    wait_for("the daemon to leave with its last handler", 10, || daemon_pid().is_none());
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&decoy);
}

/// A Claude session ends (`margin hook end`, Claude Code's SessionEnd): its runtime files go and
/// the pane stops speaking of Claude, saying once when the session ended; the board, its events,
/// the session's cursor and its link stay. A new session started later, with nothing but the
/// store left (as after a reboot), is told the board exists.
#[test]
fn a_session_end_cleans_up_and_the_next_session_finds_the_board() {
    let (decoy, home, rt) = (temp("end-decoy"), temp("end-home"), temp("end-runtime"));
    let run = |argv: &[&str], stdin: &str| {
        let mut ch = bare(&decoy, &home, argv).env("XDG_RUNTIME_DIR", &rt).spawn().unwrap();
        ch.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        let o = ch.wait_with_output().unwrap();
        (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned())
    };
    let hook = |kind: &str, session: &str, extra: &str| run(&["hook", kind], &format!(r#"{{"cwd":"{}","session_id":"{session}"{extra}}}"#, decoy.display()));
    assert_eq!(run(&["new", "plan"], DOC).0, 0);
    assert_eq!(run(&["link", "plan", "--session", "S1"], "").0, 0);
    assert_eq!(run(&["select", "mg:todo/plan.t1=next"], "").0, 0);
    let (code, out, _) = hook("prompt", "S1", "");
    assert!(code == 0 && out.contains("margin:"), "{out}");
    assert_eq!(hook("stop", "S1", "").0, 0);
    // a live, linked session: the board speaks of Claude
    let (_, frame, _) = run(&["render", "plan", "--width", "100"], "");
    assert!(frame.contains("Claude is current") && !frame.contains("ended at"), "{frame}");
    // what a live session leaves in the runtime directory
    let rtm = rt.join("margin");
    std::fs::create_dir_all(&rtm).unwrap();
    for f in ["S1.json", "S1.rewake", "S1.woke", "other.json"] {
        std::fs::write(rtm.join(f), "{}").unwrap();
    }
    let (code, out, err) = hook("end", "S1", r#","reason":"prompt_input_exit""#);
    assert!(code == 0 && out.is_empty(), "{out}{err}");
    for f in ["S1.json", "S1.rewake", "S1.woke"] {
        assert!(!rtm.join(f).exists(), "{f} is the ended session's");
    }
    assert!(rtm.join("other.json").exists(), "another session's files are not touched");
    let act: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(home.join("p/activity/S1.json")).unwrap()).unwrap();
    assert_eq!((act["state"].as_str(), act["text"].as_str()), (Some("ended"), Some("prompt_input_exit")));
    // everything durable is where it was
    assert!(home.join("p/plan/doc.json").exists() && home.join("p/delivered/S1").exists());
    assert_eq!(json_lines(&run(&["events", "--json"], "").1).len(), 1);
    assert!(run(&["link", "--session", "S1"], "").1.starts_with("plan"), "the link stays: a resume takes it up");
    // the pane: no Claude wording, and one line saying when
    let (_, frame, _) = run(&["render", "plan", "--width", "100"], "");
    assert!(!frame.contains("Claude") && frame.contains("the session that kept this board ended at "), "{frame}");
    // a subagent ending is not the session ending
    assert_eq!(hook("stop", "S3", "").0, 0);
    assert_eq!(hook("end", "S3", r#","agent_id":"a1""#).0, 0);
    assert!(std::fs::read_to_string(home.join("p/activity/S3.json")).unwrap().contains("\"idle\""));
    // a reboot: the runtime directory is gone, only the store remains; a new session starts
    std::fs::remove_dir_all(&rt).unwrap();
    let (code, out, _) = hook("start", "S2", r#","source":"startup""#);
    assert!(code == 0 && out.contains("this project already has 1 board: plan") && out.contains("margin link <art>"), "{out}");
    assert_eq!(run(&["link", "plan", "--session", "S2"], "").0, 0);
    let (_, out, _) = hook("start", "S2", "");
    assert!(out.contains("plan") && out.contains("linked to this session"), "{out}");
    let (_, frame, _) = run(&["render", "plan", "--width", "100"], "");
    assert!(frame.contains("Claude is current") && !frame.contains("ended at"), "a live session keeps it again: {frame}");
    // and the doctor has nothing stale to report in a store that went through all that
    let (code, out, _) = run(&["doctor"], "");
    assert!(code == 0 && !out.contains("stale "), "{out}");
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&decoy);
}

/// The `runs` example: a separate tool on margin's queries and hooks, with no margin daemon and
/// no handler. Three submissions run strictly one after another and each row moves Waiting →
/// Running → Finished; a cancel stops the running one and the next starts; a drop removes a
/// waiting one; the tool killed and started again loses nothing and repeats nothing.
#[test]
fn the_runs_example_is_an_outside_tool_that_runs_submissions_one_at_a_time() {
    let has = |c: &str, arg: &str| Command::new(c).arg(arg).output().map(|o| o.status.success()).unwrap_or(false);
    if !has("bash", "--version") || !has("jq", "--version") || !has("setsid", "--version") || !has("flock", "--version") {
        eprintln!("skipped: bash, jq, setsid or flock is not on PATH");
        return;
    }
    let (decoy, home) = (temp("runs-decoy"), temp("runs-home"));
    let run = |argv: &[&str], stdin: &str| coded(&decoy, &home, argv, stdin);
    let ok = |argv: &[&str], stdin: &str| {
        let (code, out, err) = run(argv, stdin);
        assert_eq!(code, 0, "margin {argv:?}: {out}{err}");
        out
    };
    let (_, board, steps) = run(&["example", "runs"], "");
    assert!(steps.contains("margin example runs --tool") && steps.contains("no margin daemon or handler is involved"), "{steps}");
    assert_eq!(ok(&["new", "runs"], &board).trim(), "runs rev 1");
    let script = decoy.join("runs.tool.sh");
    std::fs::write(&script, ok(&["example", "runs", "--tool"], "")).unwrap();
    let tool_log = decoy.join("tool.log");
    let start_tool = || {
        let log = std::fs::OpenOptions::new().create(true).append(true).open(&tool_log).unwrap();
        Command::new("bash")
            .arg(&script)
            .current_dir(&decoy)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", &decoy)
            .env("MARGIN_HOME", &home)
            .env("MARGIN_PROJECT", "p")
            .env("MARGIN_BIN", BIN)
            .env("RUNS_DIR", decoy.join("state"))
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap()
    };
    let said = || std::fs::read_to_string(&tool_log).unwrap_or_default();
    // where each run's row is now: (running, waiting, finished), by block id
    let sections = || -> [Vec<serde_json::Value>; 3] {
        let v: serde_json::Value = serde_json::from_str(&run(&["show", "runs", "--json"], "").1).unwrap_or_default();
        let kids = |i: usize| v["doc"]["blocks"][i]["children"].as_array().cloned().unwrap_or_default();
        [kids(0), kids(1), kids(2)]
    };
    let place = |id: &str| -> &'static str {
        let s = sections();
        ["now", "wait", "done"].into_iter().zip(s.iter()).find(|(_, rows)| rows.iter().any(|r| r["id"] == id)).map(|(n, _)| n).unwrap_or("-")
    };
    let finished = |id: &str| sections()[2].iter().find(|r| r["id"] == id).cloned();
    let times = decoy.join("times");
    let submit = |label: &str, secs: &str| -> String {
        let cmd = format!("echo \"start {label} $(date +%s%N)\" >> {t}; echo out-of-{label}; sleep {secs}; echo \"end {label} $(date +%s%N)\" >> {t}", t = times.display());
        let v = serde_json::json!({"label": label, "cmd": ["sh", "-c", cmd]});
        format!("run-{}", ok(&["emit", "sim.requested", &v.to_string()], "").trim())
    };

    let mut tool = start_tool();
    wait_for("the tool to register and take its cursor", 15, || home.join("p/owners.json").exists() && home.join("p/cursors/runs-tool").exists());
    assert!(ok(&["owner", "ls"], "").contains("runs-tool") && ok(&["owner", "ls"], "").contains("runs/run"));

    // three short runs, submitted back to back
    let ids = [submit("a", "0.3"), submit("b", "0.3"), submit("c", "0.3")];
    let mut seen: Vec<Vec<&str>> = vec![Vec::new(); 3];
    let t = Instant::now();
    while sections()[2].len() < 3 {
        assert!(t.elapsed() < Duration::from_secs(30), "the three runs did not finish: {:?}\n{}", sections(), said());
        for (i, id) in ids.iter().enumerate() {
            let p = place(id);
            if p != "-" && seen[i].last() != Some(&p) {
                seen[i].push(p);
            }
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    for (i, id) in ids.iter().enumerate() {
        seen[i].push(place(id));
        seen[i].dedup();
    }
    assert!(seen[0] == ["now", "done"] || seen[0] == ["wait", "now", "done"], "{seen:?}");
    assert_eq!(seen[1], ["wait", "now", "done"], "{seen:?}");
    assert_eq!(seen[2], ["wait", "now", "done"], "{seen:?}");
    // strictly one at a time: start a, end a, start b, end b, start c, end c, by the clock
    let lines: Vec<(String, String, u128)> = std::fs::read_to_string(&times).unwrap().lines().map(|l| { let w: Vec<&str> = l.split(' ').collect(); (w[0].to_string(), w[1].to_string(), w[2].parse().unwrap()) }).collect();
    assert_eq!(lines.iter().map(|l| format!("{} {}", l.0, l.1)).collect::<Vec<_>>(), ["start a", "end a", "start b", "end b", "start c", "end c"]);
    assert!(lines.windows(2).all(|w| w[0].2 <= w[1].2), "{lines:?}");
    let row = finished(&ids[0]).unwrap();
    assert!(row["aside"].as_str().unwrap().starts_with("ok · 0.") && row["glyph"] == "done" && row["detail"].as_str().unwrap().contains("out-of-a"), "{row}");
    assert!(std::fs::read_to_string(home.join("p/runs/doc.json")).unwrap().contains("\"by\": \"owner:runs-tool\""), "the rows are written as the owner");

    // cancel the running one, drop a waiting one
    let (long1, long2, never) = (submit("long1", "30"), submit("long2", "30"), submit("never", "0.1"));
    wait_for("long1 to run with two waiting", 15, || place(&long1) == "now" && place(&long2) == "wait" && place(&never) == "wait");
    let (code, _, err) = run(&["act", "runs", &never, "cancel"], "");
    assert!(code == 1 && err.contains("offers no action \"cancel\"; its actions: drop"), "a waiting row is dropped, not cancelled: {err}");
    ok(&["act", "runs", &never, "drop"], "");
    wait_for("the dropped run to leave the queue", 15, || place(&never) == "-");
    ok(&["act", "runs", &long1, "cancel"], "");
    wait_for("the cancelled run to finish and the next to start", 15, || place(&long1) == "done" && place(&long2) == "now");
    let row = finished(&long1).unwrap();
    assert!(row["aside"].as_str().unwrap().starts_with("cancelled · ") && row["glyph"] == "fail", "{row}");
    ok(&["act", "runs", &long2, "cancel"], "");
    wait_for("the second cancel", 15, || place(&long2) == "done" && sections()[0].is_empty());
    let log = std::fs::read_to_string(&times).unwrap();
    assert!(log.contains("start long1") && !log.contains("end long1") && !log.contains("never"), "cancelled runs never reached their end, the dropped one never ran: {log}");

    // the tool dies; submissions made while it is down wait in the log, and its cursor is its memory
    tool.kill().unwrap();
    tool.wait().unwrap();
    let before = sections()[2].len();
    let (d, e) = (submit("d", "0.2"), submit("e", "0.2"));
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!((place(&d), place(&e)), ("-", "-"), "nobody is there to take them");
    let mut tool = start_tool();
    wait_for("the restarted tool to run what was submitted while it was down", 30, || place(&d) == "done" && place(&e) == "done");
    let done = sections()[2].clone();
    assert_eq!(done.len(), before + 2, "nothing lost, nothing run twice: {done:?}");
    let mut done_ids: Vec<String> = done.iter().map(|r| r["id"].as_str().unwrap().to_string()).collect();
    done_ids.sort();
    done_ids.dedup();
    assert_eq!(done_ids.len(), before + 2);
    let log = std::fs::read_to_string(&times).unwrap();
    assert_eq!((log.matches("start d ").count(), log.matches("start e ").count(), log.matches("start a ").count()), (1, 1, 1), "{log}");
    assert!(log.find("end d").unwrap() < log.find("start e").unwrap(), "still one at a time");
    // margin ran nothing for any of it
    assert!(!home.join("p/daemon.pid").exists() && !home.join("p/handlers.json").exists() && !home.join("p/feeds.json").exists());
    assert!(!said().contains("margin "), "the tool hit no margin error: {}", said());
    tool.kill().unwrap();
    tool.wait().unwrap();
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&decoy);
}
