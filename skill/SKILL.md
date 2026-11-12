---
name: margin
description: Keep the state of the work in a margin pane beside the chat (Zellij, or any terminal): a plan ticked as you go, questions {{user}} answers with a click, assumptions they can strike, long-form to review, and standing boards that scripts keep current without you. Invoke when {{user}} asks for margin or for something tracked or kept live beside the chat ("start a margin", "track this", "keep a plan up", "watch the agents"), and whenever a hook line starting `margin:` says a pane is linked to this session or reports what they did in it. On a long task you may offer it once in one line; never start a pane or register a handler unasked.
---

# margin: the state beside the chat

margin is a pane next to this chat. You write typed components with the `margin` CLI; {{user}} reads
it at a glance and acts on it with clicks or keys; what they do comes back to you as `margin:` lines
at the start of your next turn, or mid-turn after a tool call. **Why:** chat is a log and the work
needs a state: a plan in a pane stays current, and a question on a card never takes over the chat.

**Binary:** `margin` when it is on PATH, else `{{bin}}`.

## The pane shows what is true now

One shape for every plan pane, top to bottom. Orientation is one `purpose` line (`margin purpose
plan "…"`) pinned above the content.

1. **Now** (`section now`): the current thread, at most about seven todos, exactly one `doing`.
2. **Needs you** (`section ask`): only open questions and claims. Remove it when it is empty.
3. **Next** (`section next`): what comes after, one todo per item.
4. **Rules in force** (`section rules`), only when some exist: a few bullets, one rule each.

- **The pane holds the present and nothing else.** No Done section and no log of finished
  threads; no archive of resolved questions; never a "superseded" or "earlier" version of
  anything. History is in version control and the event log (`margin events`). **Why:** "our views
  should be constantly updated and refined, i have a human context window too" (2026-11-10).
- **Before adding anything, ask what it replaces, and remove that.**
  - A thread in Now finishes: `margin rm` its steps and promote the next from Next.
  - A question is answered: fold the answer into the content it affects (the step, the rule,
    the text), then `margin rm` the card.
  - A rule changes: rewrite its bullet in place (`margin set`); never add a second one beside it.
  - Long-form writing is replaced: write the new text under the same id.
- **Authored text wraps, so never pre-truncate it.** A todo, a bullet, a `text`, a row's `detail`
  wrap to the pane's width, and whatever the pane cuts opens in full on focus. Write the whole
  sentence, and keep it short; never end your own text with `…`.
- **Color only where it tells things apart.** Put `tone` on a block that differs in kind from
  its neighbors (the step that failed, the row that is theirs), never for decoration.
- **A counted list carries its numbers in the text**: bullets whose text starts `1.`, `2.`; the
  pane draws the number in place of the bullet. Otherwise write short `bullet`s with a `**bold**`
  lead-in, one idea each, sub-bullets for detail.
- **Long-form goes beside the chat**, in its own board as a tab of the pane: `margin new <notes>`
  once, `margin doc <notes> <id> < notes.md` (markdown, in bullets; `margin doc --help` lists
  what it reads), `margin tab <board> add <notes>`; chat gets the conclusion in two or three
  lines plus "it's in the pane". Revise it under the same id; `margin tab <board> rm <notes>`
  when the topic closes. **Why:** in chat a long piece scrolls away while they answer it.
- **Name a row by an id {{user}} can see, plus a few words of its text**: "t3 (seed the RNG)",
  never a bare hash. Todos, questions and claims show short ids (8 characters or fewer, no
  dash), so give those ids like `t3`, `Q1`, `A1`; quote any other row's text.

## The turn loop

```fish
margin ls                                  # this project's boards (they outlive sessions)
margin new plan <<'EOF'                    # create: the only command that makes a board
{"title": "fix the flake", "purpose": "CI green: the flaky test passes 200 runs", "blocks": [
  {"type": "section", "id": "now", "title": "Now", "children": [
    {"type": "todo", "id": "t1", "text": "Reproduce the flaky test"},
    {"type": "todo", "id": "t2", "text": "Seed the RNG per test"}]},
  {"type": "section", "id": "next", "title": "Next", "children": [
    {"type": "todo", "id": "t4", "text": "Run the suite 200 times in CI"}]}]}
EOF
margin open plan                           # a split in your tab, never moves focus
cargo test && margin set plan t1 status=done note="fails 1 run in 40" && margin set plan t2 status=doing
margin add plan now todo t3 "Regression test for the seed"    # add creates, set changes; also rm, move
margin add plan - section ask title="Needs you" --after now   # only now that there is a question
margin ask plan Q1 "Seed from what?" test-name run-id --suggest test-name --why "the same on a rerun" --parent ask
margin add plan ask claim A1 "CI runs the tests in one process"
# they picked test-name and left A1 standing: fold both in, remove the cards and the empty section
margin set plan t2 text="Seed the RNG from the test name" && margin rm plan ask
margin add plan - section rules title="Rules in force"
margin add plan rules bullet r1 "1. **Seeds** come from the test name, never the clock"
margin set plan r1 text="1. **Seeds** come from the test name and the shard"    # a rule changed: in place
margin set plan t2 status=done && margin rm plan t1 && margin rm plan t2        # finished steps leave
margin set plan t3 status=doing && margin move plan t4 --parent now && margin rm plan next
margin show plan                           # the pane as text, with {{user}}'s decisions merged in
```

1. **One pane per chat.** If a hook line names your pane, use it. Else `margin new` the shape and
   `margin open <board>` once (outside Zellij it prints a `margin view …` line for {{user}} to
   run). After that, change the pane or add a tab to it; never open a second one for a new topic.
2. **Each turn begins with the hook's lines.** `margin: your pane is <board> · doing … · N open`
   names the pane linked to your session (`margin claude link <board>` switches it); the other `margin:`
   lines are what {{user}} did in the pane: treat them as their own input.
3. **Exactly one todo is `doing`; tick it as the step lands, and clear what the turn finished.**
   The live strip already shows the tool call you're running: don't narrate calls into the doc.
4. **Never spend a tool call on margin alone.** Chain the write onto the step's real command or
   send it with the next real call; after 10 tool calls with no pane write the hook nudges you to.
   **Why:** a standalone call re-reads the whole context (est. 3× a task's tokens vs 1.1× chained).
5. **Creating and changing are different commands.** `new` creates a board and refuses one that
   exists; every other write refuses one that doesn't, so a typo never makes a second board.
   Inside one: `add`/`ask` create, `set`/`move`/`rm` change. `apply` is state from a script
   (components upsert by id, ops, or a whole document). `margin schema [type]` lists the fields.

## Rules

- **You own content; {{user}} owns decisions.** You write text, steps and options; you can't set
  their picks, strikes or ticks. To change one, `margin claude propose` it (see topics) or say so in
  chat. **Why:** a decision that silently changes is worse than none.
- **Ask only when blocked, and ask in the pane**, with `margin ask`, not the `AskUserQuestion`
  tool. A question has no default: `--suggest` marks the answer you recommend and nothing is
  highlighted until {{user}} picks. Blocked means the work that needs the answer waits; everything
  else carries on. The pick arrives as a `margin:` line (and wakes you if idle): fold it into the
  content it affects and remove the card. Everything else you decide yourself; a decision that
  keeps governing the work becomes a bullet in Rules in force. **Why:** a dialog takes over the
  chat; a card waits until they're ready (2026-11-08).
- **"Talk about this" means talk.** It reaches you as "{{user}} wants to talk about Q1 …": raise
  the question in chat (the tradeoff, what would change your suggestion) and leave the card open.
- **State an assumption as a `claim` when being wrong would change your plan.** Clicking a claim
  strikes it. On a strike: re-check, re-plan any step listed as resting on it, then
  `margin set <board> <id> ack "…"`. A claim that has held and no longer matters is removed.
- **Stop means stop.** `S` in the pane (or `margin claude stop`) refuses your next tool call. Run nothing:
  say in two or three lines where you are, and wait. It lifts with their next message.
- **Done is yours; next, skip and stop are theirs.** In the pane {{user}} can mark an open step next
  or skipped and stop a running one, never done. The exception is a step that is theirs
  (`"owner": "you"`, or claimed in the pane): don't do it or route around it; say "your step:
  t4", work on what doesn't depend on it, and continue when their finish arrives.
- **Row notes are a review**: `{{user}} reviewed <board>: less <id> …; disagree <id> on "quote"`
  (`more`, `less`, `keep`, `disagree`, `why`). Revise those rows in place; a note clears then.
- **Woken while idle.** When {{user}} finishes in the pane after your turn ended, their `margin:`
  lines arrive as a system reminder (the `Stop` hook): act on them as on a prompt's.
- **After a compaction or resume, read back** when the session-start hook asks: 2–3 claims in a
  `readback` section at the top (the goal, the live thread, your next step), removed once they
  have stood for a turn. **Why:** a picture rebuilt from a summary can be quietly wrong.
- **Brief subagents for the pane**: "Before each phase of the work, write one short sentence
  saying what you're about to do (not which tool)." Give an implementation or review subagent a
  section (`ag-<name>`, todo ids under `ag-<name>-`); remove it once its result is folded in.
- **Owned ids are read-only**: a write into rows a handler or an outside owner keeps fails with
  "belongs to …"; ask before removing the owner. **Don't poll what an event could refresh.**
- **A standing dashboard is not a plan pane.** For a board that stays up and shows outside state
  (reviews, services), read `margin help dashboards` and start from `margin example`. Its rows
  come from a handler run for a refresh event that cron emits; a key on a row is an event too,
  done by a handler, never by the pane (`margin help handlers`); numbers that change often go in
  a dataset (`margin data`). **Why:** a board you refresh costs a turn each time.

## On demand: `margin help <topic>`

Read a topic before first using its feature; `margin <cmd> --help` gives flags.

- `dashboards`: standing boards: tables, run strips, tabs, badges, plots fed by `margin data`; step by step.
- `handlers`: commands margin runs for an event (a key, a refresh): the contract, the line grammar.
- `integrate`: other programs: cron and systemd recipes, following events, owning rows, JSON queries.
- `contract`: what an outside tool may rely on: the JSON shapes, the environment, the exit codes.
- `lifecycle`: what survives a session end, a closed pane, a reboot; `margin doctor`, `daemon status`.
- `toasts`: `margin toast`, news that matters for a moment (a test result, a failure).
- `graphs`: `owner` ({{user}}'s steps), `rests_on` (steps tied to claims), lanes.
- `diagrams`: the house style for a diagram typed by hand: boxes, edges, arrowheads, 64 cells.
- `proposals`: `margin claude propose`, offering a change to one of their decisions.
- `subagents`: the Agents section the pane builds from your subagents, and the recap row.
