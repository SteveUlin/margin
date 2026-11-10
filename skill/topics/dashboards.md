# dashboards: a standing board (tables, run strips, tabs, badges) that a feed keeps current

A dashboard is an artifact that stays up and shows the state of something outside the chat: pull
requests to review, services, a work tracker. It differs from a plan pane in who writes it: you
design it once, and a script (a feed) keeps it current with no AI involved.

## The model

- **One artifact = one dashboard = one tab.** An artifact is a named document (`reviews`); a pane
  shows one, or several as tabs along its title line.
- **A document is a tree of blocks**, each `{"type": …, "id": …, fields…, "children": […]}`. Ids are
  yours to choose (letters, digits, `-`, `_`) and unique in the artifact.
- **Ids are identity.** A write that carries a block with a known id updates that block where it
  stands; an unknown id is a new block; rewriting a parent without a child removes the child. So
  derive ids from the data (`pr-4812`, `svc-ingest-worker`), never from a position.
- **A feed rewrites its blocks; the pane reconciles by id.** The script prints whole blocks; a
  row whose id persists changes in place and keeps the focus, and an unchanged run writes nothing.
- **A feed owns an id prefix** (`--owns pr`: the ids `pr` and `pr-…`). Inside it only the feed
  writes; every other id in the same artifact is yours. Lay the board out so that the machine-kept
  parts sit under one prefix.
- **What {{user}} does is stored apart from the document**, by id: the fold of a section, the tab
  on show, the focus, a step marked next. A rewrite with the same ids keeps all of it.
- **Artifacts and feeds belong to a project**: `$MARGIN_PROJECT`, else the repository root above
  the working directory, else the directory. A board made in one directory is invisible from
  another. For standing boards pick one name and pass it everywhere: `--project boards` on every
  command (or export `MARGIN_PROJECT=boards`).

## Recipe

`margin example` lists three complete boards (`reviews`, `services`, `tracker`), each with its
feed script and sample data. Start from the nearest one; `margin example <name>` prints the steps.

**(a) Design it.** Decide, before writing JSON:

- What changes goes first. No title block, no Goal section: orientation is the one `purpose` line.
- Which component per fact (reference below): many items with the same fields → `table`; a few
  items with a sentence each → `row`s; recent outcomes → a `runs` strip or a `runs` column; two
  views of the same data ("Failing" / "All") → `tabs`; a break with a label → `rule`.
- The ids: one prefix for everything the feed writes, with a root block whose id is the prefix
  itself (`pr`) when the feed writes one block, or siblings `wk-now`, `wk-next` when it writes several.
- Text someone wrote (a description, a note) goes where text wraps: `text`, `bullet`, a row's
  `detail`. Data from outside (a title, a branch name) goes in a table cell or a row's `text`,
  which are cut with `…` at the edge and shown in full when the row has the focus.
- Color only to tell one kind of thing from another on the same screen: who (badges), outcome
  (run squares, state glyphs), phase (a `tone`). If two things have the same color they should be
  the same kind of thing. Never color for decoration.
- A symbol is followed by a space (`✗ failed`, never `✗failed`) in any text you write.

**(b) Write it once**: a whole document on stdin. This fixes the layout and the parts you own.

```sh
margin apply reviews <<'EOF'
{"title": "Reviews", "purpose": "Pull requests waiting on my review",
 "hints": ["enter  open the pull request", "d  its description"],
 "blocks": [
  {"type": "rule", "id": "waiting", "text": "Waiting on you"},
  {"type": "table", "id": "pr", "empty": "nothing to review",
   "columns": [{"key": "pr", "title": "PR", "kind": "id"}, {"key": "title", "title": "Title"}]},
  {"type": "feed", "id": "pr-feed", "name": "reviews", "state": "not registered"}]}
EOF
margin render reviews --width 100 --height 40     # one frame as text: check it without a pane
margin render services --section svc-all          # a `tabs` block's other section (default: its first)
margin render services --focus svc-db --focused   # the focus on a block; its section comes on show
margin show reviews                               # the tree: type, id, text
```

- Envelope keys: `title`, `purpose`, `hints`, `blocks` (nothing else). It prints `reviews rev N`.
- A schema error names the block and the field and writes nothing; `margin schema <type>` lists
  a type's fields.
- The last block above places the feed's health line (state, trigger, last write, the error when
  it crashes). Its id is `<prefix>-feed`. Leave it out and the daemon inserts it above the block
  whose id is the prefix (or at the end of the document), which pushes it ahead of the content.

**(c) Write the feed script**: a program that prints `margin apply` input and exits 0. For a
table that is one line: the table block with all its `tr` children. `margin help feeds` has the
contract and a complete script; test it by hand with `./script | margin apply reviews`.

**(d) Register it** (from the directory the script's relative paths start at):

```sh
margin feed add reviews --every 2m --artifact reviews --owns pr -- ./reviews-feed.sh
margin feed ls            # live / restarting, last write, last error
```

From now on the ids `pr` and `pr-…` are read-only for you, and a whole-document `margin apply`
must carry them through unchanged, so change your own parts by id instead: `margin set reviews
note1 text="…"`, `margin add reviews - text note2 "…" --after note1`, `margin rm reviews note2`.

**(e) Show it.** One pane holds several dashboards as tabs:

```sh
margin open reviews --tabs services,tracker   # a pane on reviews, the others as tabs beside it
margin tab reviews add services               # add a tab to the pane whose own artifact is reviews
margin tab reviews rm services | margin tab reviews show services | margin tab reviews
margin view reviews services tracker          # the renderer itself, for a plain terminal
```

- `open` never moves the focus. Outside Zellij it prints the `margin view …` command for
  {{user}} to run in a terminal. Open a pane only when {{user}} asked for one.
- A tab that is not on screen carries marks: one for changes not yet seen, one when the board
  waits on {{user}} (an open `question`, a `todo` with `"owner": "you"`), and `✗` when something
  failed. The `✗` comes from a `row` with `"glyph": "fail"`, a `status` with `"state": "fail"` or a `todo` with
  status `blocked`. A table's `state` cell does not raise it: if a failure should mark the tab,
  give it a `row` (the `services` example does).
- Keys in the pane: arrows or `gt` / `gT` switch tabs; `]t` / `[t` switch a `tabs` block's
  sections; `j` / `k` move the focus; enter opens the focused row's `target`; `d` opens the detail
  panel (the focused row's `detail` and every field); `]c` / `[c` walk what changed; `zh` folds the
  pinned head; `za` folds a section; `?` lists the rest.

**(f) Purpose and hints** (in the envelope, or later; a whole-document write without them keeps them):

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
  URL or `mg:` token that enter or a click opens), `detail` (shown in the detail panel).

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
  the far right. A row's `text` is plain and cut at the edge: no `**bold**` or `@[name]` there.
- A row may hold children (rows under it).

**status**: a check or a job: `{"type": "status", "id": "s1", "text": "deploy", "state": "fail",
"result": "exit 2", "spark": [1, 3, 2], "target": "https://…"}`. `state`: `running` `pass` `fail`.

**link**: `{"type": "link", "id": "l1", "text": "runbook", "target": "https://…"}`. A target is a
URL, or `mg:open/<artifact>` to open another artifact.

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

**Inline markup** in `text`, `bullet`, `todo`, `claim` and text cells: `**bold**`, `` `code` ``,
and `@[name]`, which draws the name's badge (`"review from @[ada] is pending"`). A name gets the
same glyph and color everywhere it appears: a `who` column, a row's `who`, `@[name]`.

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
