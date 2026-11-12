# diffs: `margin diff`, one file's change shown as a structural diff {{user}} approves or rejects

When {{user}} should look at a change before it stands (an edit you made, a patch you propose),
put it in the pane as a `diff` block instead of pasting it into chat.

```fish
margin diff plan d1 --old before/retry.cc --new src/retry.cc --path src/retry.cc --parent changed
margin diff plan d1 --rev @- src/retry.cc        # the file at a jj revision against the working copy
margin diff plan d1 --rev @-- --to @- src/retry.cc
```

- The block shows the changed lines with two lines of context: removed lines tinted red, added
  lines green, the changed tokens in a stronger tint, the rest in the language's colors. A pane
  100 cells or wider shows the two files side by side; a narrower one shows one column with
  `-` and `+`.
- A line too long for the pane is cut with `…`, and 16 rows show (`max_rows`). With the focus on
  the block, long lines wrap and every row shows.
- Running it again with the same id replaces the block in place. `--path` is the label in the
  header; `--parent` is the section it goes in.
- The answer is given on the block by {{user}}: approve (`a`), reject (`r`), or ask. The verdict reaches you as a
  `margin:` line and is a `verdict` event (`margin events`); a click on the verdict takes it back.
  A verdict is theirs: to suggest one, `margin claude propose <board> <id> approve`.
- A verdict stays on the id when the block is replaced. After a new diff under an old id, say in
  chat that the content changed, or use a new id.
- margin runs difftastic (`difft`) to compute the diff and does not include it: `$MARGIN_DIFFT`,
  else `difft` on PATH, else the config key `difft`, else the path the nix build compiled in.
  Without it `margin diff` fails with one line that says so, and writes nothing.
- A block with no `rows` and a `text` field draws that text as a plain unified diff: for a patch
  that came from somewhere else.
