//! Settings, each with a default that answers one of design.md §10's open questions, so every
//! default is one line of JSON away from changing.
//!
//! File: `$MARGIN_CONFIG`, else `~/.config/margin/config.json`. Env overrides win over the file:
//! `MARGIN_MOTION=off`, `MARGIN_IMAGES=blocks`, `MARGIN_HOST=plain`, `MARGIN_HOME=<store dir>`.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// "on" or "off". Off: every tween and spring lands at once, rings rest, the clock never ticks
    /// for ambient effects.
    pub motion: String,
    /// Multiplies animation durations (2.0 = half speed).
    pub motion_slow: f32,
    /// Frame-rate ceiling while something animates. Zellij re-renders on its own cadence, so
    /// more than ~30 buys nothing (research.md §4).
    pub fps: f32,
    /// URL opener: "auto" (opener over ssh, else xdg-open), or a command such as "xdg-open",
    /// "opener", "open". Zellij may run on the laptop or on a server; auto follows `SSH_CONNECTION`.
    pub opener: String,
    /// Clipboard for non-token selections: "auto" (wl-copy when a Wayland display is reachable and
    /// we're not over ssh, else OSC 52 to the session's client ttys), "wl-copy", "osc52", "none".
    pub clipboard: String,
    /// Archive an artifact after this many days untouched (0 = never).
    pub archive_days: u32,
    /// Default pane shape for `margin open`: "split" or "float" (pinned). Split by default: a
    /// double-click on a token acts in one gesture there (with mouse_click_through), while a pinned
    /// float with the floating layer hidden needs one focusing click first (tests/e2e, 2026-11-05).
    pub pane: String,
    pub float_x: String,
    pub float_y: String,
    pub float_width: String,
    pub float_height: String,
    /// "auto" (probe for kitty graphics), "kitty", "blocks" (halfblock fallback).
    pub images: String,
    /// Focused-panel border: "static" (the gradient, still), "flow" (it travels around the panel:
    /// about 40 KB/s of redraw for decoration, measured 2026-11-06), "plain".
    pub ring: String,
    /// Seconds per lap of the flowing ring.
    pub ring_period: f32,
    /// Breaths a card that needs sulin pulses when it appears or regains attention (0 = no pulse).
    pub pulse_breaths: f32,
    pub pulse_period: f32,
    /// Start with mouse reporting on: the wheel scrolls, a click on a section header folds it and
    /// a click on a token acts. Off hands the mouse back to Zellij, so double-click token selection
    /// and plain text selection work instead (design §4); headers then show an `mg:fold` token.
    pub mouse: bool,
    /// What the status bar looks like: "lipgloss" (the Lip Gloss layout example's colours).
    pub status_style: String,
    /// Where panes live (host.rs): "zellij", "plain" (no multiplexer: `margin open` prints the
    /// `margin view` command, selection is in-pane keys and mouse), or "auto" (Zellij when
    /// `$ZELLIJ_SESSION_NAME` is set, else plain).
    pub host: String,
    /// difftastic for `margin diff` when no `difft` is on PATH: an absolute path. `MARGIN_DIFFT`
    /// overrides it and PATH. The nix build wraps difft in; a dev build outside `nix develop`
    /// needs this, PATH, or one of the usual nix profile dirs.
    pub difft: String,
    /// Opt-in phone push when Claude has been waiting on sulin this long with the pane out of
    /// view: an ntfy topic URL (empty = off), the delay in seconds, and an optional bearer token.
    /// No bell, ever: the tab mark (● on the pane's tab) is the in-terminal signal.
    pub ntfy_url: String,
    pub ntfy_after: f32,
    pub ntfy_token: String,
    /// Wake an idle Claude when sulin finishes in the pane (quiet 5 s, no half-answered question
    /// set, out of review mode). "auto": the `Stop` hook `margin hook rewake` does it when its
    /// watcher is alive, else the pane writes one labelled message into the session's inbox
    /// (wake.rs); "hook": only the watcher; "inbox": only the message; "off". `true` and `false`
    /// (the key's first form) read as "auto" and "off".
    #[serde(deserialize_with = "wake_mode")]
    pub wake: String,
    /// The person's name in what margin tells Claude ("sulin picked cli"); default "the user".
    pub user_name: String,
    /// The dependency graph's glyphs: "box" (box-drawing, any font) or "flog" (Flog symbols,
    /// rbong/flog-symbols, U+F5D0–U+F60D: rounder, but the font must have them).
    pub graph_glyphs: String,
    /// How a toast shows its time running out: number (8s), dots, pie, edge (the bottom border
    /// drains) or fade (none, it fades at the end).
    pub toast_countdown: String,
    /// A toast's frame: double (default), round, light, heavy or none.
    pub toast_border: String,
    /// How dashboard pieces look (rules, tables, name badges, run strips): docs/dashboards.md.
    pub dashboard: crate::theme::DashConfig,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            motion: "on".into(),
            motion_slow: 1.0,
            fps: 30.0,
            opener: "auto".into(),
            clipboard: "auto".into(),
            archive_days: 30,
            pane: "split".into(),
            float_x: "62%".into(),
            float_y: "2".into(),
            float_width: "37%".into(),
            float_height: "60%".into(),
            images: "auto".into(),
            ring: "static".into(),
            ring_period: 6.0,
            pulse_breaths: 3.0,
            pulse_period: 2.4,
            mouse: true,
            status_style: "lipgloss".into(),
            host: "auto".into(),
            difft: String::new(),
            ntfy_url: String::new(),
            ntfy_after: 120.0,
            ntfy_token: String::new(),
            wake: "auto".into(),
            user_name: String::new(),
            graph_glyphs: "box".into(),
            toast_countdown: "bar".into(),
            toast_border: "double".into(),
            dashboard: Default::default(),
        }
    }
}

fn wake_mode<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(match serde_json::Value::deserialize(d)? {
        serde_json::Value::Bool(false) => "off".into(),
        serde_json::Value::String(s) if matches!(s.as_str(), "auto" | "hook" | "inbox" | "off") => s,
        serde_json::Value::Bool(true) => "auto".into(),
        v => return Err(serde::de::Error::custom(format!("wake: {v} isn't auto, hook, inbox or off"))),
    })
}

pub fn config_path() -> PathBuf {
    if let Ok(p) = std::env::var("MARGIN_CONFIG") {
        return PathBuf::from(p);
    }
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config"));
    base.join("margin/config.json")
}

impl Config {
    pub fn load() -> Config {
        // a config that won't parse falls back to defaults, but says so: silently ignoring it
        // made every setting look broken at once
        let mut c: Config = match std::fs::read_to_string(config_path()) {
            Err(_) => Config::default(),
            Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
                eprintln!("margin: ignoring {} ({e}); using defaults", config_path().display());
                Config::default()
            }),
        };
        if let Ok(m) = std::env::var("MARGIN_MOTION") {
            c.motion = m;
        }
        if let Ok(m) = std::env::var("MARGIN_IMAGES") {
            c.images = m;
        }
        if let Ok(h) = std::env::var("MARGIN_HOST") {
            c.host = h;
        }
        c
    }

    pub fn motion(&self) -> vellum::anim::Motion {
        vellum::anim::Motion { on: self.motion != "off", slow: self.motion_slow.max(0.05) }
    }
}

/// One key of a config file as text, without the env overrides `Config::load` layers on.
pub fn read_key(path: &std::path::Path, key: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    v.get(key)?.as_str().map(String::from)
}

/// What a config file's text becomes with one key set: every other key stays as written (a whole
/// `Config` saved back would freeze today's defaults and env overrides into the file). `None`
/// when the key already holds the value. A file that isn't a JSON object is refused, not replaced.
pub fn with_key(text: Option<&str>, key: &str, value: &str) -> Result<Option<String>, String> {
    let mut v: serde_json::Value = match text {
        Some(t) if !t.trim().is_empty() => serde_json::from_str(t).map_err(|e| format!("not JSON ({e})"))?,
        _ => serde_json::json!({}),
    };
    let m = v.as_object_mut().ok_or("not a JSON object")?;
    if m.get(key).and_then(|x| x.as_str()) == Some(value) {
        return Ok(None);
    }
    m.insert(key.into(), serde_json::json!(value));
    Ok(Some(serde_json::to_string_pretty(&v).unwrap() + "\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wake_reads_its_modes_and_the_old_booleans() {
        let w = |t: &str| serde_json::from_str::<Config>(t).map(|c| c.wake);
        assert_eq!(w("{}").unwrap(), "auto");
        assert_eq!((w("{\"wake\": true}").unwrap(), w("{\"wake\": false}").unwrap()), ("auto".into(), "off".into()));
        assert_eq!(w("{\"wake\": \"hook\"}").unwrap(), "hook");
        assert!(w("{\"wake\": \"sometimes\"}").is_err());
    }

    #[test]
    fn with_key_sets_one_key_and_keeps_the_rest() {
        let s = with_key(None, "user_name", "robin").unwrap().unwrap();
        assert_eq!(serde_json::from_str::<Config>(&s).unwrap().user_name, "robin");
        let s = with_key(Some("{\"host\": \"plain\", \"mine\": 1}"), "user_name", "robin").unwrap().unwrap();
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!((v["host"].as_str(), v["mine"].as_i64(), v["user_name"].as_str()), (Some("plain"), Some(1), Some("robin")));
        assert_eq!(with_key(Some(&s), "user_name", "robin").unwrap(), None, "already set");
        assert!(with_key(Some("[1]"), "user_name", "x").is_err() && with_key(Some("{oops"), "user_name", "x").is_err());
    }
}
