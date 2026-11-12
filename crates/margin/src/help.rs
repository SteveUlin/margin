//! The reference topics, embedded in the binary, and the template renderer.
//!
//! `skill/topics/*.md` is reference read when a feature is first needed (`margin help <topic>`):
//! most of it is about the core (dashboards, handlers, datasets, the contract for outside tools),
//! and the topics in `WITH_CLAUDE` are about Claude Code sessions. `skill/SKILL.md`, the
//! always-loaded part a Claude session reads, belongs to the adapter (`claude::SKILL`); its
//! consistency with the topics is checked in this file's tests. Both are templates: no one's name or paths are in them, and `render`
//! fills `{{user}}` (the config's `user_name`), `{{bin}}` (this binary) and `{{providers}}` (the
//! shipped provider scripts) on the way out, so one source serves any machine and any user.

use std::path::{Path, PathBuf};

/// Topic name and its text. A topic's first line is `# name: what it covers`.
pub const TOPICS: &[(&str, &str)] = &[
    ("dashboards", include_str!("../../../skill/topics/dashboards.md")),
    ("handlers", include_str!("../../../skill/topics/handlers.md")),
    ("integrate", include_str!("../../../skill/topics/integrate.md")),
    ("contract", include_str!("../../../skill/topics/contract.md")),
    ("lifecycle", include_str!("../../../skill/topics/lifecycle.md")),
    ("toasts", include_str!("../../../skill/topics/toasts.md")),
    ("graphs", include_str!("../../../skill/topics/graphs.md")),
    ("diagrams", include_str!("../../../skill/topics/diagrams.md")),
    ("proposals", include_str!("../../../skill/topics/proposals.md")),
    ("subagents", include_str!("../../../skill/topics/subagents.md")),
];

/// The topics about Claude Code sessions: listed under their own heading, after the core's.
pub const WITH_CLAUDE: &[&str] = &["proposals", "subagents"];

/// What the templates' placeholders stand for on this machine.
pub struct Ctx {
    pub user: String,
    pub bin: String,
    /// The directory of the shipped provider scripts (jj, queue, turns, agents).
    pub providers: String,
}

impl Ctx {
    /// For this process: the configured name, the running binary, the provider scripts beside it.
    pub fn here() -> Ctx {
        Ctx::new(crate::who::name(), &bin_path())
    }

    pub fn new(user: &str, bin: &Path) -> Ctx {
        let user = if user.trim().is_empty() { "the user" } else { user.trim() };
        Ctx { user: user.into(), bin: bin.display().to_string(), providers: providers_dir(bin) }
    }
}

/// The running binary's absolute path. A nix build wraps the binary in a script that puts difft
/// on PATH (`bin/margin` execs `bin/.margin-wrapped`), so the wrapper is the one to name.
pub fn bin_path() -> PathBuf {
    let exe = undeleted(std::env::current_exe().unwrap_or_else(|_| PathBuf::from("margin")));
    if exe.file_name().and_then(|n| n.to_str()) == Some(".margin-wrapped") {
        let w = exe.with_file_name("margin");
        if w.is_file() {
            return w;
        }
    }
    exe
}

/// A running binary that was replaced on disk (a rebuild, an upgrade) reads its own path as
/// `/path/margin (deleted)`: the file to run is the one at that path now. A daemon that outlived
/// a rebuild could not start `margin handle open` until this was undone (2026-11-10).
fn undeleted(exe: PathBuf) -> PathBuf {
    match exe.to_str().and_then(|s| s.strip_suffix(" (deleted)")) {
        Some(p) if Path::new(p).is_file() => PathBuf::from(p),
        _ => exe,
    }
}

/// Where the shipped provider scripts live for a binary: a checkout (`target/<profile>/margin` →
/// `providers/`) or a nix install (`bin/margin` → `share/margin/providers`).
fn providers_dir(bin: &Path) -> String {
    let dir = bin.parent().unwrap_or(Path::new("."));
    for rel in ["../../providers", "../share/margin/providers"] {
        let p = dir.join(rel);
        if p.join("jj").is_file() {
            return p.canonicalize().unwrap_or(p).display().to_string();
        }
    }
    "<margin checkout>/providers".into()
}

/// A template with its placeholders filled.
pub fn render(text: &str, c: &Ctx) -> String {
    text.replace("{{user}}", &c.user).replace("{{bin}}", &c.bin).replace("{{providers}}", &c.providers)
}

pub fn topic(name: &str) -> Option<&'static str> {
    TOPICS.iter().find(|(n, _)| *n == name).map(|(_, t)| *t)
}

/// One line per topic, from each topic's heading.
pub fn topic_list(c: &Ctx) -> String {
    let mut out = String::from("topics (margin help <topic>): reference, read when a feature is first needed\n");
    let line = |(name, text): &(&str, &str)| {
        let head = text.lines().next().unwrap_or("");
        let what = head.split_once(": ").map(|(_, w)| w).unwrap_or(head);
        format!("  {name:<10} {}\n", render(what, c))
    };
    out.extend(TOPICS.iter().filter(|(n, _)| !WITH_CLAUDE.contains(n)).map(line));
    out += "with Claude Code:\n";
    out.extend(TOPICS.iter().filter(|(n, _)| WITH_CLAUDE.contains(n)).map(line));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::SKILL;

    fn ctx(user: &str) -> Ctx {
        Ctx::new(user, Path::new("/opt/margin/bin/margin"))
    }

    /// A binary replaced under a running process names the file that is there now.
    #[test]
    fn a_replaced_binary_resolves_to_the_file_at_its_path() {
        let me = std::env::current_exe().unwrap();
        assert_eq!(undeleted(PathBuf::from(format!("{} (deleted)", me.display()))), me);
        assert_eq!(undeleted(me.clone()), me);
        let gone = PathBuf::from("/nonexistent/margin (deleted)");
        assert_eq!(undeleted(gone.clone()), gone, "nothing at that path: left as it is");
    }

    #[test]
    fn rendered_skill_names_the_configured_user_and_leaves_no_placeholder() {
        let s = render(SKILL, &ctx("robin"));
        assert!(s.contains("robin owns decisions") && s.contains("robin's decisions"), "the name is in");
        assert!(s.contains("`/opt/margin/bin/margin`"), "the binary's path is in");
        assert!(!s.contains("{{"));
        for (name, t) in TOPICS {
            let t = render(t, &ctx("robin"));
            assert!(!t.contains("{{"), "{name}");
        }
    }

    /// The source carries no one's name, pronoun or home directory: the default render is for anyone.
    #[test]
    fn default_render_is_for_anyone() {
        let mut all = vec![("skill", render(SKILL, &ctx("")))];
        all.extend(TOPICS.iter().map(|(n, t)| (*n, render(t, &ctx("")))));
        for (name, s) in &all {
            let low = s.to_lowercase();
            assert!(!low.contains("sulin") && !low.contains("ulins"), "{name} names someone");
            assert!(!low.contains("~/margin") && !low.contains("/home/"), "{name} has a machine's path");
            for w in low.split(|c: char| !c.is_alphanumeric()) {
                assert!(!matches!(w, "he" | "him" | "his" | "she" | "her" | "hers"), "{name}: gendered pronoun {w:?}");
            }
        }
        assert!(all[0].1.contains("the user owns decisions"));
    }

    /// A template never starts a sentence with the name: "the user" there would need a capital.
    #[test]
    fn the_name_never_starts_a_sentence() {
        for (name, t) in std::iter::once(&("skill", SKILL)).chain(TOPICS) {
            for (i, _) in t.match_indices("{{user}}") {
                let before = t[..i].trim_end_matches([' ', '\n', '*', '-', '"', '`', '(']);
                let starts = before.is_empty() || before.ends_with(['.', '?', '!']) || t[..i].ends_with("\n\n");
                assert!(!starts, "{name}: a sentence starts with {{{{user}}}} near {:?}", &t[i.saturating_sub(40)..i]);
            }
        }
    }

    /// The skill stays short, teaches today's question model, and names every topic.
    #[test]
    fn skill_is_short_current_and_points_at_every_topic() {
        assert!(SKILL.lines().count() <= 150, "the always-loaded skill grew to {} lines", SKILL.lines().count());
        assert!(SKILL.contains("--suggest") && SKILL.contains("no default"));
        for gone in ["margin wait", "--default", "takes_at", "wait for me", "on timeout"] {
            assert!(!SKILL.contains(gone), "{gone:?} is the old question model");
        }
        for (name, text) in TOPICS {
            assert!(SKILL.contains(&format!("- `{name}`:")), "the skill doesn't name topic {name}");
            assert!(text.starts_with(&format!("# {name}: ")), "{name}'s heading");
        }
        assert_eq!(topic_list(&ctx("")).lines().count(), TOPICS.len() + 2);
        // the topics about sessions come last, under their own heading; the core's name no session
        let list = topic_list(&ctx(""));
        let (core, with) = list.split_once("with Claude Code:\n").expect("the heading");
        assert!(with.contains("  proposals ") && with.contains("  subagents ") && with.lines().count() == WITH_CLAUDE.len(), "{with}");
        assert!(core.contains("  handlers ") && !core.contains("Claude"), "{core}");
        assert!(topic("handlers").is_some() && topic("nope").is_none());
    }

    /// `margin help` lists every topic file in `skill/topics/`, under its own name, on one line.
    #[test]
    fn every_topic_file_is_listed() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skill/topics");
        let mut files: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().trim_end_matches(".md").to_string()).collect();
        files.sort();
        let mut names: Vec<String> = TOPICS.iter().map(|(n, _)| n.to_string()).collect();
        names.sort();
        assert_eq!(files, names, "a file in skill/topics/ is not in TOPICS, or the other way round");
        let list = topic_list(&ctx(""));
        for (name, _) in TOPICS {
            assert_eq!(list.lines().filter(|l| l.starts_with(&format!("  {name} "))).count(), 1, "{name}");
            assert!(name.len() <= 10, "{name} overflows the list's name column");
        }
    }

    /// The dashboard kit is what an AI that has never seen this repository builds a board from:
    /// the skill sends it to the topic and the examples, the topic names every component, the
    /// column kinds and the theme keys, and the two topics name each other.
    #[test]
    fn the_dashboard_kit_is_complete() {
        let flat = SKILL.split_whitespace().collect::<Vec<_>>().join(" ");
        for must in ["margin help dashboards", "margin example", "margin help handlers", "margin data"] {
            assert!(flat.contains(must), "the skill doesn't say {must:?}");
        }
        let d = topic("dashboards").unwrap();
        for kind in ["rule", "table", "tr", "runs", "row", "status", "link", "tabs", "section", "text", "bullet", "todo", "decided", "question", "claim"] {
            assert!(d.contains(&format!("**{kind}**")), "dashboards doesn't describe {kind}");
            assert!(crate::doc::SCHEMA.iter().any(|k| k.name == kind), "{kind} is not a component type");
        }
        for col in ["text", "num", "who", "id", "runs", "age", "state"] {
            assert!(d.contains(&format!("| `{col}`")), "dashboards doesn't list the column kind {col}");
        }
        for key in ["zebra", "zebra_color", "rule_glyph", "rule_color", "emphasis", "who.glyphs", "who.colors", "who.names", "runs.gap"] {
            assert!(d.contains(key), "dashboards doesn't name the theme key {key}");
        }
        for cmd in ["margin new", "margin apply", "margin handler add", "margin emit", "margin owner add", "margin data add", "margin open", "margin tab", "margin view", "margin purpose", "margin hints", "margin render", "margin example", "margin help handlers", "margin help integrate", "--project"] {
            assert!(d.contains(cmd), "dashboards doesn't show {cmd}");
        }
        // the script contract and the line grammar live with handlers
        let h = topic("handlers").unwrap();
        for must in ["--owns", "Exit 0", "jq -c", "\"op\": \"set\"", "\"op\": \"rm\"", "\"op\": \"move\"", "\"op\": \"add\"", "\"parent\"", "an array of ops", "\"toast\"", "\"emit\"", "Print the whole thing every run", "Write times, not ages", "margin handler run", "margin handler logs", "margin help integrate"] {
            assert!(h.contains(must), "handlers doesn't cover {must:?}");
        }
    }

    /// A pane shows what is true now: the skill's shape has no Done and no Decided section, says
    /// what replaces what, and carries the rules on wrapping, color and counted lists; its
    /// worked example builds no archive either.
    #[test]
    fn the_skill_teaches_a_pane_that_holds_the_present() {
        let flat = SKILL.split_whitespace().collect::<Vec<_>>().join(" ");
        for must in [
            "## The pane shows what is true now",
            "No Done section and no log of finished threads",
            "no archive of resolved questions",
            "never a \"superseded\" or \"earlier\" version",
            "History is in version control and the event log",
            "Before adding anything, ask what it replaces, and remove that.",
            "fold the answer into the content it affects",
            "Remove it when it is empty",
            "**Rules in force** (`section rules`)",
            "rewrite its bullet in place",
            "write the new text under the same id",
            "**One pane per chat.**",
            "**Authored text wraps, so never pre-truncate it.**",
            "whatever the pane cuts opens in full on focus",
            "**Color only where it tells things apart.**",
            "never for decoration",
            "bullets whose text starts `1.`, `2.`",
            "the pane draws the number in place of the bullet",
        ] {
            assert!(flat.contains(must), "the skill doesn't say {must:?}");
        }
        for gone in ["section done", "section decided", "\"Decided\"", "move that row to Decided", "one row in Done", "decided D"] {
            assert!(!SKILL.contains(gone), "the skill still teaches {gone:?}");
        }
        let example = SKILL.split("```fish\n").nth(1).unwrap().split("```").next().unwrap();
        for must in ["margin rm plan ask", "section rules title=\"Rules in force\"", "bullet r1 \"1. ", "margin set plan r1 text=\"1. ", "margin rm plan t1"] {
            assert!(example.contains(must), "the worked example doesn't show {must:?}");
        }
        assert!(!example.contains("…\"") && !example.to_lowercase().contains("done\"") && !example.contains("tone"), "no cut text, no Done section, no decoration");
    }

    /// The diagrams topic records the house style, and its two examples obey it: every line of
    /// an example is the same display width (vellum's `width`), at most 64 cells, with no tab;
    /// boxes have rounded corners and one space inside each side; an edge leaves through a joint
    /// and its head touches the target's border; a vertical edge's label sits one space to its
    /// right. The same holds with the fallback arrowheads in place of the Nerd Font carets.
    #[test]
    fn the_diagram_examples_obey_the_house_style() {
        use vellum::text::width;
        let d = topic("diagrams").unwrap();
        for must in ["At most 64 cells wide", "rounded light corners", "one space\n  inside each side", "through a joint", "touches the target's border", "above a horizontal edge, or breaking it", "one space to the right of a\n  vertical edge", "U+F0DA right, U+F0D9 left, U+F0D8 up, U+F0D7 down", "`► ◄ ▲ ▼`", "Pad every line to the same width", "No tabs, no emoji", "Prefer a tree, a numbered flow or a table"] {
            assert!(d.contains(must), "diagrams doesn't say {must:?}");
        }
        let examples: Vec<&str> = d.split("```text\n").skip(1).map(|c| c.split("\n```").next().unwrap()).collect();
        assert_eq!(examples.len(), 2, "a pipeline and a vertical stack");
        let (right, down) = ('\u{F0DA}', '\u{F0D7}');
        for (ex, head) in examples.iter().zip([right, down]) {
            let fallback = ex.replace('\u{F0DA}', "►").replace('\u{F0D9}', "◄").replace('\u{F0D8}', "▲").replace('\u{F0D7}', "▼");
            for text in [ex.to_string(), fallback] {
                let lines: Vec<&str> = text.lines().collect();
                let w = width(lines[0]);
                assert!(w <= 64 && lines.len() >= 4, "{w} cells wide:\n{text}");
                for l in &lines {
                    assert_eq!(width(l), w, "every line is {w} cells: {l:?}\n{text}");
                    assert!(!l.contains('\t') && l.chars().all(|c| width(&c.to_string()) == 1), "no tab, no wide character: {l:?}");
                }
            }
            // the rules, cell by cell (one character is one cell here, checked above)
            let g: Vec<Vec<char>> = ex.lines().map(|l| l.chars().collect()).collect();
            let at = |y: usize, x: usize| g.get(y).and_then(|r| r.get(x)).copied().unwrap_or(' ');
            let (mut heads, mut boxes) = (0, 0);
            for (y, row) in g.iter().enumerate() {
                for (x, &c) in row.iter().enumerate() {
                    match c {
                        '╭' => {
                            boxes += 1;
                            // the row under a top-left corner is the box's text: `│ text │` or `│ text ├`
                            let end = (x + 1..row.len()).find(|&i| g[y][i] == '╮').expect("a top-right corner");
                            let inner: String = g[y + 1][x + 1..end].iter().collect();
                            assert!(at(y + 1, x) == '│' && matches!(at(y + 1, end), '│' | '├'), "the box's sides: {inner:?}");
                            assert!(inner.starts_with(' ') && inner.ends_with(' ') && inner.trim().len() == inner.len() - 2, "one space inside each side: {inner:?}");
                            assert!(at(y + 2, x) == '╰' && at(y + 2, end) == '╯', "rounded corners all round");
                        }
                        '├' => assert_eq!(at(y, x + 1), '─', "an edge leaves the side joint"),
                        '┬' => assert_eq!(at(y + 1, x), '│', "an edge leaves the bottom joint"),
                        c if c == right => {
                            heads += 1;
                            assert!(at(y, x - 1) == '─' && at(y, x + 1) == '│', "the head touches the target's left border");
                        }
                        c if c == down => {
                            heads += 1;
                            assert!(at(y - 1, x) == '│' && at(y + 1, x) == '─', "the head touches the target's top border");
                            assert!(at(y - 1, x + 1) == ' ' && at(y - 1, x + 2).is_alphabetic(), "the label is one space right of the vertical edge");
                        }
                        _ => {}
                    }
                }
            }
            assert!(boxes == 3 && heads == 2 && ex.contains(head), "three boxes, two edges:\n{ex}");
        }
        assert!(examples[0].contains("emit") && examples[0].contains("── rows ──"), "one label above its edge, one breaking it");
        assert!(SKILL.contains("- `diagrams`:"));
    }

    /// The parked components (diff, image, dependency lanes, `demo live`) are not taught: the
    /// skill and the topics leave them out, but for the one line on lanes the graphs topic keeps.
    /// (A table's graph column is not parked: dashboards documents it.)
    /// Their schema entries stay, each marked.
    #[test]
    fn parked_components_are_not_taught() {
        assert!(topic("diffs").is_none() && topic("images").is_none());
        for (name, text) in std::iter::once(&("skill", SKILL)).chain(TOPICS) {
            for gone in ["margin diff", "margin image", "demo live", "a diff", "a PNG"] {
                assert!(!text.contains(gone), "{name} still teaches {gone:?}");
            }
            let lanes = text.lines().filter(|l| l.contains("\"after\": [") || l.contains("`after`")).count();
            // a table's graph column is taught (dashboards), and the graphs topic points at it
            let taught = match *name {
                "graphs" => 2,
                "dashboards" => lanes,
                _ => 0,
            };
            assert_eq!(lanes, taught, "{name}: lanes between todos are documented in one line of the graphs topic only");
        }
        let mark = "not part of the documented set; may be removed";
        for kind in ["diff", "image"] {
            assert!(crate::doc::kind(kind).unwrap().about.ends_with(mark), "{kind}");
        }
        assert!(crate::doc::kind("todo").unwrap().fields.iter().any(|f| f.0 == "after" && f.2.ends_with(mark)));
    }

    /// One way keeps a board current: a handler run by an event, an outside owner, or a dataset.
    /// The feed runner is gone, with its topic; no topic and no line of the skill names it, and
    /// the three topics that explain keeping a board current each say where the schedule lives.
    #[test]
    fn no_topic_mentions_a_feed() {
        assert!(topic("feeds").is_none());
        for (name, text) in std::iter::once(&("skill", SKILL)).chain(TOPICS) {
            let lines: Vec<&str> = text.lines().filter(|l| l.to_lowercase().contains("feed")).collect();
            assert!(lines.is_empty(), "{name} still talks about feeds: {lines:?}");
        }
        for (name, must) in [("handlers", "it has no timer and watches no file"), ("integrate", "cron"), ("dashboards", "margin handler add")] {
            assert!(topic(name).unwrap().contains(must), "{name} doesn't say {must:?}");
        }
        // the shipped provider scripts are found beside the binary, in a checkout or an install
        let c = Ctx::here();
        assert!(c.providers.ends_with("/providers"), "{}", c.providers);
        assert_eq!(render("{{providers}}/jj", &ctx("")), "<margin checkout>/providers/jj");
    }

    /// Datasets are documented where the binary sends a reader: the command's help names the
    /// dashboards heading, that section shows each binding and says when a dataset is the right
    /// tool, and the integrate topic has the two ways to publish (append from cron, register a file).
    #[test]
    fn datasets_are_documented_where_the_help_points() {
        assert!(crate::DATA_HELP.contains("margin help dashboards") && crate::DATA_HELP.contains("Numbers that change often"));
        let d = topic("dashboards").unwrap();
        let sec = d.split("## Numbers that change often").nth(1).expect("the heading").split("\n## ").next().unwrap();
        for must in ["margin data add", "margin data append", "--file", "--cap", "--expect-every", "\"data\": \"latency\"", "\"rows\": \"queue\"", "\"key\": \"id\"", "~[@latency.p95]", "~[@latency.p95:24]", "\"y\": \"outcome\"", "no new revision", "A dataset, or rows written by a handler or an owner?", "margin help integrate"] {
            assert!(sec.contains(must), "dashboards, Numbers that change often: {must:?} is missing");
        }
        let i = topic("integrate").unwrap();
        let sec = i.split("## Publish numbers for a board to draw").nth(1).expect("the heading").split("\n## ").next().unwrap();
        for must in ["margin data add", "margin data append", "crontab", "--file", "--as NAME", "never writes", "margin help dashboards"] {
            assert!(sec.contains(must), "integrate, Publish numbers: {must:?} is missing");
        }
        // the fields the topics name are the schema's
        for (kind, fields) in [("plot", &["data", "y", "last", "x"][..]), ("table", &["rows", "key"][..]), ("runs", &["data", "y", "last"][..])] {
            let k = crate::doc::kind(kind).unwrap();
            for f in fields {
                assert!(k.fields.iter().any(|x| x.0 == *f), "{kind} has no field {f}");
            }
        }
        assert!(SKILL.contains("margin data"), "the skill names the command once");
    }

    /// Every JSON block in the dashboards topic is a component the schema takes (or the theme).
    #[test]
    fn dashboards_topic_json_passes_the_schema() {
        let d = topic("dashboards").unwrap();
        let mut n = 0;
        for chunk in d.split("```json\n").skip(1) {
            let body = chunk.split("```").next().unwrap();
            let v: serde_json::Value = serde_json::from_str(body).unwrap_or_else(|e| panic!("{e}: {body}"));
            if v.get("dashboard").is_some() {
                let c: crate::theme::DashConfig = serde_json::from_value(v["dashboard"].clone()).unwrap();
                assert_eq!(c.zebra, Some(false));
                continue;
            }
            let b: crate::doc::Block = serde_json::from_value(v).unwrap();
            crate::doc::check_block(&b).unwrap_or_else(|e| panic!("{e}: {body}"));
            n += 1;
        }
        assert!(n >= 5, "only {n} component examples");
    }
}
