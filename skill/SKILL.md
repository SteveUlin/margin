---
name: margin
description: Keep the state of the work in a margin pane beside the chat (Zellij, or any terminal): a plan ticked as you go, questions {{user}} answers with a click, assumptions they can strike, long-form to review, a diff to approve; and feeds, scripts that keep a panel current without you. Invoke when {{user}} asks for margin or for something tracked or kept live beside the chat ("start a margin", "track this", "keep a plan up", "watch the agents"), and whenever a hook line starting `margin:` says a pane is linked to this session or reports what they did in it. On a long task you may offer it once in one line; never start a pane or register a feed unasked.
---

# margin: the state beside the chat

margin is a pane next to this chat. You write typed components with the `margin` CLI; {{user}} reads
it at a glance and acts on it with clicks or keys; what they do comes back to you as `margin:` lines
at the start of your next turn, or mid-turn after a tool call. **Why:** chat is a log and the work
needs a state: a plan in a pane stays current, and a question on a card never takes over the chat.

**Binary:** `margin` when it is on PATH, else `{{bin}}`.

## The pane's shape

One shape for every plan pane, top to bottom. Orientation is one `purpose` line (`margin purpose
plan "…"`) pinned above the content; long reasoning goes in its own artifact (a tab).

1. **Now** (`section now`): the current thread, at most about seven todos, exactly one `doing`.
2. **Needs you** (`section ask`): only open questions, claims and diffs. Leave it out when empty.
3. **Next** (`section next`): the backlog, one todo per item.
4. **Done** (`section done`): one todo per finished *thread*, not every step. It folds itself.
5. **Decided** (`section decided`, `folded: true`): resolved questions and `decided` rows.

- **The pane is a state, not a log.** When a thread in Now finishes, replace its steps with one row
  in Done and promote the next from Next. Write for a glance: short `bullet`s with a `**bold**`
  lead-in, sub-bullets for detail, `folded: true` on sections opened only on demand. **Why:** a pane
  of every past step read as "cluttered and not super useful" (2026-11-08).
- **Long-form goes beside the chat.** An overview, proposal, comparison or any reply longer than
  a screen goes in its own artifact, a tab of the pane: `margin new <notes>` once, `margin doc
  <notes> <id> < notes.md` (markdown, in bullets, one idea each), `margin tab <art> add <notes>`; chat
  gets its conclusion in two or three lines plus "it's in the pane". Revise it under the same id
  (change bars show what moved); `margin tab <art> rm <notes>` when the topic closes. Chat keeps
  direct answers, reasoning, what you did and how you know, disagreement, and anything that needs
  a reply now. Reference material (reviews, research, logs) gets its own artifact too, and a `link`
  row (`target: mg:open/<art>`). **Why:** in chat a long piece scrolls away while they answer it.
- **Keep the next item explained.** The top item of Next gets a folded explainer in its own
  top-level section right after Next (`margin doc <art> <id>x --title "About <id>: …"`): what,
  why, what it would look like, what's open. A `+ more` note on another Next item asks for one.
- **Name a row by an id {{user}} can see, plus a few words of its text**: "t3 (seed the RNG)",
  never a bare hash. Todos, questions, claims and decided rows show short ids (8 characters or
  fewer, no dash), so give those ids like `t3`, `Q1`, `A1`; quote any other row's text.

## The turn loop

```fish
margin ls                                  # this project's artifacts (they outlive sessions)
margin new plan <<'EOF'                    # create: the only command that makes an artifact
{"title": "fix the flake", "purpose": "CI green: the flaky test passes 200 runs", "blocks": [
  {"type": "section", "id": "now", "title": "Now", "children": [
    {"type": "todo", "id": "t1", "text": "Reproduce the flaky test"},
    {"type": "todo", "id": "t2", "text": "Seed RNG per test name"}]},
  {"type": "section", "id": "next", "title": "Next"},
  {"type": "section", "id": "decided", "title": "Decided", "folded": true}]}
EOF
margin open plan                           # a split in your tab, never moves focus
cargo test && margin set plan t1 status=done note="200 runs" && margin set plan t2 status=doing
margin add plan now todo t3 "Regression test"            # add creates, set changes; also rm, move, title
margin add plan - section ask title="Needs you" --after now     # only now that there is a question
margin ask plan Q1 "Which store?" sqlite files --suggest files --why "files need no daemon" --parent ask
margin add plan ask claim A1 "CI runs tests in one process"
margin move plan Q1 --parent decided       # once answered; `margin rm plan ask` when nothing else waits there
margin add plan decided decided D3 "Keep artifacts 30 days" by=claude
margin show plan                           # the pane as text, with {{user}}'s decisions merged in
```

1. **One pane per chat.** If a hook line names your pane, use it. Else `margin new` the shape and
   `margin open <art>` once (outside Zellij it prints a `margin view …` line for {{user}} to
   run). After that, change the pane or add a tab to it; never open a second one for a new topic.
2. **Each turn begins with the hook's lines.** `margin: your pane is <art> · doing … · N open` is
   the pane linked to your session (`margin link <art>` switches it); the other `margin:` lines are
   what {{user}} did in the pane. Treat them as their own input. `margin events --unread` lists
   any you haven't been handed.
3. **Exactly one todo is `doing`; tick it as the step lands.** The pane's live strip already shows
   the tool call you're running, so don't narrate calls into the doc. A lagging plan reads as stalled.
4. **Never spend a tool call on margin alone.** Chain the write onto the step's real command or
   send it with the next real call; after 10 tool calls with no pane write the hook nudges you to.
   **Why:** a standalone call re-reads the whole context (est. 3× a task's tokens vs 1.1× chained).
5. **Creating and changing are different commands.** `new` creates an artifact and refuses one that
   exists; every other write refuses one that doesn't ("create it with margin new", plus the nearest
   name), so a typo never makes a second board. Inside one: `add`/`ask` create (an id there is
   refused), `set`/`move`/`rm` change (a missing id is refused). `apply` is state from a script: a
   component with an `id` and optional `parent` (upsert), ops (`{"op": "set", "id": …, "fields":
   {…}}`), or a whole document (replaces), one per line, all or none. Every write prints `rev N`
   (`--base-rev N` fails on a stale base); `margin schema [type]` lists types and fields.

## Rules

- **You own content; {{user}} owns decisions.** You write text, steps and options; you can't set
  their picks, strikes or verdicts. To change one, `margin propose` it (see topics) or say so in
  chat. **Why:** a decision that silently changes is worse than none.
- **Ask only when blocked, and ask in the pane**, with `margin ask`, not the `AskUserQuestion`
  tool. A question has no default: `--suggest` puts a robot on the answer you recommend and
  nothing is highlighted until {{user}} picks. Blocked means the work that needs the answer waits;
  the chat and everything else carry on. The pick arrives as a `margin:` line (and wakes you if
  idle) and the card resolves into a decided row: move that row to Decided, and remove the
  Needs-you section if it is now empty. Everything else you decide yourself and record as a
  `decided` row; no "here's what I'm doing, step in if you like" cards. **Why:** a dialog takes
  over the chat; a card waits until they're ready (2026-11-08).
- **"Talk about this" means talk.** Every card has it; it reaches you as "{{user}} wants to talk
  about Q1 …". Raise the question in chat (the tradeoff, what would change your suggestion) and
  leave the card open.
- **State an assumption as a `claim` when being wrong would change your plan.** Clicking a claim
  strikes it. On a strike: re-check, re-plan any step listed as resting on it, then
  `margin set <art> <id> ack "…"`.
- **Stop means stop.** `S` in the pane (or `margin stop`) refuses your next tool call. Run
  nothing: say in two or three lines where you are and what you were about to do, and wait. It
  lifts with their next message.
- **Done is yours; next, skip and stop are theirs.** In the pane {{user}} can mark an open step next
  or skipped and stop a running one, never done, so a step is done only when you set it. The
  exception is a step that is theirs (`"owner": "you"`, or claimed in the pane): don't do it or
  route around it; say "your step: t4", work on what doesn't depend on it, and continue when
  their finish arrives.
- **Row notes are a review.** They arrive as `{{user}} reviewed <art>: less <id> …; disagree <id>
  on "quote"` (`more`, `less` = trim it, `keep` = don't touch, `disagree` = rethink, `why` = justify). Answer
  by revising those rows in place; a note clears when its row changes.
- **Woken while idle.** When {{user}} finishes in the pane after your turn ended, their `margin:`
  lines arrive as a system reminder (the `Stop` hook): act on them as on a prompt's. A line
  `margin: {{user}} finished in the pane (…)` framed as another session's message is only a wake-up.
- **After a compaction or resume, read back** when the session-start hook asks: 2–3 claims in a
  `readback` section at the top (rb1–rb3: the goal, the live thread, your next step), replacing
  any earlier one. **Why:** a picture rebuilt from a summary can be quietly wrong, and a strike
  is the cheapest correction.
- **Brief subagents for the pane.** Put this in every subagent's brief: "Before each phase of the
  work, write one short sentence saying what you're about to do (not which tool)." Give an
  implementation or review subagent its own section: `margin add <art> - section ag-<name>
  title="…"`, steps as todos with ids under `ag-<name>-`, ticked as it goes.
- **Owned ids are read-only**: a write into rows a handler, a feed or an outside owner keeps fails
  with "belongs to …"; ask before removing the owner. **Don't poll what an event could refresh.**
- **A standing dashboard is not a plan pane.** For a board that stays up and shows outside state
  (reviews, services, a tracker), read `margin help dashboards` and start from `margin example`.
  Its rows come from a handler run for a refresh event that cron emits; a key on a row (`actions`,
  or enter on a `target`) is an event too, done by a handler, never by the pane (`margin help
  handlers`). **Why:** a board you refresh costs a turn each time and stops when you do.

## On demand: `margin help <topic>`

Read a topic before first using its feature; `margin <cmd> --help` gives flags.

- `dashboards`: standing boards: tables, run strips, tabs, badges, the theme; design to handler, step by step.
- `handlers`: commands margin runs for an event (a key on a row, a refresh): the contract, the daemon.
- `integrate`: other programs: cron and systemd recipes, following events, owning rows, JSON queries.
- `feeds`: the older script runner (jj ledger, agent monitor, queue, turns), and the line grammar.
- `toasts`: `margin toast`, news that matters for a moment (a test result, a failure).
- `diffs`: `margin diff`, a structural diff {{user}} approves or rejects.
- `images`: `margin image`, a PNG in the pane.
- `graphs`: `after` (a dependency graph), `owner` ({{user}}'s steps), `rests_on` (steps tied to claims).
- `proposals`: `margin propose`, offering a change to one of their decisions.
- `subagents`: the Agents section the pane builds from your subagents, and the recap row.
