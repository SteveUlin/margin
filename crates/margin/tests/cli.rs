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

const DOC: &str = r#"{"title":"walk","blocks":[{"type":"section","id":"s","title":"S","children":[
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
        ("put", s(&["b"]), r#"[{"type":"todo","id":"y","text":"y"}]"#, true),
        ("schema", s(&[]), "", true),
        ("add", s(&["a", "s", "todo", "t2", "two"]), "", true),
        ("set", s(&["a", "t2", "note=n"]), "", true),
        ("rm", s(&["a", "t2"]), "", true),
        ("move", s(&["a", "t1", "--after", "t3"]), "", true),
        ("title", s(&["a", "Walk"]), "", true),
        ("hints", s(&["a", "k  what it does"]), "", true),
        ("purpose", s(&["a", "walk the table"]), "", true),
        ("patch", s(&["a"]), r#"{"op":"set","id":"t3","fields":{"note":"n"}}"#, true),
        ("ask", s(&["a", "q2", "Again?", "x", "y"]), "", true),
        ("propose", s(&["a", "t1", "tick", "done"]), "", true),
        ("diff", s(&["a", "d1", "--old", "/nonexistent-a", "--new", "/nonexistent-b"]), "", false),
        ("image", s(&["a", "i1", "/nonexistent.png"]), "", false),
        ("show", s(&["a"]), "", true),
        ("ls", s(&[]), "", true),
        ("gc", s(&[]), "", true),
        ("events", s(&["--all"]), "", true),
        ("wait", s(&["a.zz", "--timeout", "1s"]), "", false),
        ("hook", s(&["prompt"]), &hook_in, true),
        ("doc", s(&["a", "notes"]), "# Notes\n\nA line.\n", true),
        ("tail", s(&["a", "tl", "--", "true"]), "", true),
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
    assert!(!ok && out.trim() == "margin add: no artifact \"plan\" in project p: create it with margin new plan", "{out}");
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
    let (ok, out) = run(&["put", "plan"], DOC);
    assert!(ok, "{out}");
    let png = decoy.join("x.png");
    let want = "no artifact \"paln\" in project p: create it with margin new paln (did you mean \"plan\"?)";
    let png_s = png.display().to_string();
    let walk: Vec<(Vec<&str>, &str)> = vec![
        (vec!["apply", "paln"], DOC),
        (vec!["apply", "paln"], r#"{"type":"todo","id":"t9","text":"nine"}"#),
        (vec!["put", "paln"], DOC),
        (vec!["put", "paln"], ""),
        (vec!["patch", "paln"], r#"{"op":"set","id":"t1","fields":{"note":"n"}}"#),
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
        (vec!["tail", "paln", "tl", "--", "true"], ""),
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
    assert!(out.contains("rev 3") || out.contains("rev 2"), "plan is as the last put left it: {out}");
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
    let (ok, out) = run(&["feed", "add", "f", "--every", "1h", "--artifact", "board", "--owns", "fd", "--", "printf", r#"{"type":"text","id":"fd","text":"hi"}\n"#], "");
    assert!(ok, "{out}");
    let (ok, out) = run(&["feed", "run", "f"], "");
    assert!(ok && (out.contains("board rev") || out.contains("board unchanged")), "{out}");
    // the artifact goes (by hand: `margin rm board --yes` would unregister the feed with it)
    std::fs::rename(home.join("p/board"), home.join("gone")).unwrap();
    let (ok, out) = run(&["feed", "run", "f"], "");
    assert!(!ok && out.contains("margin feed: no artifact \"board\" in project p: create it with margin new board"), "{out}");
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
