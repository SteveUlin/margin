#!/usr/bin/env bash
# The script that fetches the rows of the `reviews` example: a JSON file of pull requests -> the
# table `pr`. margin runs it as a handler, when the event `reviews.refresh` is appended.
#
#   margin example reviews | margin new reviews                   # the board it updates (a handler never creates one)
#   ./reviews.fetch.sh reviews.data.json                          # look at the line it prints
#   ./reviews.fetch.sh reviews.data.json | margin apply reviews   # apply it by hand (before registering)
#   margin handler add reviews-refresh --on reviews.refresh --board reviews --owns pr \
#       -- ./reviews.fetch.sh reviews.data.json
#   margin emit reviews.refresh                                   # run it now; cron or a timer runs this line on a schedule
#
# The contract (margin help handlers): print `margin apply` input on stdout, one JSON value per line,
# and exit 0. This script prints ONE line: the whole table, id `pr`, with a `tr` child per pull
# request, id `pr-<number>`. margin replaces the table with that id and reconciles the rows by
# id: a known id updates in place, a new id appears, a missing id leaves. So the script never
# deletes anything and keeps no memory between runs.
#
# Every id it writes sits under the prefix the handler owns (`--owns pr`: `pr` and `pr-...`).
# A non-zero exit drops the whole run: the board keeps its last rows and shows the last stderr
# line, so let errors be errors (`set -e`) and say what went wrong on stderr.
#
# For real data, replace the file with a command, for example:
#   gh pr list --search 'review-requested:@me' --json number,title,author,url,createdAt
# and emit the event from cron: `*/2 * * * *  margin emit reviews.refresh --project <project>`.
set -euo pipefail
data=${1:-reviews.data.json}
[ -r "$data" ] || { echo "reviews fetch: cannot read $data" >&2; exit 1; }

jq -c '
  # how the source names a CI run -> the four words a run strip takes (null: no run in that slot)
  def outcome: if . == null then null
    elif . == "success" then "ok" elif . == "failure" then "fail"
    elif . == "skipped" then "skip" else "running" end;
  {
    type: "table", id: "pr", empty: "nothing to review",
    columns: [
      {key: "g",                      kind: "graph"},  # lanes: each row joined to the rows its `after` names
      {key: "pr",    title: "PR",     kind: "id"},
      {key: "by",    title: "Author", kind: "who"},
      {key: "title", title: "Title"},
      {key: "ci",    title: "CI",     kind: "runs", optional: true},
      {key: "age",   title: "Age",    kind: "age"}
    ],
    children: [
      sort_by(.created_at) | reverse | .[] | {
        type: "tr", id: "pr-\(.number)",
        target: .url,                       # enter or a double click opens it
        detail: (.body // ""),              # the detail panel (d) shows it
        after: [.base // empty | "pr-\(.)"],  # stacked on another pull request: the id of that row (one not listed is ignored)
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
