# graphs: links between rows drawn as lanes (`after`); {{user}}'s steps (`owner`); steps that rest on claims (`rests_on`)

**A row can name the rows it follows.** `"after": "id"` or `"after": ["id", "id"]` on a row names
other rows of the same list, and the pane draws each link as a lane beside the rows. What a link
means is yours: a step that waits on a step, a pull request stacked on its base, a ticket linked
to a ticket, a job and the jobs it waits on. It is one mechanism, drawn in two places: the todos
of a section, and the rows of a table.

How lanes read:

- Rows stay in the order written. Each row has a node; a line joins a row to every row it names,
  whether that row is above or below.
- One row followed by several is a fork; one row that follows several is a merge. A line that
  passes a row it does not belong to runs straight through, in its own lane.
- A row with no link has a node and no line. A new chain starts in another lane than the one a
  chain has just ended in, so two chains never read as one.
- An id that names no row of the list is ignored: rows come and go, and the write is not refused.
- The glyphs are box-drawing characters; the config key `graph_glyphs: "flog"` draws them with
  Flog symbols, in a font that has them.

Todos of one section: the node is the step's status glyph, and a line takes the colors of the two
steps it joins.

```json
{"type": "section", "id": "work", "title": "Work", "children": [
  {"type": "todo", "id": "t1", "text": "Split the retry path", "status": "done"},
  {"type": "todo", "id": "t2", "text": "Cap the backoff", "after": "t1"},
  {"type": "todo", "id": "t3", "text": "Metrics for the cap", "after": ["t1", "t2"]}]}
```

Rows of one table: add a column of kind `graph`; lanes are dim, and a row with no link has a
hollow node (`margin help dashboards`, "A graph column").

```json
{"type": "table", "id": "pr", "columns": [{"key": "g", "kind": "graph"}, {"key": "title"}], "children": [
  {"type": "tr", "id": "pr-14", "after": "pr-13", "cells": {"title": "Cap the backoff"}},
  {"type": "tr", "id": "pr-13", "cells": {"title": "Split the retry path"}}]}
```

Two fields on a todo say more than a link does:

**Some steps belong to {{user}}.** A step only they can do (a decision, trying something on a
phone, asking someone) gets `"owner": "you"`; they can also claim a step with `o` in the pane, or
hand one back. Their steps show in their own color, your prompt line lists them, and their circle
finishes them: the one place they set done. When one is next in your plan, don't do it and don't
route around it: say "your step: t4", work on what doesn't depend on it, and pick up when
"{{user}} finished their step t4" arrives.

**A step can name the claims it rests on**: `"rests_on": ["A1"]`. The pane tags the step with the
claim's id; when {{user}} strikes A1, every open step resting on it turns red, and the strike
reaches you with those steps listed. Re-plan them before doing them. State the assumption as a
claim and link it whenever a step only makes sense if the claim holds.
