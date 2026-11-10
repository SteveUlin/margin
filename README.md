# margin

margin is a live terminal pane for a [Claude Code](https://claude.com/claude-code) session: Claude
writes its plan, questions and assumptions into the pane, and you answer by clicking or pressing a
key instead of typing. The same pane holds standing dashboards that scripts keep current with no
model calls. `vellum`, the second crate in this workspace, is the terminal UI toolkit margin draws
with.

![A margin pane: two plan steps, a question answered "yes" and marked sent, and an assumption with a strike token](docs/img/readme/plan.png)

The pane is not a rendered markdown file. Each row is a typed component with a stable id; Claude
owns the content, you own the decisions, and every decision you make goes back to Claude through
Claude Code hooks as one line of context.

## What it does

- **Plan steps.** Claude lists its steps and ticks them as it works. You can mark a step next,
  skipped or done, or take one as yours.
- **Questions and claims you answer by clicking.** A question carries options, with Claude's
  suggestion marked and nothing chosen until you choose; a claim is an assumption you strike with
  a click. Claude keeps working on whatever does not depend on the answer.
- **Hooks that hand decisions back.** `margin init` installs Claude Code hooks. Your clicks reach
  Claude at its next prompt, after its next tool call, or wake it when it is idle.
- **Tabs.** One pane shows several artifacts (`margin view plan reviews`, `margin tab`).
- **Tables, run strips and name badges** for dashboards: a pull-request queue, service health, a
  work tracker (`margin example` prints three complete ones; see [docs/dashboards.md](docs/dashboards.md)).
- **A detail panel** for the focused row: a pull request's description, a failing service's error.
- **Feeds.** A registered script (on an interval, on a file change, or as a stream) prints JSON
  lines and a per-project daemon applies them. The feed owns its rows; Claude and you cannot
  overwrite them. Shipped feeds: a jj change ledger, a monitor of Claude Code sessions, a board
  of queued messages, a turn ledger (`feeds/`).
- Also: structural diffs to approve or reject (through difftastic), PNG images (kitty graphics),
  live tails of long commands, toasts.

![A dashboard tab: a table of pull requests with author badges, CI run strips and ages](docs/img/readme/dashboard.png)

*Both pictures are real frames from `margin render`, drawn by the test harness
(`tests/e2e/shot.py`); two badge glyphs were swapped for ones its font has.*

## Install

[docs/install.md](docs/install.md) has the full steps. In short:

```sh
git clone https://github.com/SteveUlin/margin && cd margin
cargo build --release                           # or: nix build
target/release/margin init --user-name "<you>"  # your name, the Claude skill, the hooks
```

`init` writes your name to margin's config, installs the skill to `~/.claude/skills/margin/`
and merges the hooks into `~/.claude/settings.json` (it keeps a `.bak` and leaves other hooks
alone; `--dry-run` prints the changes first). Restart Claude Code afterwards.

## Quick start

With Claude: say "start a margin for this" in a Claude Code session. By hand, in any directory:

```sh
margin add plan - section steps title=Steps
margin add plan steps todo t1 "Reproduce the flaky test"
margin ask plan Q1 "Seed from the test name?" yes no --suggest yes
margin open plan        # in Zellij: a split beside the chat; elsewhere it prints the `margin view` command to run
margin set plan t1 status done
margin events           # what you did in the pane, as Claude will read it
```

`margin demo` writes a larger sample plan (it needs difftastic), `margin example reviews | margin
apply reviews` a sample dashboard, and `margin help` lists every command and reference topic.
[docs/guide.md](docs/guide.md) walks through an artifact, a feed, and a session with Claude.

## Requirements

- **Linux.** margin uses inotify, `/proc` and `flock`; other systems are untested.
- **Rust** (stable) to build, or nix (`flake.nix` pins the toolchain and the tools below).
- **Claude Code**, for the hooks and the skill. The pane itself runs without it.
- **Zellij 0.45 or newer is optional.** Inside Zellij, `margin open` places the pane beside the
  chat without moving focus, and `margin session` makes a double-click on a token act. Without
  Zellij, run `margin view <artifact>` in any terminal; keys and mouse clicks work there.
- **A Nerd Font is recommended** for the state glyphs; a terminal with the kitty graphics
  protocol (Ghostty, kitty) for images.
- Optional tools: `difft` (difftastic) for `margin diff` and the demo; `bash`, `jq` and `jj` for
  the shipped feeds.

## Documentation

| file | what it covers |
|---|---|
| [docs/install.md](docs/install.md) | build, `margin init`, a first pane, undoing it |
| [docs/guide.md](docs/guide.md) | the model, four walkthroughs, a cheat sheet of commands, keys and config |
| [docs/dashboards.md](docs/dashboards.md) | tables, run strips, badges, rules, and the theme |
| [docs/design.md](docs/design.md) | the original design: owners, events, the toolkit, panes, feeds |
| [docs/research.md](docs/research.md), [docs/landscape.md](docs/landscape.md), [docs/research/](docs/research/) | the measurements and prior art behind the design |

The design and research documents are dated notes from the build. Where they disagree with
`margin help` or the guide, the tool is right.

## Status

A personal project, early (0.1), built for one person's daily use. Commands, the document schema
and the store's file layout change without notice, and nothing is published to crates.io. Issues
are welcome; expect slow replies.

## License

MIT. See [LICENSE](LICENSE).
