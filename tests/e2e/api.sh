#!/usr/bin/env bash
# e2e for the write API (2026-11-06 API review): apply, schema, stable ids, --owns, wait.
# Runs the CLI against a throwaway MARGIN_HOME; needs no Zellij.
#   tests/e2e/api.sh [path/to/margin]
set -u
M=${1:-$(dirname "$0")/../../target/debug/margin}
MARGIN_HOME=$(mktemp -d)
export MARGIN_HOME MARGIN_PROJECT=e2e
trap 'rm -rf "$MARGIN_HOME"' EXIT
fails=0
ok() { echo "ok   $1"; }
bad() { echo "FAIL $1"; fails=$((fails + 1)); }
# expect <name> <expected substring> <cmd...>: the command's stdout+stderr contains it
expect() {
  local name=$1 want=$2
  shift 2
  local out
  out=$("$@" 2>&1)
  if [[ $out == *"$want"* ]]; then ok "$name"; else bad "$name: wanted [$want], got [$out]"; fi
}

# apply: a whole doc, a component upsert with parent, a multi-field set, all-or-nothing
expect "apply envelope" '"rev":1' "$M" apply p --json <<<'{"v":1,"title":"plan","blocks":[{"type":"section","id":"steps","children":[{"type":"todo","id":"t1","text":"a"}]}]}'
expect "apply upsert" "p rev 2" "$M" apply p <<<'{"type":"todo","id":"t2","text":"b","parent":"steps"}'
expect "apply upsert again" "p rev 3" "$M" apply p <<<'{"type":"todo","id":"t2","text":"b2","parent":"steps"}'
expect "patch takes a single op" "p rev 4" "$M" patch p <<<'{"op":"set","id":"t2","fields":{"note":"n","text":"bb"}}'
expect "all or nothing" 'no component "nope"' "$M" apply p <<<$'{"type":"todo","id":"t3","text":"c","parent":"steps"}\n{"op":"rm","id":"nope"}'
expect "...nothing landed" "rev 4" "$M" show p
expect "first write creates" "fresh rev 1" "$M" apply fresh <<<'{"type":"row","id":"r1","text":"hi"}'

# schema: unknown fields and values rejected with a suggestion; text stays text; add keeps words
expect "unknown field" 'did you mean text?' "$M" set p t1 txt hi
expect "bad enum" 'did you mean blocked?' "$M" set p t1 status bloked
expect "bad field via apply" 'did you mean value?' "$M" apply p <<<'{"type":"progress","id":"pp","vlaue":1}'
"$M" set p t1 text 2026 >/dev/null
expect "text stays text" '"text": "2026"' "$M" show p --json
"$M" add p steps todo x2 hello world >/dev/null
expect "add keeps every word" "hello world" "$M" show p
expect "set k=v pairs" "p rev" "$M" set p t1 status=doing note="two words"
expect "schema prints a type" "one of todo doing done blocked skipped next" "$M" schema todo
expect "schema json" '"types"' "$M" schema --json
expect "schema suggests" 'did you mean todo?' "$M" schema tod

# stable ids: an insert before a ticked todo leaves the tick on it
"$M" put s <<<'[{"type":"todo","text":"beta"},{"type":"todo","text":"gamma"}]' >/dev/null
"$M" set s beta status done --by you >/dev/null
"$M" put s <<<'[{"type":"todo","text":"alpha"},{"type":"todo","text":"beta"},{"type":"todo","text":"gamma"}]' >/dev/null
expect "tick stays on beta" "todo beta [done] beta" "$M" show s
expect "alpha unticked" "todo alpha [todo] alpha" "$M" show s

# tick is an old spelling of todo; a bare token cycles like the circle (next first: done is Claude's); wait fails fast on a missing id
expect "mg:tick alias" "" "$M" select mg:tick/s.gamma
expect "...ticked" "todo gamma [next]" "$M" show s
expect "wait missing id" 'no component "zz"' timeout 5 "$M" wait s.zz --timeout 3s

echo "$fails failed"
exit $((fails > 0))
