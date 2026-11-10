//! Toasts (to4): a short message that says one thing and goes away. Anchored to the row it's about
//! (drawn on that row's right, so nothing moves), or floating over the bottom of the pane when it
//! names no row. Its countdown starts once the pane has been seen, not when it was sent; an error
//! stays until sulin clicks it. Claude, feeds and margin itself can send one.
//!
//! Toasts live beside the document (`<project>/toasts/<art>.json`), not in it: sending one bumps
//! no revision and never conflicts with anyone's writes. One toast per row: a newer one on the
//! same row replaces the older; floating ones keep the latest three.

use crate::store::{write_atomic, Store};
use crate::util::{now_rfc3339, parse_rfc3339};
use serde::{Deserialize, Serialize};

pub const LEVELS: &[&str] = &["ok", "info", "warn", "error"];
/// Seconds a toast shows once seen, unless it says otherwise.
pub const TTL: f64 = 8.0;
const FLOATING: usize = 6;
/// Toasts older than this are dropped from the file on the next write, seen or not.
const KEEP: f64 = 3600.0;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Toast {
    pub id: String,
    pub text: String,
    pub level: String,
    /// The row it's about; empty floats.
    #[serde(default)]
    pub at: String,
    pub ttl: f64,
    /// RFC 3339.
    pub sent: String,
    /// Who sent it: claude, a feed's name, or margin.
    #[serde(default)]
    pub by: String,
    /// How it shows its time running out: number, dots, pie, edge or fade; empty takes the
    /// config's toast_countdown.
    #[serde(default)]
    pub countdown: String,
    /// Its frame: round, light, heavy, double or none; empty takes the config's toast_border.
    #[serde(default)]
    pub border: String,
}

pub const COUNTDOWNS: &[&str] = &["bar", "number", "dots", "pie", "edge", "fade"];
pub const BORDERS: &[&str] = &["round", "light", "heavy", "double", "none"];

fn path(st: &Store, art: &str) -> std::path::PathBuf {
    st.dir().join("toasts").join(format!("{art}.json"))
}

pub fn load(st: &Store, art: &str) -> Vec<Toast> {
    std::fs::read(path(st, art)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Send a toast. Replaces the toast on the same row; floating toasts keep the latest few.
pub fn send(st: &Store, art: &str, text: &str, level: &str, at: &str, ttl: Option<f64>, by: &str) -> Result<Toast, String> {
    if !LEVELS.contains(&level) {
        return Err(format!("a toast's level is one of {}, not {level:?}", LEVELS.join(" ")));
    }
    if text.trim().is_empty() {
        return Err("a toast needs text".into());
    }
    if !at.is_empty() && st.load(art).ok().and_then(|d| d.find(at).map(|_| ())).is_none() {
        return Err(format!("no component {at:?} in {art} to anchor the toast to (leave --at off to float it)"));
    }
    let _l = st.lock().map_err(|e| e.to_string())?;
    let now = crate::util::epoch_now();
    let mut all: Vec<Toast> = load(st, art).into_iter().filter(|t| parse_rfc3339(&t.sent).map(|s| now - s < KEEP).unwrap_or(false)).collect();
    if !at.is_empty() {
        all.retain(|t| t.at != at);
    }
    let t = Toast {
        // nanoseconds: two toasts sent in one millisecond (a feed's burst) stay two
        id: format!("t{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0) & 0xffff_ffff),
        text: text.trim().to_string(),
        level: level.into(),
        at: at.into(),
        ttl: ttl.unwrap_or(TTL),
        sent: now_rfc3339(),
        by: by.into(),
        countdown: String::new(),
        border: String::new(),
    };
    all.push(t.clone());
    let floating: Vec<String> = all.iter().filter(|t| t.at.is_empty()).map(|t| t.id.clone()).collect();
    if floating.len() > FLOATING {
        let drop: Vec<String> = floating[..floating.len() - FLOATING].to_vec();
        all.retain(|t| !drop.contains(&t.id));
    }
    let _ = std::fs::create_dir_all(st.dir().join("toasts"));
    write_atomic(&path(st, art), serde_json::to_string_pretty(&all).unwrap().as_bytes()).map_err(|e| e.to_string())?;
    Ok(t)
}

/// A test toast (`mg:toasttest`, behind a link row Claude puts in the pane while toasts are being
/// tried out): each one the next level in turn, so a few clicks show the stack and every colour.
pub fn test(st: &Store, art: &str) -> Result<Toast, String> {
    let n = load(st, art).len();
    let (level, text) = [
        ("ok", "test: tests passed · 137 in 4.1s"),
        ("info", "test: a feed wrote 3 rows"),
        ("warn", "test: a subagent is taking longer than usual"),
        ("error", "test: the jj feed crashed (errors wait for a click)"),
    ][n % 4];
    send(st, art, text, level, "", Some(10.0), "margin")
}

/// Give a sent toast its own countdown and border (`margin toast --countdown --border`).
pub fn set_style(st: &Store, art: &str, id: &str, countdown: Option<&str>, border: Option<&str>) {
    let Ok(_l) = st.lock() else { return };
    let mut all = load(st, art);
    for t in all.iter_mut().filter(|t| t.id == id) {
        if let Some(c) = countdown {
            t.countdown = c.into();
        }
        if let Some(b) = border {
            t.border = b.into();
        }
    }
    let _ = write_atomic(&path(st, art), serde_json::to_string_pretty(&all).unwrap().as_bytes());
}

/// Take a toast down (sulin clicked it, or it ran out).
pub fn dismiss(st: &Store, art: &str, id: &str) {
    let Ok(_l) = st.lock() else { return };
    let mut all = load(st, art);
    let n = all.len();
    all.retain(|t| t.id != id);
    if all.len() != n {
        let _ = write_atomic(&path(st, art), serde_json::to_string_pretty(&all).unwrap().as_bytes());
    }
}

/// How much of a toast's time is left, 1 → 0, given when the pane first showed it; None once it's
/// done. Not yet seen: full. An error never runs out.
pub fn left(t: &Toast, seen: Option<f64>, now: f64) -> Option<f32> {
    match seen {
        _ if t.level == "error" => Some(1.0),
        None => Some(1.0),
        Some(s) => {
            let f = 1.0 - (now - s) / t.ttl.max(0.5);
            (f > 0.0).then_some(f as f32)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        let mut st = Store::open_cwd();
        st.home = std::env::temp_dir().join(format!("margin-toast-{}-{}", std::process::id(), crate::util::epoch_now() as u64));
        st.project = "p".into();
        let d: crate::doc::Doc = serde_json::from_value(serde_json::json!({"id": "plan", "title": "p", "blocks": [{"type": "todo", "id": "t1", "text": "x"}]})).unwrap();
        st.put(d, None).unwrap();
        st
    }

    #[test]
    fn one_per_row_and_a_few_floating() {
        let st = store();
        send(&st, "plan", "first", "ok", "t1", None, "claude").unwrap();
        send(&st, "plan", "second", "warn", "t1", None, "claude").unwrap();
        for i in 0..8 {
            send(&st, "plan", &format!("float {i}"), "info", "", None, "claude").unwrap();
        }
        let all = load(&st, "plan");
        assert_eq!(all.iter().filter(|t| t.at == "t1").map(|t| t.text.as_str()).collect::<Vec<_>>(), ["second"]);
        assert_eq!(all.iter().filter(|t| t.at.is_empty()).count(), FLOATING);
        assert!(send(&st, "plan", "x", "loud", "", None, "claude").is_err());
        assert!(send(&st, "plan", "x", "ok", "nope", None, "claude").is_err(), "an anchor must exist");
    }

    #[test]
    fn the_clock_starts_when_seen_and_errors_wait() {
        let t = Toast { level: "ok".into(), ttl: 8.0, ..Default::default() };
        assert_eq!(left(&t, None, 100.0), Some(1.0), "unseen: full");
        assert_eq!(left(&t, Some(100.0), 104.0), Some(0.5));
        assert_eq!(left(&t, Some(100.0), 109.0), None, "ran out");
        let e = Toast { level: "error".into(), ttl: 8.0, ..Default::default() };
        assert_eq!(left(&e, Some(0.0), 1000.0), Some(1.0), "an error waits for a click");
    }
}
