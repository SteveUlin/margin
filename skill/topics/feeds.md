# feeds: scripts that keep a panel current without you

A feed is a command margin's daemon runs for you; what it prints on stdout is `margin apply`
input. It keeps part of an artifact current with no AI in the loop, so it costs no tokens and
keeps going after the session ends. Register one only when {{user}} asked for something kept live.

## The contract of a feed script

- **It prints lines on stdout; each line is one JSON value**, complete on that line (`jq -c`):
  - a whole component `{"type": …, "id": …, …}`: it **replaces** the component with that id where
    it stands, children included. A new id is appended at the top level, or under `"parent": "<id>"`.
  - an op: `{"op": "set", "id": "pr-1", "fields": {"detail": "…"}}` (a `null` value deletes the
    field), `{"op": "rm", "id": "pr-1"}`, `{"op": "move", "id": …, "before"|"after"|"parent": …}`,
    `{"op": "add", "block": {…}, "parent": …}`;
  - an array of ops, applied all or none;
  - a toast: `{"toast": "deploy failed", "level": "error", "at": "<id>"}` (levels `ok info warn error`).
  - Blank lines and lines starting with `#` are skipped.
- **Print the whole thing every run.** The usual feed prints one line: the root component (a
  table with all its rows, a section with all its children). margin reconciles by id: a child
  with a known id updates in place, a new id appears, a missing id is removed. The script needs
  no memory and never deletes. When nothing differs from what is there, nothing is written.
- **Every id it writes is inside its prefix.** `--owns PREFIX` (default: the feed's name) owns the
  ids `PREFIX` and `PREFIX-…` in `--artifact` (`ci` owns `ci-1`, never `city`). A line that writes
  an id outside it is rejected. Make ids from the data (`pr-4812`), restricted to letters,
  digits, `-` and `_`.
- **Exit 0.** A non-zero exit (or a timeout, `--timeout`, default 60 s) drops the whole run: the
  artifact keeps its last content, the feed's health line shows `crashed · restart in 4s` with
  the last line of stderr, and the daemon retries with backoff (1 s … 60 s). So fail loudly
  (`set -euo pipefail`) and say why on stderr; never print a half-built board.
- **A bad line is skipped alone**: a line that isn't JSON or fails the schema is rejected and
  logged, and the other lines of the run still land. `margin feed logs <name>` shows `rejected: …`
  and `stderr: …` lines.
- **It runs with no shell and no terminal**: the command after `--` is executed directly, in the
  directory `feed add` was run from, with stdin closed. It gets `MARGIN_FEED`, `MARGIN_ARTIFACT`,
  `MARGIN_OWNS`, `MARGIN_PROJECT` and `MARGIN_HOME`. Pipes and redirections belong inside the script.
- **Write times, not ages**: a row's `since` or an `age` cell takes an RFC 3339 time and the pane
  ticks it. An age string would change every run and rewrite the artifact for nothing.

## When it runs

| flag | the daemon | use it for |
|---|---|---|
| `--every 30s` (`10s`, `2m`, `1h`) | runs the script to completion on that interval; one run's lines land as one revision | polling an API or a command |
| `--watch <path>…` | runs it once at start, then whenever a watched file or directory changes | a file another job writes |
| `--stream` | starts it once and keeps it running; lines land as they arrive (at most one write per `--min-gap`, default 1 s); restarts it with backoff when it exits | a tail, a subscription, a loop of its own |

- The daemon is per project; `feed add` and `feed resume` start it. Pausing or removing the last
  running feed stops it (`margin feed ls` then says `daemon: not running`).
- A watched file that is replaced by rename (an editor's save, `jq … > tmp && mv tmp data.json`)
  is still watched: margin watches the file's directory too.
- `margin feed ls` shows each feed's last run apart from its last write: `ran 2s ago · unchanged`
  means it ran and printed what is already there, so nothing was written (right after
  registering over a board you applied by hand, that is the normal state); `ran 2s ago · wrote`;
  `failed 5s ago: <last stderr line>`.

## A complete feed: pull requests into a table

`prs.json` (what the script reads; in real use, the output of a command such as `gh pr list --json …`):

```json
[{"number": 4812, "title": "Cap retry backoff", "author": "priya", "created_at": "2026-11-10T16:05:00Z",
  "url": "https://example.com/pulls/4812", "ci": ["success", "failure", "success"]},
 {"number": 4807, "title": "Move config to TOML", "author": "marcus", "created_at": "2026-11-10T11:40:00Z",
  "url": "https://example.com/pulls/4807", "ci": ["success", "success"]}]
```

`prs-feed.sh`:

```bash
#!/usr/bin/env bash
# prints one line: the table `pr` with a `tr` row per pull request, id `pr-<number>`
set -euo pipefail
data=${1:-prs.json}
[ -r "$data" ] || { echo "prs feed: cannot read $data" >&2; exit 1; }
jq -c '
  def outcome: if . == "success" then "ok" elif . == "failure" then "fail" else "running" end;
  { type: "table", id: "pr", empty: "nothing to review",
    columns: [ {key: "pr", title: "PR", kind: "id"},
               {key: "by", title: "Author", kind: "who"},
               {key: "title", title: "Title"},
               {key: "ci", title: "CI", kind: "runs", optional: true},
               {key: "age", title: "Age", kind: "age"} ],
    children: [ sort_by(.created_at) | reverse | .[] | {
      type: "tr", id: "pr-\(.number)", target: .url,
      cells: { pr: "#\(.number)", by: .author, title: .title,
               ci: [.ci[] | outcome], age: .created_at } } ] }' "$data"
```

Test it by hand before registering it, then register it and watch one change go through:

```sh
chmod +x prs-feed.sh
./prs-feed.sh prs.json                        # 1. one line of JSON on stdout, exit 0
./prs-feed.sh prs.json | margin apply reviews # 2. `reviews rev 1`: it passes the schema (creates the artifact)
margin render reviews --width 90 --height 12  # 3. the table as text
margin feed add reviews --watch prs.json --artifact reviews --owns pr -- ./prs-feed.sh prs.json
margin feed ls                                # 4. `reviews  live  watch prs.json  reviews/pr … ran 2s ago · unchanged`
margin feed logs reviews                      # 5. a line per run, its stderr, rejected lines, restarts
```

- Step 2 works only before the feed is registered; afterwards the ids are the feed's and the
  write fails with "belongs to feed". Test a changed script with `margin feed run reviews`
  instead: one run in the foreground, as the feed (`reviews rev N` or `reviews unchanged`).
- An artifact a feed line created has its id for a title and no purpose: set them once with
  `margin title reviews "Reviews"` and `margin purpose reviews "…"`, or (better) write the layout
  first as a whole document and let the feed fill its part (`margin help dashboards`, step b).
- `margin example` has three larger ones (a table, tabs with rows and run strips, sections of
  todos), each with a script and sample data; `margin help dashboards` covers the components.

## Managing feeds

```sh
margin feed add <name> [--every 10s | --watch <path>... | --stream] [--artifact A] [--owns PREFIX]
                [--min-gap 1s] [--timeout 60s] [--replace] -- <cmd...>
margin feed ls                      # state, trigger, restarts, last run and how it went
margin feed add --help              # each subcommand has its own (the trigger flags, a line each)
margin feed logs <name> [--follow] [--lines N]
margin feed run <name>              # once, in the foreground (a stream: until it exits or ctrl-c)
margin feed pause <name> | resume <name> | rm <name>
```

- `--replace` changes a registered feed's command or trigger in place. Two feeds can't own
  overlapping prefixes in one artifact.
- **Feed-owned components are read-only for you.** `margin show <art>` lists each feed and its
  prefix. A write there fails with "belongs to feed `x`"; write elsewhere in the artifact, or ask
  {{user}} before `margin feed pause|rm x`. A whole-document `margin apply` must carry the feed's
  components through unchanged, so edit your own parts by id (`margin set|add|rm`). **Why:** the
  feed rewrites those ids on its own, so your edit would vanish at its next run, or fight it.
- You can share an artifact with a feed: your ids outside the prefix, its ids inside.
- **The health line** is a `feed` component with the id `<prefix>-feed` (state, trigger, last
  write, the error). The daemon inserts it above the component whose id is the prefix, else at the
  end of the document. To choose its place, put `{"type": "feed", "id": "<prefix>-feed"}` where you
  want it before registering: the daemon then updates it there.
- `margin feed rm` leaves the feed's last output in the artifact, unowned, and sets the health
  line to `removed` itself (daemon or not); delete what you don't want with `margin rm <art> <id>`.
- **Don't poll something a feed could watch.** **Why:** every refresh you do costs a turn; a feed
  costs nothing and keeps going after you stop.

## Feeds that ship with margin

```sh
# the repo's recent jj changes, rerun whenever jj records an operation
margin feed add jj --watch .jj/repo/op_heads/heads --artifact changes -- {{feeds}}/jj
# Claude Code sessions on this machine: who's working, who needs {{user}}, queued messages
margin feed add agents --stream --artifact agents --owns ag -- {{feeds}}/agents
# this session's queued messages (waiting → absorbed → answered) and its turn ledger
margin feed add queue --stream --artifact live --owns q -- {{feeds}}/queue
margin feed add turns --stream --artifact live --owns tn -- {{feeds}}/turns
```

- `queue` and `turns` read the newest transcript of the directory they were added from (pass a
  session id or a .jsonl path after the script to pin one). `queue` shows only the first 60
  characters of each message, never the rest: it is {{user}}'s text, shown back only to them.
  **Why:** the board says whether a message typed mid-turn reached you; the full text is already
  in the chat.
