#!/usr/bin/env bash
# The script that fetches the rows of the `services` example: a JSON file of background services
# -> the tabs `svc` ("Failing" and "All"). margin runs it as a handler, when the event
# `services.refresh` is appended.
#
#   margin example services | margin new services                    # the board it updates (a handler never creates one)
#   ./services.fetch.sh services.data.json                           # look at the line it prints
#   ./services.fetch.sh services.data.json | margin apply services   # apply it by hand (before registering)
#   margin handler add services-refresh --on services.refresh --board services --owns svc \
#       -- ./services.fetch.sh services.data.json
#   margin emit services.refresh                                     # run it now; cron or a timer runs this line on a schedule
#
# It prints ONE line: the whole `tabs` component, id `svc`. Its two children are sections; the
# pane shows their titles as a tab strip and one section at a time (]t / [t). Every id below
# starts with `svc-`, the prefix the handler owns. Ids come from the service's name, so a service
# keeps its row (and the focus on it) from run to run.
#
# "Failing" holds, per failing service, a `row` (glyph `fail`: that also puts the failure mark on
# this dashboard's tab in the pane) with the error as its `detail`, and a `runs` strip with a
# failure count. "All" is a table with a state glyph, a run strip that ends in its failure count
# (`summary: true` on the column) and the last run's age.
#
# For real data, replace the file with a command (systemctl list-units --output json, your job
# runner's API, ...) and map its words to margin's in the two `def`s below.
set -euo pipefail
data=${1:-services.data.json}
[ -r "$data" ] || { echo "services fetch: cannot read $data" >&2; exit 1; }

jq -c '
  # an id may hold letters, digits, - and _ only
  def slug: gsub("[^A-Za-z0-9_-]"; "-");
  # the source names for a run -> ok fail skip running (null: no run in that slot)
  def outcome: if . == null then null
    elif . == "success" then "ok" elif . == "failure" then "fail"
    elif . == "skipped" then "skip" else "running" end;
  # the source names for a service state -> a state glyph: spin done fail idle
  def glyph: if . == "failed" then "fail" elif . == "running" then "spin"
    elif . == "ok" then "done" else "idle" end;

  sort_by(.name) as $all
  | [$all[] | select(.state == "failed")] as $bad
  | {
      type: "tabs", id: "svc",
      children: [
        { type: "section", id: "svc-failing", title: "Failing (\($bad | length))",
          children: (
            if ($bad | length) == 0
            then [{type: "text", id: "svc-failing-none", style: "dim", text: "Nothing is failing."}]
            else [ $bad[] | (.name | slug) as $s
              | {type: "row", id: "svc-f-\($s)", glyph: "fail",
                 text: .name, who: .owner, since: .last_run,
                 detail: .error, target: .url},
                {type: "runs", id: "svc-fr-\($s)", label: "last \(.runs | length) runs",
                 values: [.runs[] | outcome], summary: true}
            ] end
          ) },
        { type: "section", id: "svc-all", title: "All (\($all | length))",
          children: [
            { type: "table", id: "svc-table", empty: "no services",
              columns: [
                {key: "st",    title: "",        kind: "state"},
                {key: "name",  title: "Service"},
                {key: "owner", title: "Owner",   kind: "who", optional: true},
                {key: "runs",  title: "Runs",    kind: "runs", summary: true},
                {key: "last",  title: "Last",    kind: "age"}
              ],
              children: [ $all[] | {
                type: "tr", id: "svc-s-\(.name | slug)",
                target: .url, detail: (.error // ""),
                cells: {st: (.state | glyph), name: .name, owner: .owner,
                        runs: [.runs[] | outcome], last: .last_run}
              } ] }
          ] }
      ]
    }' "$data"
