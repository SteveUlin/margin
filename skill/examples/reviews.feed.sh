#!/usr/bin/env bash
# margin feed for the `reviews` example: a JSON file of pull requests -> the table `pr`.
#
#   margin example reviews | margin new reviews                  # the board it updates (a feed never creates one)
#   ./reviews.feed.sh reviews.data.json                          # look at the line it prints
#   ./reviews.feed.sh reviews.data.json | margin apply reviews   # apply it by hand (before registering)
#   margin feed add reviews --watch reviews.data.json --board reviews --owns pr \
#       -- ./reviews.feed.sh reviews.data.json
#
# The contract (margin help feeds): print `margin apply` input on stdout, one JSON value per line,
# and exit 0. This script prints ONE line: the whole table, id `pr`, with a `tr` child per pull
# request, id `pr-<number>`. margin replaces the table with that id and reconciles the rows by
# id: a known id updates in place, a new id appears, a missing id leaves. So the script never
# deletes anything and keeps no memory between runs.
#
# Every id it writes sits under the prefix the feed owns (`--owns pr`: `pr` and `pr-...`).
# A non-zero exit drops the whole run: the board keeps its last rows and shows the last stderr
# line, so let errors be errors (`set -e`) and say what went wrong on stderr.
#
# For real data, replace the file with a command, for example:
#   gh pr list --search 'review-requested:@me' --json number,title,author,url,createdAt
# and run on a timer instead of a watch: `--every 2m`.
set -euo pipefail
data=${1:-reviews.data.json}
[ -r "$data" ] || { echo "reviews feed: cannot read $data" >&2; exit 1; }

jq -c '
  # how the source names a CI run -> the four words a run strip takes (null: no run in that slot)
  def outcome: if . == null then null
    elif . == "success" then "ok" elif . == "failure" then "fail"
    elif . == "skipped" then "skip" else "running" end;
  {
    type: "table", id: "pr", empty: "nothing to review",
    columns: [
      {key: "pr",    title: "PR",     kind: "id"},
      {key: "by",    title: "Author", kind: "who"},
      {key: "title", title: "Title"},
      {key: "ci",    title: "CI",     kind: "runs", optional: true},
      {key: "age",   title: "Age",    kind: "age"}
    ],
    children: [
      sort_by(.created_at) | reverse | .[] | {
        type: "tr", id: "pr-\(.number)",
        target: .url,                       # enter or a click opens it
        detail: (.body // ""),              # the detail panel (d) shows it
        cells: {
          pr: "#\(.number)",
          by: .author,
          title: .title,
          ci: [.ci[]? | outcome],
          age: .created_at                  # a time, not "2h": the pane ticks the age itself
        }
      }
    ]
  }' "$data"
