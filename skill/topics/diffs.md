# diffs: a structural diff to approve or reject

```fish
margin diff plan d1 --old /tmp/a.ts --new src/recs.ts --path src/recs.ts --parent ask
margin diff plan d1 --rev @- src/recs.ts          # jj: the file at @- vs the working copy
margin diff plan d1 --rev @-- --to @- src/recs.ts # jj: between two revisions
```

The diff is difftastic's structural diff, drawn side by side when the pane is wide. Writing the
same id again replaces it in place. In the pane {{user}} approves or rejects it (or with
`mg:approve/d1` / `mg:reject/d1`); you get a `margin:` line with the verdict. A verdict can be
taken back: then it waits for one again, so hold off acting on the earlier one.

`margin diff` needs `difft`: `$MARGIN_DIFFT`, else PATH, else the config key `difft`.
