# dashboards: a standing board (tables, run strips, tabs, badges) kept current by a handler, with keys on its rows

A dashboard is a board that stays up and shows the state of something outside the chat: pull
requests to review, services, a work tracker. It differs from a plan pane in who writes it: you
design it once, and a script keeps it current with no AI involved. The script is a handler:
margin runs it when a refresh event is appended, and whatever owns the schedule (cron, a
systemd timer) appends that event. margin itself has no timer.

## The model

- **One board = one dashboard = one tab.** A board is a named document (`reviews`); a pane
  shows one, or several as tabs along its title line.
- **A document is a tree of blocks**, each `{"type": …, "id": …, fields…, "children": […]}`. Ids are
  yours to choose (letters, digits, `-`, `_`) and unique in the board.
- **Ids are identity.** A write that carries a block with a known id updates that block where it
  stands; an unknown id is a new block; rewriting a parent without a child removes the child. So
  derive ids from the data (`pr-4812`, `svc-ingest-worker`), never from a position.
- **The script rewrites its blocks; the pane reconciles by id.** It prints whole blocks; a
  row whose id persists changes in place and keeps the focus, and an unchanged run writes nothing.
- **Whoever keeps rows current owns an id prefix** (`--owns pr`: the ids `pr` and `pr-…`): a
  handler, an outside program registered with `margin owner add`, or a feed. Inside it only that
  owner writes; every other id in the same board is yours. Lay the board out so that the
  machine-kept parts sit under one prefix.
- **The pane runs nothing.** A key on a row appends an event (`<board>.<action>`; enter on a row
  with a `target` appends `<board>.open`), and the handler registered for that name does the
  work. So a board can offer "retry", "cancel", "open" without the pane knowing how.
- **What {{user}} does is stored apart from the document**, by id: the fold of a section, the tab
  on show, the focus, a step marked next. A rewrite with the same ids keeps all of it.
- **Boards, handlers and events belong to a project**: `$MARGIN_PROJECT`, else the repository root above
  the working directory, else the directory. A board made in one directory is invisible from
  another, and a command run from cron lands in whatever project its directory maps to. For
  standing boards pick one name and pass it everywhere: `--project boards` on every command,
  the cron line included (or export `MARGIN_PROJECT=boards`).

## Recipe

`margin example` lists complete boards (`reviews`, `services`, `tracker`), each with the
script that fetches its rows and sample data. Start from the nearest one; `margin example <name>` prints the steps.

**(a) Design it.** Decide, before writing JSON:

- What changes goes first. No title block, no Goal section: orientation is the one `purpose` line.
- Which component per fact (reference below): many items with the same fields → `table`; a few
  items with a sentence each → `row`s; recent outcomes → a `runs` strip or a `runs` column; two
  views of the same data ("Failing" / "All") → `tabs`; a break with a label → `rule`.
- The ids: one prefix for everything the script writes, with a root block whose id is the prefix
  itself (`pr`) when it writes one block, or siblings `wk-now`, `wk-next` when it writes several.
- What {{user}} should be able to do to a row, as `actions` (below): each is a key, a name and a
  handler somebody registers.
- Text someone wrote (a description, a note) goes where text wraps: `text`, `bullet`, a row's
  `detail`. Data from outside (a title, a branch name) goes in a table cell or a row's `text`,
  which are cut with `…` at the edge and shown in full when the row has the focus.
- Color only to tell one kind of thing from another on the same screen: who (badges), outcome
  (run squares, state glyphs), phase (a `tone`). If two things have the same color they should be
  the same kind of thing. Never color for decoration.
- A symbol is followed by a space (`✗ failed`, never `✗failed`) in any text you write.

**(b) Create it**: `margin new` with a whole document on stdin. This fixes the layout and the
parts you own. `new` is the only command that creates a board, and it refuses one that exists.

```sh
margin new reviews <<'EOF'
{"title": "Reviews", "purpose": "Pull requests waiting on my review",
 "hints": ["enter  open the pull request", "d  its description"],
 "blocks": [
  {"type": "rule", "id": "waiting", "text": "Waiting on you"},
  {"type": "table", "id": "pr", "empty": "nothing to review",
   "columns": [{"key": "pr", "title": "PR", "kind": "id"}, {"key": "title", "title": "Title"}]}]}
EOF
margin render reviews --width 100 --height 40     # one frame as text: check it without a pane
margin render services --section svc-all          # a `tabs` block's other section (default: its first)
margin render services --focus svc-db --focused   # the focus on a block; its section comes on show
margin show reviews                               # the tree: type, id, text
```

- Envelope keys: `title`, `purpose`, `hints`, `kind`, `actions`, `blocks`, nothing else (`margin schema
  document` describes each). It prints `reviews rev 1`.
- From here on every write is an update, and an update never creates: `margin apply`, `add`,
  `set`, a handler's run and the rest fail on a name with no board (`no board "reveiws" …:
  create it with margin new reveiws (did you mean "reviews"?)`). To redo the layout, replace the
  document with `margin apply reviews < doc.json` (a whole document replaces), or start over: `margin rm reviews --yes`, then `new`.
- A schema error names the block and the field and writes nothing; `margin schema <type>` lists
  a type's fields.
- The owned block starts empty and its script fills it: the `table` above has no rows. For a
  script that writes a `tabs` block, the skeleton is the block with its sections, each empty:

```json
{"type": "tabs", "id": "svc", "children": [
  {"type": "section", "id": "svc-failing", "title": "Failing", "children": []},
  {"type": "section", "id": "svc-all", "title": "All", "children": []}]}
```

- The pane's bottom line reads `MARGIN <board> <project>`: the second field is the project
  the board belongs to (`--project`), not a directory.

**(c) Write the script that fetches the rows**: a program that prints `margin apply` input and
exits 0. For a table that is one line: the table block with all its `tr` children.
`margin help feeds` has the line grammar and a complete script; test it by hand with
`./script | margin apply reviews`.

**(d) Register it as a handler, and schedule the event outside margin** (register from the
directory the script's relative paths start at). The board must exist by now: a handler
updates its board and never creates it.

```sh
margin handler add reviews-refresh --on reviews.refresh --board reviews --owns pr -- ./reviews-feed.sh
margin emit reviews.refresh       # run it now; `margin handler ls` shows `ran 1s ago · ok`
margin handler add open --on '*.open' -- margin handle open     # enter on a row opens its target
crontab -e                        # the schedule: */2 * * * *  /path/to/margin emit reviews.refresh --project <project>
```

- `margin help integrate` has the same as a systemd user timer, and on a file change
  (`watchexec`, `entr`, a path unit). `margin help handlers` has the handler contract.
- A program that already runs on its own can skip the handler and write its rows itself:
  `margin owner add fetcher --board reviews --owns pr --expect-every 10m`, then `./fetch |
  margin apply reviews --as fetcher`. The rows show as stale when it stops writing.
- Optional convenience, the older runner: `margin feed add reviews --every 2m --board reviews
  --owns pr -- ./reviews-feed.sh` schedules the script inside margin's daemon
  (`margin help feeds`). One prefix has one owner: use it instead of the handler, not beside it.

From now on the ids `pr` and `pr-…` are read-only for you, and a whole-document `margin apply`
must carry them through unchanged, so change your own parts by id instead: `margin set reviews
note1 text="…"`, `margin add reviews - text note2 "…" --after note1`, `margin rm reviews note2`.

**(e) Show it.** One pane holds several dashboards as tabs:

```sh
margin open reviews --tabs services,tracker   # a pane on reviews, the others as tabs beside it
margin tab reviews add services               # add a tab to the pane whose own board is reviews
margin tab reviews rm services | margin tab reviews show services | margin tab reviews
margin view reviews services tracker          # the renderer itself, for a plain terminal
```

- The live pane's status glyphs (a `state` cell, a row's `glyph`, a todo's circle) are Nerd Font
  icons: without one they show as boxes or blanks, and the board still works. `margin render`
  prints ordinary stand-ins (`✓ ✗ ○ ▸`, `[ Mine ]` for the tab on show) unless `--ansi`.
- `open` never moves the focus. Outside Zellij it prints the `margin view …` command for
  {{user}} to run in a terminal. Open a pane only when {{user}} asked for one.
- A tab that is not on screen carries marks: one for changes not yet seen, one when the board
  waits on {{user}} (an open `question`, a `todo` with `"owner": "you"`), and `✗` when something
  failed. The `✗` comes from a `row` with `"glyph": "fail"`, a table row whose `state` cell is
  `fail`, a `status` with `"state": "fail"` or a `todo` with status `blocked`.
- Keys in the pane: arrows or `gt` / `gT` switch tabs; `]t` / `[t` switch a `tabs` block's
  sections; `j` / `k` move the focus; enter on a row with a `target` appends `<board>.open` (the
  `open` handler above opens it); a key from the row's `actions` appends that action's event; `d`
  opens the detail panel (the focused row's `detail` and every field); `]c` / `[c` walk what changed; `zh` folds the
  pinned head; `za` folds a section; `?` lists the rest.

- To take a board down: `margin rm <board>` prints what would go (title, rev, its feeds, the panes
  showing it) and removes nothing; `margin rm <board> --yes` unregisters its feeds and moves it to
  the project's `.archive/` directory (nothing is deleted). Ask {{user}} first.

**(f) Keys on rows: `actions`.** A table (for each of its rows), a `tr`, a `row`, a `status`, a
`link` or the document itself declares what {{user}} can do there:

```json
{"type": "table", "id": "job", "actions": [{"key": "e", "name": "retry", "label": "retry the job"}],
 "columns": [{"key": "name", "title": "Job"}], "children": []}
```

- With such a row focused, `e` appends the event `<board>.retry` with the row's id and a snapshot
  of it (`cells`, `target`, `detail`); the hint row shows `e retry the job`, and the detail panel
  lists it. A click on either does the same. `margin act <board> <id> retry` is the key from a script.
- The handler registered `--on <board>.retry` does the work (`margin help handlers`, "Make a row
  do something"). With none, the row says `nothing handles <board>.retry`.
- `key` is one character the pane does not use itself (a write that takes one is refused and
  lists them); `label` defaults to `name`. A row's own action wins over its table's, and the
  table's over the document's. Document-level actions go in the envelope: `"actions": [{"key":
  "u", "name": "refresh"}]` beside `title`.
- `open` is never declared: every block with a `target` has it, on enter.

**(g) Purpose and hints** (in the envelope, or later; a whole-document write without them keeps them):

```sh
margin purpose reviews "Pull requests waiting on my review"   # one line, pinned at the top
margin hints reviews "enter  open the pull request" "merge queue freezes Fri 3pm"
```

- `purpose`: what the board is for, in one line. No text clears it.
- `hints`: reminders at the bottom while this tab is on screen, one argument each. `key  text`
  with two spaces draws the key as a key. Use them for what is specific to this board (what a
  column means, a key that matters here), not for margin's own keys. No arguments clears them.

## Components

Every block takes `type` and `id`. `margin schema` prints every field; these are the ones a
dashboard uses. `tone` values: `green ok yellow warn red bad orange changed blue claude aqua purple
you grey dim fg`.

**rule**: a dim line, with an optional label at the left. A break between parts.

```json
{"type": "rule", "id": "waiting", "text": "Waiting on you"}
```

**table** and **tr**: a header and aligned columns; each row is a child block of type `tr` with
its own id.

```json
{"type": "table", "id": "pr", "empty": "nothing to review", "zebra": false,
 "columns": [
   {"key": "pr", "title": "PR", "kind": "id"},
   {"key": "by", "title": "Author", "kind": "who"},
   {"key": "title", "title": "Title"},
   {"key": "ci", "title": "CI", "kind": "runs", "summary": true, "optional": true},
   {"key": "age", "title": "Age", "kind": "age"}],
 "children": [
   {"type": "tr", "id": "pr-4812", "target": "https://example.com/pulls/4812",
    "detail": "Longer text about the row, for the detail panel.",
    "cells": {"pr": "#4812", "by": "priya", "title": "Cap retry backoff at **5 min**",
              "ci": ["ok", "ok", "fail", "running"], "age": "2026-11-10T16:05:00Z"}}]}
```

- Table fields: `columns`, `empty` (text shown while it has no rows), `zebra` (stripe alternate
  rows; default from the config, off).
- A column is `{key, title?, kind?, align?, width?, optional?, summary?}`. `align`: `left` `right`
  `center` (`num`, `age` and `runs` default to right). `width`: fix it at this many cells.
  `optional: true`: drop this column first when the pane is narrow. `summary: true` (a `runs`
  column only): each strip ends with its failure count.
- Row fields: `cells` (an object keyed by column key; a missing key is a blank cell), `target` (a
  URL or `mg:` token: enter or a click appends `<board>.open` for the `open` handler), `detail`
  (shown in the detail panel), `actions` (keys on this row; on the table: on every row).

| kind | the cell holds | drawn as |
|---|---|---|
| `text` (default) | text | the text; `**bold**`, `` `code` `` and `@[name]` work |
| `num` | a number or text | right-aligned |
| `who` | a name | the name's badge: a glyph and the name in that name's color |
| `id` | a PR number, a ticket key | the id in its own stable color |
| `runs` | a list of `"ok"` `"fail"` `"skip"` `"running"` or `null` | a run strip, newest at the right (at most the newest 20); with `"summary": true` on the column, then failed/ran such as `1/6` (dim; red when the newest run failed), inside the column's width |
| `age` | an RFC 3339 time, or unix seconds | a live age (`12s` `4m` `2h` `3d`) the pane keeps ticking |
| `state` | `spin` `done` `fail` `you` `perm` `idle` `ended` `commit` `wc` `file` `term`, or any one character | a status glyph in its color (`spin` animates) |

- Width: every kind but `text` takes its natural width; `text` columns share the rest and are cut
  with `…`. Below 20 cells of text, `optional` columns drop, rightmost first. The focused row
  shows its cut cells in full.
- Refused on write: a `tr` outside a table, any other block inside one, a cell whose key names
  no column, a `runs` value outside the five, an unknown `kind` or column field, a repeated key,
  `summary` on a column that is not `runs`.

**runs**: a run strip on its own line: the last n outcomes, oldest left, one square each, a cross
for a failure. `summary: true` ends it with a count such as `2/12 failed`.

```json
{"type": "runs", "id": "svc-fr-ingest", "label": "last 6 runs",
 "values": ["ok", "ok", "fail", "skip", null, "running"], "summary": true}
```

**row**: one line about one thing, when a table is too rigid: a glyph, a tag, a badge, text, and
on the right an aside and a live age. `detail` wraps under the row (three lines until focused).

```json
{"type": "row", "id": "svc-f-ingest", "glyph": "fail", "tag": "OPS-190", "who": "ada",
 "text": "ingest-worker", "aside": "exit 1", "since": "2026-11-10T17:40:00Z",
 "detail": "connection refused (db:5432) after 3 retries", "target": "https://example.com/x"}
```

- `glyph`: the `state` names above. `since`: RFC 3339 or unix seconds, drawn as an age (write the
  time, never "2h ago": the pane ticks it, and the feed writes only when something changed).
- `tag_tone`, `aside_tone`, `text_tone`, `tone` take a tone; `bold: true`; `meta` is dim text at
  the far right. A row's `text` takes inline markup like a text cell, and is cut at the edge
  (shown in full when the row has the focus).
- A row may hold children (rows under it).

**status**: a check or a job: `{"type": "status", "id": "s1", "text": "deploy", "state": "fail",
"result": "exit 2", "spark": [1, 3, 2], "target": "https://…"}`. `state`: `running` `pass` `fail`.

**link**: `{"type": "link", "id": "l1", "text": "runbook", "target": "https://…"}`. A target is a
URL, or `mg:open/<board>` to open another board.

**tabs**: views of one board. Its children are `section`s; their titles form a strip and one
section's content shows at a time (`]t` / `[t`, or a click). The first is the default. It takes
no fields of its own. Put counts in the titles.

```json
{"type": "tabs", "id": "svc", "children": [
  {"type": "section", "id": "svc-failing", "title": "Failing (2)", "children": []},
  {"type": "section", "id": "svc-all", "title": "All (5)", "children": []}]}
```

**section**: a titled group that folds: `title`, `note` (right of the title), `summary` (shown
while folded), `folded: true` (starts folded), `tone`. Children are any blocks.

**text**, **bullet**: prose that wraps. `text` takes `style`: `note` `dim` `warn`. A `bullet`
holds sub-bullets as children. Both take `tone`.

**todo**: a step: `text`, `note`, `status` (`todo` `doing` `done` `blocked` `skipped` `next`),
`owner` (`you`: a step that is {{user}}'s). Its id shows beside it when it is 8 characters or
fewer with no dash. **decided**: a settled decision: `text`, `by` (`claude` or `you`), `at`.
**question** (`margin ask … --suggest`) and **claim** work on a dashboard as in a plan.

**Inline markup** in `text`, `bullet`, `todo`, `claim`, a `row`'s `text` and text cells: `**bold**`, `` `code` ``,
and `@[name]`, which draws the name's badge (`"review from @[ada] is pending"`). A name gets the
same glyph and color everywhere it appears: a `who` column, a row's `who`, `@[name]`.

## Numbers that change often

A board holds layout and text someone wrote, which change rarely. Values that change more often
than the layout (a request rate, a latency, a queue length, the last runs of a job) go in a
**dataset**: named, kept in the project outside every document, and read by the pane when it
draws. Appending a number to a dataset redraws the plot bound to it and changes nothing about the
board: no new revision, no change bar, no unseen count. The pane runs nothing to get data;
whoever has the numbers writes them (`margin help integrate`, "Publish numbers for a board to draw").

```sh
margin data add latency --cap 2000 --expect-every 1m     # once: register it (a write to an unregistered name is an error)
margin data append latency '{"t":"2026-11-10T19:40:00Z","p50":41,"p95":180}'   # from anywhere, as often as you like
margin data add requests --file /var/log/app/requests.jsonl   # or: a JSON lines or CSV file a program already writes
margin data ls        margin data show latency --last 5        margin data put prs < prs.json
```

A record is a JSON object, or a bare number (a dataset of bare numbers is one unnamed series).
`--cap N` keeps the newest N records. `--expect-every 1m` marks what is bound to the dataset
`◷ stale` once it has not changed for that long. `--file` is read in place and never written.

A component binds to a dataset by name:

**plot**: lines, an area, bars, columns, a histogram, sparklines or heat strips (`kind`; default
`line`). Bound with `data`, it draws one series per field in `y`, from the newest `last` records
(default: as many as the plot is wide).

```json
{"type": "plot", "id": "latency", "data": "latency", "y": ["p50", "p95"], "x": "t", "last": 120,
 "height": 6, "legend": true, "threshold": 250}
```

- `y`: a field, or a list of fields. Each series is labeled with its field's name; `series`
  renames and colors them by position (`"series": [{"label": "median"}, {"color": "orange"}]`).
  Without `y`, bare numbers are one series, and records draw every numeric field.
- `x`: a field holding the record's time (RFC 3339 or epoch seconds). The ends of the axis then
  read as ages (`-5m`, `now`; the right end shows an age instead of `now` when the newest record
  is late).
- A plot may instead carry its values in the document (`"series": [{"label": "p50", "values":
  [41, 40, 44]}]`): right for numbers that are part of what was written, such as a benchmark result.

**table** rows from a dataset: `rows` names it, `key` names the record field that identifies a
row (default `id`). Each record is a row: its fields are the cells by column key, its `target`
and `detail` fields are the row's, and its id is `<table id>-<key value>`, so the focus, the
table's `actions` and `open` follow the record (the event's snapshot carries it). Records sharing
a key are one row, the newest. The 200 newest records show.

```json
{"type": "table", "id": "queue", "rows": "queue", "key": "id", "empty": "nothing queued",
 "columns": [{"key": "id", "title": "Job", "kind": "id"}, {"key": "what", "title": "What"},
             {"key": "since", "title": "Queued", "kind": "age"}]}
```

**runs** from a dataset: `{"type": "runs", "id": "nightly", "label": "nightly", "data": "nightly",
"y": "outcome", "summary": true}` reads `ok` `fail` `skip` `running` from that field of the newest
records.

**Inline**: `~[@latency.p95]` in any text that takes markup is the newest values as a sparkline in
the line: 12 cells, or `~[@latency.p95:24]` for 24; `~[@latency.p95|orange]` colors it; `~[@name]`
reads bare numbers. (`~[3 5 2 9]` draws numbers written in the text itself.)

A dataset that is not registered draws one dim line where the component would be, naming the
command that registers it; the rest of the board draws as usual.

**A dataset, or rows written by a handler or an owner?**

- **A dataset** for values: many points of the same shape, where the newest ones matter and no
  single point has an identity. Cheap to write from a loop or a cron line, and a write never marks
  the board as changed.
- **Rows** (a handler's, an owner's: `tr`, `row`, `status`) for state with identity that changes
  occasionally: this pull request, that service. A change there is news: the row gets a change
  bar and the tab its unseen count, which is what you want when a service goes red and not when a
  number ticks.
- A table bound to a dataset sits between: records with a key, replaced wholesale by whoever has
  them (`margin data put`), shown without marking the board. Use it for a list that churns
  (a queue); use owned rows when each change should be noticed.

## Theme

The look is set per machine under `dashboard` in `~/.config/margin/config.json` (or the file
`$MARGIN_CONFIG` names). Every key is optional; these are the defaults. A pane reads it at start.
Change it only when {{user}} asks: it restyles every board.

```json
{"dashboard": {
  "zebra": false, "zebra_color": "bg1",
  "rule_glyph": "─", "rule_color": "bg4",
  "emphasis": "fg",
  "who": {"glyphs": ["●", "■", "▲", "▼", "◆", "★", "♥", "♣", "♠", "✚", "◗", "⬢"],
          "colors": ["orange", "yellow", "green", "aqua", "blue", "purple",
                     "#62b8f0", "#8f9df8", "#b88cf0", "#ee7fd0", "#c8dc50", "#b89a78"],
          "names": {"ada": {"glyph": "★", "color": "purple"}}},
  "runs": {"gap": false,
           "ok": {"glyph": "▪", "color": "green"}, "fail": {"glyph": "✗", "color": "red"},
           "skip": {"glyph": "▪", "color": "dim"}, "running": {"glyph": "▪", "color": "yellow"},
           "none": {"glyph": "·", "color": "bg4"}}}}
```

- `zebra`: stripe rows in tables that don't say; `zebra_color`: the stripe's ground.
- `rule_glyph`, `rule_color`: the line of a `rule` and under a table's header.
- `emphasis`: the color of `**bold**` (default: the text color, so bold is only bold).
- `who.glyphs`, `who.colors`: the lists a name hashes into (ids hash into the colors too).
  `who.names.<name>`: a fixed `glyph`, `color` or both for one name, matched without case.
- `runs.gap`: a blank cell between squares. `runs.ok|fail|skip|running|none`: `glyph`, `color`.
- A color is `#rrggbb` or `red orange yellow green aqua blue purple grey dim fg bg1 bg2 bg4`. A
  glyph is one cell wide. A value that can't be used is skipped and the default stands. The
  default badge colors leave out red and grey: red means failure and grey inactive everywhere.
