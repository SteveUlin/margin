//! Claude Code's session recap, kept in the pane. When sulin comes back to a terminal he left,
//! Claude Code writes a one-paragraph recap ("away summary": the goal, where things stand, what's
//! next) into the chat, where it scrolls away like everything else. The hooks pick the latest one
//! out of the transcript and keep it beside the activity files; the pane shows it as its first row.
//! (r6: this replaced a read-back margin would have asked for; Claude Code already writes one.)

use crate::store::{write_atomic, Store};
use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Recap {
    pub session: String,
    pub text: String,
    /// RFC 3339, from the transcript record.
    pub at: String,
}

/// How far back from the transcript's end to look: a recap is written when he returns, so it is
/// near the end at the next prompt; the transcript itself runs to megabytes.
const TAIL: u64 = 512 * 1024;

/// The newest recap in a transcript, if any.
pub fn latest(transcript: &Path) -> Option<(String, String)> {
    let mut f = std::fs::File::open(transcript).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(TAIL))).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    text.lines().rev().find_map(|l| {
        if !l.contains("\"away_summary\"") {
            return None;
        }
        let v: serde_json::Value = serde_json::from_str(l).ok()?;
        if v["type"] != "system" || v["subtype"] != "away_summary" {
            return None;
        }
        let t = v["content"].as_str()?;
        // Claude Code's own footer is for the chat, not the pane
        let t = t.trim_end().trim_end_matches("(disable recaps in /config)").trim_end().to_string();
        Some((t, v["timestamp"].as_str().unwrap_or("").to_string()))
    })
}

fn dir(st: &Store) -> std::path::PathBuf {
    st.dir().join("recap")
}

fn key(session: &str) -> String {
    session.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect()
}

/// Called by the hook: keep the session's newest recap. Writes only when it changed.
pub fn save(st: &Store, session: &str, transcript: &Path) {
    let Some((text, at)) = latest(transcript) else { return };
    let p = dir(st).join(format!("{}.json", key(session)));
    let r = Recap { session: session.into(), text, at };
    if std::fs::read(&p).ok().and_then(|b| serde_json::from_slice::<Recap>(&b).ok()).as_ref() == Some(&r) {
        return;
    }
    let _ = std::fs::create_dir_all(dir(st));
    if let Ok(j) = serde_json::to_vec(&r) {
        let _ = write_atomic(&p, &j);
    }
}

/// The newest recap among `sessions` (the pane's linked sessions), or any session's when none is
/// linked.
pub fn newest(st: &Store, sessions: &[String]) -> Option<Recap> {
    std::fs::read_dir(dir(st))
        .ok()?
        .filter_map(|e| serde_json::from_slice::<Recap>(&std::fs::read(e.ok()?.path()).ok()?).ok())
        .filter(|r| sessions.is_empty() || sessions.contains(&r.session))
        .max_by(|a, b| a.at.cmp(&b.at))
}

/// What a pane on `art` shows: the newest recap of its linked sessions while it's under six hours
/// old, as (text, local time).
pub fn for_pane(st: &Store, art: &str) -> Option<(String, String)> {
    let mine: Vec<String> = super::link::all(st).into_iter().filter(|l| l.art == art).map(|l| l.session).collect();
    let r = newest(st, &mine)?;
    let t = crate::util::parse_rfc3339(&r.at).filter(|t| crate::util::epoch_now() - t < 6.0 * 3600.0)?;
    Some((r.text, crate::util::local_hm(t)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_newest_recap_without_its_footer() {
        let d = std::env::temp_dir().join(format!("margin-recap-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        let t = d.join("t.jsonl");
        let lines = [
            r#"{"type":"user","message":{"content":"hi"}}"#,
            r#"{"type":"system","subtype":"away_summary","content":"old one","timestamp":"2026-11-08T01:00:00Z"}"#,
            r#"{"type":"system","subtype":"away_summary","content":"The goal is X. Next, Y. (disable recaps in /config)","timestamp":"2026-11-08T02:00:00Z"}"#,
            r#"{"type":"assistant","message":{"content":"mentions away_summary in passing"}}"#,
        ];
        std::fs::write(&t, lines.join("\n")).unwrap();
        assert_eq!(latest(&t), Some(("The goal is X. Next, Y.".into(), "2026-11-08T02:00:00Z".into())));
        let _ = std::fs::remove_dir_all(&d);
    }
}
