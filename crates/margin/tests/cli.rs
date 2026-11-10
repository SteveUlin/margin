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
    let start = Instant::now();
    while ch.try_wait().unwrap().is_none() {
        if start.elapsed() > Duration::from_secs(30) {
            let _ = ch.kill();
            panic!("margin {argv:?} did not finish");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let o = ch.wait_with_output().unwrap();
    (o.status.success(), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
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
        ("apply", s(&["a"]), DOC, true),
        ("put", s(&["b"]), r#"[{"type":"todo","id":"x","text":"x"}]"#, true),
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
        assert!(dir.join("a/doc.json").exists(), "flags {side}");
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
    argv.extend(s(&["apply", "art0"]));
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
