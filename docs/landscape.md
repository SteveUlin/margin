# margin: landscape, prior art, and where to push

*research note, 2026-11-05. written for margin's design, not as a survey for its own sake. every claim carries a warrant: **[src n]** = a cited page (see references), **[recall]** = my prior knowledge of the product, not re-checked today, **[inferred]** = my reasoning, not a reported fact.*

## the answer first

1. **the whole industry converged on one primitive: a small, model-owned, typed state object that the model replaces wholesale through a tool call.** claude code's `TodoWrite`, gemini cli's `write_todos` (a single `todos` argument that replaces the list) [src 14], codex cli's `update_plan` [src 15], devin's planner tab [src 12], cursor's plan file with to-dos [src 9]. replacement works because the objects stay small; bigger documents (claude artifacts, chatgpt canvas) switch to string find-and-replace, which fails silently when the anchor doesn't match [src 3, 4].
2. **the "markdown file in a pane" trap is the default outcome, and the terminal ecosystem proves it.** every third-party claude code companion i found — `sidecar` (renders a `SIDECAR.md` that a UserPromptSubmit hook keeps current), `cc-todos`, `planc`, `agent-dashboard`, `tmux-overseer`, the zellij sidebars — reads a file and draws it [src 17–22]. none has a channel back into the model.
3. **the systems that escape the trap split state three ways: the AI owns content, the client owns view state, the user owns decisions — and decisions flow back as structured events, not prose.** A2UI binds components to a data model by path and sends user actions back to the agent in structured form [src 23]; AG-UI syncs a shared state object with snapshots plus RFC 6902 deltas and sends the client's edits back as the next run's `state` [src 24]; MCP Apps lets a sandboxed UI read tool results and call tools, with host approval [src 25]; zed's agent review gives every hunk keep/reject and auto-dismisses hunks you commit [src 26]. the markdown pane fails on all three: no user-owned fields, no event schema, no return path.
4. **the return channel is now buildable in claude code.** hooks inject context on each prompt [src 16]; `AskUserQuestion` gives typed single/multi-select questions [src 27]; channels (research preview since 2026-03-20) let an MCP server push events into a *running* session, two-way [src 28]. margin's `mg:todo/3` tokens are the manual version of that channel; a margin channel server is the automatic one.
5. **having the model maintain the artifact costs real actions, and a competing artifact confuses it.** manus found that rewriting `todo.md` keeps the model on-goal ("recitation") but its executor spent roughly a third of its actions on list updates (secondary report) [src 29, 30]; codex reportedly made `update_plan` opt-in because the model kept choosing between it and linear/jira [src 15]. → derive what you can from ground truth (transcript, jj, hooks); let the model author only what needs judgment.
6. **steering at the plan level works until regeneration gets expensive.** copilot workspace let you edit the spec or plan and regenerated everything downstream [src 7]; practitioners hit diminishing returns after a few regenerations, and github sunset it (2025-05-30) and folded planning into an async agent that hides the plan [src 7, 8]. → plan edits must cause *local* consequences, not a full regen.
7. **HCI says: structure beats linear chat, and peripheral beats interruptive.** sensecape and graphologue (uist '23) show that spatial, hierarchical, diagrammatic views of LLM output help people explore and comprehend more than the linear transcript [src 31, 32]. a chi '25 study found proactive coding assistants raised task completion 12–18%, but raising suggestion *frequency* halved users' preference for them [src 33]. horvitz's mixed-initiative principles (chi '99) frame it: weigh the cost of interrupting against the value, and resolve uncertainty with cheap dialog [src 34].
8. **what developers lose with agents is oversight capacity, not information.** a 2026 interview study of 17 experienced developers names four kinds of oversight work — a priori control, co-planning, real-time monitoring, post hoc review — and finds people fall back on heuristics like "tests passed" because reviewing agent code is hard [src 35]; another paper calls the lost system model "cognitive debt that resides in people rather than code" [src 36]; hedwig (21 users) asks for oversight that is selective, legible and adjustable [src 37]. margin's panes are oversight instruments; design them as such.

## 1. prior art

### 1a. chat products with a persistent side object

| system | side object | how the AI updates it | what the human can do back | warrant |
|---|---|---|---|---|
| claude.ai artifacts | one document/app per id, versioned | `create` / `rewrite` / `update(old_str,new_str)`; update replaces the first match and returns ok even on a miss (reported) | edit by asking in chat; version picker; artifacts can call claude and keep state (runtime capabilities) | [src 1, 2, 3, 4], [recall] |
| chatgpt canvas | one doc or code file beside the chat | targeted rewrite of a highlighted span; whole-doc edits; shortcut actions ("fix bugs", "port to a language") | highlight → ask; edit directly; version back | [src 5, 6] |
| claude projects / chatgpt projects | file knowledge + instructions | none live — human curates | upload/remove | [recall] |
| openai apps sdk | iframe widget per tool result | tool output → `window.openai.toolOutput`; widget keeps `widgetState` the model can see | widget calls tools, posts follow-up messages, persists its own state | [recall], [src 25] |
| mcp apps (SEP-1865, final) | `ui://` HTML template + tool result data | server declares template up front; tool result fills it | UI calls tools over postMessage JSON-RPC, host may require approval | [src 25] |

the instructive detail: mcp apps separates the static template (reviewable, cacheable) from the dynamic data (tool results) [src 25]. that is the same split margin needs between **component kinds** (compiled into vellum) and **content** (written by the model).

### 1b. coding agents

- **github copilot workspace** — issue → editable spec (current/proposed behavior) → editable plan (files + steps) → diffs. editing any stage regenerates the stages after it [src 7]. sunset 2025-05-30; ideas moved into copilot agent mode and the coding agent, where the plan is internal [src 7, 8]. lesson: the *spec* layer is the right steering altitude, but whole-downstream regeneration makes each edit expensive, so people stop editing.
- **cursor plan mode** — the agent researches, asks clarifying questions, writes a markdown plan with file paths, code refs and to-dos; you edit it, optionally save it to `.cursor/plans/`, then "build" [src 9]. the forum shows the cost of markdown-as-state: to-dos in the plan don't always write back, and todos only sync for agents launched *from* the plan [src 10, 11]. the plan is a file; the to-do state lives elsewhere; they drift.
- **devin** — workspace tabs (shell, browser/desktop, editor, planner); a "following" switch auto-switches tabs as devin works; a progress log where clicking a step shows its detail; you can take over the browser [src 12, 13]. the follow toggle is the interesting part: the default is *follow*, and a human who wants to read something must opt out of the automatic tab switching. reviewers found the label unclear [src 12].
- **cline / roo** — plan/act modes; after every tool use cline commits to a **shadow git repo**; each timeline step has restore → "task only" (rewind the conversation), "workspace only", or both; editing a past message restores files to that checkpoint [src 38]. this is the cleanest prior art for per-turn change diffs and session rewind — and jj gives margin the same thing for free (every turn can be a jj change).
- **zed agent panel** — "review changes" opens a multibuffer of all agent edits with keep/reject per hunk or for the whole set; inline diffs in the active file; committing changes auto-dismisses their review UI; undo of reject-all [src 26]. per-hunk decisions on a structure the *editor* owns (the buffer), not the model.
- **aider** — repo map: tree-sitter tags → reference graph → personalized pagerank boosted by identifiers mentioned in chat → rendered to fit a token budget, refreshed when files change [src 39]. a derived artifact that never asks the model to maintain it. good pattern for a "what the model is looking at" pane.
- **claude code itself** — `TodoWrite` (whole-list replace, rendered inline), plan mode (read-only research then an approved plan), `AskUserQuestion` (single/multi-select + "other", common in plan mode, not available to subagents) [src 27], statusline (a command fed session JSON), hooks with JSON stdin including `transcript_path` [src 16], transcript JSONL under `~/.claude/projects/` [src 40]. a may-2026 feature request notes the built-in activity/plan panels don't surface TodoWrite state [src 18]. channels push external events in [src 28].
- **codex cli** — `update_plan` rendered in the TUI; reportedly opt-in since 2026-08-31 because it competed with external trackers [src 15] (single blog source; unverified).
- **gemini cli** — `write_todos`, one in-progress item at a time, drawn above the input prompt [src 14].
- **warp** — "blocks": every command+output is an addressable unit; agent conversations own their blocks; you can attach terminal blocks to a conversation as context; an agent management view lists runs by status [src 41]. the block as the unit of *reference* is directly relevant to `mg:` tokens.
- **amp, goose, openhands** — [recall] amp shares threads and shows subagent work inline; goose is an MCP host and shipped MCP apps support per one third-party list [src 42]; openhands shows a chat + workspace (terminal, browser, editor) in the devin pattern. nothing new for side-state beyond what's above.
- **neovim** — avante.nvim: a sidebar chat that proposes diffs you apply with one key; codecompanion.nvim: a chat *buffer* plus inline edits with accept (`ga`) / reject (`gr`) [src 43, 44]. codecompanion's insight: the chat is a normal buffer, so every vim motion works on it. margin's panes should be equally addressable (search, yank, jump).

### 1c. canvases and notebooks

- **tldraw agent starter kit** — the model gets a viewport screenshot *plus* structured shape records, recent user actions, and summaries of off-screen clusters; it emits typed actions (create/update/delete/align/distribute), each validated and repaired (duplicate ids, missing shapes, bad coordinates) before applying [src 45]. three ideas to steal: (a) send the model *recent user actions*, (b) validate model ops before applying, (c) summarize what's off-screen.
- **marimo** — a reactive notebook: changing a cell or a UI element reruns dependents or marks them stale; the AI sees live variables [src 46]. staleness as a first-class, *visible* state is exactly what margin artifacts need.
- **jupyter ai** — [recall] `%%ai` magics and a chat sidebar; the side panel doesn't own state. not instructive.
- **ink & switch (patchwork, potluck, embark)** — malleable, live documents for planning and thinking [src 47]. [recall] potluck: plain text plus user-defined "searches" that extract structure and compute over it — the text stays primary, the structure is derived. that is the right shape for "AI writes prose, margin extracts structure."

### 1d. terminal-native companions (the current state of margin's niche)

all read-only, all file-backed:

- `than/sidecar` — go viewer for a `SIDECAR.md` claude maintains, kept fresh by a UserPromptSubmit hook [src 17].
- `cc-todos` (rust) — reads claude code's local todo files, groups by status, for a tmux split [src 17].
- `planc` — browse and annotate plan files; plans update live [src 17]. *annotation is the one step toward a return channel.*
- `agent-dashboard` (bubble tea), `tmux-overseer` — multi-agent monitors with plan/diff viewers and todo progress bars [src 17].
- zellij: `zellij-project-sidebar` (session working/idle/waiting via hooks + `zellij pipe`), `zellij-vtabs` (attention indicators), `claude-code-zellij-status` (zjstatus) [src 19–21]; `dimillian/ai-cli` is a layout [src 22].
- zellij's plugin pipes give margin a real substrate: `zellij pipe --plugin … --name …` delivers stdin to a plugin and returns its stdout, with backpressure (`block_cli_pipe_input`/`unblock_cli_pipe_input`) and plugin-to-plugin messaging [src 48]. that's a synchronous request/response path from a hook into a pane.

## 2. how they do it technically

### document models

| model | who uses it | strength | failure |
|---|---|---|---|
| whole-object replace (small typed JSON) | TodoWrite, write_todos, update_plan | trivially consistent; no anchors | cost grows with size; clobbers user edits |
| string find/replace on text | claude artifacts `update`, chatgpt canvas spans | cheap for big docs | anchor misses, silent no-ops [src 3, 4]; duplicate anchors hit the first match |
| markdown file on disk | cursor plans, sidecar, cc-todos | human-editable, greppable | structure is implicit; to-do state drifts from the file [src 10, 11] |
| flat component list with ids + bound data model | A2UI | streamable; user input writes the data model; agent data refreshes UI [src 23] | catalog must be fixed up front |
| shared state object, snapshot + JSON Patch delta | AG-UI / copilotkit | small deltas; atomic apply; resync by snapshot [src 24] | RFC 6902 paths are *index-based* — concurrent inserts shift them |
| template + data, sandboxed html | mcp apps, openai apps sdk | rich; template reviewable/cacheable [src 25] | needs a browser; overkill in a terminal |
| typed op log validated before apply | tldraw agent | robust to model slop [src 45] | needs a schema per op |
| CRDT (yjs/automerge) | collaborative editors | true multi-writer merge | [inferred] overkill when each field has one owner |

**recommendation for vellum [inferred]:** a block tree with **stable ids** (`todo/3`, `diff/turn-12/hunk-4`), typed blocks drawn from a compiled catalog (A2UI's lesson), and **id-addressed ops** (`upsert`, `remove`, `move`, `set field`) rather than index paths or string anchors. ids survive reordering and make every `mg:` token durable. ops carry a `base_version`; a stale op is rejected *loudly* (fix the claude.ai silent-ok bug by design). whole-artifact replace stays available for small artifacts, since that's what models do most reliably.

### incremental updates

- streaming: A2UI streams a flat list so the client renders progressively [src 23]; AG-UI streams events and applies deltas atomically, requesting a snapshot on failure [src 24].
- for margin: the model writes ops through a CLI/MCP tool (`margin op …`) → the margin daemon validates, versions and appends to a log → panes subscribe and re-render. the model never sees the rendered pane, only a compact text projection on request.

### how the AI is prompted/tooled to maintain the artifact

- **tool-call per update** (todo tools, artifacts) — explicit, auditable, costs a tool call each time.
- **reminders**: [recall] claude code injects a system reminder when the todo list hasn't been touched for a while; manus relies on rewriting `todo.md` at the end of context to keep the goal in attention [src 29].
- **hooks that derive**: sidecar refreshes from a UserPromptSubmit hook [src 17]; aider derives its repo map with no model involvement [src 39].
- **MCP**: mcp apps attaches UI to tools [src 25]; channels push events into the session [src 28].

### versioning and history

claude artifacts keep versions; canvas has back/forward; cline commits a shadow git repo per tool use and restores task, workspace or both [src 38]; zed ties review state to git commits [src 26]. **margin should version artifacts in its op log and tie code changes to jj changes per turn** [inferred] — then "rewind to turn 12" means `jj` for files and op-log truncation for artifacts, both cheap.

### human edits feeding back

the spectrum, from weakest to strongest:

1. human edits the file; the model may or may not reread it (cursor plans; drift [src 10]).
2. human types in chat referencing the artifact (canvas highlight-and-ask [src 5]).
3. structured action → model context: A2UI actions [src 23], AG-UI `state` on the next run [src 24], openai widgetState [recall], tldraw "recent user actions" in every prompt [src 45].
4. structured action → *ground-truth change without the model*: zed keep/reject on hunks [src 26], cline restore [src 38]. the action takes effect immediately; the model learns of it as a fact.

margin should do (4) whenever the action has a deterministic effect (reject a hunk = `jj restore` that hunk; check a todo = set a field) and (3) for everything that needs the model's judgment.

### keeping artifacts consistent with the conversation

nobody solves this well. the patterns that help: derive instead of author (aider), make staleness visible (marimo), version preconditions (AG-UI resync), and stamp every block with the turn that last touched it [inferred].

## 3. why attempts collapse into "a markdown file in a pane"

[inferred, from the prior art above]

1. **markdown is the model's native output**, so the first prototype always asks the model to write markdown — and it works on day one.
2. **no user-owned state.** a markdown file has one writer. the moment the user checks a box or folds a section, the next model rewrite clobbers it, so builders disable editing and the pane becomes a viewer.
3. **no return channel.** even if the user acts in the pane, nothing tells the model. building that channel needs host hooks; until 2026 claude code had only prompt-time hooks, not push [src 16, 28].
4. **no stable addresses.** without ids, the user can't point at "that hunk" except by quoting text back.
5. **maintenance burden lands on the model**, so updates lag, the pane goes stale, the user stops looking, and the builder concludes "side panes don't help." (manus' one-third-of-actions number shows the cost [src 30].)

**the patterns that escape it:**

- **three owners** — model: content and structure; client: view state (fold, scroll, cursor, seen-marks); user: decision fields (approve, reject, pick, pin, priority, answer). the model's ops can't write user fields; a re-render never resets view state.
- **typed components with actions**, not prose: a question card has options; a hunk has keep/reject; a todo has status and "do next". each action has a schema and a deterministic local effect where possible.
- **events back to the model as data**: "user rejected `diff/t12/h4`; user answered `q/7` = option b; user pinned `note/3`" — injected at the next turn (UserPromptSubmit `additionalContext`) or pushed mid-turn (channels).
- **derived panes** that never ask the model for anything (diffs from jj, context gauge from the transcript, agent monitor from hooks); the model authors only judgment (summaries, explanations, decisions, questions).
- **addressability** everywhere (warp blocks, `mg:` ids), so the chat can refer to the pane and the pane to the chat.

## 4. HCI grounding

- **linear chat vs workspace.** sensecape: LLM interfaces are linear while sensemaking needs spatial, multilevel structure; participants explored more topics and built hierarchies [src 31]. graphologue: long LLM answers are hard to comprehend; converting them to node-link diagrams in real time helps [src 32]. wattenberger: a text box has no affordances; responses are isolated; chat is "AI's command line" — good for rough intent, clumsy for refinement [src 49].
- **sensemaking.** pirolli & card's loop — foraging (find, filter) feeding sensemaking (schematize, hypothesize) [src 50]. a research-pin pane is the "shoebox → evidence file" stage made visible.
- **explorable explanations / ladder of abstraction** (bret victor) — the reader manipulates parameters and sees consequences; one steps up and down levels of abstraction [src 51, 52]. in a terminal, that means a parameter the user nudges (`+`/`-`) with a re-rendered graph or table, and a "zoom out" that collapses detail.
- **progressive disclosure** (nielsen) — show the few important things, reveal the rest on demand [src 53]. sulin "reads one line and reacts": each block's first line must stand alone.
- **peripheral awareness / calm technology** (weiser & brown) — information that moves between the periphery and the center of attention without demanding it [src 54]. the pane should *change* quietly and let the user pull.
- **mixed-initiative** (horvitz) — provide real value over direct manipulation; consider the cost of acting under uncertainty about the user's goals and attention; use efficient dialog to resolve uncertainty; keep learning the user's goals [src 34]. the "intent pane" below applies this directly: expose the inferred goal so the user corrects it cheaply.
- **proactivity costs.** chen et al. (chi '25): +12–18% tasks with proactive suggestions, but higher frequency halved preference [src 33]; pu et al. (chi '25, codellaborator): adapt proactivity to the programming process, use interruption management and social transparency [src 55].
- **oversight of coding agents.** four forms of oversight work and the "tests passed" heuristic [src 35]; cognitive debt [src 36]; selective/legible/adjustable oversight [src 37]; large-scale sabotage-detection study shows how hard live detection is [src 56]; experienced developers "don't vibe, they control" [src 57]. map margin panes onto the four forms: plan pane = co-planning, monitor = real-time monitoring, change ledger = post hoc review, constraints/intent = a priori control.

## 5. pushing the idea: use cases ranked by value × feasibility

scoring is my judgment [inferred]: value = how often it rescues attention or catches a mistake in sulin's actual pattern (many queued messages, one-line reads, oversight of long turns); feasibility = how much of it is derivable from data that already exists (transcript, hooks, jj) versus needing new model behavior.

| # | pattern | value | feas. | what it demands of margin/vellum |
|---|---|---|---|---|
| 1 | **decision inbox** — every question the model would otherwise bury mid-paragraph becomes a card (options, default, consequence); answers return as events; unanswered cards persist across turns; the model proceeds on the stated default and marks it "assumed" | ★★★★★ | ★★★★ | question component with single/multi/free answers; user-owned answer field; return channel; "assumed default" state; ties to `AskUserQuestion` semantics but async |
| 2 | **turn ledger with per-hunk keep/reject** — each turn = one jj change; the pane lists turns (one line each: intent → files → result) and expands to structural diffs (difftastic); reject a hunk = immediate `jj` restore, then the model gets "user rejected X" | ★★★★★ | ★★★★ | jj integration in the daemon; diff component with hunk ids; deterministic actions; derived, not authored (only the one-line intent comes from the model) |
| 3 | **queue board** — sulin's queued messages as cards: pending / absorbed in turn n / answered (link) / dropped; the model ticks them off; nothing he asked silently disappears | ★★★★★ | ★★★ | read queued prompts (hook or transcript); a "satisfies `q/…`" op from the model; derived status where possible |
| 4 | **live plan with user-owned fields** — steps carry status (model) and pin/veto/priority/"do next" (user); edits make local changes (reorder, skip), never regen-everything (copilot workspace's lesson) | ★★★★ | ★★★★ | id-addressed list ops; ownership enforcement; plan events into the next turn |
| 5 | **claim ledger** — every factual claim the model makes about behavior gets tagged *ran / read / inferred* with the evidence (command output, file+symbol); select a claim to demand "verify this" | ★★★★ | ★★★ | claim block with evidence link; model tool to emit claims; this mechanizes sulin's own "show the warrant" rule |
| 6 | **since-you-last-looked** — every pane tracks what the user has seen (zellij focus events), and changed blocks carry a mark until seen; a one-line digest on the tab bar | ★★★★ | ★★★★ | per-block seen-state (client-owned); focus events; diff of op log since last view |
| 7 | **intent pane (mixed-initiative)** — the model's current reading of the goal, constraints and assumptions as editable lines; striking one is the cheapest correction possible | ★★★★ | ★★★★ | editable short-text fields owned by the user once touched; events back |
| 8 | **agent/subagent monitor with intervene** — tree of subagents and background jobs, status, elapsed, last tool; actions: stop, message, open transcript | ★★★ | ★★★★ | hook stream (PreToolUse/PostToolUse/SubagentStop); tree component; actions mapped to `claude` CLI / SendMessage |
| 9 | **context gauge + "what's in my head"** — tokens used, files read (aider-style ranked), compaction warnings; pin or evict suggestions | ★★★ | ★★★★ | transcript parsing; gauge component; derived only |
| 10 | **explorable explainer** — the model writes an explanation as steps with a scrubber (step-through a call chain, an algorithm, a state machine) and parameters the user nudges; the graph/table re-renders without a model round-trip when the model supplied the function/data | ★★★★ | ★★ | graph/table components, step state (client-owned), small eval sandbox or precomputed frames; heavier lift |

honourable mentions (lower feasibility or overlap): **session rewind** (cline's task/workspace/both restore on jj + op log) — high value, but ledger (#2) covers 80%; **research pins** with source and "why it matters" lines, foldable into a sensemaking board; **debug board** (hypotheses with evidence for/against, the falsifying experiment next to each); **spec/test board** that shows which acceptance checks pass now; **glossary/one-name pane** that enforces "one name for one thing" across the session.

### interaction patterns that cut across all of them

- **one line stands alone.** every block's head line is a complete sentence-level summary; detail folds. matches "reads one line and reacts."
- **tokens as the return path.** `mg:todo/3` selected in a pane → copied into the prompt *or* sent as an event; prefix verbs (`mg:todo/3!done`, `mg:q/7=b`) make one-keystroke answers. [inferred]
- **never steal focus.** panes update in place; attention marks and a tab badge do the signalling (devin defaults to following; margin should default to *not* moving the user). [inferred from src 12, 33, 54]
- **deterministic first, model second.** an action with a known effect applies now and notifies the model; only judgment calls wait for a turn.
- **stamps.** each block shows the turn that last wrote it; a block older than the code it describes greys out (marimo's stale marking) [src 46].

## 6. pitfalls and mitigations

| pitfall | who hit it | mitigation for margin |
|---|---|---|
| **stale artifacts** | cursor plan/todo drift [src 10, 11]; feature-request gap in claude code [src 18] | derive from ground truth wherever possible; stamp blocks with turn; grey out blocks whose referenced files changed after the stamp |
| **AI forgets to update** | [recall] claude code's todo reminders exist because of this; manus recitation [src 29] | a Stop hook that checks "did this turn touch files but not the ledger/plan?" and nudges once; keep authored artifacts few |
| **maintenance eats the turn** | manus: ~⅓ of actions on todo updates (secondary) [src 30] | small id-addressed ops, batched; derived panes cost zero model actions; optionally a cheap side worker writes summaries from the transcript |
| **token cost of re-rendering** | whole-replace grows with size; manus on KV-cache stability [src 29] | the model never re-sends whole artifacts; inject only *events and deltas* into context, at the end, so the stable prefix stays cached |
| **silent edit failures** | claude.ai `update` returns ok on a miss [src 3, 4] | ops address ids, carry `base_version`, and return explicit errors |
| **conflicting edits** | single-writer markdown clobbers user changes [inferred] | field ownership; model ops can't write user fields; version precondition |
| **competing surfaces** | codex `update_plan` vs linear/jira [src 15] | one canonical plan; if claude code's own TodoWrite is in use, *mirror* it rather than run a parallel list |
| **noise and interruption** | frequency halved preference [src 33]; follow-mode confusion [src 12] | no focus changes; per-pane quiet marks; rate-limit attention signals; let the user pull |
| **regeneration fatigue** | copilot workspace diminishing returns [src 8] | local consequences for plan edits; never "regenerate everything below" |
| **false confidence / over-trust** | "tests passed" as proxy for correctness [src 35]; cognitive debt [src 36] | claim ledger with ran/read/inferred; ledger shows *what changed*, not *that it's fine* |
| **the markdown trap** | every terminal companion [src 17–22] | typed components + three owners + return channel + derived panes (section 3) |

## screenshots and frames worth looking at

i did not open each page's media; these pages are where the product's own images or demo video live [inferred unless noted].

- zed agent panel review multibuffer: https://zed.dev/docs/ai/agent-panel
- cursor plan mode (plan editor with to-dos): https://cursor.com/fr/blog/plan-mode
- copilot workspace spec → plan → diff (video on page): https://githubnext.com/projects/copilot-workspace
- devin workspace tabs and following switch, screenshots in review: https://thegroundtruth.substack.com/p/devin-first-impressions
- devin interactive browser: https://docs.devin.ai/product-guides/interactive-browser
- cline checkpoints restore menu: https://docs.cline.bot/core-workflows/checkpoints
- sensecape demo video: https://sensecape.github.io
- graphologue repo gifs: https://github.com/saliksik/graphologue
- tldraw agent starter kit: https://tldraw.dev/starter-kits/agent
- warp terminal vs agent modes: https://docs.warp.dev/agent-platform/local-agents/interacting-with-agents/terminal-and-agent-modes
- terminal companions with readme screenshots: https://github.com/jakebf/planc, https://pkg.go.dev/github.com/than/sidecar@v0.3.0, https://gh.hhj520.top/nuTTeLLo/zellij-project-sidebar
- bret victor, explorable explanations (interactive in-page): https://worrydream.com/ExplorableExplanations/

## references

1. anthropic, claude.ai system prompt release notes — https://platform.claude.com/docs/en/release-notes/system-prompts
2. simon willison on the artifacts prompt — https://simonwillison.net/b/8800
3. claudeissues.com, "[BUG] Claude web not given an error message when it passes in an incorrect string for old_str" — https://claudeissues.com/issue/9434-bug-claude-web-not-given-an-error-message-when-it-passes-in-an-incorrect-string
4. bram cohen, "how claude web is broken" — https://bramcohen.com/p/how-claude-web-is-broken
5. openai help, "what is the canvas feature in chatgpt" — https://help-lb.openai.com/en/articles/9930697-what-is-the-canvas-feature-in-chatgpt-and-how-do-i-use-it
6. datacamp, "chatgpt canvas" — https://www.datacamp.com/blog/chatgpt-canvas
7. github next, copilot workspace — https://githubnext.com/projects/copilot-workspace
8. copilot workspace review 2026 — https://baeseokjae.github.io/posts/github-copilot-workspace-review-2026/
9. cursor, plan mode — https://cursor.com/fr/blog/plan-mode
10. cursor forum, "in plan mode, models cannot seem to update todo list properly" — https://forum.cursor.com/t/in-plan-mode-models-cannot-seem-to-update-todo-list-properly/142200
11. cursor forum, "plans are not well supported in agents view" — https://forum.cursor.com/t/plans-are-not-well-supported-in-agents-view/175643
12. the ground truth, "devin – first impressions" — https://thegroundtruth.substack.com/p/devin-first-impressions
13. devin docs, session tools / interactive browser — https://docs.devin.ai/work-with-devin/devin-session-tools , https://docs.devin.ai/product-guides/interactive-browser
14. gemini cli, todo tool — https://geminicli.com/docs/tools/todos
15. "the update_plan tool goes opt-in" (single blog source) — https://codex.danielvaughan.com/2026/08/31/codex-cli-update-plan-tool-opt-in-external-planning-competing-surfaces/
16. claude code hooks — https://docs.claude.com/en/docs/claude-code/hooks ; hook stdin guide https://dev.to/aicoding-guide/the-json-your-claude-code-hooks-receive-on-stdin-common-and-per-event-fields-4kbf
17. terminal companions: sidecar https://pkg.go.dev/github.com/than/sidecar@v0.3.0 ; cc-todos https://rust-digger.code-maven.com/crates/cc-todos ; planc https://github.com/jakebf/planc ; agent-dashboard https://awesome.ecosyste.ms/projects/github.com%2Fbjornjee%2Fagent-dashboard ; tmux-overseer https://gh.r-mj.com/inquire/tmux-overseer
18. feature request, persistent todo list panel — https://claudeissues.com/issue/59195-feature-persistent-todo-list-panel-in-sidebar-alongside-activity-plan
19. zellij-project-sidebar — https://gh.hhj520.top/nuTTeLLo/zellij-project-sidebar
20. zellij-vtabs — https://awesome.ecosyste.ms/projects/github.com%2Fotezz%2Fzellij-vtabs
21. claude-code-zellij-status — https://skillselion.com/plugin/thoo/claude-code-zellij-status
22. dimillian/ai-cli — https://github.com/dimillian/ai-cli
23. A2UI via copilotkit docs — https://docs.copilotkit.ai/adk/generative-ui/a2ui ; sd times launch — https://sdtimes.com/ai/google-launches-a2ui-project-to-enable-agents-to-build-contextually-relevant-uis/
24. AG-UI state management — https://docs.ag-ui.com/concepts/state ; assistant-ui agent state — https://www.assistant-ui.com/docs/runtimes/ag-ui/agent-state
25. SEP-1865 mcp apps — https://modelcontextprotocol.io/seps/1865-mcp-apps-interactive-user-interfaces-for-mcp ; blog https://blog.modelcontextprotocol.io/posts/2025-11-21-mcp-apps/
26. zed agent panel — https://zed.dev/docs/ai/agent-panel ; review actions commit https://git.secluded.site/zed/commit/00ff89f00fc771c41c36bb577c6e50893e4a9625
27. AskUserQuestion — https://claudelog.com/faqs/what-is-ask-user-question-tool-in-claude-code/ ; subagent gap https://github.com/anthropics/claude-code/issues/20275
28. claude code channels — https://dev.to/ji_ai/claude-code-channels-how-anthropic-built-a-two-way-bridge-between-telegram-and-your-terminal-2dpn ; https://www.agent37.com/blog/what-are-claude-code-channels-in-2026
29. manus, "context engineering for AI agents" — https://manus.im/blog/Context-Engineering-for-AI-Agents-Lessons-from-Building-Manus
30. tianpan, "context engineering lessons from manus" (source of the ⅓ figure) — https://tianpan.co/blog/2026-03-02-context-engineering-lessons-from-manus
31. suh et al., sensecape (uist '23) — https://arxiv.org/abs/2305.11483v2 ; https://sensecape.github.io
32. jiang et al., graphologue (uist '23) — https://arxiv.org/pdf/2305.11473
33. chen et al., "need help? designing proactive AI assistants for programming" (chi '25) — https://arxiv.org/abs/2410.04596
34. horvitz, "principles of mixed-initiative user interfaces" (chi '99) — https://erichorvitz.com/chi99horvitz.pdf
35. dhanorkar, passi, vorvoreanu, human oversight of agentic systems in practice — https://www.alphaxiv.org/abs/2606.05391.md
36. "beyond the diff" (cognitive debt) — https://arxiv.org/html/2604.16323v2
37. hedwig: dynamic autonomy for coding agents under local oversight — https://arxiv.org/pdf/2605.11495
38. cline checkpoints — https://docs.cline.bot/core-workflows/checkpoints
39. aider repo map (secondary) — https://kitemetric.com/blogs/how-aider-indexes-your-codebase-for-ai-powered-code-assistance
40. claude code transcript format (community) — https://evilpiepirate.org/forge/kent/consciousness/src/branch/master/doc/claude-code-transcript-format.md
41. warp, terminal and agent modes — https://docs.warp.dev/agent-platform/local-agents/interacting-with-agents/terminal-and-agent-modes
42. morphllm, mcp apps hosts (third-party) — https://www.morphllm.com/mcp-apps
43. codecompanion.nvim — https://github.com/olimorris/codecompanion.nvim
44. avante.nvim keybindings — https://www.mintlify.com/yetone/avante.nvim/configuration/keybindings
45. tldraw agent starter kit — https://tldraw.dev/starter-kits/agent ; https://tldraw.dev/docs/ai
46. marimo — https://marimo.io/
47. ink & switch, malleable software essay — https://www.inkandswitch.com/essay/malleable-software/ (cited via https://buttondown.com/geoffreylitt/archive/new-essay-malleable-software)
48. zellij plugin pipes — https://zellij.dev/documentation/plugin-pipes ; https://zellij.dev/documentation/zellij-pipe
49. wattenberger, "why chatbots are not the future" — https://wattenberger.com/thoughts/boo-chatbots
50. pirolli & card, "the sensemaking process and leverage points for analyst technology" (2005) — [recall] https://andymatuschak.org/files/papers/Pirolli,%20Card%20-%202005%20-%20The%20sensemaking%20process%20and%20leverage%20points%20for%20analyst.pdf
51. bret victor, explorable explanations — https://worrydream.com/ExplorableExplanations/
52. bret victor, up and down the ladder of abstraction — https://worrydream.com/LadderOfAbstraction/
53. nielsen norman group, progressive disclosure — https://www.nngroup.com/articles/progressive-disclosure/
54. weiser & brown, "designing calm technology" (1995) — [recall] https://calmtech.com/papers/designing-calm-technology.html
55. pu et al., "assistance or disruption?" (chi '25) — https://arxiv.org/pdf/2502.18658v1
56. "coding with 'enemy'": human detection of agent sabotage — https://arxiv.org/pdf/2606.05647
57. huang et al., "professional software developers don't vibe, they control" — https://arxiv.org/abs/2512.14012
