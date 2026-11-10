//! The retained tree: per-node state that outlives each frame, keyed by a stable id.
//!
//! margin's documents are rewritten by the AI, but every component keeps its id. `reconcile`
//! compares the new list of (id, content hash) against the old one and reports what was kept,
//! changed, inserted, removed and moved. The app keeps one `NodeAnim` per id here: its animated
//! height (inserts ease open, removals collapse), its FLIP offset spring (reorders glide), its change
//! bar fade and its flash. Because state is keyed by id rather than by position, an AI update never
//! resets what the pane was doing with a node.

use crate::anim::{Easing, Motion, Spring, SpringCfg, Tween, Wake};
use std::collections::HashMap;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Reconcile {
    pub kept: Vec<String>,
    pub changed: Vec<String>,
    pub inserted: Vec<String>,
    pub removed: Vec<String>,
    /// ids whose position among their surviving siblings changed.
    pub moved: Vec<String>,
}

/// Diff two ordered (id, hash) lists. Order is compared after dropping inserted and removed ids,
/// so an insertion above a node doesn't count as moving it.
pub fn reconcile(old: &[(String, u64)], new: &[(String, u64)]) -> Reconcile {
    let om: HashMap<&str, u64> = old.iter().map(|(i, h)| (i.as_str(), *h)).collect();
    let nm: HashMap<&str, u64> = new.iter().map(|(i, h)| (i.as_str(), *h)).collect();
    let mut r = Reconcile::default();
    for (id, h) in new {
        match om.get(id.as_str()) {
            None => r.inserted.push(id.clone()),
            Some(oh) if oh != h => r.changed.push(id.clone()),
            Some(_) => r.kept.push(id.clone()),
        }
    }
    for (id, _) in old {
        if !nm.contains_key(id.as_str()) {
            r.removed.push(id.clone());
        }
    }
    let os: Vec<&str> = old.iter().map(|(i, _)| i.as_str()).filter(|i| nm.contains_key(i)).collect();
    let ns: Vec<&str> = new.iter().map(|(i, _)| i.as_str()).filter(|i| om.contains_key(i)).collect();
    // a node moved if it isn't on the longest common subsequence of the two orders
    let keep = lcs(&os, &ns);
    for id in &ns {
        if !keep.contains(id) {
            r.moved.push(id.to_string());
        }
    }
    r
}

fn lcs<'a>(a: &[&'a str], b: &[&'a str]) -> Vec<&'a str> {
    let (n, m) = (a.len(), b.len());
    let mut t = vec![vec![0u16; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            t[i][j] = if a[i] == b[j] { t[i + 1][j + 1] + 1 } else { t[i + 1][j].max(t[i][j + 1]) };
        }
    }
    let (mut i, mut j, mut out) = (0, 0, Vec::new());
    while i < n && j < m {
        if a[i] == b[j] {
            out.push(a[i]);
            i += 1;
            j += 1;
        } else if t[i + 1][j] >= t[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

/// Timings, in seconds. Kept together so sulin can tune the feel in one place.
#[derive(Clone, Copy, Debug)]
pub struct Timings {
    pub open: f32,
    pub close: f32,
    pub flash: f32,
    pub change_fade: f32,
}

impl Default for Timings {
    fn default() -> Self {
        Timings { open: 0.22, close: 0.18, flash: 0.45, change_fade: 2.4 }
    }
}

/// Everything animated about one node.
#[derive(Clone, Debug)]
pub struct NodeAnim {
    /// Fraction of the node's natural height shown, 0..1 (eased).
    pub reveal: Tween<f32>,
    /// FLIP offset in rows, sprung back to 0 after a move.
    pub offset: Spring,
    /// Change-bar intensity 1 → 0 once seen (None: no bar).
    pub change: Option<Tween<f32>>,
    /// True while a change waits for the pane to be seen.
    pub unseen: bool,
    /// Flash intensity 1 → 0 (a selection acknowledgement).
    pub flash: Option<Tween<f32>>,
    /// Last drawn y in page rows, for FLIP.
    pub last_y: Option<i32>,
    /// Being removed: kept until its collapse finishes.
    pub leaving: bool,
}

impl NodeAnim {
    pub fn present() -> NodeAnim {
        NodeAnim {
            reveal: Tween::settled(1.0),
            offset: Spring::new(0.0, SpringCfg::GENTLE),
            change: None,
            unseen: false,
            flash: None,
            last_y: None,
            leaving: false,
        }
    }
}

#[derive(Default)]
pub struct Retained {
    pub nodes: HashMap<String, NodeAnim>,
    pub order: Vec<(String, u64)>,
    pub timings: Timings,
}

impl Retained {
    pub fn new(t: Timings) -> Retained {
        Retained { timings: t, ..Default::default() }
    }

    pub fn get(&self, id: &str) -> Option<&NodeAnim> {
        self.nodes.get(id)
    }

    pub fn node(&mut self, id: &str) -> &mut NodeAnim {
        self.nodes.entry(id.to_string()).or_insert_with(NodeAnim::present)
    }

    /// Apply a new frame of (id, hash). `first` loads without animating anything (opening a pane
    /// shouldn't replay history). Returns the reconcile so the app can log or react.
    pub fn update(&mut self, new: Vec<(String, u64)>, now: f64, m: Motion, first: bool) -> Reconcile {
        let r = reconcile(&self.order, &new);
        let t = self.timings;
        if !first {
            for id in &r.inserted {
                let n = self.node(id);
                n.leaving = false;
                n.reveal = Tween::new(0.0, 1.0, now, t.open, Easing::EaseOut);
                n.change = Some(Tween::settled(1.0));
                n.unseen = true;
            }
            for id in &r.changed {
                let n = self.node(id);
                n.change = Some(Tween::settled(1.0));
                n.unseen = true;
            }
            for id in &r.removed {
                let n = self.node(id);
                n.leaving = true;
                n.reveal = Tween::new(1.0, 0.0, now, t.close, Easing::EaseIn);
            }
        } else {
            for (id, _) in &new {
                self.node(id);
            }
        }
        // drop finished leavers; keep their old position in the order so ghosts draw in place
        self.order = new;
        let _ = m;
        r
    }

    /// The pane has been seen: start fading every unseen change bar.
    pub fn mark_seen(&mut self, now: f64) -> Vec<String> {
        let mut seen = Vec::new();
        let fade = self.timings.change_fade;
        for (id, n) in self.nodes.iter_mut() {
            if n.unseen {
                n.unseen = false;
                n.change = Some(Tween::new(1.0, 0.0, now, fade, Easing::EaseIn));
                seen.push(id.clone());
            }
        }
        seen
    }

    pub fn any_unseen(&self) -> bool {
        self.nodes.values().any(|n| n.unseen)
    }

    pub fn flash(&mut self, id: &str, now: f64) {
        let d = self.timings.flash;
        self.node(id).flash = Some(Tween::new(1.0, 0.0, now, d, Easing::EaseOut));
    }

    /// Record where a node was drawn; if it jumped since last frame because of a reorder, kick
    /// its offset spring so it glides from the old place (FLIP).
    pub fn place(&mut self, id: &str, y: i32, moved: bool, now: f64, m: Motion) {
        let n = self.node(id);
        if let (Some(old), true) = (n.last_y, moved) {
            if old != y && m.on {
                n.offset.kick((old - y) as f32, now);
            }
        }
        n.last_y = Some(y);
    }

    /// Ask the frame clock for what's still moving; drop settled transient state.
    pub fn tick(&mut self, now: f64, m: Motion, wake: &mut Wake, fps: f32) {
        let mut live = false;
        self.nodes.retain(|_, n| !(n.leaving && n.reveal.done(now, m)));
        for n in self.nodes.values_mut() {
            if !n.reveal.done(now, m) || !n.offset.settled(now, m) {
                live = true;
            }
            if let Some(c) = &n.change {
                if !n.unseen && c.done(now, m) {
                    n.change = None;
                } else if !n.unseen {
                    live = true;
                }
            }
            if let Some(f) = &n.flash {
                if f.done(now, m) {
                    n.flash = None;
                } else {
                    live = true;
                }
            }
        }
        if live {
            wake.fps(now, fps);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ids(v: &[(&str, u64)]) -> Vec<(String, u64)> {
        v.iter().map(|(a, b)| (a.to_string(), *b)).collect()
    }
    #[test]
    fn reconcile_by_id() {
        let old = ids(&[("a", 1), ("b", 2), ("c", 3)]);
        let new = ids(&[("x", 9), ("a", 1), ("c", 4)]);
        let r = reconcile(&old, &new);
        assert_eq!(r.inserted, vec!["x"]);
        assert_eq!(r.changed, vec!["c"]);
        assert_eq!(r.kept, vec!["a"]);
        assert_eq!(r.removed, vec!["b"]);
        // inserting above doesn't count as moving
        assert!(r.moved.is_empty());
        let r = reconcile(&ids(&[("a", 1), ("b", 1), ("c", 1)]), &ids(&[("c", 1), ("a", 1), ("b", 1)]));
        assert_eq!(r.moved, vec!["c"]);
    }
    #[test]
    fn insert_reveals_and_settles_to_idle() {
        let m = Motion::default();
        let mut t = Retained::new(Timings::default());
        t.update(ids(&[("a", 1)]), 0.0, m, true);
        assert!(t.get("a").unwrap().change.is_none(), "first load animates nothing");
        t.update(ids(&[("a", 1), ("b", 1)]), 1.0, m, false);
        let b = t.get("b").unwrap();
        assert!(b.reveal.value(1.0, m) < 0.01 && b.unseen);
        let mut w = Wake::default();
        t.tick(1.05, m, &mut w, 30.0);
        assert!(w.at.is_some(), "an opening node keeps the clock running");
        // unseen change bars hold still: no frames while nobody looks
        let mut w = Wake::default();
        t.tick(5.0, m, &mut w, 30.0);
        assert!(w.at.is_none(), "idle at 0 fps while the change waits to be seen");
        t.mark_seen(5.0);
        let mut w = Wake::default();
        t.tick(5.1, m, &mut w, 30.0);
        assert!(w.at.is_some());
        let mut w = Wake::default();
        t.tick(10.0, m, &mut w, 30.0);
        assert!(w.at.is_none() && t.get("b").unwrap().change.is_none());
    }
    #[test]
    fn removal_keeps_a_ghost_until_collapsed() {
        let m = Motion::default();
        let mut t = Retained::new(Timings::default());
        t.update(ids(&[("a", 1), ("b", 1)]), 0.0, m, true);
        t.update(ids(&[("a", 1)]), 1.0, m, false);
        assert!(t.get("b").unwrap().leaving);
        let mut w = Wake::default();
        t.tick(2.0, m, &mut w, 30.0);
        assert!(t.get("b").is_none());
    }
}
