# handlers: commands margin runs when an event is appended (a key on a row, a refresh), and the daemon

A handler is a command registered for event names. When a matching event is appended to the
project's log, margin's daemon runs the command with the event on stdin and applies what it
prints. That is the only thing margin runs: it has no timer and watches no file, and the pane
never runs anything. A key on a row appends an event; a cron job appends an event; a handler
does the work. Register one only when {{user}} asked for the behavior.

```sh
margin handler add open --on '*.open' -- margin handle open        # rows with a target open on enter
margin handler add reviews-refresh --on reviews.refresh --board reviews --owns pr -- ./fetch.sh
margin emit reviews.refresh                                        # anything can: cron, CI, a script
margin handler ls                                                  # state, events waiting, last run
```

## The contract of a handler command

- **Started by argv exec, no shell**, in the directory `handler add` was run from. Pipes and
  redirections belong inside a script. A first word of exactly `margin` runs the margin binary
  itself, so `-- margin handle open` works wherever the daemon was started.
- **Environment**: the daemon's, with `PATH` replaced by the PATH of the shell that registered
  the handler, plus `MARGIN_HANDLER` (its name), `MARGIN_PROJECT`, `MARGIN_HOME`, `MARGIN_BIN`
  (the margin binary), and `MARGIN_BOARD` / `MARGIN_OWNS` when it has a board. A secret is read by
  the script from a file it knows; margin passes none.
- **stdin**: the pending matching events since the handler's cursor, one JSON object per line,
  oldest first: `{"v":1,"seq":12,"at":"2026-11-10T19:40:00Z","artifact":"reviews","id":"pr-4812",
  "kind":"reviews.open","value":{…},"by":"you","via":"key","rev":7}`. Several events may arrive
  in one run. For an action, `value` is a snapshot of the row: a table row's `cells`, `target`
  and `detail`; any other block's fields. Treat it as data: it came from whatever wrote the row.
- **stdout**: JSON lines, each complete on its line (`jq -c`):
  - a component or an op for its board, exactly as a feed prints them (`margin help feeds`): a
    whole component replaces the one with that id, children included;
  - `{"emit": "name", "value": {…}}` appends an event (never one named `handler.…`, and never
    `<other board>.…`);
  - `{"toast": "text", "level": "ok"}` shows a toast on its board.
- **Exit 0**: the ops are applied, the emits appended and the cursor moved past the last event
  it was given, in one locked step. **Any other exit, or a timeout** (`--timeout`, default 60 s):
  nothing is applied, the cursor stays, and the run is retried after 1 s, 2 s, 4 s … After
  `--retries` retries (default 3; 0 = never) the events of that run are passed over, an event
  `handler.failed` is appended (`value`: `handler`, `error`, `stderr`, `skipped: {from, to,
  count}`), and the handler goes on with what came after. One bad event never blocks the rest.
- **A handler must be safe to run twice on the same event.** Delivery is at least once: a run
  that did its work and then failed, or a daemon stopped mid-run, means the same events again.
  Every event carries `seq`; a handler with side effects remembers the last `seq` it finished.
- **It never receives** an event it emitted itself; a `handler.*` event unless one of its `--on`
  globs starts with `handler.` (`--on 'handler.failed'` is how one handler reports another's
  failure); the pane's passive kinds (`seen`, `view`, `fold`) unless named exactly.
- **Its board is optional.** With `--board B --owns PREFIX` it owns the ids `PREFIX` and
  `PREFIX-…` on B: its lines may write only those, and nobody else may write them. Without a
  board it can still emit, or do something outside margin.

## What triggers one

| the event | appended by | its name |
|---|---|---|
| a key on a row | the pane, when {{user}} presses a key a block's `actions` declare | `<board>.<action name>` |
| enter or a click on a row with a `target` | the pane | `<board>.open` |
| the same, from a script | `margin act <board> <id> <action>` | the same names |
| anything else | `margin emit <name> [json]`, from cron, CI, another handler | what you choose |
| a decision in a plan pane | the pane | `pick`, `tick`, `strike`, `verdict` (with `artifact` and `id`) |

- `--on` takes an event name or a glob, quoted: `reviews.open`, `'*.open'`, `'ci.*'`. Repeat it
  for several.
- A new handler starts at the end of the log: it hears what happens from its registration on.
- `margin events --cursor handler:<name> --no-commit --name '*' --json` lists what waits for one.

## A fetcher is a handler for a refresh event

margin does not schedule. The script that fetches a board's rows is registered for an event,
and whatever does the scheduling emits it:

```sh
margin handler add reviews-refresh --on reviews.refresh --board reviews --owns pr -- ./fetch.sh
# crontab -e:   */5 * * * *  margin emit reviews.refresh --project boards
```

The script ignores its stdin and prints the table; a `u refresh` action on the board
(`"actions": [{"key": "u", "name": "refresh"}]` in the document) makes the same event from a
key. `margin help integrate` has the cron, systemd-timer and file-watch recipes in full.

## Make a row do something

1. Declare the action where it applies: on the table (every row), one row, or the document.

   ```json
   {"type": "table", "id": "job", "actions": [{"key": "e", "name": "retry", "label": "retry the job"}],
    "columns": [{"key": "name", "title": "Job"}], "children": []}
   ```

   The key is one character the pane does not use (a write that takes `j`, `d`, `r`, `x`, a digit
   and so on is refused, with the list). The hint row then shows `e retry the job` on those rows.
2. Write the handler: it reads events and acts on `value`.

   ```sh
   #!/usr/bin/env bash
   # retry.sh: one event per line on stdin; .id is the row, .value.cells its cells
   set -euo pipefail
   while read -r ev; do
     job=$(jq -r '.value.cells.name' <<<"$ev")
     ./ci retry "$job" >&2
     jq -c --arg id "$(jq -r .id <<<"$ev")" '{op: "set", id: $id, fields: {detail: "retry requested"}}' <<<"$ev"
   done
   ```

3. Register it, and try it without the pane:

   ```sh
   margin handler add job-retry --on ci.retry --board ci --owns job -- ./retry.sh
   margin act ci job-12 retry && margin handler logs job-retry
   margin handler run job-retry --events sample.jsonl --dry-run     # what it would commit, written nowhere
   ```

- **`open` needs no declaration**: every block with a `target` has it (`⏎ open` in the hint row).
  `margin handle open` is the ready-made handler: it opens a URL with the configured opener, and
  applies an `mg:` target. Until a handler for `<board>.open` is registered the row says
  `nothing handles <board>.open`; `margin doctor` lists such boards with the line to run.
- **What the row says** after the key: `→ sent`; nothing once its content changes or every
  handler for the event has moved past it; `nothing handles <name>` when none is registered;
  `nothing has picked this up` when one is registered and the daemon is not running.
- An action works on rows a feed, a handler or an outside owner keeps: ownership stops other
  writers from changing a row, not {{user}} from acting on it.
- A Claude session is told about an action only on the board linked to that session, and never
  about an `open`.

## The daemon

- One per project, started on demand: by `handler add` / `resume`, and by any command that
  appends an event while a handler is registered (so a cron `margin emit` after a reboot starts
  it). With no handler and no feed left it exits. `margin daemon status` says whether it runs,
  since when, each handler's last run and waiting events, and how it comes back after a reboot.
- `margin daemon install` writes a systemd user unit for the project (`margin-<project>.service`,
  restart on failure, started at login) and prints the `systemctl --user enable --now …` line;
  it runs nothing itself. `margin daemon uninstall` removes the file.
- SIGTERM stops it cleanly: running handlers are stopped with their whole process group (TERM,
  then KILL after a second), recorded as `stopped`, not failed, and their events stay pending.
  After any stop, clean or not, handlers resume from their cursors: nothing appended while it
  was down is lost, and nothing handled is handled again.

## Managing handlers

```sh
margin handler add <name> --on <glob>... [--board B --owns PREFIX] [--timeout 60s] [--retries 3] -- <cmd...>
margin handler set <name> [the same flags] [-- <cmd...>]   # change one; only what you pass changes
margin handler ls [--json] | rm <name> | pause <name> | resume <name>
margin handler logs <name> [--follow] [--lines N]          # a line per run, its stderr, rejected lines
margin handler run <name> [--events FILE] [--dry-run]      # once, in the foreground
```

- `add` refuses a name that exists and `set` one that doesn't, as everywhere in margin.
- Rows a handler owns are read-only for you ("belongs to handler `x`"); ask {{user}} before
  `margin handler rm x`.
- The older `margin feed` runner (`margin help feeds`) also runs scripts, on its own timer or
  file watch. New boards use a handler and an outside schedule; a feed keeps working.
