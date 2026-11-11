#!/usr/bin/env bash
# margin feed for the `tracker` example: a JSON file of work items -> the sections `wk-now`,
# `wk-next` and `wk-waiting`.
#
#   margin example tracker | margin new tracker                  # the board it updates (a feed never creates one)
#   ./tracker.feed.sh tracker.data.json                          # look at the lines it prints
#   ./tracker.feed.sh tracker.data.json | margin apply tracker   # apply them by hand (before registering)
#   margin feed add tracker --watch tracker.data.json --board tracker --owns wk \
#       -- ./tracker.feed.sh tracker.data.json
#
# It prints THREE lines, one section each. A line is a whole component: margin replaces the one
# with that id where it stands, so the sections keep the places `margin new` gave
# them, and the `decided` section between and after them (not under `wk`) is never touched: the
# feed owns `wk` and `wk-...`, Claude and the person own every other id in the same board.
#
# Now and Next are todos (one `doing` at most reads best); the item's key leads the text in
# **bold** and its owner follows as a badge, @[name]. Waiting is rows: `tag` is the key, `who` the
# person the item waits on, `aside` why, `since` when the wait began (the pane keeps the age
# current). A row's `text` is plain: **bold** and @[name] markup is for todo, text and bullet.
#
# What the person does to a todo in the pane (marks it next, skips it) is stored apart from the
# document, by id; a rewrite with the same id keeps it.
set -euo pipefail
data=${1:-tracker.data.json}
[ -r "$data" ] || { echo "tracker feed: cannot read $data" >&2; exit 1; }

jq -c '
  def slug: gsub("[^A-Za-z0-9_-]"; "-");
  def lane($l): [.items[] | select(.lane == $l)];
  def step: {type: "todo", id: "wk-\(.key | slug)",
             text: "**\(.key)** \(.title) @[\(.owner)]",
             status: .status, note: (.note // "")};
  def wait: {type: "row", id: "wk-\(.key | slug)",
             tag: .key, who: .waiting_on, text: .title, aside: .why,
             since: .since, detail: (.detail // ""), target: .url};

  {type: "section", id: "wk-now",     title: "Now",     children: [lane("now")[]  | step]},
  {type: "section", id: "wk-next",    title: "Next",    children: [lane("next")[] | step]},
  {type: "section", id: "wk-waiting", title: "Waiting", note: "on someone else",
   children: [lane("waiting")[] | wait]}
' "$data"
