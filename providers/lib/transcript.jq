# Shared by the transcript providers. Input: one transcript entry per line (raw), read with -R.

# entries of the main thread, in file order
def entries: [inputs | fromjson? // empty | select(type == "object" and (.isSidechain | not))];

# a user entry that starts a turn: a typed prompt (or a slash command, or a delivered
# notification), not a tool result and not a meta line
def starts_turn:
  .type == "user" and (.isMeta | not) and (.isCompactSummary | not) and (.isVisibleInTranscriptOnly | not)
  and ((.message.content | type) == "string"
       or ((.message.content | type) == "array" and ([.message.content[] | select(.type == "tool_result")] | length) == 0));

def ends_turn: .type == "assistant" and .message.stop_reason == "end_turn";

# the first line of $s, at most $n chars, with an ellipsis when cut
def short($n): (split("\n") | map(select(test("\\S"))) | first // "") as $l
  | if ($l | length) > $n then ($l[0:$n - 1] | sub("\\s+$"; "")) + "…" else $l end;
