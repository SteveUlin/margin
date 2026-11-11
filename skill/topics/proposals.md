# proposals: offering a change to one of {{user}}'s decisions

Their picks, ticks and strikes are theirs. When you think one should change, propose it;
don't take it and don't route around it.

```fish
margin claude propose plan t3 tick skipped --why "no user-facing change"   # tick done|todo|skipped|next
margin claude propose plan Q1 pick files --why "sqlite needs a daemon"
margin claude propose plan A1 strike --why "CI runs two processes since last week"
```

The proposal shows on the component. Their accept applies it as their decision; you get a
`margin:` line either way ("accepted your proposal…" / "declined…: leave it as they have it").
**Why:** a proposal costs one click and never moves their state behind their back.
