# The integration contract

What an outside tool may rely on lives in one place, the help topic the binary embeds:

- `margin help contract` (source: [`skill/topics/contract.md`](../skill/topics/contract.md)):
  the stability promise and the contract version (`v`), the rule that a board holds no logic,
  the shape of an event and the names margin emits (the view events `<board>.shown`,
  `<board>.hidden` and `<board>.detail` among them), a board and a block as `margin show --json`
  prints them, a dataset's records and registry entry, the handler contract (argv, cwd,
  environment, stdin, stdout, exit codes, retries, at-least-once), the owner contract, the exit
  codes of the CLI, and cursors.

This file keeps only what a tool author does not need: how the contract is held in place.

## How it is kept true

- `crates/margin/tests/contract.rs` runs the real binary against a scratch store and asserts
  each documented shape: every documented field of an event, a board, a block, a dataset
  listing and a handler's stdin line is present with the documented type; the handler's
  environment variables, argv and working directory; the documented exit codes; the two
  delivery modes of a cursor; that an unknown field is refused wherever a board could carry
  one. It also checks that the topic names every field it asserts, so a field added to the
  tool and the tests without the document fails too.
- A change that removes or renames a field there, or changes its type, is a new contract
  version: bump `v` and say so in the topic. Adding a field needs a line in the topic's table
  and an assertion in the test.

## Where the code lives

- `crates/margin/src/store.rs`: `Event` (the event line), `is_passive`, `is_view` and
  `VIEW_EVENTS` (the passive rule and the reserved endings), `is_decision`, `is_action`,
  `snapshot` (what an action or a `detail` carries), `wants_daemon` (a passive event starts the
  daemon only for a handler that names it), the cursors.
- `crates/margin/src/app.rs`: `Pane::view_step`, `Pane::hide` and `Pane::focus_changed` append
  `shown` and `hidden`; `Live::detail_step` appends `detail` after `DETAIL_SETTLE`;
  `Live::asked_note` is the detail panel's "asked for more" line. The constants `REFOCUS_SECS`,
  `DETAIL_SETTLE` and `ASKED_SECS` are there.
- `crates/margin/src/handler.rs`: `matches` (which names an `--on` selects), `Worker::spawn`
  (argv, cwd, environment), `Worker::exec` (stdin, stdout, the commit).
- `crates/margin/src/doc.rs`: `check_block` (unknown fields are refused), the schema.
- `crates/margin/src/main.rs`: `exit_code`.
