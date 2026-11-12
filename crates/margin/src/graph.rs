//! Dependency lanes for a section's todos, drawn with Flog symbols (rbong/flog-symbols: a font of
//! git-graph glyphs at U+F5D0–U+F60D, the set sulin keeps in tempura's `src/flog_chars.txt`).
//!
//! A todo names the steps it waits on with `after: [ids]`. Rows run top to bottom in the
//! section's order, so an edge runs from a step down to the later steps that wait on it, the way
//! `jj log` reads upside down. Each row gets a strip of cells: a lane per open edge, two cells per
//! lane (the glyph, then a connector), and a node where the row's own step sits. The node is the
//! todo's status glyph, so the component picks it (filled done, hollow open) from the flags here.

use std::collections::HashMap;

pub const H: char = '\u{F5D0}'; // ─
pub const V: char = '\u{F5D1}'; // │
pub const TL: char = '\u{F5D6}'; // ╭
pub const TR: char = '\u{F5D7}'; // ╮
pub const BL: char = '\u{F5D8}'; // ╰
pub const BR: char = '\u{F5D9}'; // ╯
// junctions on a horizontal run that carries on past them: a branch going down, a merge coming
// from above, each curving toward the node's side (the font has one glyph per side)
pub const DOWN_FROM_LEFT: char = '\u{F5E0}'; // ┬, the run arrives from the left
pub const DOWN_FROM_RIGHT: char = '\u{F5E1}'; // ┬, the run arrives from the right
pub const UP_TO_LEFT: char = '\u{F5E3}'; // ┴, the lane from above turns left
pub const UP_TO_RIGHT: char = '\u{F5E4}'; // ┴, the lane from above turns right
/// Where the node goes in `cells`.
pub const NODE: char = '\0';

/// One row of the graph: its cells (2 per lane) and the node's links.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Row {
    pub cells: Vec<char>,
    /// Per cell, the edge a line cell draws: (from, to, how far along it, 0 at `from`, 1 at `to`),
    /// so the scene can colour it between the two steps' colours.
    pub edges: Vec<Option<(String, String, f32)>>,
    pub node: usize,
    pub up: bool,
    pub down: bool,
    pub left: bool,
    pub right: bool,
}

/// Does a line leave this cell downward? A passing lane and a branch do (a merge and a
/// horizontal run end here), so a row taller than one line continues them with `V` below, and
/// the lanes stay joined to the next row. The node's own cell is the caller's: `Row::down`.
pub fn continues_down(c: char) -> bool {
    matches!(c, V | TL | TR | DOWN_FROM_LEFT | DOWN_FROM_RIGHT)
}

/// The node glyph for a row: Flog's circles come in filled / hollow pairs, one per combination of
/// stubs (none, R, L, LR, D, U, UD, RD, LD, UR, UL, UDR, UDL, LRD, LRU, LRUD).
pub fn node_glyph(r: &Row, filled: bool) -> char {
    let base = match (r.up, r.down, r.left, r.right) {
        (false, false, false, false) => 0xF5EE,
        (false, false, false, true) => 0xF5F0,
        (false, false, true, false) => 0xF5F2,
        (false, false, true, true) => 0xF5F4,
        (false, true, false, false) => 0xF5F6,
        (true, false, false, false) => 0xF5F8,
        (true, true, false, false) => 0xF5FA,
        (false, true, false, true) => 0xF5FC,
        (false, true, true, false) => 0xF5FE,
        (true, false, false, true) => 0xF600,
        (true, false, true, false) => 0xF602,
        (true, true, false, true) => 0xF604,
        (true, true, true, false) => 0xF606,
        (false, true, true, true) => 0xF608,
        (true, false, true, true) => 0xF60A,
        (true, true, true, true) => 0xF60C,
    };
    char::from_u32(base + if filled { 0 } else { 1 }).unwrap()
}

/// A graph glyph as drawn: itself in Flog mode; else its box-drawing twin (`│ ─ ╭ ╮ ╰ ╯`, and a
/// node as ● filled or ○ hollow), which every font has (config graph_glyphs, default "box").
pub fn glyph(c: char, flog: bool) -> char {
    if flog {
        return c;
    }
    match c as u32 {
        0xF5D0 | 0xF5D2 => '─',
        0xF5D1 | 0xF5D4 => '│',
        0xF5D6 => '╭',
        0xF5D7 => '╮',
        0xF5D8 => '╰',
        0xF5D9 => '╯',
        0xF5E0 | 0xF5E1 => '┬',
        0xF5E3 | 0xF5E4 => '┴',
        n @ 0xF5EE..=0xF60D => if n % 2 == 0 { '●' } else { '○' },
        _ => c,
    }
}

/// Lay out `nodes` (id and the ids it comes after, in display order). Edges to ids that aren't
/// earlier in the list are dropped: an edge can only run downward. Returns a row per node, all
/// the same width, or nothing when no node has an edge (the section draws as before).
pub fn layout(nodes: &[(String, Vec<String>)]) -> HashMap<String, Row> {
    let pos: HashMap<&str, usize> = nodes.iter().enumerate().map(|(i, (id, _))| (id.as_str(), i)).collect();
    // who waits on each node, in display order
    let mut waiters: Vec<Vec<String>> = vec![Vec::new(); nodes.len()];
    let mut any = false;
    for (i, (id, after)) in nodes.iter().enumerate() {
        for a in after {
            if let Some(&j) = pos.get(a.as_str()) {
                if j < i && !waiters[j].contains(id) {
                    waiters[j].push(id.clone());
                    any = true;
                }
            }
        }
    }
    if !any {
        return HashMap::new();
    }
    // a lane is an edge in flight: (waiting for, from, the row it left)
    let mut lanes: Vec<Option<(String, String, usize)>> = Vec::new();
    let mut rows: Vec<(String, Row)> = Vec::new();
    for (i, (id, _)) in nodes.iter().enumerate() {
        let incoming: Vec<usize> = (0..lanes.len()).filter(|&l| lanes[l].as_ref().map(|x| &x.0) == Some(id)).collect();
        let before: Vec<bool> = lanes.iter().map(|l| l.is_some()).collect();
        // where each live edge is at this row, before the row changes it
        let along = |e: &(String, String, usize)| -> (String, String, f32) {
            let to = pos.get(e.0.as_str()).copied().unwrap_or(i);
            let span = to.saturating_sub(e.2).max(1) as f32;
            (e.1.clone(), e.0.clone(), (i.saturating_sub(e.2)) as f32 / span)
        };
        let was_edge: Vec<Option<(String, String, f32)>> = lanes.iter().map(|l| l.as_ref().map(along)).collect();
        let k = match incoming.first() {
            Some(&k) => k,
            None => match lanes.iter().position(|l| l.is_none()) {
                Some(k) => k,
                None => {
                    lanes.push(None);
                    lanes.len() - 1
                }
            },
        };
        let merges: Vec<usize> = incoming.iter().skip(1).copied().collect();
        for &m in &merges {
            lanes[m] = None;
        }
        let outs = &waiters[i];
        lanes[k] = outs.first().map(|o| (o.clone(), id.clone(), i));
        let mut branches = Vec::new();
        for o in outs.iter().skip(1) {
            let j = match (k + 1..lanes.len()).find(|&l| lanes[l].is_none()) {
                Some(j) => j,
                None => {
                    lanes.push(None);
                    lanes.len() - 1
                }
            };
            lanes[j] = Some((o.clone(), id.clone(), i));
            branches.push(j);
        }
        let n = lanes.len().max(before.len());
        let mut cells = vec![' '; n * 2];
        let mut edges: Vec<Option<(String, String, f32)>> = vec![None; n * 2];
        // the edge a merge or branch end draws: a merge arrives (t = 1), a branch leaves (t = 0)
        let end_edge = |l: usize| -> Option<(String, String, f32)> {
            if merges.contains(&l) {
                was_edge.get(l).cloned().flatten().map(|(f, t, _)| (f, t, 1.0))
            } else if branches.contains(&l) {
                lanes[l].as_ref().map(|x| (x.1.clone(), x.0.clone(), 0.0))
            } else {
                None
            }
        };
        let ends: Vec<usize> = merges.iter().chain(branches.iter()).copied().collect();
        let lo = ends.iter().copied().chain([k]).min().unwrap_or(k);
        let hi = ends.iter().copied().chain([k]).max().unwrap_or(k);
        for l in 0..n {
            let was = before.get(l).copied().unwrap_or(false);
            cells[l * 2] = if l == k {
                NODE
            } else if merges.contains(&l) {
                // the farthest end is a corner; one the run passes on its way there is a junction,
                // or the line would stop at it and the ends beyond would hang unconnected
                match (l > k, l == hi || l == lo) {
                    (true, true) => BR,
                    (true, false) => UP_TO_LEFT,
                    (false, true) => BL,
                    (false, false) => UP_TO_RIGHT,
                }
            } else if branches.contains(&l) {
                match (l > k, l == hi || l == lo) {
                    (true, true) => TR,
                    (true, false) => DOWN_FROM_LEFT,
                    (false, true) => TL,
                    (false, false) => DOWN_FROM_RIGHT,
                }
            } else if was && lanes.get(l).map(|x| x.is_some()).unwrap_or(false) {
                V
            } else if lo < l && l < hi {
                H
            } else {
                ' '
            };
            // the connector after this lane: horizontal while inside the span between the node
            // and its farthest merge or branch
            if lo <= l && l < hi {
                cells[l * 2 + 1] = H;
            }
            // which edge each line cell draws: a passing lane its own, a corner its end's, a
            // horizontal run the end it leads to (the nearest end beyond it, away from the node)
            if l != k {
                edges[l * 2] = match cells[l * 2] {
                    V => was_edge.get(l).cloned().flatten(),
                    BR | BL | TR | TL | DOWN_FROM_LEFT | DOWN_FROM_RIGHT | UP_TO_LEFT | UP_TO_RIGHT => end_edge(l),
                    H => ends.iter().filter(|&&e| if e > k { e > l } else { e < l }).min_by_key(|&&e| e.abs_diff(l)).and_then(|&e| end_edge(e)),
                    _ => None,
                };
            }
            if cells[l * 2 + 1] == H {
                let toward = if l >= k { l + 1 } else { l };
                edges[l * 2 + 1] = ends.iter().filter(|&&e| if e > k { e >= toward } else { e <= toward }).min_by_key(|&&e| e.abs_diff(toward)).and_then(|&e| end_edge(e));
            }
        }
        let row = Row {
            cells,
            edges,
            node: k * 2,
            up: !incoming.is_empty(),
            down: !outs.is_empty(),
            left: lo < k,
            right: hi > k,
        };
        rows.push((id.clone(), row));
    }
    // one width for the section, so the text column lines up
    let w = rows.iter().map(|(_, r)| r.cells.iter().rposition(|&c| c != ' ').map(|p| p + 1).unwrap_or(0)).max().unwrap_or(0).max(1);
    let w = w + (w % 2);
    rows.into_iter()
        .map(|(id, mut r)| {
            r.cells.resize(w, ' ');
            r.edges.resize(w, None);
            (id, r)
        })
        .collect()
}

/// `layout` for rows that may name a row on either side (a table's graph column: a pull request
/// names its base, which may be listed above or below it). Each link becomes an edge from the
/// upper row down to the lower one; ids that aren't in the list are dropped.
pub fn layout_linked(nodes: &[(String, Vec<String>)]) -> HashMap<String, Row> {
    let pos: HashMap<&str, usize> = nodes.iter().enumerate().map(|(i, (id, _))| (id.as_str(), i)).collect();
    let mut down: Vec<(String, Vec<String>)> = nodes.iter().map(|(id, _)| (id.clone(), Vec::new())).collect();
    for (i, (id, links)) in nodes.iter().enumerate() {
        for l in links {
            match pos.get(l.as_str()) {
                Some(&j) if j < i => down[i].1.push(l.clone()),
                Some(&j) if j > i => down[j].1.push(id.clone()),
                _ => {}
            }
        }
    }
    layout(&down)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(id: &str, after: &[&str]) -> (String, Vec<String>) {
        (id.into(), after.iter().map(|s| s.to_string()).collect())
    }
    fn show(rows: &HashMap<String, Row>, order: &[&str]) -> Vec<String> {
        order
            .iter()
            .map(|id| {
                let r = &rows[*id];
                r.cells
                    .iter()
                    .map(|&c| match c {
                        NODE => if r.up && r.down { '┿' } else if r.down { '┯' } else if r.up { '┷' } else { '●' },
                        V => '│',
                        H => '─',
                        TL => '╭',
                        TR => '╮',
                        BL => '╰',
                        BR => '╯',
                        c => c,
                    })
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn no_edges_no_graph() {
        assert!(layout(&[n("a", &[]), n("b", &[])]).is_empty());
    }

    #[test]
    fn a_chain_is_one_lane() {
        let r = layout(&[n("a", &[]), n("b", &["a"]), n("c", &["b"])]);
        assert_eq!(show(&r, &["a", "b", "c"]), ["┯", "┿", "┷"]);
    }

    #[test]
    fn a_branch_and_its_merge() {
        // g1 → g2 → g3 → g5, and g1 → g4 → g5: the branch opens at g1 and merges back at g5
        let rows = layout(&[n("g1", &[]), n("g2", &["g1"]), n("g3", &["g2"]), n("g4", &["g1"]), n("g5", &["g3", "g4"])]);
        assert_eq!(show(&rows, &["g1", "g2", "g3", "g4", "g5"]), ["┯─╮", "┿ │", "┿ │", "│ ┿", "┷─╯"]);
        let s = show(&rows, &["g1", "g2", "g3", "g4", "g5"]);
        assert!(s[0].starts_with('┯') && s[0].ends_with('╮'), "{s:?}");
        assert!(rows["g1"].right && rows["g1"].down);
        assert!(rows["g5"].up && rows["g5"].right, "{s:?}");
        assert!(s[4].ends_with('╯'), "{s:?}");
        // every row the same width, so the text lines up
        let w = rows["g1"].cells.len();
        assert!(rows.values().all(|r| r.cells.len() == w));
    }

    #[test]
    fn a_run_past_a_branch_or_merge_joins_it() {
        // a fans out to b, c and d; e waits on all three
        let nodes: Vec<(String, Vec<String>)> = [("a", vec![]), ("b", vec!["a"]), ("c", vec!["a"]), ("d", vec!["a"]), ("e", vec!["b", "c", "d"])]
            .into_iter()
            .map(|(id, after)| (id.to_string(), after.into_iter().map(String::from).collect()))
            .collect();
        let rows = layout(&nodes);
        let text = |id: &str| rows[id].cells.iter().map(|&c| if c == NODE { '●' } else { glyph(c, false) }).collect::<String>();
        // the middle branch is a junction the run passes through; only the farthest is a corner
        assert_eq!(text("a").trim_end(), "●─┬─╮");
        assert_eq!(text("e").trim_end(), "●─┴─╯");
        assert_eq!(rows["a"].cells[2], DOWN_FROM_LEFT);
        assert_eq!(rows["e"].cells[2], UP_TO_LEFT);
    }

    #[test]
    fn a_link_may_name_a_row_above_or_below() {
        // a stack listed top first: each row names its base, the row under it
        let up = layout_linked(&[n("c", &["b"]), n("b", &["a"]), n("a", &[])]);
        assert_eq!(show(&up, &["c", "b", "a"]), ["┯", "┿", "┷"]);
        // the same picture as the todo order, and an unknown id is no edge
        let down = layout_linked(&[n("a", &["gone"]), n("b", &["a"]), n("c", &["b"])]);
        assert_eq!(show(&down, &["a", "b", "c"]), ["┯", "┿", "┷"]);
    }

    #[test]
    fn line_cells_know_their_edge_and_how_far_along() {
        // a → c skips b: the lane passes b's row halfway along a→c
        let rows = layout(&[n("a", &[]), n("b", &["a"]), n("c", &["a"])]);
        let b = &rows["b"];
        let passing: Vec<_> = b.edges.iter().flatten().collect();
        assert_eq!(passing.len(), 1, "{b:?}");
        assert_eq!((passing[0].0.as_str(), passing[0].1.as_str()), ("a", "c"));
        assert!((passing[0].2 - 0.5).abs() < 1e-6);
        // the branch corner on a's row leaves a (t = 0)
        assert!(rows["a"].edges.iter().flatten().any(|e| e.1 == "c" && e.2 == 0.0));
    }

    #[test]
    fn box_mode_has_a_twin_for_every_glyph() {
        assert_eq!(glyph(V, false), '│');
        assert_eq!(glyph(BR, false), '╯');
        assert_eq!(glyph('\u{F5FA}', false), '●');
        assert_eq!(glyph('\u{F5FB}', false), '○');
        assert_eq!(glyph(V, true), V, "flog mode draws Flog");
    }

    #[test]
    fn glyphs_pair_filled_and_hollow() {
        let r = Row { up: true, down: true, ..Default::default() };
        assert_eq!(node_glyph(&r, true), '\u{F5FA}');
        assert_eq!(node_glyph(&r, false), '\u{F5FB}');
        assert_eq!(node_glyph(&Row::default(), true), '\u{F5EE}');
    }
}
