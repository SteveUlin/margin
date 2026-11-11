# Install margin on a machine

Ten minutes, no source edits. You need Rust (or nix), Claude Code, and a terminal. Zellij and
difftastic are optional: without them the pane runs in any terminal and `margin diff` is off.

## 1. Build

```sh
git clone https://github.com/SteveUlin/margin margin    # or: jj git clone <the margin repository> margin
cd margin
cargo build --release                       # the binary: target/release/margin
```

With nix: `nix build` (the binary is `result/bin/margin`, with difftastic wrapped in), or
`nix develop` for a shell with the toolchain. Install the package into a profile before running
`init`, so the hooks point at a path that survives the next rebuild.

Put the binary's directory on your PATH if you like; nothing requires it, because `init` writes
absolute paths.

## 2. Set up

```sh
target/release/margin init --user-name "<your name>"
```

`init` does three things and prints each:

- **Name.** Writes `user_name` to margin's config (`$MARGIN_CONFIG`, else
  `~/.config/margin/config.json`). Claude is told "<your name> picked files"; without a name it
  says "the user".
- **Skill.** Installs the skill Claude loads to `~/.claude/skills/margin/SKILL.md`, rendered with
  your name and the binary's path. A copy that differs is replaced. `margin skill` prints it.
- **Hooks.** Merges nine hooks into `~/.claude/settings.json`, each running this binary:
  `UserPromptSubmit` → `hook prompt`, `PostToolUse` → `hook post-tool`, `PreToolUse` → `hook
  pre-tool`, `Stop` → `hook stop` and `hook rewake`, `SessionStart` → `hook start`,
  `PermissionRequest` → `hook permission`, `Notification` → `hook notify`, `SessionEnd` → `hook end`. Your other hooks and
  settings stay as they are, and the file as it was is kept as `settings.json.bak` the first time
  `init` changes it.

  `hook rewake` is the one that waits (`"asyncRewake": true`, `"timeout": 3600`): Claude Code runs
  it in the background when a turn ends, and when you finish acting in the pane it exits 2 with
  your decisions on stderr, which wakes the idle Claude with them. Without it margin falls back
  to a message into the session's inbox, which Claude Code presents as another session's and may
  hold for your approval; `"crossSessionInbound": "accept"` in your Claude Code settings is the
  alternative that lets that message through.

Run it again after moving or rebuilding the binary somewhere else: it updates its own entries in
place and adds nothing twice. Options:

| flag | what it does |
|---|---|
| `--dry-run` | print the changes, write nothing |
| `--claude-dir DIR` | Claude Code's directory, if not `~/.claude` (a project's `.claude` installs for that project only) |
| `--bin PATH` | the binary the hooks run, if not the one you're running |
| `--user-name NAME` | your name in what margin tells Claude; leave it off to keep the config's |

If a project's own `.claude/settings.json` already runs margin hooks, `init` says so: remove them
there, or a session in that project runs both sets.

## 3. Open a first pane

Restart any running Claude Code session first: hooks and skills load at session start.

```sh
margin demo            # writes a sample plan board for the current directory's project
margin open plan       # inside Zellij: a split beside the chat, focus stays where it was
margin demo live       # plays a turn against it: steps tick, a card resolves
```

- **Zellij, wired for double-click tokens:** `margin session` starts a Zellij session with the
  copy command set; run the lines above inside it.
- **No Zellij:** `margin open plan` prints a `margin view plan` command. Run that in any terminal
  beside the chat; keys and mouse clicks work in the pane.
- **No difftastic:** `margin demo` draws a diff and fails without `difft`. Skip the demo:
  `margin new hello && margin add hello - todo t1 "my first step" && margin view hello` needs
  nothing else. `init`
  prints whichever of these fits the machine.

Then, in Claude Code: "start a margin for this". Claude reads the skill, writes the pane's shape,
and from then on each prompt starts with a `margin:` line carrying the pane's state and whatever
you clicked.

## Check and undo

- `margin init --dry-run` shows what a run would change; "up to date" twice and "nothing to
  change" means the machine is set.
- `margin help` lists the commands and the reference topics (`margin help feeds`, …).
- To undo: delete `~/.claude/skills/margin/`, and remove the `margin hook …` entries from
  `~/.claude/settings.json` (or restore `settings.json.bak`).
- `margin diff` and `margin demo` need difftastic (`difft`) on PATH, in `$MARGIN_DIFFT`, or under
  the config key `difft`; the nix build wraps it in.
