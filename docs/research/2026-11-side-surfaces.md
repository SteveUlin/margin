# side surfaces beside an ai chat: what the designs teach margin

researched 2026-11-08. warrant tags:
- **[read: URL]**: i fetched the page and the claim comes from it.
- **[snippet: URL]**: the claim comes from a search-result summary of that page. i didn't fetch the page itself, so it's weaker evidence.
- **[inferred]**: my reasoning, not stated by any source.

`gh` was not used. Everything here comes from web search and fetch.

---

## 1. comparison table

| system | chat holds | side surface holds | user acts on side state → AI | revisions shown | questions / decisions / approvals | anti-clutter |
|---|---|---|---|---|---|---|
| **ChatGPT Canvas** | the conversation and requests | one doc or code file | direct edit; highlight text to ask about it; shortcuts ("suggest edits", "review code") **[snippet: help.openai.com/en/articles/9930697]** | version arrows plus restore; a "show changes" diff against the previous version **[snippet: datacamp.com/blog/chatgpt-canvas]** | the AI leaves inline **comment bubbles**. The user fixes the text and closes the bubble, or clicks **Apply** so the AI fixes it **[snippet: help.openai.com/…/9930697]** | one artifact at a time **[inferred]** |
| **Claude Artifacts** | the conversation, and anything short or conversational | self-contained content, roughly >15 lines, that you'll edit or reuse **[read: support.claude.com/en/articles/9487310]** | ask in chat; highlight → "Edit with Claude" targets that span; **queue edits across several files, applied in one pass** **[read: same]** | a version selector **[snippet: same article, older copy]**; editing an earlier message forks the chat and its artifacts **[read: same]** | "Try fixing with Claude" sends the error details **[read: same]** | an explicit **admission rule** (size, self-contained, reused) decides what goes beside **[read: same]** |
| **Gemini Canvas** | the conversation | a doc, code, or an app preview | highlight → a small prompt box scoped to the selection; quick tone and length actions **[snippet: androidauthority.com/google-gemini-canvas-launch-3535808]**; "Select and Ask" on UI elements in the preview, reported as testing **[snippet: inkl.com]** | not found | not found | not found |
| **Cursor Plan Mode** | research and **clarifying questions** before the plan **[read: cursor.com/blog/plan-mode]** | a markdown plan with file paths and code refs, plus to-dos **[read: same]** | edit the plan inline; add or remove to-dos; **Build** button **[read: same]** | n/a | questions come before the plan exists **[read: same]** | the plan can be saved to the repo (`.cursor/plans/`) and leave the chat **[read + snippet: forum]**. Forum: to-dos drift from the md file, and the plan sometimes builds without Build being pressed **[snippet: forum.cursor.com/t/…/142200, …/149408]** |
| **Windsurf Cascade** | the to-do list lives *in the conversation* **[snippet: docs.windsurf.com/windsurf/cascade/cascade]** | in planning mode, a long-term plan that a separate planning agent refines while the main model takes short-term actions **[snippet: docs.windsurf.com/windsurf/cascade/planning-mode]** | "ask Cascade to update the Todo list". There's no direct manipulation **[snippet: same]** | not found | not found | not found |
| **Zed agent panel** | the conversation, plus a summary: N files and lines changed **[snippet: zed.dev docs, git.secluded.site commit 4e06656]** | a **Review Changes multibuffer** with every edit **[snippet: same]** | accept or reject **per hunk or all**; also inline in single-file editors **[snippet: same]** | hunks; reject-all is undoable (0.226); `cmd-y` accepts, matching git review **[snippet: zed.dev/releases/preview/0.226.0, 0.227.0]** | approvals are hunk verdicts | review lives in a separate tab, not in the chat **[snippet]** |
| **Copilot Workspace** (sunset 2025; its ideas moved into the Copilot coding agent **[snippet: aiwiki.ai]**) | task (issue) → topic question | spec as **current state ↔ proposed state** bullet lists → plan (files × steps) → diffs **[read: github.com/githubnext/copilot-workspace-user-manual/…/overview.md]** | edit or delete spec bullets; edit plan steps; "try again" at any level; check files → "Update selected files" regenerates only those **[read: same]**; editing upstream regenerates downstream **[snippet: githubnext.com/projects/copilot-workspace]** | editable diffs per file; plan entries go in progress → done; undo/redo; share snapshot; branch from it **[read: overview.md]** | "View references" shows which files informed the spec **[read: same]** | the spec is a **delta**, so it states only what changes **[inferred]** |
| **Devin** | the session chat | plan, IDE, shell, browser | edit the plan before it runs; citations deep-link into the IDE **[snippet: docs.devin.ai/work-with-devin/interactive-planning]** | not checked | **confidence green/yellow/red** at session start, after the plan, and on answers. When it isn't green, Devin asks clarifying questions and waits for approval; when it is, it proceeds and takes feedback async. Green → ~2× merged-PR rate **[read: cognition.com/blog/devin-2-1]**. Older behaviour: proceed after 30 s with no feedback **[snippet: docs.devin.ai]** | not checked |
| **Google Jules** | chat feedback on the plan | plan → diff | "Approve plan"; feedback before, during, and after execution **[snippet: enginelabs.ai, infoq]** | n/a | **auto-approves after a timeout** (one review says 5 min). One user said it approved before they could read it **[snippet: secondary reviews, unverified]** | n/a |
| **Replit Agent** | the conversation | Plan mode: a task list with priorities and dependencies **[read: docs.replit.com/learn/plan-vs-build-mode]** | **Revise / Cancel / Build here / Build in background** **[read: same]** | **a checkpoint per completed task**, each with rollback. A checkpoint covers files, AI conversation context, and the DB **[read: same; snippet: …/checkpoints-and-rollbacks]** | background builds wait for review before applying unless auto-apply is on **[read: same]** | n/a |
| **v0 (Vercel)** | prompts | a live preview plus a design panel | Design Mode: click an element → edit props, or prompt about it (v0 attaches a screenshot of the element) **[snippet: v0.app/docs/design-mode]** | pending tweaks show undo/redo/reset and a **before/after toggle**; applying them creates a normal chat version **[snippet: same]** | n/a | forum: targeted edits changed surrounding elements too **[snippet: community.vercel.com/t/…/36673]** |
| **Notion AI / Agent** | the agent chat | the page itself | "suggest edits" → **approve each proposal top-to-bottom** **[read: notion.com/releases/2026-08-28]**; plan mode (off by default): no edits while planning **[snippet: notion.com/help/review-and-approve-plans…]** | a track-changes overlay you can show or hide **[snippet: matthiasfrank.de]**; any agent change can be undone **[snippet: notion.com/help/notion-agent]** | the plan is approved before the agent runs **[snippet]** | toggle the overlay off to see the clean result **[snippet]** |
| **Linear agents** | the issue thread, as an "Agent Session" | session **state** (`pending, active, awaitingInput, error, complete, stale`), derived by Linear from the agent's last activity; **`agentSession.plan`**: `[{content, status: pending/inProgress/completed/canceled}]`, always **replaced whole**; `externalUrls` **[read: linear.app/developers/agent-interaction]** | user replies are `prompt` activities, the only type users create. The agent ends a turn with `response`, `elicitation` (needs input) or `error` **[read: same; snippet]** | `thought` and `action` activities can be **ephemeral**: the next activity replaces them **[read: same]** | Guidelines: disclose that it's an agent, acknowledge instantly, show internal state, **stop immediately on disengage and resume only on a clear signal**, the human stays accountable **[read: linear.app/developers/aig]** | ephemeral activities; acknowledge within 10 s, else the session is marked unresponsive; stale after 30 min **[read + snippet]** |
| **tldraw agent kit** | a chat panel listing each action as an icon plus a description **[read: tldraw.dev/starter-kits/agent]** | the canvas | the user's selection and added shapes or areas become the agent's context. In-focus shapes get full detail; on-screen shapes get a "blurry" summary; off-screen shapes are summarised as clusters with counts **[read: same]** | edits appear incrementally as each typed action finishes streaming; `sanitizeAction()` repairs bad IDs **[read: same]** | the agent keeps its own todo list; `schedule()` queues follow-ups; `interrupt()` **[read: same]** | **detail tiers by attention** (focus, viewport, off-screen) **[read: same]** |
| **Jupyter AI / Notebook Intelligence** | a sidebar chat **[snippet: blog.jupyter.org/introducing-notebook-intelligence]** | the notebook | the agent edits cells through tools **[snippet]** | a jupyterlab-cell-diff unified diff with accept/reject and an X to dismiss **[snippet: github.com/jupyter-ai-contrib/jupyterlab-cell-diff/pull/16]** | pynote: type `apply` in chat **[snippet]** | n/a |
| **Kiro** | the conversation; exploration stays here so the coding context stays focused **[read: kiro.dev/blog/from-chat-to-specs-deep-dive]** | `.kiro/specs/<name>/requirements.md` (EARS acceptance criteria), `design.md`, `tasks.md`, plus steering docs **[read: kiro.dev/docs/specs]** | edit the files; a floating **Review** button → **Markdown Review: comment on passages, send all comments at once** **[read: same]**; run one task or all **[read: same]** | tasks marked in progress or completed in the IDE, CLI, and web **[read: same]** | gates between phases (Quick Spec skips them) **[read: kiro.dev/docs/specs/feature-specs]**; "Analyze Requirements" checks for conflicts and gaps before design **[read: same]**; run-all groups tasks into waves: sequential waves, concurrent tasks within a wave **[read: kiro.dev/docs/specs]** | the spec moves out of chat into files **[read: blog]** |
| **Magentic-UI** (MSR, arXiv 2507.22358) | conversation; clarifying questions get answered in chat | a plan editor, an embedded browser, a sessions list | edit plan steps or give text feedback; **Accept Plan**; pause, take over the browser, hand it back ("co-tasking") **[read: arxiv.org/html/2507.22358v1]** | progress bar; **each step is a collapsible banner that collapses when done** **[read: same]** | **action guards by irreversibility**: always / never / ambiguous → an LLM judge. Users wanted approval for payments and email but not add-to-cart, **wanted a third option beyond approve/deny, and found an approval after they had already edited the plan excessive** **[read: same]** | collapse finished steps; a **red dot per session needing input**. Users found it hard to recall what a background task had done and asked for a per-task status summary **[read: same]**; saved-plan gallery and "plan from trace" **[read: same]** |
| **Cocoa** (CHI 2026, arXiv 2412.10999) | n/a: the plan lives inside the document | an inline **interactive plan** plus an output sidebar | steps are list items you rewrite, add, or delete; **a per-step toggle sets whether user or agent owns the step**; run-all stops at a user step, or step mode pauses after each **[read: arxiv.org/html/2412.10999v4]** | step glyphs: bullet (not run), spinner, **? badge (needs input)**, check; an edited output gets an indicator; **an edit that changes direction removes the inconsistent downstream steps and regenerates them**; re-running a step re-runs everything downstream **[read: same]** | the ? badge on a step **[read: same]** | DG3 "manage outputs to avoid clutter": outputs open in a sidebar, and the final output goes in a collapsed panel **[read: same]**. 33% of steps were edited and 17% reassigned; users wanted short partial plans for open work and full plans for structured work **[read: same]** |
| **Elicit "living documents"** | n/a | a table that persists for weeks | add rows, columns, or filters while work runs; new edits queue more work **[read: elicit.com/blog/living-documents-ai-ux]** | streaming cells; **subtle placeholders rather than spinners**, to avoid a grid of motion; visible cells first **[read: same]** | **low-confidence answers flagged subtly**; click a cell → its source quote **[read: same]** | retries stay silent; errors surface only when they're frequent **[read: same]** |

HCI on ask-vs-assume:
- *Ask or Assume?* (alphaxiv 2603.26233). A separate **Intent Agent** watches for underspecification and asks just-in-time: about 3 short questions per task, **mid-task, not up front**. This recovered 69.4% vs 70.8% fully specified and 54.8% with no asking. Forcing questions up front *hurt* one model (47.2%). The ask rate rose with task difficulty **[read: alphaxiv.org/abs/2603.26233v2]**.
- Ambig-SWE: LLMs default to non-interactive behaviour and can't tell when a spec is underspecified **[snippet: arxiv.org/html/2502.13069]**.
- arXiv 2605.07937: asking too early wastes questions that later context would answer; asking too late commits irreversible actions **[snippet]**.
- Horvitz 1999 (CHI): mixed-initiative principles. One of them: if the system is uncertain about the user's intent, it should engage in an efficient dialogue **[snippet: erichorvitz.com/chi99horvitz.pdf; full list not read]**.

---

## 2. patterns

1. **The plan is a first-class, editable object, and execution waits on it or reads from it.** Cursor, Replit, Devin, Jules, Magentic-UI, Notion, Kiro, Copilot Workspace, and Cocoa all do this. The variation lies in *how hard the gate is*:
   - hard button: Cursor Build, Replit Build, Magentic Accept Plan
   - timeout: Devin's old 30 s, Jules' timeout
   - confidence-gated: Devin 2.1

   Users complain loudly when the gate leaks (Cursor's forum, Jules' early auto-approve). margin's "default Claude proceeds on" sits in the timeout/confidence family. The lesson is to make the moment Claude proceeds **visible and predictable**.
2. **Step status uses a small fixed vocabulary.** Linear's `pending/inProgress/completed/canceled`, Cocoa's four glyphs, and Kiro's in-progress/completed match each other closely. The one extra state that recurs is **needs-input**: Cocoa's ? badge, Linear's `awaitingInput`, the Magentic red dot. Linear derives the state from the last activity rather than having the agent set it.
3. **Upstream edits invalidate downstream work.** Copilot Workspace regenerates downstream steps; Cocoa deletes inconsistent steps and re-runs downstream. The side surface is a *dependency graph*, even when it renders as a list.
4. **Revisions show as verdict-able units, not prose.** Zed hunks, Notion's per-suggestion approve, Canvas comment bubbles with Apply, v0's before/after toggle, Jupyter's cell accept/reject. Undo of a bulk verdict recurs too: Zed made reject-all undoable, and Notion says every agent change can be undone.
5. **Batch the user's feedback, then send it once.** Kiro Markdown Review ("send all comments at once") and Claude Artifacts' queued multi-file edits. This fits a hook-delivered turn model naturally.
6. **Selection is context.** Canvas, Gemini, Artifacts ("Edit with Claude"), v0 Design Mode, and tldraw: what the user points at becomes the scope of the next request.
7. **Clutter control means collapse and tier, not delete.** Magentic collapses finished steps; Cocoa moves outputs to a sidebar and a collapsed panel; tldraw tiers detail by attention; Elicit uses quiet placeholders and silent retries; Linear's ephemeral activities replace each other.
8. **Approvals grade by risk.** Magentic's irreversibility tiers and Replit's auto-apply switch both do this. Users reject approvals for cheap, reversible actions and want a third answer besides yes/no.
9. **Grounding links.** Devin's citations, Copilot Workspace's "View references", Cursor's plan file paths, Elicit's source quote per cell: each claim on the side surface points at its evidence.
10. **Asking works best just-in-time and selectively** (Ask-or-Assume). Front-loading questions (Cursor, Devin) is the product norm, but the research favours mid-task questions when ambiguity actually blocks the work.

---

## 3. ideas for margin (11)

1. **Assumptions carry dependents, and striking one marks them stale.** *From Copilot Workspace's downstream regeneration and Cocoa's replanning.*
   - When Claude writes a todo or a diff that rests on an assumption, it names the assumption's id.
   - Striking that assumption flags those todos and diffs ⚠ stale in the pane. The hook payload says "assumption A3 struck; todos T4, T6 and diff D2 depend on it".
   - Today a strike reaches Claude, but Claude has to work out the blast radius itself **[inferred]**.
2. **Per-step owner: me vs Claude.** *From Cocoa's assignment toggle.*
   - A todo can belong to sulin. Claude's run-through stops at a sulin-owned step, the way Cocoa's run-all stops at a user step.
   - This fits "next/skip" cycling and adds what Cocoa found people do over time: they keep the reasoning and writing steps for themselves (field study).
3. **Anchored notes, sent as one batch.** *From Kiro Markdown Review and Artifacts' queued edits.*
   - In a long-form overview or a plan item, sulin attaches a note to a line or section. Notes pile up as "pending notes (3)" and reach Claude together at the next turn, each with its anchor.
   - margin's hook-delivered model already batches by turn. This adds the anchor and a visible pending count.
4. **A jj checkpoint per done todo.** *From Replit's checkpoint-per-task with rollback.*
   - When Claude marks a todo done, margin records the jj change id. A done row offers "restore to here", which sends Claude an intent rather than running `jj` blindly.
   - jj's mutable commits make this cheap **[inferred]**.
5. **The proceed moment is explicit.** *From the Devin and Jules timeouts and from the complaints about leaky gates.*
   - Each open question shows "Claude proceeds on ‹default› at its next turn". Once Claude acts on it, the question flips to "applied: default".
   - This exists partly already (the `applied` bool). The display wording should make the pending-vs-applied difference obvious. The Jules complaint ("approved before I could read it") is the failure to avoid.
6. **A confidence light on goal and plan.** *From Devin 2.1.*
   - A green/yellow/red dot that Claude sets. Non-green obliges Claude to pair it with a question or an assumption. That turns "I'm guessing" into state sulin can see at a glance.
   - Devin reports green ≈ 2× merged-PR rate, but that is Devin's correlation, not measured for margin.
7. **Graded approvals plus a third answer.** *From Magentic-UI's action guards.*
   - Diffs and risky steps get approve / reject / **change…**, where "change" opens a one-line note.
   - Don't ask for approval on something sulin just edited. Magentic users called that excessive.
   - Reserve approval for irreversible or external actions and let reversible ones flow, since jj makes most local edits reversible.
8. **Collapse finished, tier the rest.** *From Magentic's collapsing banners, tldraw's focus/viewport/off-screen tiers, Cocoa DG3, and Linear's ephemeral activities.*
   - Done todos collapse into a single "✓ 7 done" line.
   - A single ephemeral "now:" line that each update replaces, never appends, carries what's running.
   - Struck assumptions and answered questions fold into a count.
   - The pane shows full detail only for the focused item.
9. **A derived pane state with a needs-you signal.** *From Linear's session states, which are derived from the last activity, and Magentic's red dot.*
   - The pane header shows `working / needs you / done / stale`, computed from components (an open question with no default → needs you) rather than set by Claude.
   - The Zellij tab or the pane title can carry it, so sulin sees it without focusing the pane. That respects the no-focus-steal rule.
10. **Grounding refs on every claim.** *From Devin's citations, Copilot Workspace's "View references", and Elicit's source quotes.*
    - Todos, assumptions, and diffs can carry `file::symbol` refs that margin renders as dim suffixes. Activating one opens the location in an nvim pane, without stealing focus.
    - It also lets sulin check an assumption without asking.
11. **Hunk-level verdicts with an undoable bulk reject.** *From Zed.*
    - Diffs approve per hunk, show a header summary ("3 files +40 −12"), and keep "reject all" undoable for one turn.

Lower priority, noted: a saved-plan gallery / "plan from trace" (Magentic), and calibrating plan length (Cocoa: short plans for open work).

### top 4
1. assumptions → dependents → stale (#1)
2. step owner toggle (#2)
3. anchored, batched notes (#3)
4. a visible proceed moment plus a confidence light (#5 + #6)
