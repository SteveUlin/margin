//! The skill and its on-demand topics, embedded in the binary.
//!
//! `skill/SKILL.md` is the always-loaded part (what the pane is for, its shape, the turn loop, the
//! rules); `skill/topics/*.md` is reference Claude reads when it first needs a feature
//! (`margin help <topic>`). Both are templates: no one's name or paths are in them, and `render`
//! fills `{{user}}` (the config's `user_name`), `{{bin}}` (this binary) and `{{feeds}}` (the
//! example feed scripts) on the way out, so one source serves any machine and any user.

use std::path::{Path, PathBuf};

pub const SKILL: &str = include_str!("../../../skill/SKILL.md");

/// Topic name and its text. A topic's first line is `# name: what it covers`.
pub const TOPICS: &[(&str, &str)] = &[
    ("dashboards", include_str!("../../../skill/topics/dashboards.md")),
    ("handlers", include_str!("../../../skill/topics/handlers.md")),
    ("integrate", include_str!("../../../skill/topics/integrate.md")),
    ("lifecycle", include_str!("../../../skill/topics/lifecycle.md")),
    ("toasts", include_str!("../../../skill/topics/toasts.md")),
    ("diffs", include_str!("../../../skill/topics/diffs.md")),
    ("images", include_str!("../../../skill/topics/images.md")),
    ("graphs", include_str!("../../../skill/topics/graphs.md")),
    ("proposals", include_str!("../../../skill/topics/proposals.md")),
    ("subagents", include_str!("../../../skill/topics/subagents.md")),
    ("feeds", include_str!("../../../skill/topics/feeds.md")),
];

/// What the templates' placeholders stand for on this machine.
pub struct Ctx {
    pub user: String,
    pub bin: String,
    pub feeds: String,
}

impl Ctx {
    /// For this process: the configured name, the running binary, the feeds beside it.
    pub fn here() -> Ctx {
        Ctx::new(crate::who::name(), &bin_path())
    }

    pub fn new(user: &str, bin: &Path) -> Ctx {
        let user = if user.trim().is_empty() { "the user" } else { user.trim() };
        Ctx { user: user.into(), bin: bin.display().to_string(), feeds: feeds_dir(bin) }
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

/// Where the example feed scripts live for a binary: a checkout (`target/<profile>/margin` →
/// `feeds/`) or a nix install (`bin/margin` → `share/margin/feeds`).
fn feeds_dir(bin: &Path) -> String {
    let dir = bin.parent().unwrap_or(Path::new("."));
    for rel in ["../../feeds", "../share/margin/feeds"] {
        let p = dir.join(rel);
        if p.join("jj").is_file() {
            return p.canonicalize().unwrap_or(p).display().to_string();
        }
    }
    "<margin checkout>/feeds".into()
}

/// A template with its placeholders filled.
pub fn render(text: &str, c: &Ctx) -> String {
    text.replace("{{user}}", &c.user).replace("{{bin}}", &c.bin).replace("{{feeds}}", &c.feeds)
}

pub fn topic(name: &str) -> Option<&'static str> {
    TOPICS.iter().find(|(n, _)| *n == name).map(|(_, t)| *t)
}

/// One line per topic, from each topic's heading.
pub fn topic_list(c: &Ctx) -> String {
    let mut out = String::from("topics (margin help <topic>): reference the skill leaves out until it's needed\n");
    for (name, text) in TOPICS {
        let head = text.lines().next().unwrap_or("");
        let what = head.split_once(": ").map(|(_, w)| w).unwrap_or(head);
        out += &format!("  {name:<10} {}\n", render(what, c));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(topic_list(&ctx("")).lines().count(), TOPICS.len() + 1);
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
        for must in ["margin help dashboards", "margin example", "margin help\n  handlers"] {
            assert!(SKILL.contains(must), "the skill doesn't say {must:?}");
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

    /// The feed runner is legacy and off the main path: its topic is a short page that says so
    /// and names what replaces each trigger, and nothing else teaches it. The skill and every
    /// other topic keep a board current with a handler, an owner or a dataset.
    #[test]
    fn feeds_are_legacy_and_off_the_main_path() {
        let f = topic("feeds").unwrap();
        assert!(f.starts_with("# feeds: legacy: ") && f.lines().count() <= 25, "a short page titled as legacy ({} lines)", f.lines().count());
        for must in ["frozen", "will be removed", "`--every 30s`", "`--watch <path>`", "`--stream`", "a cron line or a systemd timer", "`watchexec`, `entr` or a systemd path unit", "follows the event stream", "margin emit <board>.refresh", "margin handler add", "margin owner add", "margin data append", "margin help handlers", "margin help integrate", "margin feed rm"] {
            assert!(f.contains(must), "feeds doesn't say {must:?}");
        }
        for gone in ["margin feed add", "margin feed run", "margin feed logs", "--min-gap", "health line is"] {
            assert!(!f.contains(gone), "feeds still teaches {gone:?}");
        }
        for (name, text) in TOPICS.iter().filter(|(n, _)| *n != "feeds") {
            let lines: Vec<&str> = text.lines().filter(|l| l.to_lowercase().contains("feed")).collect();
            let pointers = lines.iter().filter(|l| l.contains("legacy") && l.contains("margin help feeds")).count();
            assert!(lines.len() == pointers && pointers <= 1, "{name} still talks about feeds: {lines:?}");
        }
        let lines: Vec<&str> = SKILL.lines().filter(|l| l.to_lowercase().contains("feed")).collect();
        assert!(lines.len() == 1 && lines[0].starts_with("- `feeds`: legacy"), "the skill names feeds once, as legacy: {lines:?}");
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
