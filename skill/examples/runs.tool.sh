#!/usr/bin/env bash
# runs.tool.sh: a run queue as a SEPARATE TOOL beside margin. margin runs nothing here and needs
# no daemon: this script follows the event log, decides everything itself, and writes its rows.
#
#   margin example runs | margin new runs          # the board: Running / Waiting / Finished
#   ./runs.tool.sh &                               # the tool (keep it running: a unit, tmux, &)
#   margin emit sim.requested '{"label":"sim A","cmd":["sleep","30"]}'     # submit, from anywhere
#   X on the running row cancels it; D on a waiting row drops it (margin act runs run-7 cancel)
#
# What it uses of margin: `owner add` (its rows are its own), `events --follow --cursor … --no-commit`
# with `--commit` after each event is handled (at-least-once), and `apply --as` to write rows.
# Every decision is below: the order (first in, first out), one run at a time, what a cancel
# does, what happens to a run found dead after a restart. Replace this file with your own tool.
set -euo pipefail
M=${MARGIN_BIN:-margin} BOARD=runs ME=runs-tool
DIR=${RUNS_DIR:-$PWD/runs-state}                 # the tool's own state: its queue, its logs
mkdir -p "$DIR" && touch "$DIR/queue.jsonl" "$DIR/done.jsonl"
exec 9>"$DIR/lock"; flock -n 9 || { echo "runs tool: another one holds $DIR/lock" >&2; exit 1; }
"$M" owner add "$ME" --board "$BOARD" --owns run >/dev/null 2>&1 || true   # once; refused if there

paint() {  # the board's three sections, whole, from the tool's files (margin reconciles by id).
  # Printed in the order a run moves through them, Waiting first: an id leaves one section
  # before it enters the next, and the sections stay where the board has them
  jq -cn --slurpfile q "$DIR/queue.jsonl" --slurpfile d "$DIR/done.jsonl" \
     --argjson cur "$(cat "$DIR/current" 2>/dev/null || echo null)" '
    def sec($id; $title; $rows): {type: "section", id: $id, title: "\($title) (\($rows | length))", children: $rows};
    sec("run-wait"; "Waiting"; [$q[] | {type: "row", id: "run-\(.seq)", glyph: "idle", text: .label,
        actions: [{key: "D", name: "drop", label: "drop from the queue"}]}]),
    sec("run-now"; "Running"; [$cur | select(. != null) | {type: "row", id: "run-\(.seq)", glyph: "spin", text: .label,
        since: .started, actions: [{key: "X", name: "cancel", label: "cancel this run"}]}]),
    sec("run-done"; "Finished"; [$d | reverse | .[:20][] | {type: "row", id: "run-\(.seq)", text: .label, detail: .tail,
        glyph: (if .outcome == "ok" then "done" else "fail" end), aside: "\(.outcome) · \(.secs)s"}])
  ' | "$M" apply "$BOARD" --as "$ME" >/dev/null
}

finish() {  # $1 item, $2 outcome, $3 seconds: record it, free the slot, ask for the next
  [ "$(jq -r .seq "$DIR/current" 2>/dev/null)" = "$(jq -r .seq <<<"$1")" ] || return 0   # already recorded
  jq -c --arg o "$2" --arg s "$3" --arg t "$(tail -n 5 "$DIR/run-$(jq -r .seq <<<"$1").log" 2>/dev/null)" \
     '{seq, label, outcome: $o, secs: $s, tail: $t}' <<<"$1" >>"$DIR/done.jsonl"
  rm -f "$DIR/current" "$DIR/cancelled"; paint
  "$M" emit runs.next >/dev/null             # the main loop starts the next one: one place decides
}

run_one() {  # $1 item: run its command to the end, output to a log; one at a time by construction
  local t0 rc=0 pid d; t0=$(date +%s%N)
  setsid bash -c "exec $(jq -r '.cmd | @sh' <<<"$1")" >"$DIR/run-$(jq -r .seq <<<"$1").log" 2>&1 9>&- &
  pid=$!
  jq -c --arg at "$(date -u +%FT%TZ)" --arg pid "$pid" '. + {started: $at, pid: ($pid | tonumber)}' <<<"$1" >"$DIR/current"
  paint; wait "$pid" || rc=$?
  local out="ok"; [ "$rc" = 0 ] || out="exit $rc"; [ ! -e "$DIR/cancelled" ] || out="cancelled"
  d=$(( ($(date +%s%N) - t0) / 100000000 )); finish "$1" "$out" "$((d / 10)).$((d % 10))"
}

start_next() {  # the queue's head runs when nothing is running
  [ ! -e "$DIR/current" ] && [ -s "$DIR/queue.jsonl" ] || return 0
  local item; item=$(head -n 1 "$DIR/queue.jsonl"); sed -i 1d "$DIR/queue.jsonl"
  run_one "$item" 9>&- &
}

# after a restart: a run whose process is gone was interrupted (say so, never rerun it unasked);
# one that is still going is waited for, then recorded
if [ -e "$DIR/current" ]; then
  was=$(cat "$DIR/current")
  if kill -0 "$(jq -r .pid <<<"$was")" 2>/dev/null; then
    ( while kill -0 "$(jq -r .pid <<<"$was")" 2>/dev/null; do sleep 0.5; done; finish "$was" "ended while the tool was down" "?" ) 9>&- &
  else
    finish "$was" "interrupted" "?"
  fi
fi
paint; start_next

while read -r ev; do
  seq=$(jq -r .seq <<<"$ev") id=$(jq -r '.id | ltrimstr("run-")' <<<"$ev")
  case $(jq -r .kind <<<"$ev") in
    sim.requested)   # a submission: into the tool's own queue, unless a replay already put it there
      grep -qs "\"seq\":$seq[,}]" "$DIR/queue.jsonl" "$DIR/done.jsonl" "$DIR/current" ||
        jq -c --argjson s "$seq" '{seq: $s, label: (.value.label // "run \($s)"), cmd: .value.cmd}' <<<"$ev" >>"$DIR/queue.jsonl"
      paint; start_next ;;
    runs.next) start_next ;;
    runs.cancel)     # the person pressed X on the running row: stop its whole process group
      if [ "$(jq -r .seq "$DIR/current" 2>/dev/null)" = "$id" ]; then
        touch "$DIR/cancelled"; kill -TERM -- "-$(jq -r .pid "$DIR/current")" 2>/dev/null || true
      fi ;;
    runs.drop)       # D on a waiting row: out of the queue, never run
      jq -c --argjson s "$id" 'select(.seq != $s)' "$DIR/queue.jsonl" >"$DIR/queue.tmp" && mv "$DIR/queue.tmp" "$DIR/queue.jsonl"; paint ;;
  esac
  "$M" events --cursor "$ME" --commit "$seq"   # handled: the cursor moves (at-least-once)
done < <("$M" events --follow --cursor "$ME" --no-commit --all --json 9>&-)
