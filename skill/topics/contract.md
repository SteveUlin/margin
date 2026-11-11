# contract: the JSON, environment and exit codes an outside tool may rely on, and what may change

What a program that works with margin can depend on. Everything here is checked against the
binary by `crates/margin/tests/contract.rs`; the examples are real output from a scratch store.
This is contract version **1**: the number every event, document and `--json` answer carries
in `v`.

## The stability promise

- Within a contract version a field may be **added**. No field listed here is removed, renamed
  or changed in type or meaning. A change of that kind makes version 2, and `v` says so.
- **Ignore what you do not know**: an unknown field on an object, an unknown event `kind`, an
  unknown `via`, an unknown component `type`, an unknown key in a `--json` answer. Do not fail
  on them and do not echo them back as if you understood them.
- A reader that meets a `v` higher than it knows should stop and say so. margin does the same:
  it refuses a document from a newer format, and never rewrites it as version 1.
- **Not part of the contract**: the layout of files inside the store (`events.jsonl`,
  `doc.json`, `cursors/`, the registries: read them through the commands); the wording of
  `--help`, of messages on stderr and of non-JSON output; how the pane draws anything; the
  timing constants; the `parked` commands in `margin --help`.

## No logic in a board

A board declares layout, text and verbs. It holds **no commands, no templates, no conditions
and no computed fields**: there is nowhere in a document to write an expression. A provider (a
handler, an owner, any program) computes the values and writes them; the pane draws what is
there and appends an event for every key. Three things follow:

- The store rejects a field it does not know, on a block, a table column, an action and the
  document itself (`unknown field "when" on row…`, exit 1, nothing written), and a table row's
  `cells` take only the keys of its table's columns. There is no free-form field to smuggle
  logic into. Only a dataset's records are free: any keys, and margin reads no meaning into them.
- A binding names data and nothing else: `"data": "<dataset>"`, `"rows": "<dataset>"`, and
  `~[@dataset.field:width]` inside a text. It selects a field; it cannot transform one.
- What the pane derives is fixed per component type and not authored: a time shown as an age,
  a `progress` bar counting a section's done todos, a run strip's failure count. To show
  anything else, compute it and write the result.

Data needed only when it is looked at is not an exception: the pane appends a view event (below)
and a handler may answer by writing rows or a dataset. The pane never calls anything.

## An event

One JSON object per line, in the log and from `margin events --json` (the same bytes), and on a
handler's stdin:

```json
{"v":1,"seq":1,"at":"2026-11-11T17:43:06Z","artifact":"reviews","id":"pr-7","kind":"reviews.approve","value":{"target":"https://example.com/pr/7","detail":"Fails one run in five.","cells":{"title":"Fix the flaky test"}},"by":"you","via":"act","rev":1}
```

Every field is always present.

| field | type | meaning |
|---|---|---|
| `v` | number | the contract version, 1 |
| `seq` | number | the event's position in the project's log: starts at 1, goes up by one, is never reused, and goes on counting across a rotation. What a cursor holds and what to dedupe on |
| `at` | string | when it was appended, RFC 3339 in UTC to the second |
| `artifact` | string | **the board's name**, or `""` when the event is about no board (the field keeps its first name) |
| `id` | string | the block it is about, or `""` for the board itself or none |
| `kind` | string | the event's name (below) |
| `value` | any JSON | what the event carries; `null` when nothing |
| `by` | string | who appended it: `you` (the person), `claude`, a name given with `--by`, `handler:<name>`, `owner:<name>` |
| `via` | string | how: `key`, `mouse`, `pane`, `select`, `cli`, `act`, `emit`, `handler` (more may be added) |
| `rev` | number | the board's revision when it was appended; 0 with no board |

**Names.** A `kind` is letters, digits and `. _ - :`, starting with a letter or digit, at most
128 characters. Three families:

- **A decision in a pane**, a bare word, with `artifact` and `id`: `tick`, `untick`, `pick`,
  `hold`, `unhold`, `talk`, `untalk`, `strike`, `unstrike`, `verdict`, `unverdict`, `pin`,
  `own`, `unown`, `note`, `unnote`, `proposal`, `stop`, `stop-agent`; and `overruled`, by
  `claude`, when a status write went over the person's own.
- **A named event**, with a dot. `<board>.<action>` is an action declared on a block and
  `<board>.open` the action every block with a `target` has (from a key, a click or `margin
  act`); `value` is a snapshot of the block: a table row's `cells`, `target` and `detail`, any
  other block's fields without its `actions` and children, plus `arg` when `margin act` was
  given a value. Anything else is what `margin emit <name>` or a handler's `{"emit": …}` line
  chose: name yours `<your tool or board>.<what happened>`. Names starting `handler.` are the
  runner's: `handler.failed` (`value`: `handler`, `error`, `stderr`, `attempts`, `skipped`
  as `{from, to, count}`). Emit nothing under `handler.`: a handler's emit
  line and `margin emit` both refuse it.
- **Passive events**, the pane's bookkeeping: `seen`, `view`, `fold`, `unfold`, and the view
  events. They are never handed to a Claude session, are not decisions, are left out of `margin
  events` unless `--all`, `--name` or `--kind` asks, and reach a handler only when its `--on`
  **is the exact name**. The view events alone are also selected by a glob that names their
  board (`--on reviews.detail` or `--on 'reviews.*'`; `'*'` and `'*.detail'` do not select them).

**The view events** say what is being looked at, so a provider can load what is on screen and
stop when it is not. All have `by: "you"`, `via: "pane"`:

| kind | when | `id` | `value` |
|---|---|---|---|
| `<board>.shown` | the board becomes the one on screen in a pane: at the pane's start and once per tab switch; and again when the pane regains the terminal focus after 60 s or more away | `""` | `{"since": N}`: seconds it was off screen in this pane (or away), `null` the first time |
| `<board>.hidden` | it stops being the one on screen: another tab, or the pane exits | `""` | `{"for": N}`: seconds it was on screen |
| `<board>.detail` | the detail panel (`d`) has rested on a block for 0.3 s: on opening, and on each block the focus settles on while it is open | the block | the same snapshot an action carries |

- There is no `<board>.focus`: moving the focus is too frequent to log. `detail` is the signal
  that a block is being read.
- The three endings are reserved: an event emitted under such a name (`margin emit
  reviews.detail`, useful to test a handler) is passive too.
- Losing the terminal focus alone appends nothing; a pane killed outright appends no `hidden`.
- Nobody listening costs nothing: a passive event starts the daemon only when a handler that
  is not paused names it, and the log rotates as it always does.
- With a handler registered for `<board>.detail`, the panel says `asked for more` under the
  block until the block or a dataset it reads changes, and `no answer yet` after 10 s. The
  answer is an ordinary write: `{"op": "set", "id": …, "fields": {"detail": "…"}}`.

## A board, and one block

`margin show <board> --json`: the document as stored and the decisions made on it. The answer
carries `v`, as every JSON answer does; `doc.v` is the stored document's own.

```json
{"v": 1,
 "doc": {"v": 1, "id": "reviews", "title": "Reviews", "kind": "", "rev": 1,
   "blocks": [
     {"type": "table", "id": "pr",
      "children": [{"type": "tr", "id": "pr-7", "target": "https://example.com/pr/7", "detail": "Fails one run in five.", "cells": {"title": "Fix the flaky test"}}],
      "columns": [{"key": "title", "title": "Title"}],
      "actions": [{"key": "e", "name": "approve"}]},
     {"type": "todo", "id": "t1", "text": "Review the queue"}],
   "updated": "2026-11-11T17:43:06Z", "by": "claude",
   "actions": [{"key": "u", "name": "refresh", "label": "refresh all"}]},
 "state": {"decisions": {"t1": {"kind": "status", "value": "done", "by": "claude", "at": "2026-11-11T17:43:06Z", "via": "cli", "seq": 0}}}}
```

- `doc`: always `v` (number), `id` (string, the board's name), `title` (string), `kind`
  (string, may be empty), `rev` (number: goes up by one on every change of content), `blocks`
  (list), `updated` (RFC 3339), `by` (string: the last writer). When set: `purpose`, `hints`,
  `actions`, `sessions`.
- A block: always `type` and `id` (strings; an id is unique in its board); `children` (list)
  on a block that holds some; every other key is one of the type's fields, as `margin schema
  <type>` lists them (`margin schema --json` for all, with each field's type).
- `state.decisions`: by block id (`<id>#…` for margin's own bookkeeping such as `t1#time`:
  ignore keys with a `#` you do not know). Each: `kind`, `value`, `by`, `at`, `seq` (the event
  that recorded it; 0 when none did); `via` and `reason` when known. A todo's status lives here,
  not in the block.
- Rows a table reads from a dataset (`"rows": "<name>"`) are not in the document: read the
  dataset.

`margin show <board> <id> --json`: one block.

```json
{"v": 1, "artifact": "reviews", "rev": 1,
 "block": {"type": "tr", "id": "pr-7", "target": "https://example.com/pr/7", "detail": "Fails one run in five.", "cells": {"title": "Fix the flaky test"}},
 "decision": null}
```

Always `v`, `artifact` (the board's name), `rev`, `block`, and `decision` (an object as above,
or `null`). `margin ls --json` lists the boards: `{"v":1,"artifacts":[{"id","rev","updated",
"title","kind"}]}`.

## A dataset

Records are JSON values, one per line: a bare number, or an object whose fields a plot or a
table picks by name. margin adds nothing to a record and reads no meaning into its keys.

`margin data ls --json`, the registry entries:

```json
{"v":1,"datasets":[{"name":"queue-depth","path":"<store>/demo/data/queue-depth.jsonl","external":false,"owner":null,"cap":100,"expect_every":120.0,"added":"2026-11-11T17:43:06Z","bytes":42,"changed":"2026-11-11T17:43:06Z","stale":false}]}
```

`margin data show <name> [--last N] --json`, one entry with its newest records, oldest first:

```json
{"v":1,"name":"queue-depth","path":"<store>/demo/data/queue-depth.jsonl","external":false,"owner":null,"cap":100,"expect_every":120.0,"stale":false,"bytes":42,"bytes_read":42,"records":[5,{"depth":7,"at":"2026-11-11T10:00:00Z"}]}
```

| field | type | meaning |
|---|---|---|
| `name` | string | what a board binds to |
| `path` | string | the file the records are in (yours, with `--file`) |
| `external` | bool | registered with `--file`: margin reads it and never writes it |
| `owner` | string or null | the registered owner that alone may write it |
| `cap` | number or null | the newest records kept |
| `expect_every` | number or null | seconds between writes before it counts as stale |
| `stale` | bool | overdue by `expect_every` now |
| `bytes` | number | the file's size |
| `added`, `changed` | string | (`ls`) RFC 3339: registered, last written (`changed` is null before the first write) |
| `bytes_read`, `records` | number, list | (`show`) what this read cost, and the records |

Write with `margin data append <name> <value|json|->` and `margin data put <name> < records`.
A write to a dataset is not a write to a board: no revision, no event.

## The handler contract

`margin handler add <name> --on <glob>… [--board B --owns PREFIX] [--timeout 60s] [--retries 3]
-- <cmd…>`. For each batch of matching events the daemon runs the command:

- **argv**: exactly the words after `--`, executed directly, no shell. A first word of exactly
  `margin` is this margin binary.
- **cwd**: the directory `handler add` was run in.
- **Environment**: the daemon's, with `PATH` replaced by the registering shell's, and
  `MARGIN_HANDLER` (its name), `MARGIN_PROJECT`, `MARGIN_HOME`, `MARGIN_BIN` (the margin binary's
  path); with a board also `MARGIN_BOARD` and `MARGIN_OWNS`. margin passes no secret.
- **stdin**: the matching events after the handler's cursor, one event object per line (the
  shape above), oldest first, then end of input. A handler may ignore it.
- **stdout**: JSON lines, one value per line; blank lines and lines starting `#` are skipped. A
  component `{"type", "id", …}` (replaces the one with that id, children reconciled by id; a
  new id is added at the top level or under `"parent"`); an op `{"op": "set"|"rm"|"move"|"add",
  …}` or a list of ops applied all or none; `{"emit": "<name>", "value": …}`; `{"toast":
  "text", "level": "ok|info|warn|error", "at": "<id>"}`. A line that is refused (outside
  `--owns`, a schema error) is logged and the others still apply.
- **stderr**: kept in `margin handler logs <name>`; its last line is the `stderr` of a
  `handler.failed`.
- **Exit 0**: the lines are applied, the emits appended (`by: "handler:<name>"`, `via:
  "handler"`) and the cursor moved past the last event given, in one step. **Any other exit, a
  signal, or the timeout**: nothing is applied and the cursor stays; the run is retried after 1
  s, 2 s, 4 s …, and after `--retries` retries the batch is passed over with `handler.failed`.
- **At least once.** A run that did its work and then failed, or a daemon stopped mid-run,
  gets the same events again. Make the command safe to run twice; with side effects, remember
  the last `seq` finished and skip events at or below it.
- **Never given**: its own emits; `handler.*` unless a glob of its own starts `handler.`; a
  passive event unless named exactly (a view event: or by `'<board>.*'`). A new handler starts at the end of the log.

`margin handler ls --json`: `{"v":1,"daemon":{"running","pid"},"handlers":[{"name","on","cmd",
"board","owns","paused","state","cursor","pending","attempts","last_run","last_outcome",
"last_error"}]}`.

## The owner contract

A program that runs on its own and writes its rows itself, with no daemon involved:

```sh
margin owner add fetcher --board reviews --owns ci --expect-every 10m   # once; the board must exist
./fetch | margin apply reviews --as fetcher                             # the handler stdout grammar
```

- The owner's ids are `ci` and those starting `ci-`, `ci_` or `ci/` (a namespace, not a
  string prefix: never `city`); `--owns '*'` is the whole board. Only `--as fetcher` may write
  them, and it may write nothing else: either mistake is exit 1 and nothing is written. One
  id has one owner; an overlapping registration is refused.
- A write is all or nothing. One that changes nothing costs no revision and still counts as a
  write for staleness.
- **Staleness**: with `--expect-every`, once the last write is older than that the rows show
  `◷ stale` in the pane and `stale` is true in `margin owner ls --json`:
  `{"v":1,"owners":[{"name","board","owns","expect_every","added","last_write","stale"}]}`.
- An owner's rows still take the person's actions: ownership stops other writers, not keys.

## Exit codes

| code | when |
|---|---|
| 0 | done. Also: a read that found nothing new, `margin events --follow` ended by ctrl-c or a closed stdout |
| 1 | everything else that failed: a usage error (unknown command or flag, a missing argument), a refusal (the name exists, a stale `--base-rev`, an id outside `--owns`, an action the block does not offer), invalid input (bad JSON, a schema error) |
| 3 | not found: the message starts `no board`, `no component`, `no handler`, `no owner` or `no dataset` |
| 2 | only `margin hook rewake`: the Claude Code hook convention for "wake the session" |

On failure the message is on stderr as `margin <command>: <why>`, stdout is empty, and nothing
was written. There is no separate code for usage errors today: tell "not created yet" (3)
from "something is wrong" (1), and read no more into it.

## Cursors

A cursor is a named position in the log: the last `seq` its reader has taken.

- `margin events --cursor NAME` starts after the cursor and moves it past each event as that
  event is printed. A name used for the first time starts at the **end** of the log; `--since
  N` starts after N instead (`--since 0` replays what is still kept).
- **At most once**: with a moving cursor, an event counts as taken once printed. A reader that
  dies before acting on it does not see it again.
- **At least once**: read with `--no-commit` (the cursor stays), act, then `margin events
  --cursor NAME --commit SEQ`. A reader that dies sees the event again, so dedupe on `seq`.
- `--no-commit` and `--commit` need `--cursor`. A handler's cursor is `handler:<name>`:
  `margin events --cursor handler:<name> --no-commit --name '*' --json` lists what waits for it.
- The log rotates past a few MB and keeps two older generations. `seq` goes on counting, so a
  cursor stays valid; one left further behind than what is kept has lost the events between.

```sh
margin events --follow --cursor mine --no-commit --name 'reviews.*' --json | while read -r e; do
  handle "$e" && margin events --cursor mine --commit "$(jq .seq <<<"$e")"; done
```
