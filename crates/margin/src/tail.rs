//! Log tails (tui1): the last lines of something running, live in the pane. `margin tail <art>
//! <id> -- <command>` runs the command, keeps its output beside the document and passes it through
//! (whoever ran it still sees it); the pane shows a header (spinner, title, time) and the last few
//! lines, errors in red; when the command ends the tail folds to its outcome and a toast says so.
//!
//! The lines live in `<project>/tails/<art>/<id>.log`, the run's state in `<id>.json`, never in
//! doc.json: a busy log would otherwise bump a revision per line. The document holds only a
//! `tail` component saying where to show it. The writer batches lines (at most one append every
//! 200 ms), so a chatty command doesn't make the pane reload per line.

use crate::store::{write_atomic, Store};
use crate::util::{epoch_now, now_rfc3339};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::mpsc;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Meta {
    /// running, ok or failed.
    pub state: String,
    pub cmd: String,
    pub started: String,
    #[serde(default)]
    pub ended: String,
    #[serde(default)]
    pub code: Option<i32>,
}

pub fn dir(st: &Store, art: &str) -> PathBuf {
    st.dir().join("tails").join(art)
}

pub fn meta(st: &Store, art: &str, id: &str) -> Option<Meta> {
    serde_json::from_slice(&std::fs::read(dir(st, art).join(format!("{id}.json"))).ok()?).ok()
}

/// The last `n` lines of a tail (read from the file's end, however long it grew).
pub fn last_lines(st: &Store, art: &str, id: &str, n: usize) -> Vec<String> {
    let Ok(mut f) = std::fs::File::open(dir(st, art).join(format!("{id}.log"))) else { return Vec::new() };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let _ = f.seek(SeekFrom::Start(len.saturating_sub(64 * 1024)));
    let mut buf = Vec::new();
    let _ = f.read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf);
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].iter().map(|l| l.to_string()).collect()
}

/// Every tail component in a doc with its run: state and its last 30 lines.
pub fn for_doc(st: &Store, art: &str, doc: &crate::doc::Doc) -> std::collections::HashMap<String, (Meta, Vec<String>)> {
    let mut out = std::collections::HashMap::new();
    crate::doc::walk(&doc.blocks, &mut |b, _, _| {
        if b.kind == "tail" {
            out.insert(b.id.clone(), (meta(st, art, &b.id).unwrap_or_default(), last_lines(st, art, &b.id, 30)));
        }
    });
    out
}

/// Does a line read as an error or a failure (drawn red)?
pub fn is_error(line: &str) -> bool {
    let l = line.to_lowercase();
    ["error", "failed", "panicked", "fatal", "traceback", "exception"].iter().any(|k| l.contains(k)) || l.contains(" fail ") || l.starts_with("fail")
}

fn write_meta(st: &Store, art: &str, id: &str, m: &Meta) {
    let _ = write_atomic(&dir(st, art).join(format!("{id}.json")), serde_json::to_string(m).unwrap().as_bytes());
}

/// Run `cmd` as tail `id`: its stdout and stderr go to the tail (and through to ours), its exit to
/// the tail's state and a toast. Returns the command's exit code.
pub fn run(st: &Store, art: &str, id: &str, title: &str, cmd: &[String]) -> Result<i32, String> {
    let (prog, args) = cmd.split_first().ok_or("margin tail needs a command after --")?;
    std::fs::create_dir_all(dir(st, art)).map_err(|e| e.to_string())?;
    let log = dir(st, art).join(format!("{id}.log"));
    let _ = std::fs::write(&log, b"");
    let started = epoch_now();
    let mut m = Meta { state: "running".into(), cmd: cmd.join(" "), started: now_rfc3339(), ..Default::default() };
    write_meta(st, art, id, &m);
    let mut child = std::process::Command::new(prog)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("{prog}: {e}"))?;
    let (tx, rx) = mpsc::channel::<(bool, String)>();
    let mut readers = Vec::new();
    for (err, pipe) in [(false, child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>)), (true, child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>))] {
        let Some(pipe) = pipe else { continue };
        let tx = tx.clone();
        readers.push(std::thread::spawn(move || {
            for line in std::io::BufReader::new(pipe).split(b'\n').map_while(Result::ok) {
                let _ = tx.send((err, String::from_utf8_lossy(&line).trim_end_matches('\r').to_string()));
            }
        }));
    }
    drop(tx);
    // batch appends: one write per 200 ms at most, so the pane reloads at that pace, not per line
    let mut file = std::fs::OpenOptions::new().append(true).open(&log).map_err(|e| e.to_string())?;
    let mut pending = String::new();
    let mut last = std::time::Instant::now();
    loop {
        match rx.recv_timeout(std::time::Duration::from_millis(200)) {
            Ok((err, line)) => {
                if err {
                    eprintln!("{line}");
                } else {
                    println!("{line}");
                }
                pending.push_str(&line);
                pending.push('\n');
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if !pending.is_empty() && last.elapsed().as_millis() >= 200 {
            let _ = file.write_all(pending.as_bytes());
            pending.clear();
            last = std::time::Instant::now();
        }
    }
    let _ = file.write_all(pending.as_bytes());
    for r in readers {
        let _ = r.join();
    }
    let code = child.wait().map_err(|e| e.to_string())?.code().unwrap_or(-1);
    m.state = if code == 0 { "ok".into() } else { "failed".into() };
    m.ended = now_rfc3339();
    m.code = Some(code);
    write_meta(st, art, id, &m);
    // the outcome, as a toast on the tail's row
    let took = crate::components::age_short(epoch_now() - started);
    let what = if title.is_empty() { m.cmd.clone() } else { title.to_string() };
    let (text, level) = if code == 0 { (format!("{what} · done · {took}"), "ok") } else { (format!("{what} · failed (exit {code}) · {took}"), "error") };
    let _ = crate::toast::send(st, art, &text, level, id, None, "margin");
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        let mut st = Store::open_cwd();
        st.home = std::env::temp_dir().join(format!("margin-tail-{}-{}", std::process::id(), epoch_now() as u64));
        st.project = "p".into();
        let d: crate::doc::Doc = serde_json::from_value(serde_json::json!({"id": "plan", "title": "p", "blocks": [{"type": "tail", "id": "ci", "title": "tests"}]})).unwrap();
        st.put(d, None).unwrap();
        st
    }

    #[test]
    fn runs_keeps_lines_and_reports_the_outcome() {
        let st = store();
        let code = run(&st, "plan", "ci", "tests", &["sh".into(), "-c".into(), "echo one; echo two >&2; echo 'test x FAILED'; exit 3".into()]).unwrap();
        assert_eq!(code, 3);
        let m = meta(&st, "plan", "ci").unwrap();
        assert_eq!((m.state.as_str(), m.code), ("failed", Some(3)));
        let lines = last_lines(&st, "plan", "ci", 10);
        assert!(lines.contains(&"one".to_string()) && lines.contains(&"two".to_string()) && lines.iter().any(|l| is_error(l)), "{lines:?}");
        let t = crate::toast::load(&st, "plan");
        assert!(t.iter().any(|t| t.at == "ci" && t.level == "error" && t.text.contains("failed (exit 3)")), "{t:?}");
        assert_eq!(last_lines(&st, "plan", "ci", 1).len(), 1);
    }
}
