//! The dashboard theme: the `dashboard` table of margin's config file turned into the
//! `vellum::widgets::Theme` that rules, tables, name badges and run strips draw from.
//!
//! vellum owns the look and its defaults; this file only maps JSON onto it, so vellum stays free
//! of serde. Every key is optional. A value that can't be used (a color that doesn't parse, a
//! glyph that isn't one cell wide) is skipped and the default stands, because a typo in one key
//! shouldn't cost the whole dashboard its colors.

use serde::{Deserialize, Serialize};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use vellum::color::{Palette, Rgb, EVERFOREST};
use vellum::widgets::{Mark, Theme, WhoOverride};

/// `"dashboard": {…}` in config.json. See docs/dashboards.md.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct DashConfig {
    /// Stripe alternate table rows when the table doesn't say (default false).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zebra: Option<bool>,
    /// The stripe's ground (default: the palette's bg1).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zebra_color: Option<String>,
    /// The glyph a rule repeats (default ─).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule_glyph: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule_color: Option<String>,
    /// The color of bold text in prose (default: the standard foreground).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emphasis: Option<String>,
    pub who: WhoConfig,
    pub runs: RunsConfig,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct WhoConfig {
    /// Replaces the badge glyph list (each one cell wide).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub glyphs: Option<Vec<String>>,
    /// Replaces the badge color list; ids hash into it too.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub colors: Option<Vec<String>>,
    /// Per-name overrides: `{"sulin": {"color": "purple", "glyph": "★"}}`.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub names: BTreeMap<String, MarkConfig>,
}

/// A glyph, a color, or both.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MarkConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub glyph: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RunsConfig {
    /// A blank cell between squares (default false).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gap: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ok: Option<MarkConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fail: Option<MarkConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip: Option<MarkConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub running: Option<MarkConfig>,
    /// A slot where nothing ran (a null).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub none: Option<MarkConfig>,
}

/// A color as config writes it: `#rrggbb`, or one of the palette's names.
pub fn color(p: &Palette, s: &str) -> Option<Rgb> {
    Some(match s.trim() {
        "red" => p.red,
        "orange" => p.orange,
        "yellow" => p.yellow,
        "green" => p.green,
        "aqua" => p.aqua,
        "blue" => p.blue,
        "purple" => p.purple,
        "grey" | "gray" => p.grey1,
        "dim" => p.grey0,
        "fg" => p.fg,
        "bg1" => p.bg1,
        "bg2" => p.bg2,
        "bg4" => p.bg4,
        t => return Rgb::parse(t),
    })
}

/// A glyph the widgets can place: exactly one cell wide.
fn glyph(s: &str) -> Option<String> {
    (vellum::text::width(s) == 1).then(|| s.to_string())
}

/// The theme `c` describes on palette `p`: vellum's defaults with every usable key applied.
pub fn build(c: &DashConfig, p: &Palette) -> Theme {
    let mut t = Theme::new(p);
    if let Some(z) = c.zebra {
        t.zebra = z;
    }
    if let Some(x) = c.zebra_color.as_deref().and_then(|s| color(p, s)) {
        t.zebra_ground = x;
    }
    if let Some(g) = c.rule_glyph.as_deref().and_then(glyph) {
        t.rule_glyph = g;
    }
    if let Some(x) = c.rule_color.as_deref().and_then(|s| color(p, s)) {
        t.rule_color = x;
    }
    if let Some(x) = c.emphasis.as_deref().and_then(|s| color(p, s)) {
        t.emphasis = x;
    }
    // a replacement list counts only if something usable is in it
    if let Some(gs) = &c.who.glyphs {
        let gs: Vec<String> = gs.iter().filter_map(|g| glyph(g)).collect();
        if !gs.is_empty() {
            t.who_glyphs = gs;
        }
    }
    if let Some(cs) = &c.who.colors {
        let cs: Vec<Rgb> = cs.iter().filter_map(|s| color(p, s)).collect();
        if !cs.is_empty() {
            t.who_colors = cs;
        }
    }
    for (name, m) in &c.who.names {
        let o = WhoOverride { glyph: m.glyph.as_deref().and_then(glyph), color: m.color.as_deref().and_then(|s| color(p, s)) };
        t.who_names.insert(name.trim().to_lowercase(), o);
    }
    if let Some(g) = c.runs.gap {
        t.runs.gap = g;
    }
    let mark = |m: &mut Mark, c: &Option<MarkConfig>| {
        let Some(c) = c else { return };
        if let Some(g) = c.glyph.as_deref().and_then(glyph) {
            m.glyph = g;
        }
        if let Some(x) = c.color.as_deref().and_then(|s| color(p, s)) {
            m.color = x;
        }
    };
    mark(&mut t.runs.ok, &c.runs.ok);
    mark(&mut t.runs.fail, &c.runs.fail);
    mark(&mut t.runs.skip, &c.runs.skip);
    mark(&mut t.runs.running, &c.runs.running);
    mark(&mut t.runs.none, &c.runs.none);
    t
}

thread_local! {
    static CURRENT: RefCell<Option<Rc<Theme>>> = const { RefCell::new(None) };
    static GENERATION: Cell<u64> = const { Cell::new(0) };
}

/// The theme components draw with: built once per thread from the config file, on first use.
/// (A thread-local, not a `Ctx` field, because the scene builds `Ctx` and the theme is the
/// same for every frame; tests swap it with [`set`].) Under test the config file is never read:
/// a test's colors must not depend on the machine's config.
pub fn current() -> Rc<Theme> {
    CURRENT.with(|c| {
        c.borrow_mut()
            .get_or_insert_with(|| {
                let dash = if cfg!(test) { DashConfig::default() } else { crate::config::Config::load().dashboard };
                Rc::new(build(&dash, &EVERFOREST))
            })
            .clone()
    })
}

/// Replace this thread's theme (tests, and a future live reload).
#[allow(dead_code)]
pub fn set(t: Theme) {
    CURRENT.with(|c| *c.borrow_mut() = Some(Rc::new(t)));
    GENERATION.with(|g| g.set(g.get() + 1));
}

/// Counts theme swaps, so a cache of things laid out with the theme knows when it is stale.
pub fn generation() -> u64 {
    GENERATION.with(|g| g.get())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_keys_override_the_defaults_and_bad_values_are_skipped() {
        let p = &EVERFOREST;
        assert_eq!(build(&DashConfig::default(), p), Theme::new(p), "no keys: vellum's defaults");
        let c: DashConfig = serde_json::from_value(serde_json::json!({
            "zebra": true, "zebra_color": "#101010", "rule_glyph": "═", "rule_color": "nonsense", "emphasis": "aqua",
            "who": {"glyphs": ["λ", "wide", "π"], "colors": ["#ff8800", "aqua", "oops"],
                    "names": {"Sulin": {"color": "purple", "glyph": "★"}, "ada": {"glyph": "too wide"}}},
            "runs": {"gap": false, "fail": {"glyph": "x", "color": "orange"}, "ok": {"color": "#00ff00"}}
        })).unwrap();
        let t = build(&c, p);
        assert!(t.zebra && t.zebra_ground == Rgb(16, 16, 16) && t.rule_glyph == "═" && t.emphasis == p.aqua);
        assert_eq!(t.rule_color, Theme::new(p).rule_color, "a color that doesn't parse leaves the default");
        assert_eq!(t.who_glyphs, ["λ", "π"], "a glyph that isn't one cell is dropped");
        assert_eq!(t.who_colors, [Rgb(255, 136, 0), p.aqua]);
        assert_eq!(t.badge("sulin"), Mark::new("★", p.purple), "the per-name override wins over the hash");
        assert!(["λ", "π"].contains(&t.badge("ada").glyph.as_str()), "an unusable override falls back to the hash");
        assert_eq!((t.runs.gap, t.runs.fail.clone(), t.runs.ok.clone()), (false, Mark::new("x", p.orange), Mark::new("▪", Rgb(0, 255, 0))));
        // the whole config still loads with a dashboard table in it, and without one
        let full: crate::config::Config = serde_json::from_str(r#"{"dashboard": {"zebra": true}, "fps": 20}"#).unwrap();
        assert_eq!((full.dashboard.zebra, full.fps), (Some(true), 20.0));
        assert_eq!(serde_json::from_str::<crate::config::Config>("{}").unwrap().dashboard, DashConfig::default());
    }
}
