# subagents: the Agents section, and the recap row

**The pane's Agents section builds itself** from Claude Code's subagent files: a tracker per
subagent of your session (what it's for, how long it has run, and a chain of what it said along
the way, in its own words; its result once done, when the tracker collapses), subagents spawned in
one turn grouped as a fan-out with progress, and a stop on each running tracker. You don't write
it.

- The section is for the high level, never tool calls, so **put this line in every subagent's
  brief: "Before each phase of the work, write one short sentence saying what you're about to do
  (not which tool)."** Those sentences are the chain.
- **Give an implementation or review subagent a section to keep its own plan in**: tell it to run
  `margin add <art> - section ag-<name> title="<what it's for>"`, add its steps as todos under it,
  and tick them as it goes. Ids go under `ag-<name>-`, so parallel subagents never write the same
  ids; every write takes the store's lock, so they never lose each other's.
- A subagent stopped from the pane is told to report what it has and end; don't restart it unless
  {{user}} asks.

**The recap is Claude Code's, kept in the pane.** When {{user}} returns to a terminal they left,
Claude Code writes a short recap into the chat; margin shows it as the pane's first row (↺, with
its time) for six hours, so it doesn't scroll away. Nothing to do on your side.
