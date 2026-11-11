//! `margin init`: make a machine ready in one command.
//!
//! Three things have to be true before Claude Code and a pane talk to each other, and each used to
//! be a hand edit: margin knows the person's name (`user_name` in the config), Claude has the
//! skill (`<claude-dir>/skills/margin/SKILL.md`, rendered from the template in this binary), and
//! Claude Code runs this binary's hooks (`<claude-dir>/settings.json`). `plan` works out the
//! changes and `run` writes them; a dry run stops after `plan`. Everything is decided before the
//! first write, so a settings file that won't parse leaves the machine untouched.

use crate::help;
#[cfg(test)]
use crate::rewake;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

/// One hook `init` installs: the Claude Code event, the `margin hook <kind>` it runs, whether its
/// group carries `"matcher": "*"` (the tool events do), its timeout in seconds, and whether it is
/// an `asyncRewake` hook (run in the background; exit code 2 wakes Claude with its stderr). The
/// shape Claude Code reads:
/// `"hooks": {"<Event>": [{"matcher": "*", "hooks": [{"type": "command", "command": "…", "timeout": 5}]}]}`.
pub struct Hook {
    pub event: &'static str,
    pub kind: &'static str,
    pub matcher: bool,
    pub timeout: u64,
    pub rewake: bool,
}

const fn hook(event: &'static str, kind: &'static str, matcher: bool) -> Hook {
    Hook { event, kind, matcher, timeout: 5, rewake: false }
}

pub const HOOKS: &[Hook] = &[
    hook("UserPromptSubmit", "prompt", false),
    hook("PostToolUse", "post-tool", true),
    hook("PreToolUse", "pre-tool", true),
    hook("Stop", "stop", false),
    // beside `stop`: waits for sulin to finish in the pane, then wakes an idle Claude (rewake.rs).
    // The timeout is the wait's ceiling; rewake::DEADLINE ends it a little sooner
    Hook { event: "Stop", kind: "rewake", matcher: false, timeout: 3600, rewake: true },
    hook("SessionStart", "start", false),
    hook("PermissionRequest", "permission", true),
    hook("Notification", "notify", false),
    // the session is over: its runtime files go and the pane stops showing it as live (a crash
    // runs no hook: `margin doctor` and the readers' own liveness checks cover that)
    hook("SessionEnd", "end", false),
];

impl Hook {
    /// The settings entry for this hook run by `bin`.
    pub fn entry(&self, bin: &str) -> Value {
        let mut h = json!({"type": "command", "command": hook_command(bin, self.kind)});
        if self.rewake {
            h["asyncRewake"] = json!(true);
        }
        h["timeout"] = json!(self.timeout);
        h
    }
    /// How the report names it: the event, and the kind when the event has more than one hook.
    fn name(&self) -> String {
        if HOOKS.iter().filter(|h| h.event == self.event).count() > 1 && HOOKS.iter().find(|h| h.event == self.event).map(|h| h.kind) != Some(self.kind) {
            format!("{} {}", self.event, self.kind)
        } else {
            self.event.to_string()
        }
    }
}

pub struct Opts {
    /// Claude Code's directory (`~/.claude`): `settings.json` and `skills/` live in it.
    pub claude_dir: PathBuf,
    /// margin's config file.
    pub config: PathBuf,
    /// The binary the hooks will run.
    pub bin: PathBuf,
    /// `--user-name`: written to the config. None: keep what the config has.
    pub user_name: Option<String>,
    pub dry_run: bool,
}

/// One file `init` wants to write, and what to say about it.
struct Write {
    path: PathBuf,
    text: String,
    /// copy the existing file here first (the first time init changes someone's settings)
    backup: Option<PathBuf>,
    /// the path is a symlink: replace the link itself, never write through it
    unlink: bool,
}

pub struct Plan {
    writes: Vec<Write>,
    /// the report, one line per thing looked at
    pub lines: Vec<String>,
    pub warnings: Vec<String>,
    pub user: String,
}

/// The command a hook runs. A path with a space or a shell character is single-quoted.
fn hook_command(bin: &str, kind: &str) -> String {
    let plain = bin.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+@:,".contains(c));
    let bin = if plain { bin.to_string() } else { format!("'{}'", bin.replace('\'', "'\\''")) };
    format!("{bin} hook {kind}")
}

/// Is this command some margin binary's `hook <kind>`? Whatever path it has: that is how a
/// re-run finds the entry an earlier install (or a hand edit) left, and updates it in place.
fn is_margin_hook(command: &str, kind: &str) -> bool {
    let Some(prog) = command.trim().strip_suffix(kind).map(str::trim_end).and_then(|c| c.strip_suffix("hook")) else { return false };
    if !prog.ends_with(' ') {
        return false;
    }
    let prog = prog.trim().trim_matches(['\'', '"']);
    matches!(Path::new(prog).file_name().and_then(|n| n.to_str()), Some("margin") | Some(".margin-wrapped"))
}

/// Merge margin's hooks into a settings document. Touches only entries that run `margin hook …`:
/// updates the first one per event to `bin`, drops any further copies, appends a group when there
/// is none. Returns (added, updated) event names; both empty means nothing changed.
pub fn merge_hooks(settings: &mut Value, bin: &str) -> Result<(Vec<String>, Vec<String>), String> {
    let root = settings.as_object_mut().ok_or("settings.json isn't a JSON object")?;
    let hooks = root.entry("hooks").or_insert_with(|| json!({}));
    let hooks = hooks.as_object_mut().ok_or("settings.json: \"hooks\" isn't an object")?;
    let (mut added, mut updated) = (Vec::new(), Vec::new());
    for spec in HOOKS {
        let (event, kind) = (spec.event, spec.kind);
        let want = hook_command(bin, kind);
        let groups = hooks.entry(event).or_insert_with(|| json!([]));
        let groups = groups.as_array_mut().ok_or_else(|| format!("settings.json: hooks.{event} isn't an array"))?;
        let (mut found, mut changed) = (false, false);
        for g in groups.iter_mut() {
            let Some(list) = g.get_mut("hooks").and_then(|h| h.as_array_mut()) else { continue };
            list.retain_mut(|h| {
                let ours = h.get("command").and_then(|c| c.as_str()).map(|c| is_margin_hook(c, kind)).unwrap_or(false);
                if !ours {
                    return true;
                }
                if found {
                    changed = true; // a second copy: one is enough
                    return false;
                }
                found = true;
                if h["command"] != json!(want) {
                    h["command"] = json!(want);
                    changed = true;
                }
                // a waiting hook without these would hold the turn's end, or be cut off at 5 s;
                // the other hooks keep whatever timeout the person gave them
                if spec.rewake && (h["asyncRewake"] != json!(true) || h["timeout"] != json!(spec.timeout)) {
                    h["asyncRewake"] = json!(true);
                    h["timeout"] = json!(spec.timeout);
                    changed = true;
                }
                true
            });
        }
        // a group our dedupe emptied goes; groups that were already empty are someone else's
        if changed {
            groups.retain(|g| g.get("hooks").and_then(|h| h.as_array()).map(|l| !l.is_empty()).unwrap_or(true));
        }
        if !found {
            let mut g = Map::new();
            if spec.matcher {
                g.insert("matcher".into(), json!("*"));
            }
            g.insert("hooks".into(), json!([spec.entry(bin)]));
            groups.push(Value::Object(g));
            added.push(spec.name());
        } else if changed {
            updated.push(spec.name());
        }
    }
    Ok((added, updated))
}

/// Does a settings file run margin hooks? (For the note about a second copy in the project.)
fn has_margin_hooks(path: &Path) -> bool {
    let Some(v) = std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()) else { return false };
    HOOKS.iter().any(|Hook { event, kind, .. }| {
        v["hooks"][event].as_array().into_iter().flatten().any(|g| g["hooks"].as_array().into_iter().flatten().any(|h| h["command"].as_str().map(|c| is_margin_hook(c, kind)).unwrap_or(false)))
    })
}

/// Work out every change without making one.
pub fn plan(o: &Opts) -> Result<Plan, String> {
    let mut p = Plan { writes: vec![], lines: vec![], warnings: vec![], user: String::new() };
    let read = |path: &Path| std::fs::read_to_string(path).ok();

    // 1. the name
    let name = o.user_name.as_deref().map(str::trim).filter(|n| !n.is_empty());
    match name {
        Some(n) => {
            match crate::config::with_key(read(&o.config).as_deref(), "user_name", n).map_err(|e| format!("{}: {e}", o.config.display()))? {
                Some(text) => {
                    p.lines.push(format!("config   {}: user_name = {n:?}", o.config.display()));
                    p.writes.push(Write { path: o.config.clone(), text, backup: None, unlink: false });
                }
                None => p.lines.push(format!("config   {}: user_name is already {n:?}", o.config.display())),
            }
            p.user = n.into();
        }
        None => {
            p.user = crate::config::read_key(&o.config, "user_name").filter(|n| !n.trim().is_empty()).unwrap_or_default();
            if p.user.is_empty() {
                p.lines.push("config   no user_name set: Claude is told about \"the user\" (name them with --user-name NAME)".into());
            } else {
                p.lines.push(format!("config   {}: user_name is {:?}", o.config.display(), p.user));
            }
        }
    }

    // 2. the skill
    let skill_path = o.claude_dir.join("skills/margin/SKILL.md");
    let skill = help::render(help::SKILL, &help::Ctx::new(&p.user, &o.bin));
    let is_link = std::fs::symlink_metadata(&skill_path).map(|m| m.file_type().is_symlink()).unwrap_or(false);
    let n = skill.lines().count();
    match read(&skill_path) {
        Some(old) if old == skill && !is_link => p.lines.push(format!("skill    {}: up to date", skill_path.display())),
        old => {
            let how = match (&old, is_link) {
                (_, true) => "replaced a symlink with the rendered skill".to_string(),
                (Some(old), _) => format!("replaced a stale copy ({} lines → {n})", old.lines().count()),
                (None, _) => format!("installed ({n} lines)"),
            };
            p.lines.push(format!("skill    {}: {how}", skill_path.display()));
            p.writes.push(Write { path: skill_path, text: skill, backup: None, unlink: is_link });
        }
    }

    // 3. the hooks
    let settings_path = o.claude_dir.join("settings.json");
    let old = read(&settings_path);
    let mut settings: Value = match old.as_deref() {
        Some(t) if !t.trim().is_empty() => serde_json::from_str(t).map_err(|e| format!("{}: not JSON ({e}); nothing was changed", settings_path.display()))?,
        _ => json!({}),
    };
    let (added, updated) = merge_hooks(&mut settings, &o.bin.display().to_string()).map_err(|e| format!("{}: {e}; nothing was changed", settings_path.display()))?;
    if added.is_empty() && updated.is_empty() {
        p.lines.push(format!("hooks    {}: up to date ({} hooks run {})", settings_path.display(), HOOKS.len(), o.bin.display()));
    } else {
        let bak = settings_path.with_file_name("settings.json.bak");
        // one backup, of the file as it was before init first touched it
        let backup = (old.is_some() && !bak.exists()).then_some(bak);
        let mut what = Vec::new();
        if !added.is_empty() {
            what.push(format!("added {} ({})", added.len(), added.join(", ")));
        }
        if !updated.is_empty() {
            what.push(format!("updated {} to this binary ({})", updated.len(), updated.join(", ")));
        }
        p.lines.push(format!("hooks    {}: {}", settings_path.display(), what.join("; ")));
        if let Some(b) = &backup {
            p.lines.push(format!("         backup of the file as it was: {}", b.display()));
        }
        p.writes.push(Write { path: settings_path.clone(), text: serde_json::to_string_pretty(&settings).unwrap() + "\n", backup, unlink: false });
    }
    p.lines.push(format!("binary   {}", o.bin.display()));

    // warnings
    let bin = o.bin.display().to_string();
    if bin.contains("/target/debug/") {
        p.warnings.push("this is a debug build: the hooks run on every prompt and tool call. Build with `cargo build --release` and run `target/release/margin init` to point them at the fast one.".into());
    }
    if bin.starts_with("/nix/store/") {
        p.warnings.push("this binary is in the nix store: its path changes with every rebuild. Run init through a stable path (e.g. ~/.nix-profile/bin/margin, or --bin PATH).".into());
    }
    if let Ok(cwd) = std::env::current_dir() {
        for f in ["settings.json", "settings.local.json"] {
            let proj = cwd.join(".claude").join(f);
            if proj != settings_path && has_margin_hooks(&proj) {
                p.warnings.push(format!("{} also runs margin hooks: a session started here would run both sets. Remove the margin entries there.", proj.display()));
            }
        }
    }
    Ok(p)
}

/// Write a file through a temporary beside it, so a crash never leaves half of one.
fn write_atomic(w: &Write) -> Result<(), String> {
    let err = |e: std::io::Error| format!("{}: {e}", w.path.display());
    if let Some(dir) = w.path.parent() {
        std::fs::create_dir_all(dir).map_err(err)?;
    }
    if let Some(b) = &w.backup {
        std::fs::copy(&w.path, b).map_err(err)?;
    }
    if w.unlink {
        std::fs::remove_file(&w.path).map_err(err)?;
    }
    let tmp = w.path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, &w.text).map_err(err)?;
    std::fs::rename(&tmp, &w.path).map_err(err)
}

/// How to get a first pane, for where this was run.
fn next_steps(o: &Opts) -> Vec<String> {
    let on_path = |name: &str| std::env::var_os("PATH").map(|p| std::env::split_paths(&p).any(|d| d.join(name).is_file())).unwrap_or(false);
    let m = if on_path("margin") { "margin".to_string() } else { o.bin.display().to_string() };
    let mut v = vec!["next:".to_string()];
    if m != "margin" {
        v.push(format!("  - `margin` isn't on PATH: add {} to it, or use the full path as below", o.bin.parent().unwrap_or(Path::new(".")).display()));
    }
    v.push("  - restart running Claude Code sessions: hooks and skills load at session start".into());
    // `margin demo` draws a diff, so it needs difftastic; without it the first pane is a one-liner
    let (make, art) = if crate::diffblock::difft_bin().is_ok() {
        (format!("{m} demo"), "plan")
    } else {
        v.push(format!("  - difftastic (difft) isn't installed: `{m} demo` and `{m} diff` need it; the pane below doesn't"));
        (format!("{m} new hello && {m} add hello - todo t1 \"my first step\""), "hello")
    };
    let in_zellij = std::env::var("ZELLIJ_SESSION_NAME").map(|s| !s.is_empty()).unwrap_or(false);
    if in_zellij {
        v.push(format!("  - a first pane, here in Zellij:   {make} && {m} open {art}"));
    } else if on_path("zellij") {
        v.push(format!("  - a first pane, with Zellij:   {m} session    then, inside it:   {make} && {m} open {art}"));
        v.push(format!("    or in any terminal:   {make} && {m} view {art}"));
    } else {
        v.push(format!("  - a first pane (no Zellij found, so any terminal beside the chat):   {make} && {m} view {art}"));
    }
    v.push("  - then, in Claude Code: \"start a margin for this\"; `margin help` lists the reference topics".into());
    v
}

/// Plan, report, and (unless it's a dry run) write. Returns what to print.
pub fn run(o: &Opts) -> Result<String, String> {
    let p = plan(o)?;
    let mut out = vec![if o.dry_run { "margin init (dry run: nothing is written; a real run would do this)".to_string() } else { "margin init".to_string() }];
    out.extend(p.lines.iter().map(|l| format!("  {l}")));
    if !o.dry_run {
        for w in &p.writes {
            write_atomic(w)?;
        }
    }
    out.push(match (o.dry_run, p.writes.len()) {
        (_, 0) => "nothing to change".to_string(),
        (true, n) => format!("would write {n} file{}: {}", if n == 1 { "" } else { "s" }, p.writes.iter().map(|w| w.path.display().to_string()).collect::<Vec<_>>().join(", ")),
        (false, n) => format!("wrote {n} file{}", if n == 1 { "" } else { "s" }),
    });
    out.extend(p.warnings.iter().map(|w| format!("warning: {w}")));
    out.extend(next_steps(o));
    Ok(out.join("\n") + "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway home: `<tmp>/margin-init-<pid>-<name>`, removed first so a re-run starts clean.
    fn opts(name: &str) -> Opts {
        let dir = std::env::temp_dir().join(format!("margin-init-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Opts { claude_dir: dir.join("claude"), config: dir.join("config/margin/config.json"), bin: PathBuf::from("/opt/margin/bin/margin"), user_name: None, dry_run: false }
    }
    fn settings(o: &Opts) -> Value {
        serde_json::from_str(&std::fs::read_to_string(o.claude_dir.join("settings.json")).unwrap()).unwrap()
    }
    fn skill(o: &Opts) -> String {
        std::fs::read_to_string(o.claude_dir.join("skills/margin/SKILL.md")).unwrap()
    }
    /// The group in the settings that runs `h`.
    fn group<'a>(v: &'a Value, h: &Hook) -> &'a Value {
        v["hooks"][h.event].as_array().unwrap().iter().find(|g| g["hooks"][0]["command"].as_str().map(|c| is_margin_hook(c, h.kind)).unwrap_or(false)).unwrap_or(&Value::Null)
    }
    fn done(o: &Opts) {
        let _ = std::fs::remove_dir_all(o.claude_dir.parent().unwrap());
    }

    #[test]
    fn fresh_install_writes_name_skill_and_hooks() {
        let mut o = opts("fresh");
        o.user_name = Some("robin".into());
        let out = run(&o).unwrap();
        assert!(out.contains("installed (") && out.contains("added 9") && out.contains("SessionEnd") && out.contains("wrote 3 files"), "{out}");
        assert_eq!(crate::config::read_key(&o.config, "user_name").as_deref(), Some("robin"));
        let s = skill(&o);
        assert!(s.contains("robin owns decisions") && !s.contains("{{") && s.contains("/opt/margin/bin/margin"));
        // the shape Claude Code reads, the same as the hand-written one it replaces
        let v = settings(&o);
        assert_eq!(v["hooks"].as_object().unwrap().len(), 8, "eight events; Stop has two hooks");
        assert_eq!(v["hooks"]["PostToolUse"], json!([{"matcher": "*", "hooks": [{"type": "command", "command": "/opt/margin/bin/margin hook post-tool", "timeout": 5}]}]));
        assert_eq!(v["hooks"]["SessionStart"], json!([{"hooks": [{"type": "command", "command": "/opt/margin/bin/margin hook start", "timeout": 5}]}]));
        for h in HOOKS {
            let g = group(&v, h);
            assert_eq!(g["hooks"][0]["command"], json!(format!("/opt/margin/bin/margin hook {}", h.kind)));
            assert_eq!(g.get("matcher").is_some(), h.matcher, "{}", h.event);
        }
        // the hook that waits sits beside `stop`, in the background, with an hour to wait
        assert_eq!(v["hooks"]["Stop"], json!([
            {"hooks": [{"type": "command", "command": "/opt/margin/bin/margin hook stop", "timeout": 5}]},
            {"hooks": [{"type": "command", "command": "/opt/margin/bin/margin hook rewake", "asyncRewake": true, "timeout": 3600}]}]));
        assert!(rewake::DEADLINE < 3600.0, "the watcher leaves before Claude Code would kill it");
        assert!(!o.claude_dir.join("settings.json.bak").exists(), "nothing to back up on a fresh machine");
        done(&o);
    }

    #[test]
    fn default_name_renders_for_anyone() {
        let o = opts("anyone");
        let out = run(&o).unwrap();
        assert!(out.contains("no user_name set"), "{out}");
        let s = skill(&o);
        assert!(s.contains("the user owns decisions") && !s.contains("{{") && !s.to_lowercase().contains("sulin"));
        assert!(!o.config.exists(), "no name given: the config is left alone");
        done(&o);
    }

    #[test]
    fn rerun_is_idempotent_and_a_moved_binary_updates_in_place() {
        let mut o = opts("rerun");
        run(&o).unwrap();
        let first = std::fs::read_to_string(o.claude_dir.join("settings.json")).unwrap();
        let out = run(&o).unwrap();
        assert!(out.contains("nothing to change") && out.matches("up to date").count() == 2, "{out}");
        assert_eq!(std::fs::read_to_string(o.claude_dir.join("settings.json")).unwrap(), first);
        // a new build somewhere else: same entries, new path, the skill's path follows
        o.bin = PathBuf::from("/home/someone/margin/target/release/margin");
        let out = run(&o).unwrap();
        assert!(out.contains("updated 9 to this binary"), "{out}");
        let v = settings(&o);
        for h in HOOKS {
            let n = HOOKS.iter().filter(|x| x.event == h.event).count();
            assert_eq!(v["hooks"][h.event].as_array().unwrap().len(), n, "{}: one group per hook", h.event);
            assert_eq!(group(&v, h)["hooks"], json!([h.entry("/home/someone/margin/target/release/margin")]));
        }
        assert!(skill(&o).contains("/home/someone/margin/target/release/margin"));
        done(&o);
    }

    /// A rewake entry written by hand without its two fields would hold every turn's end (no
    /// asyncRewake) or die at 5 s: init sets them, and leaves a correct entry alone.
    #[test]
    fn a_rewake_entry_gets_its_background_flag_and_timeout() {
        let mut v = json!({"hooks": {"Stop": [{"hooks": [
            {"type": "command", "command": "/x/margin hook stop", "timeout": 7},
            {"type": "command", "command": "/x/margin hook rewake", "timeout": 5}]}]}});
        let (added, updated) = merge_hooks(&mut v, "/x/margin").unwrap();
        assert_eq!((added.len(), updated), (7, vec!["Stop rewake".to_string()]));
        assert_eq!(v["hooks"]["Stop"], json!([{"hooks": [
            {"type": "command", "command": "/x/margin hook stop", "timeout": 7},
            {"type": "command", "command": "/x/margin hook rewake", "timeout": 3600, "asyncRewake": true}]}]));
        assert_eq!(merge_hooks(&mut v, "/x/margin").unwrap(), (vec![], vec![]), "a second run changes nothing");
        assert!(is_margin_hook("/x/margin hook rewake", "rewake") && !is_margin_hook("/x/margin hook rewake", "stop") && !is_margin_hook("/x/margin hook stop", "rewake"));
    }

    #[test]
    fn unrelated_hooks_and_keys_survive_and_the_first_change_is_backed_up() {
        let o = opts("merge");
        std::fs::create_dir_all(&o.claude_dir).unwrap();
        let before = json!({
            "model": "opus",
            "permissions": {"allow": ["Bash(ls:*)"]},
            "hooks": {
                "UserPromptSubmit": [{"hooks": [{"type": "command", "command": "/opt/mine/on-prompt.sh"}]}],
                "SessionStart": [{"hooks": [
                    {"type": "command", "command": "/opt/mine/role.sh", "timeout": 10},
                    {"type": "command", "command": "/old/checkout/target/debug/margin hook start", "timeout": 9}]}],
                "Stop": [
                    {"hooks": [{"type": "command", "command": "/old/checkout/target/debug/margin hook stop", "timeout": 5}]},
                    {"hooks": [{"type": "command", "command": "'/other/copy of/margin' hook stop", "timeout": 5}]}],
                "PreCompact": [{"hooks": [{"type": "command", "command": "/opt/mine/margin-notes hook pre-tool"}]}]
            },
            "statusLine": {"type": "command", "command": "margin-status"}
        });
        let text = serde_json::to_string_pretty(&before).unwrap();
        std::fs::write(o.claude_dir.join("settings.json"), &text).unwrap();
        let out = run(&o).unwrap();
        assert!(out.contains("added 7") && out.contains("Stop rewake") && out.contains("updated 2 to this binary (Stop, SessionStart)"), "{out}");
        let v = settings(&o);
        for k in ["model", "permissions", "statusLine"] {
            assert_eq!(v[k], before[k], "{k}");
        }
        assert_eq!(v["hooks"]["PreCompact"], before["hooks"]["PreCompact"], "another tool's hook, even one named like ours");
        // the person's own hooks keep their place; ours join them
        assert_eq!(v["hooks"]["UserPromptSubmit"][0], before["hooks"]["UserPromptSubmit"][0]);
        assert_eq!(v["hooks"]["UserPromptSubmit"][1]["hooks"][0]["command"], json!("/opt/margin/bin/margin hook prompt"));
        assert_eq!(v["hooks"]["SessionStart"][0]["hooks"], json!([
            {"type": "command", "command": "/opt/mine/role.sh", "timeout": 10},
            {"type": "command", "command": "/opt/margin/bin/margin hook start", "timeout": 9}]));
        // two old copies of one hook become one
        assert_eq!(v["hooks"]["Stop"][0], json!({"hooks": [{"type": "command", "command": "/opt/margin/bin/margin hook stop", "timeout": 5}]}));
        assert_eq!(v["hooks"]["Stop"].as_array().unwrap().len(), 2, "and the hook that waits joins it");
        // key order is kept, so the file's diff is only what changed
        assert_eq!(v.as_object().unwrap().keys().collect::<Vec<_>>(), ["model", "permissions", "hooks", "statusLine"]);
        // the backup is the file as it was, and a later change doesn't overwrite it
        let bak = o.claude_dir.join("settings.json.bak");
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), text);
        let mut o2 = opts("merge-unused");
        o2.claude_dir = o.claude_dir.clone();
        o2.bin = PathBuf::from("/elsewhere/margin");
        run(&o2).unwrap();
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), text);
        done(&o2);
        done(&o);
    }

    #[test]
    fn stale_skill_is_replaced_and_a_symlink_is_not_written_through() {
        let o = opts("stale");
        let dir = o.claude_dir.join("skills/margin");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), "old copy\nby hand\n").unwrap();
        let out = run(&o).unwrap();
        assert!(out.contains("replaced a stale copy (2 lines"), "{out}");
        assert!(skill(&o).starts_with("---\nname: margin\n"));
        // a link to a checkout's template: the link goes, the template stays as it was
        let target = o.claude_dir.join("template.md");
        std::fs::write(&target, "template {{user}}\n").unwrap();
        std::fs::remove_file(dir.join("SKILL.md")).unwrap();
        std::os::unix::fs::symlink(&target, dir.join("SKILL.md")).unwrap();
        let out = run(&o).unwrap();
        assert!(out.contains("replaced a symlink"), "{out}");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "template {{user}}\n");
        assert!(!std::fs::symlink_metadata(dir.join("SKILL.md")).unwrap().file_type().is_symlink());
        done(&o);
    }

    #[test]
    fn dry_run_writes_nothing() {
        let mut o = opts("dry");
        o.dry_run = true;
        o.user_name = Some("robin".into());
        let out = run(&o).unwrap();
        assert!(out.contains("dry run") && out.contains("would write 3 files") && out.contains("added 9"), "{out}");
        assert!(!o.claude_dir.exists() && !o.config.exists());
        // over an existing install too: the file is byte-for-byte what it was
        std::fs::create_dir_all(&o.claude_dir).unwrap();
        std::fs::write(o.claude_dir.join("settings.json"), "{\"model\": \"opus\"}").unwrap();
        run(&o).unwrap();
        assert_eq!(std::fs::read_to_string(o.claude_dir.join("settings.json")).unwrap(), "{\"model\": \"opus\"}");
        assert!(!o.claude_dir.join("settings.json.bak").exists() && !o.claude_dir.join("skills").exists());
        done(&o);
    }

    #[test]
    fn settings_that_wont_parse_stop_everything() {
        let mut o = opts("broken");
        o.user_name = Some("robin".into());
        std::fs::create_dir_all(&o.claude_dir).unwrap();
        std::fs::write(o.claude_dir.join("settings.json"), "{ // a comment\n}").unwrap();
        let e = run(&o).unwrap_err();
        assert!(e.contains("not JSON") && e.contains("nothing was changed"), "{e}");
        assert!(!o.config.exists() && !o.claude_dir.join("skills").exists(), "decided before the first write");
        std::fs::write(o.claude_dir.join("settings.json"), "{\"hooks\": []}").unwrap();
        assert!(run(&o).unwrap_err().contains("isn't an object"));
        done(&o);
    }

    #[test]
    fn margin_hooks_are_recognised_by_program_and_kind() {
        assert!(is_margin_hook("/a/b/margin hook start", "start"));
        assert!(is_margin_hook("margin hook post-tool", "post-tool"));
        assert!(is_margin_hook("'/a b/margin' hook stop", "stop"));
        assert!(!is_margin_hook("/a/b/margin hook start", "stop"));
        assert!(!is_margin_hook("/a/b/margin hook pre-tool", "tool"));
        assert!(!is_margin_hook("/a/b/not-margin hook start", "start"));
        assert!(!is_margin_hook("/a/b/margin-hook start", "start"));
        assert_eq!(hook_command("/a b/margin", "stop"), "'/a b/margin' hook stop");
    }
}
