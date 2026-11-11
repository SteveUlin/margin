# integrate: other programs and margin: cron and systemd recipes, following events, owning rows, queries

Two ways for a program to work with a board, and they combine. **A small action registers a
handler**: margin's daemon runs a command when an event arrives (`margin help handlers`). **A
tool that makes its own decisions follows the stream**: it stays its own process, reads events
with a named cursor, and writes the rows it owns; margin runs nothing for it and needs no daemon.
Scheduling is outside margin in both cases: whatever owns the clock emits an event or writes.

Every command below takes `--project NAME` (and `--home DIR`). A board belongs to a project,
which margin derives from the working directory (the repository root's path as a slug), so a
command run from cron or another directory must name it: the pane's bottom line reads `MARGIN
<artifact> <project>`, and `echo $MARGIN_PROJECT` or `margin ls` run beside the board agree.

## Queries (read-only, JSON)

```sh
margin ls --json                          # {"v":1,"artifacts":[{"id","rev","updated","title","kind"}]}
margin show BOARD --json                  # {"doc": the whole document, "state": {"decisions": {id: {kind, value, by, at}}}}
margin show BOARD ID --json               # {"v":1,"artifact","rev","block":{…},"decision":{…}|null}
margin events --json [--name GLOB] [--since N] [--cursor NAME --no-commit]   # one event per line
margin handler ls --json                  # handlers: state, cursor, events waiting, last run
margin owner ls --json                    # owners: last write, stale or not
margin daemon status --json
```

Exit codes: 0 done; 1 refused, invalid or failed; 3 not found (no such artifact, component,
handler or owner). The message is on stderr, and stdout stays empty on failure.

## Submit work or news from anywhere

```sh
margin emit deploy.finished '{"sha":"abc123","ok":true}' --project boards     # prints the event's seq
jq -c .summary report.json | margin emit ci.report - --board ci --project boards
```

The value is one JSON value (`-` reads it from stdin; text that is not JSON is an error). The
event is logged with `via: "emit"`; a handler registered `--on 'deploy.*'` runs for it, a
follower sees it, and no Claude session is handed it as an instruction.

## A fetcher on a timer: cron

The script is a handler for a refresh event; cron only says when.

```sh
margin handler add reviews-refresh --on reviews.refresh --board reviews --owns pr -- ./fetch.sh
crontab -e
#   */5 * * * *  /usr/local/bin/margin emit reviews.refresh --project boards
```

Cron has a short PATH: give margin's full path (`command -v margin`). The handler itself runs
with the PATH of the shell that registered it. After a reboot the first `emit` starts the daemon.

## A fetcher on a timer: a systemd user timer

`~/.config/systemd/user/reviews-refresh.service`:

```ini
[Unit]
Description=ask margin to refresh the reviews board

[Service]
Type=oneshot
ExecStart=/usr/local/bin/margin emit reviews.refresh --project boards
```

`~/.config/systemd/user/reviews-refresh.timer`:

```ini
[Unit]
Description=refresh the reviews board every 5 minutes

[Timer]
OnBootSec=1min
OnUnitActiveSec=5min
Persistent=true

[Install]
WantedBy=timers.target
```

```sh
systemctl --user daemon-reload && systemctl --user enable --now reviews-refresh.timer
```

`Persistent=true` runs a missed tick once after the machine was off. What the timer missed is
the timer's business; what margin's daemon missed is nothing, since handlers resume from their
cursors (`margin help lifecycle`).

## A fetcher on a file change

```sh
watchexec -w prs.json -- margin emit reviews.refresh --project boards
ls prs.json | entr margin emit reviews.refresh --project boards
```

or a systemd path unit (`reviews-refresh.path`, beside the service above):

```ini
[Path]
PathChanged=%h/work/prs.json

[Install]
WantedBy=default.target
```

## A fetcher with no handler at all

A program that already runs on its own schedule can write its rows directly, as a registered
owner (below): `./fetch.sh | margin apply reviews --as fetcher --project boards` from cron.
margin then runs nothing, and `--expect-every` shows the rows stale if the cron job stops.

## React to events in your own process

```sh
# at-most-once: the cursor moves as each event is printed
margin events --follow --cursor mytool --name 'reviews.*' --json --project boards |
  while read -r ev; do handle "$ev"; done

# at-least-once: read without moving the cursor, commit after the work
margin events --follow --cursor mytool --name 'reviews.*' --json --no-commit --project boards |
  while read -r ev; do
    handle "$ev" && margin events --cursor mytool --commit "$(jq .seq <<<"$ev")" --project boards
  done
```

- A cursor is a named position in the log (`mytool`); one used for the first time starts at the
  end, so the tool hears what happens from now on (`--since 0` replays what is kept).
- With `--no-commit` a restart replays what was not committed: the tool must be safe to see an
  event twice (dedupe on `seq`). Stopped and started again, it resumes at its cursor: nothing
  appended in between is lost.
- `--follow` ends with exit 0 on ctrl-c or a closed stdout. The log rotates at a few MB and keeps
  two older generations; `seq` goes on counting, so a cursor stays valid across a rotation.
- An action {{user}} takes on a row (`<board>.<name>`) arrives here like any event; the row then
  shows `→ sent` until its content changes or a cursor moves past the event.

Block until one question is answered (what `margin wait` did):

```sh
margin events --follow --name pick --artifact plan --json | jq -r --unbuffered 'select(.id == "Q1") | .value' | head -n1
```

## Own rows on a board

```sh
margin owner add queue --board runs --owns run --expect-every 10m     # once; the board must exist
printf '%s\n' '{"type":"row","id":"run-7","text":"sim A","glyph":"spin","parent":"run"}' |
  margin apply runs --as queue                                        # as often as the tool likes
margin owner ls                                                       # last write, and STALE when overdue
```

- The owner's ids (`run` and `run-…`) are written only by `--as queue`; any other write there is
  refused, as with a handler's or a feed's rows, and `--as` with an unregistered name is refused.
- `--as` takes components and ops (the feed line grammar, `margin help feeds`), all or nothing.
  A write that changes nothing makes no revision and still counts as a write.
- `--expect-every 10m`: once the last write is older than that, the owner's rows show `◷ stale ·
  last updated 3h ago, expected every 10m`, the board's tab gets the failure mark, and `owner ls`
  says STALE. margin runs and restarts nothing: the age of the last write is the one thing it
  knows about a program it does not run.
- The actions {{user}} takes on owned rows still work, and reach the tool as events.
- `margin example runs --tool` is a complete outside tool built from these pieces: a run queue
  that takes submissions as events, runs them one at a time, and owns its rows.

## Tell a phone (what the pane's push used to do)

margin sends no notifications. A handler or a follower does, with whatever service you use:

```sh
margin handler add notify --on 'handler.failed' --on 'deploy.finished' -- ./notify.sh
# notify.sh:  while read -r ev; do curl -s -d "$(jq -r '.kind + " " + (.value|tostring)' <<<"$ev")" "$NTFY_URL"; done
```

For "Claude is waiting on you", use Claude Code's own `Notification` hook: it fires when the
session needs attention, with no margin involved.
