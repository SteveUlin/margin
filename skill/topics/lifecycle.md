# lifecycle: what survives a session end, a closed pane, a stopped daemon and a reboot, and how to check

Sessions come and go, panes get closed, machines reboot. Everything durable is in the store
(`~/.local/state/margin/<project>/`, or `$MARGIN_HOME`); everything tied to a process is found
dead by a liveness check and cleaned up. Nothing below needs a person, and `margin doctor` shows
what is left over.

| what happens | the pane | the session's state | handlers | the data |
|---|---|---|---|---|
| a Claude session ends (exit, `/clear`, tab closed) | keeps running and showing the board; drops the live strip and every mention of Claude; says `the session that kept this board ended at 14:02` | `margin claude hook end` removes its runtime files (inbox address, rewake mark, wake record) and records it as ended; its link and cursor stay, so a resume carries on | untouched | untouched |
| a session crashes or is killed (no hook runs) | drops the live strip once the session's last activity is 15 minutes old (an hour for an idle one); the status bar still names Claude until a session ends properly or the link goes (`margin claude unlink --session S`) | its rewake watcher dies with it; readers check the mark's pid; `margin doctor --fix` removes the leftovers | untouched | untouched |
| a new session starts in the project | none of its own yet | `margin claude hook start` tells it which boards exist, or which one is linked to it | untouched | untouched |
| the pane is closed, or gets SIGTERM / SIGHUP / ctrl-c, or panics | restores the terminal (raw mode, mouse, alternate screen, cursor), takes its images down, saves its view, removes its review mark, exits 0 | untouched | untouched | the view file (folds, focus, scroll, tabs) comes back with the next pane |
| the process named by `--while-pid PID` ends (`margin open <board> --while-pid PID`, `margin view … --while-pid PID`) | exits as `q` does: view saved, terminal restored | untouched | untouched | untouched |
| the margin binary is rebuilt | re-execs itself in place, with the same view | untouched | the daemon keeps running the old binary until it is restarted | untouched |
| the daemon gets SIGTERM | shows what it had | untouched | running commands are stopped (their whole process group: TERM, then KILL after a second), recorded `stopped`, not failed; nothing they printed is applied; cursors do not move | untouched |
| the daemon is killed, or the machine reboots | starts again when you open it | runtime files are gone (they live in `$XDG_RUNTIME_DIR`); links and cursors are in the store | the next daemon resumes every handler from its cursor: an event appended while it was down is delivered once, an interrupted run's events are given again | boards, events, cursors, registrations: all in the store |

## How the daemon comes back

Two supported ways:

- **On demand** (nothing to set up): any margin command that appends an event while a handler
  is registered starts the daemon: `margin emit` from cron, a key in the pane, `margin act`.
  Between a reboot and that first event nothing is missed, because nothing was appended.
- **A systemd user unit**: `margin daemon install` writes `margin-<project>.service` (restart on
  failure, started at login) and prints the line that enables it; `loginctl enable-linger "$USER"`
  starts it at boot without a login. `margin daemon status` says which of the two applies.

Either way an event is never lost and never handled by two daemons (one `flock` per project,
released by the kernel when the process dies, so a kill leaves no stale lock). A handler can see
an event twice, after a run that was interrupted: it must be safe to run twice (`margin help handlers`).

## Scheduled work is the scheduler's

margin has no timer. A fetch that should happen every five minutes is a cron line or a systemd
timer that runs `margin emit <board>.refresh --project <project>` (`margin help integrate` has
both). Across a reboot: cron skips the ticks the machine was off for; a systemd timer with
`Persistent=true` runs one missed tick at boot. An outside tool that follows the log
(`margin events --follow --cursor NAME`) resumes at its cursor when it is started again; keeping
it running is its unit's or its owner's job.

## Check, and clean up

```sh
margin daemon status        # running? since when? each handler's last run and waiting events; the unit
margin handler ls           # the same per handler; margin handler logs <name> for a run's stderr
margin owner ls             # outside writers: last write, STALE when overdue
margin doctor               # leftovers of dead sessions, panes and daemons; boards nothing handles
margin doctor --fix         # remove the leftovers (never a board, an event, a named cursor or a registration)
margin gc                   # archive boards untouched for archive_days; drop cursors of sessions that long gone
```

## What is written how

- Every file is written to a temporary name and renamed into place: a reader never sees half
  a file, and a crash leaves the old one.
- A decision (`state.json`), a cursor and a registry are also synced to disk with their
  directory before the command returns: a power loss does not lose a pick or move a cursor back.
- `events.jsonl` is appended in one write and not synced on every event (that would put a disk
  flush on every key). A power loss can cost the last moments' events; a torn last line is
  skipped and never reused. View files, status files and the activity records are not synced:
  they are rewritten in the normal course of things.
- A document from a newer margin is refused, never rewritten.
