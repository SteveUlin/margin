//! Tabs: one pane, several artifacts. The pane's own artifact (the first argument of `margin
//! view`) keeps the tab set and the tab on screen in its view file (`View::tabs`, `View::tab`);
//! `margin tab` edits that file and the running pane follows it. Each tab is a whole artifact
//! with its own scroll, focus, folds and unseen changes (app.rs keeps a `Live` per tab); this
//! file holds what can be said without a terminal: the set and its edits, what a tab's badge
//! says, and the bar's layout.
//!
//! A tab may be a board of another project under the same home: the view file names it
//! `project:board` (a bare name is the pane's own project, so older view files read as they
//! did), and the pane reads, watches and writes it through that project's store.
//!
//! The bar sits on the pane's title line, where a single artifact's title would be:
//! `1 Work  2 Reviews +3 ●  3 Services ✗`. The tab on screen is a filled pill, the shape and
//! colors the title has in a pane without tabs; another tab shows how many blocks changed since it was last on
//! screen (`+3`, the change bars' orange), `●` when something in it waits on the person (an open
//! question, a diff without a verdict, their own open step, Claude waiting) and `✗` when
//! something in it failed, so a glance at the bar says where to look. When the tabs don't fit it
//! collapses to `‹ 2/5 title ›` followed by the other tabs that have something to say.

use crate::doc::{walk, Doc};
use crate::store::{State, Store, View};
use vellum::color::Palette;
use vellum::grid::Grid;
use vellum::layout::Rect;
use vellum::style::Style;
use vellum::text::{truncate, width};

/// What the bar says about one tab.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TabInfo {
    pub art: String,
    pub title: String,
    /// Blocks changed since this tab was last on screen.
    pub unseen: usize,
    /// Something in it waits on the person.
    pub needs: bool,
    /// Something in it failed.
    pub failed: bool,
    /// The project to name beside the title, already short (`short_project`): set for a tab
    /// from another project, and for a local tab whose title a tab from another project shares.
    /// Empty: nothing is shown.
    pub project: String,
}

/// A tab as the view file names it: `board` for a board of the pane's own project,
/// `project:board` for a board of another one. Board ids hold no `:`, so the last one divides.
pub fn split(entry: &str) -> (Option<&str>, &str) {
    match entry.rsplit_once(':') {
        Some((p, b)) if !p.is_empty() => (Some(p), b),
        _ => (None, entry),
    }
}

/// The view-file name of `board` in `project`, for a pane whose own project is `own`: a board of
/// the pane's own project is always the bare name, so old view files and new ones agree.
pub fn entry(own: &str, project: &str, board: &str) -> String {
    if project == own {
        board.to_string()
    } else {
        format!("{project}:{board}")
    }
}

/// A tab name as typed (`board`, `project:board`) as the view file would have it.
pub fn normalize(own: &str, name: &str) -> String {
    match split(name) {
        (Some(p), b) => entry(own, p, b),
        (None, b) => b.to_string(),
    }
}

/// The store and board id a tab entry names, seen from the pane's own store.
pub fn store_for(own: &Store, entry: &str) -> (Store, String) {
    let (p, b) = split(entry);
    (Store { home: own.home.clone(), project: p.unwrap_or(&own.project).to_string(), author: own.author.clone() }, b.to_string())
}

/// Which tab of `set` does `name` mean? The entry itself (`board`, `project:board`), else the
/// one tab from another project whose board has that bare name. Two such tabs is an error that
/// lists them.
pub fn locate(set: &[String], name: &str) -> Result<Option<usize>, String> {
    if let Some(i) = set.iter().position(|e| e == name) {
        return Ok(Some(i));
    }
    let hits: Vec<usize> = set.iter().enumerate().filter(|(_, e)| split(e).1 == name).map(|(i, _)| i).collect();
    match hits[..] {
        [] => Ok(None),
        [i] => Ok(Some(i)),
        _ => Err(format!("{name} names more than one tab: {}", hits.iter().map(|&i| set[i].as_str()).collect::<Vec<_>>().join(", "))),
    }
}

/// A project's name as a tab shows it: what is left after the leading `-` parts it shares with
/// the pane's own project (`home-me-work-api` beside `home-me-margin` is `work-api`), cut to 12
/// columns. The pane's own project is its last part.
pub fn short_project(own: &str, project: &str) -> String {
    let (a, b): (Vec<&str>, Vec<&str>) = (own.split('-').collect(), project.split('-').collect());
    let shared = a.iter().zip(&b).take_while(|(x, y)| x == y).count().min(b.len() - 1);
    truncate(&b[shared..].join("-"), 12).into_owned()
}

/// Does the doc wait on the person, and did something in it fail? The same rules as the pinned
/// head's "needs you" line (scene.rs `head_lines`), plus a `status` in state `fail`, a `row`
/// whose glyph is `fail`, and a table row with `fail` in a `state` column: the same red cross on
/// the board, so the same mark on its tab.
pub fn attention(doc: &Doc, state: &State) -> (bool, bool) {
    let (mut needs, mut failed) = (false, false);
    walk(&doc.blocks, &mut |b, parent, _| match b.kind.as_str() {
        "todo" => {
            let st = crate::components::status_of(state, b);
            if crate::components::yours(state, b) && !matches!(st.as_str(), "done" | "skipped") {
                needs = true;
            }
            if st == "blocked" {
                failed = true;
            }
        }
        "question" if !b.flag("applied") && !state.decisions.contains_key(&b.id) => needs = true,
        "diff" if !state.decisions.get(&b.id).map(|d| d.kind == "verdict").unwrap_or(false) => needs = true,
        "status" if b.s("state") == "fail" => failed = true,
        "row" if b.s("glyph") == "fail" => failed = true,
        "tr" => {
            // its table's columns of kind `state`, and this row's cells in them
            let cols = parent.and_then(|t| t.f.get("columns")).and_then(|c| c.as_array());
            let fails = cols.into_iter().flatten().filter(|c| c["kind"] == "state").filter_map(|c| c["key"].as_str()).any(|k| b.f.get("cells").map(|cs| cs[k] == "fail").unwrap_or(false));
            if fails {
                failed = true;
            }
        }
        _ => {}
    });
    (needs, failed)
}

/// Titles as the bar shows them: a prefix every title shares up to a ` · ` (a project's name) is
/// dropped, since it tells the tabs apart by nothing.
pub fn short_titles(titles: &[String]) -> Vec<String> {
    let Some(first) = titles.first() else { return Vec::new() };
    let cut = first.rfind(" · ").map(|i| &first[..i + " · ".len()]);
    match cut {
        Some(pre) if titles.len() > 1 && titles.iter().all(|t| t.starts_with(pre) && t.len() > pre.len()) => titles.iter().map(|t| t[pre.len()..].to_string()).collect(),
        _ => titles.to_vec(),
    }
}

/// The pane's tab set at start: the artifacts named on the command line when more than one is,
/// else what the view file remembers, else just the pane's own. Always begins with `primary`,
/// without repeats. Returns the set and the index of the tab on screen.
pub fn resolve(primary: &str, args: &[String], view: &View) -> (Vec<String>, usize) {
    let mut set: Vec<String> = vec![primary.to_string()];
    let from = if args.is_empty() { &view.tabs[..] } else { args };
    for a in from {
        if !set.contains(a) {
            set.push(a.clone());
        }
    }
    let active = set.iter().position(|a| *a == view.tab).unwrap_or(0);
    (set, active)
}

/// Write a tab set and the tab on screen into the pane's view: a set of one is stored as none, so
/// a single-artifact pane's view file reads as it always did.
pub fn store(view: &mut View, set: &[String], active: usize) {
    view.tabs = if set.len() > 1 { set.to_vec() } else { Vec::new() };
    view.tab = if set.len() > 1 { set.get(active).cloned().unwrap_or_default() } else { String::new() };
}

/// `margin tab <pane-artifact> add|rm|show <artifact>` on a view. Returns whether it changed.
pub fn edit(view: &mut View, primary: &str, op: &str, art: &str) -> Result<bool, String> {
    let (mut set, mut active) = resolve(primary, &[], view);
    let before = (view.tabs.clone(), view.tab.clone());
    // add takes the entry as given; rm and show also take a foreign tab's bare board name
    let at = if op == "add" { set.iter().position(|a| a == art) } else { locate(&set, art)? };
    match (op, at) {
        ("add", None) => set.push(art.to_string()),
        ("add", Some(_)) => {}
        ("rm", Some(0)) => return Err(format!("{art} is the pane's own board: close the pane instead (margin close {art})")),
        ("rm", Some(i)) => {
            set.remove(i);
            // the tab on screen went: its left neighbour comes forward
            if active >= i && active > 0 {
                active -= 1;
            }
        }
        ("rm", None) => return Err(format!("{art} is not a tab of {primary}")),
        ("show", Some(i)) => active = i,
        ("show", None) => return Err(format!("{art} is not a tab of {primary}; margin tab {primary} add {art}")),
        _ => return Err(format!("unknown tab command {op:?}: add, rm or show")),
    }
    store(view, &set, active);
    Ok((view.tabs.clone(), view.tab.clone()) != before)
}

/// A host's pane handle as the pane names its own view file: Zellij's `terminal_12` is `zj-12`,
/// the plain host's `pid 4242` is `tty`.
pub fn pane_key(handle: &str) -> String {
    match handle.strip_prefix("terminal_") {
        Some(n) => format!("zj-{n}"),
        None => "tty".into(),
    }
}

/// The panes that show `art` as a tab of another artifact, as their view files have it:
/// (the pane's own artifact, the pane's key). A view file outlives its pane, so the caller checks
/// the pane is still there (`Host::find` on the pane's own artifact).
pub fn hosts_of(st: &Store, art: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(st.dir()) else { return out };
    for e in rd.filter_map(|e| e.ok()) {
        let primary = e.file_name().to_string_lossy().into_owned();
        if primary == art {
            continue;
        }
        let Ok(views) = std::fs::read_dir(e.path().join("view")) else { continue };
        for v in views.filter_map(|v| v.ok()) {
            let name = v.file_name().to_string_lossy().into_owned();
            let Some(pane) = name.strip_suffix(".json") else { continue };
            if st.view(&primary, pane).tabs.iter().any(|t| t == art) {
                out.push((primary.clone(), pane.to_string()));
            }
        }
    }
    out.sort();
    out
}

/// One clickable run of the bar: (x, width, token). Tokens: `mg:tab/<index>`, `mg:tab/prev`,
/// `mg:tab/next`.
pub type Hit = (u16, u16, String);

fn badge(t: &TabInfo) -> Vec<(String, u8)> {
    let mut b = Vec::new();
    if t.unseen > 0 {
        b.push((format!("+{}", t.unseen), 0));
    }
    if t.needs {
        b.push(("●".to_string(), 1));
    }
    if t.failed {
        b.push(("✗".to_string(), 2));
    }
    b
}

fn badge_w(t: &TabInfo) -> usize {
    badge(t).iter().map(|(s, _)| width(s) + 1).sum()
}

/// The columns a tab's project takes after its title: a blank and the name, or nothing.
fn proj_w(t: &TabInfo) -> usize {
    if t.project.is_empty() {
        0
    } else {
        width(&t.project) + 1
    }
}

/// The widest title cap at which every tab fits in `room` columns as ` n title badges `, a
/// column apart; None when even the shortest cap doesn't (the bar collapses).
fn fit(infos: &[TabInfo], room: usize) -> Option<usize> {
    (7..=28usize).rev().find(|&cap| {
        // a cap (or its blank) each side, a blank inside each, the number and its blank
        let total: usize = infos.iter().enumerate().map(|(i, t)| 5 + width(&(i + 1).to_string()) + width(&truncate(&t.title, cap)) + proj_w(t) + badge_w(t)).sum::<usize>();
        total <= room
    })
}

fn put_badges(g: &mut Grid, mut x: u16, y: u16, t: &TabInfo, p: &Palette, ground: vellum::color::Rgb, clip: Rect) -> u16 {
    for (s, k) in badge(t) {
        let c = [p.orange, p.purple, p.red][k as usize];
        // (the blank is drawn, not skipped: on the title line a skipped cell shows the rule)
        x = g.put_clipped(x, y, " ", Style::default().on(ground), clip);
        x = g.put_clipped(x, y, &s, Style::fg(c).on(ground).bold_if(k > 0), clip);
    }
    x
}

/// Do the tabs fit `w` columns uncollapsed (their titles shortened if need be)?
pub fn fits(infos: &[TabInfo], w: u16) -> bool {
    fit(infos, w.saturating_sub(1) as usize).is_some()
}

/// Draw the bar on row `y` in the `w` columns from `x0` and return what can be clicked and the
/// column after its last cell. `focused`: the pane has the keyboard (the tab on screen then
/// carries the title pill's purple). `collapse`: the collapsed form whatever fits (glance mode).
#[allow(clippy::too_many_arguments)]
pub fn draw(g: &mut Grid, x0: u16, y: u16, infos: &[TabInfo], active: usize, w: u16, focused: bool, collapse: bool, p: &Palette) -> (Vec<Hit>, u16) {
    let mut hits = Vec::new();
    let clip = Rect::new(x0, y, w, 1);
    let room = w.saturating_sub(1) as usize;
    let n = infos.len();
    // one cell of the title line's rule stays visible between neighbors, so tabs read as separate
    if let Some(cap) = fit(infos, room.saturating_sub(n.saturating_sub(1))).filter(|_| !collapse) {
        let mut x = x0;
        for (i, t) in infos.iter().enumerate() {
            if i > 0 {
                x += 1;
            }
            let on = i == active;
            let xs = x;
            let head = format!(" {} {}", i + 1, truncate(&t.title, cap));
            let proj = if t.project.is_empty() { String::new() } else { format!(" {}", t.project) };
            if on {
                // the tab on screen: the title's pill, purple while the pane has the keyboard.
                // It has no badge: its changes are in front of him
                let (bg, fg) = if focused { (p.purple, p.bg0) } else { (p.bg3, p.fg) };
                x = vellum::parts::pill(g, x, y, &format!("{head}{proj} "), bg, fg, p.bg0, clip);
                // the project, dimmer than the title, in the pill's own cells (after its cap)
                g.put_clipped(xs + 1 + width(&head) as u16, y, &proj, Style::fg(fg.mix(bg, 0.45)).on(bg), clip);
            } else {
                // another tab: dim text in the same cells a pill would take (a blank where each
                // cap would be), so nothing shifts when the tab on screen changes
                x = g.put_clipped(x, y, " ", Style::default().on(p.bg0), clip);
                x = g.put_clipped(x, y, &format!(" {}", i + 1), Style::fg(p.grey0).on(p.bg0), clip);
                x = g.put_clipped(x, y, &format!(" {}", truncate(&t.title, cap)), Style::fg(p.grey1).on(p.bg0), clip);
                x = g.put_clipped(x, y, &proj, Style::fg(p.grey0).on(p.bg0), clip);
                x = put_badges(g, x, y, t, p, p.bg0, clip);
                x = g.put_clipped(x, y, "  ", Style::default().on(p.bg0), clip);
            }
            hits.push((xs, x - xs, format!("mg:tab/{i}")));
        }
        return (hits, x.min(x0 + w));
    }
    // collapsed: ‹ 2/5 title ›, then the other tabs that have something to say, while they fit
    let Some(cur) = infos.get(active) else { return (hits, x0) };
    let pos = format!("{}/{n}", active + 1);
    let others: Vec<(usize, &TabInfo)> = infos.iter().enumerate().filter(|(i, t)| *i != active && !badge(t).is_empty()).collect();
    // the chooser keeps at least 6 columns of title; what is left after it goes to the others
    let fixed = 3 + width(&pos) + 1 + 2 + proj_w(cur);
    // the tabs with something to say keep up to half the bar; the title on screen has the rest
    // (those of them that fit whole in that half; when not even the first does, only its mark)
    let mut reserve = 0;
    for (_, t) in &others {
        let need = width(&truncate(&t.title, 12)) + badge_w(t) + 2;
        if reserve + need > room / 2 {
            break;
        }
        reserve += need;
    }
    let reserve = if others.is_empty() { 0 } else { reserve + 3 };
    let tcap = room.saturating_sub(fixed + reserve).clamp(6, 24).min(room.saturating_sub(fixed).max(1));
    // (a blank first: on the title line the chooser must not touch the rule)
    let blank = Style::default().on(p.bg0);
    let mut x = g.put_clipped(x0, y, " ", blank, clip);
    x = g.put_clipped(x, y, "‹", Style::fg(p.grey1).on(p.bg0).bold(), clip);
    hits.push((x0, 3, "mg:tab/prev".into()));
    x = g.put_clipped(x, y, " ", blank, clip);
    x = g.put_clipped(x, y, &pos, Style::fg(if focused { p.purple } else { p.grey2 }).on(p.bg0).bold(), clip);
    x = g.put_clipped(x, y, " ", blank, clip);
    x = g.put_clipped(x, y, &truncate(&cur.title, tcap), Style::fg(p.fg).on(p.bg0).bold(), clip);
    if !cur.project.is_empty() {
        x = g.put_clipped(x, y, &format!(" {}", cur.project), Style::fg(p.grey0).on(p.bg0), clip);
    }
    let nx = g.put_clipped(x, y, " ", blank, clip);
    x = g.put_clipped(nx, y, "›", Style::fg(p.grey1).on(p.bg0).bold(), clip);
    hits.push((nx.saturating_sub(1), 2, "mg:tab/next".into()));
    let mut end = x;
    for (k, (i, t)) in others.iter().enumerate() {
        x = end + 2;
        let title = truncate(&t.title, 12);
        let need = width(&title) + badge_w(t);
        let more = others.len() - k - 1;
        // room for this one, and for the mark that stands in for the ones after it
        let tail = if more > 0 { 2 } else { 0 };
        if (x - x0) as usize + 1 + need + tail > room + 1 {
            g.put_clipped(end, y, "  ", blank, clip);
            end = g.put_clipped(x, y, "…", Style::fg(p.grey1).on(p.bg0), clip);
            break;
        }
        g.put_clipped(end, y, "  ", blank, clip);
        let xs = x;
        x = g.put_clipped(x, y, &title, Style::fg(p.grey1).on(p.bg0), clip);
        x = put_badges(g, x, y, t, p, p.bg0, clip);
        hits.push((xs, x - xs, format!("mg:tab/{i}")));
        end = x;
    }
    // a blank closes the bar, so its last mark doesn't touch the title line's rule
    let end = g.put_clipped(end, y, " ", blank, clip);
    (hits, end.min(x0 + w))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vellum::color::EVERFOREST;

    fn info(title: &str, unseen: usize, needs: bool, failed: bool) -> TabInfo {
        TabInfo { art: title.to_lowercase(), title: title.into(), unseen, needs, failed, project: String::new() }
    }

    fn bar(infos: &[TabInfo], active: usize, w: u16) -> (String, Vec<Hit>) {
        let mut g = Grid::new(w, 1);
        let (hits, end) = draw(&mut g, 1, 0, infos, active, w.saturating_sub(2), true, false, &EVERFOREST);
        assert!(end <= w, "the bar ends inside its columns");
        (g.to_text().trim_end().to_string(), hits)
    }

    /// The set comes from the command line when it names more than one artifact, else from the
    /// view file; the pane's own artifact is always first, and the tab on screen is remembered.
    #[test]
    fn tab_set_resolves_and_persists() {
        let mut v = View::default();
        assert_eq!(resolve("a", &[], &v), (vec!["a".to_string()], 0));
        let (set, active) = resolve("a", &["b".into(), "c".into(), "b".into(), "a".into()], &v);
        assert_eq!((set.clone(), active), (vec!["a".into(), "b".into(), "c".into()], 0));
        store(&mut v, &set, 2);
        assert_eq!((v.tabs.len(), v.tab.as_str()), (3, "c"));
        // a later start without arguments restores both
        assert_eq!(resolve("a", &[], &v), (set.clone(), 2));
        // through the file: the fields round-trip, and a single-artifact view writes neither
        let back: View = serde_json::from_str(&serde_json::to_string(&v).unwrap()).unwrap();
        assert_eq!(back, v);
        store(&mut v, &["a".to_string()], 0);
        let json = serde_json::to_string(&v).unwrap();
        assert!(!json.contains("tabs") && !json.contains("\"tab\""), "{json}");
    }

    /// add, rm and show; removing the tab on screen brings its left neighbour forward, and the
    /// pane's own artifact can't be removed.
    #[test]
    fn tab_edits() {
        let mut v = View::default();
        assert!(edit(&mut v, "a", "add", "b").unwrap());
        assert!(!edit(&mut v, "a", "add", "b").unwrap(), "adding a tab twice changes nothing");
        assert!(edit(&mut v, "a", "add", "c").unwrap());
        assert!(edit(&mut v, "a", "show", "c").unwrap());
        assert_eq!((v.tabs.clone(), v.tab.as_str()), (vec!["a".to_string(), "b".into(), "c".into()], "c"));
        assert!(edit(&mut v, "a", "rm", "c").unwrap());
        assert_eq!((v.tabs.clone(), v.tab.as_str()), (vec!["a".to_string(), "b".into()], "b"));
        assert!(edit(&mut v, "a", "rm", "a").is_err());
        assert!(edit(&mut v, "a", "rm", "zz").is_err() && edit(&mut v, "a", "show", "zz").is_err());
        // back to one: nothing of the tabs is left in the view
        assert!(edit(&mut v, "a", "rm", "b").unwrap());
        assert_eq!(v, View::default());
    }

    /// The view file names a tab of another project `project:board`; a bare name (every entry of
    /// an old view file) is a board of the pane's own project, and stays bare when written.
    #[test]
    fn a_tab_entry_names_its_project_and_old_files_read_as_before() {
        let old: View = serde_json::from_str(r#"{"tabs": ["a", "b"], "tab": "b"}"#).unwrap();
        assert_eq!(resolve("a", &[], &old), (vec!["a".to_string(), "b".into()], 1));
        assert_eq!((split("b"), split("work:reviews")), ((None, "b"), (Some("work"), "reviews")));
        assert_eq!((entry("own", "own", "b"), entry("own", "work", "reviews")), ("b".to_string(), "work:reviews".to_string()));
        assert_eq!((normalize("own", "own:b"), normalize("own", "work:b"), normalize("own", "b")), ("b".to_string(), "work:b".to_string(), "b".to_string()));
        let mut v = old.clone();
        assert!(edit(&mut v, "a", "add", "work:reviews").unwrap());
        assert!(edit(&mut v, "a", "show", "work:reviews").unwrap());
        let json = serde_json::to_string(&v).unwrap();
        assert!(json.contains(r#""tabs":["a","b","work:reviews"]"#) && json.contains(r#""tab":"work:reviews""#), "{json}");
        let back: View = serde_json::from_str(&json).unwrap();
        assert_eq!(resolve("a", &[], &back), (vec!["a".to_string(), "b".into(), "work:reviews".into()], 2));
        let st = Store::new("/h".into(), "own".into());
        let (f, board) = store_for(&st, "work:reviews");
        assert_eq!((f.project.as_str(), f.home == st.home, board.as_str()), ("work", true, "reviews"));
        assert_eq!(store_for(&st, "b").0.project, "own");
    }

    /// rm and show take `project:board`, or the bare board name while only one tab has it.
    #[test]
    fn a_foreign_tab_is_addressed_by_its_entry_or_an_unambiguous_bare_name() {
        let mut v = View::default();
        for t in ["work:reviews", "b", "work:b", "ops:deploys", "lab:deploys"] {
            edit(&mut v, "a", "add", t).unwrap();
        }
        assert!(edit(&mut v, "a", "show", "reviews").unwrap());
        assert_eq!(v.tab, "work:reviews");
        // the pane's own board of that name is the bare name's meaning
        assert!(edit(&mut v, "a", "show", "b").unwrap());
        assert_eq!(v.tab, "b");
        let e = edit(&mut v, "a", "rm", "deploys").unwrap_err();
        assert_eq!(e, "deploys names more than one tab: ops:deploys, lab:deploys");
        assert!(edit(&mut v, "a", "rm", "ops:deploys").unwrap());
        assert!(edit(&mut v, "a", "rm", "deploys").unwrap(), "one left: the bare name is enough");
        assert_eq!(v.tabs, ["a", "work:reviews", "b", "work:b"]);
        assert!(edit(&mut v, "a", "rm", "nope:reviews").is_err());
    }

    /// The project sits after the title, for the tabs that carry one, and takes room in the fit.
    #[test]
    fn a_foreign_tab_shows_its_project_after_the_title() {
        let mut infos = [info("Work", 0, false, false), info("Reviews", 2, false, false)];
        infos[1].project = "api".into();
        let (text, _) = bar(&infos, 0, 80);
        assert!(text.contains("2 Reviews api +2") && !text.contains("Work api"), "{text}");
        let (on, _) = bar(&infos, 1, 80);
        assert!(on.contains(" 2 Reviews api "), "{on}");
        assert_eq!(short_project("home-me-margin", "home-me-work-api"), "work-api");
        assert_eq!(short_project("home-me-margin", "boards"), "boards");
        assert_eq!(short_project("home-me-margin", "home-me-margin"), "margin");
    }

    /// Every tab with its number; another tab says how much changed and whether it needs him.
    #[test]
    fn bar_shows_each_tab_and_its_badges() {
        let infos = [info("Work", 2, false, false), info("Reviews", 3, true, false), info("Services", 0, false, true)];
        let (text, hits) = bar(&infos, 0, 80);
        assert_eq!(text, " \u{e0b6} 1 Work \u{e0b4}   2 Reviews +3 ●     3 Services ✗", "the tab on screen is a pill; the others keep a pill's cells, a cell apart");
        // the tab on screen carries no count: its changes are in front of him
        assert!(!text.contains("+2"), "{text}");
        assert_eq!(hits.iter().map(|h| h.2.as_str()).collect::<Vec<_>>(), ["mg:tab/0", "mg:tab/1", "mg:tab/2"]);
        // each hit covers its own label and no other's
        let at = |x: u16| hits.iter().find(|(hx, hw, _)| x >= *hx && x < hx + hw).map(|h| h.2.as_str());
        let col = |s: &str| text[..text.find(s).unwrap()].chars().count() as u16;
        assert_eq!(at(col("Work")), Some("mg:tab/0"));
        assert_eq!(at(col("Reviews")), Some("mg:tab/1"));
        assert_eq!(at(col("✗")), Some("mg:tab/2"));
    }

    /// Titles shorten before the bar collapses; collapsed, it is `‹ 2/5 title ›` and then the
    /// other tabs that have something to say.
    #[test]
    fn narrow_bar_shortens_then_collapses() {
        let infos = [
            info("Work tracker for the week", 0, false, false),
            info("Incoming reviews", 0, false, false),
            info("Background services", 4, false, true),
            info("Notes", 0, false, false),
            info("Deploys", 1, true, false),
        ];
        let (wide, _) = bar(&infos, 1, 120);
        assert!(wide.contains("Work tracker for the week") && wide.contains("5 Deploys +1 ●"), "{wide}");
        let (mid, _) = bar(&infos, 1, 80);
        assert!(mid.contains("1 Work t…") && mid.contains("2 Incomi…"), "{mid}");
        let (narrow, hits) = bar(&infos, 1, 64);
        assert!(narrow.starts_with("  ‹ 2/5 Incoming reviews ›"), "{narrow}");
        assert!(narrow.contains("Background … +4 ✗") && narrow.contains("Deploys +1 ●"), "{narrow}");
        let toks: Vec<&str> = hits.iter().map(|h| h.2.as_str()).collect();
        assert_eq!(toks, ["mg:tab/prev", "mg:tab/next", "mg:tab/2", "mg:tab/4"]);
        // no room for both: an ellipsis stands in for the ones left out
        let (tight, _) = bar(&infos, 1, 48);
        assert!(tight.starts_with("  ‹ 2/5 Incoming revi… ›  Background … +4 ✗") && tight.ends_with('…') && !tight.contains("Deploys"), "the title gives room to a tab with news: {tight}");
        // a strip: the chooser alone, the title cut to what is left
        let (strip, _) = bar(&infos, 1, 24);
        assert!(strip.starts_with("  ‹ 2/5 Incoming") && strip.contains('›'), "{strip}");
        assert!(strip.chars().count() <= 24, "{strip}");
    }

    #[test]
    fn shared_title_prefix_is_dropped() {
        let t = |v: &[&str]| short_titles(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(t(&["margin · goal", "margin · notes"]), ["goal", "notes"]);
        assert_eq!(t(&["margin · goal", "showcase"]), ["margin · goal", "showcase"]);
        assert_eq!(t(&["margin · goal"]), ["margin · goal"]);
    }

    /// What marks a tab: an open question needs him; a failed check is a failure.
    #[test]
    fn attention_reads_the_doc() {
        let doc: Doc = serde_json::from_value(serde_json::json!({"id": "a", "blocks": [
            {"type": "question", "id": "q", "text": "which?", "options": ["x", "y"]},
            {"type": "status", "id": "s", "text": "ci", "state": "pass"}]}))
        .unwrap();
        assert_eq!(attention(&doc, &State::default()), (true, false));
        let doc: Doc = serde_json::from_value(serde_json::json!({"id": "a", "blocks": [
            {"type": "status", "id": "s", "text": "ci", "state": "fail"}]}))
        .unwrap();
        assert_eq!(attention(&doc, &State::default()), (false, true));
    }

    /// A table row whose `state` cell is `fail` marks the tab as a `row` with that glyph does;
    /// the word in a text column, or another state, does not.
    #[test]
    fn a_failing_state_cell_marks_the_tab() {
        let table = |st: &str, name: &str| -> Doc {
            serde_json::from_value(serde_json::json!({"id": "a", "blocks": [{"type": "section", "id": "s", "children": [
                {"type": "table", "id": "t", "columns": [{"key": "st", "kind": "state"}, {"key": "name"}], "children": [
                    {"type": "tr", "id": "r1", "cells": {"st": "done", "name": "backup"}},
                    {"type": "tr", "id": "r2", "cells": {"st": st, "name": name}}]}]}]}))
            .unwrap()
        };
        assert_eq!(attention(&table("fail", "mailer"), &State::default()), (false, true));
        assert_eq!(attention(&table("done", "fail"), &State::default()), (false, false), "only a state column counts");
        assert_eq!(attention(&table("spin", "mailer"), &State::default()), (false, false));
        let row: Doc = serde_json::from_value(serde_json::json!({"id": "a", "blocks": [{"type": "row", "id": "r", "glyph": "fail", "text": "x"}]})).unwrap();
        assert_eq!(attention(&row, &State::default()), (false, true), "as a row with the glyph does");
    }

    #[test]
    fn pane_keys() {
        assert_eq!(pane_key("terminal_12"), "zj-12");
        assert_eq!(pane_key("pid 4242"), "tty");
    }
}
