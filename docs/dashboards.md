# Dashboards

The reference for dashboards lives in one place, the help topic the binary embeds:

- `margin help dashboards` (source: [`skill/topics/dashboards.md`](../skill/topics/dashboards.md)):
  the model, the recipe from design to a board a handler keeps current, every component with a JSON example, the
  column kinds, and the `dashboard` theme keys of the config file.
- `margin help handlers` (source: [`skill/topics/handlers.md`](../skill/topics/handlers.md)): the
  contract of a handler command, the line grammar it prints, and what triggers one.
  `margin help integrate` has the cron, systemd and file-watch recipes that emit its event.
- `margin example [reviews|services|tracker] [--script|--data]` (source:
  [`skill/examples/`](../skill/examples)): three complete boards, each with the script that
  fetches its rows and sample data.
- `margin schema [type]`: every component type and field, as the store checks them.

This file keeps only what an AI building a board doesn't need: where the code is.

## Where the code lives

- `crates/vellum/src/widgets.rs` draws all of it from one `Theme` value: `rule`, `table_layout`,
  `table_header`, `table_cell`, `run_pieces`, `who_pieces`, `id_pieces`. It knows nothing about
  blocks. A name's badge comes from a hash of the lowercased name (`widgets::name_hash`): the
  color from one half, the glyph from the other, so a badge never changes between runs or builds.
- `crates/margin/src/components.rs` holds the adapters (`draw_table`, `draw_tr`, `draw_runs`, the
  `rule` arm of `draw_kind`, `row_glyph`): each parses a block and calls a widget.
- `crates/margin/src/theme.rs` turns the `dashboard` table of the config file into the `Theme`.
- `crates/margin/src/doc.rs` (`SCHEMA`, `check_under`) is what every write is checked against.
- `crates/margin/src/tabs.rs` (`attention`) decides the marks on a pane's tabs.
- `crates/margin/src/example.rs` embeds `skill/examples/` and holds the tests that keep each
  example applying: the board through `Store::put` (the write under `margin new`), the script's output written as its owner.

## Keeping the kit true

- A change to a component's fields: update `skill/topics/dashboards.md`; its JSON examples are
  checked against the schema by `help::tests::dashboards_topic_json_passes_the_schema`.
- A change to an example's script: regenerate `<name>.fetch.out` (`./<name>.fetch.sh
  <name>.data.json > <name>.fetch.out`) and the part of `<name>.json` the script owns;
  `example::tests` fails until the three agree.
