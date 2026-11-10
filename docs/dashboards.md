# Dashboards

The reference for dashboards lives in one place, the help topic the binary embeds:

- `margin help dashboards` (source: [`skill/topics/dashboards.md`](../skill/topics/dashboards.md)):
  the model, the recipe from design to a feed-kept board, every component with a JSON example, the
  column kinds, and the `dashboard` theme keys of the config file.
- `margin help feeds` (source: [`skill/topics/feeds.md`](../skill/topics/feeds.md)): the contract
  of a feed script, the three triggers, and a complete script.
- `margin example [reviews|services|tracker] [--feed|--data]` (source:
  [`skill/examples/`](../skill/examples)): three complete boards, each with its feed script and
  sample data.
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
  example applying: the artifact through `Store::put`, the feed's output written as the feed.

## Keeping the kit true

- A change to a component's fields: update `skill/topics/dashboards.md`; its JSON examples are
  checked against the schema by `help::tests::dashboards_topic_json_passes_the_schema`.
- A change to an example's feed script: regenerate `<name>.feed.out` (`./<name>.feed.sh
  <name>.data.json > <name>.feed.out`) and the feed-owned part of `<name>.json`;
  `example::tests` fails until the three agree.
