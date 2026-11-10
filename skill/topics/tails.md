# tails: a long command's output, live in the pane

For a command that runs long enough to watch (a full test run, a build, a deploy, a dev server),
run it through margin:

```fish
margin tail <art> <id> --title "cargo test" --parent now -- cargo test
```

Its output still comes back to you as usual and it exits with the command's code. The pane shows
a spinner, the time and the last 6 lines (`--lines N`; errors red) while it runs, then folds to its
outcome with a toast. Use it in place of narrating "running the tests…"; skip it for a command
that takes seconds.
