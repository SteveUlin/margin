# toasts: news that matters for a moment

```fish
margin toast plan "132 tests passed · 4.1s" --at t3 --level ok     # ok | info | warn | error
```

A toast shows as a full-width bordered box at the pane's bottom (two at once, oldest lowest);
`--at` names the row it's about. Its 8 s start once {{user}} has seen the pane; `--level error`
stays until clicked.

- Use one for the result of a step you just ticked (tests, a build), a failure worth knowing
  about, or a subagent's finding as it lands.
- Not for anything needed later (that's a row) or anything that needs an answer (that's a
  question).
- A handler sends one as a line on its stdout: `{"toast": "…", "level": "ok", "at": "<id>"}`.
- Toasts live beside the doc: no revision, no conflicts.
