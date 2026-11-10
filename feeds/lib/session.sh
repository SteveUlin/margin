# Sourced by the transcript feeds (queue, turns): resolve which Claude Code transcript to read.
#
#   session_file [SESSION]   prints the transcript path, or nothing
#
# SESSION is a session id or a path to a .jsonl. Without one, the newest transcript of the
# project for the feed's working directory: Claude Code keeps them in
# $CLAUDE_PROJECTS/<cwd with every non-alphanumeric char turned into "-">/<session>.jsonl.
# "Newest" follows you to a new session after a restart without re-registering the feed.
ROOT=${CLAUDE_PROJECTS:-$HOME/.claude/projects}
session_file() {
  local s=${1:-}
  if [[ -n "$s" && -f "$s" ]]; then printf '%s\n' "$s"; return; fi
  if [[ -n "$s" ]]; then
    find "$ROOT" -mindepth 2 -maxdepth 2 -name "$s.jsonl" 2>/dev/null | head -1
    return
  fi
  local slug
  slug=$(pwd -P | sed 's/[^A-Za-z0-9]/-/g')
  ls -t "$ROOT/$slug"/*.jsonl 2>/dev/null | head -1
}

# stat signature: size and mtime, so a loop re-reads the file only when it changed
sig() { stat -c '%s.%Y' "$1" 2>/dev/null; }
