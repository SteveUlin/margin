# margin API review

*Independent review, 2026-11-06, read-only. Sources: `crates/margin/src/{main,doc,store,select,feed,hook,config}.rs`, `skill/SKILL.md`, `docs/design.md`, `docs/guide.md`, `feeds/jj`, `feeds/agents`. Experiments ran against `target/debug/margin` with `MARGIN_HOME` and `MARGIN_PROJECT=review` pointed at a scratch dir; the transcripts quoted below come from those runs.*

## Verdict

**The surface is too wide and the core too narrow.** margin has a sound data model: a tree of `{type, id, children, …fields}`, id-addressed ops, base-rev checks, prefix ownership, and an append-only event log. The CLI then puts one command on top of each op and each component (`ask`, `image`, `title`, `move`, `diff`), and 25 top-level commands plus 7 `feed` subcommands mix Claude's API with renderer, Zellij and dev tooling. Meanwhile the parts that would make it general are missing:

- **No schema.** Unknown fields, bad enum values and numbers-for-strings are all accepted silently.
- **Only Claude receives interactions.** A feed can't make anything clickable.
- **No generic data components.** There's no `table`, `kv` or `chart`.
- **Claude lacks upsert.** The best write path, whole-component replace, which both shipped feeds use exclusively, is hidden from Claude behind `patch`.

The fix is less API, not more: **one `apply` verb that takes the feed line grammar, a component schema the CLI validates against and prints, and events routed to whoever owns the component.**

## 1. Surface area

Counted from the code (`main.rs` `run`/`feed_cmd`/`BOOL_FLAGS`, `doc.rs` `KINDS`/`Op`, `select.rs` `VERBS`, `config.rs` `Config`):

| thing | count | where |
|---|---|---|
| top-level commands | 25 (`put add set rm move title patch ask diff image show ls gc events wait hook open panes close view daemon render select session demo`) + `feed` | `main.rs` `run`; `title` is missing from `HELP` |
| feed subcommands | 7 (`add ls rm pause resume logs run`) | `feed_cmd` |
| distinct flags | 46 (`--home --project --pane --motion --days --kind --speed --at` … undocumented in `HELP`: `--home`, `--project`, `--pane`, `--motion`, `--days`, `--kind` on events) | `grep a.get/a.has` in `main.rs` |
| op kinds | 6 (`add set rm move title replace`) | `doc.rs` `Op` |
| component types | 15, about 50 distinct fields (`row` alone has 13) | `doc.rs` `KINDS`, SKILL.md table |
| token verbs | 10 (`todo tick pick strike approve reject ask open pin fold`); `todo` and `tick` are synonyms | `select.rs` `VERBS` |
| todo status vocabularies | 2: Claude writes `todo doing done blocked`, sulin writes `done todo skipped next clear` | SKILL.md vs `select.rs` `apply_in` |
| config keys | 19 | `config.rs` `Config` |
| env vars read | `MARGIN_HOME MARGIN_PROJECT MARGIN_CONFIG MARGIN_HOST MARGIN_IMAGES MARGIN_MOTION MARGIN_ASSETS` + 5 exported to feeds (`MARGIN_FEED/ARTIFACT/OWNS/HOME/PROJECT`) + `CLAUDE_CODE_SESSION_ID`, `ZELLIJ_*`, `SSH_CONNECTION`, `WAYLAND_DISPLAY` | grep `env::var` |
| SKILL.md | 1702 words (about 2.3k tokens per load) | `wc -w` |

**What's essential:** the document tree, ids, the ops that compose it (`add set rm move replace`), `rev` with base-rev, prefix ownership, the event log with a per-session cursor, `show`, `events`, `wait`, `open`, and the feed registry.

**What's incidental:**
- **Per-component sugar** (`ask`, `image`, `title`, `diff --old/--new`): each one hand-builds a `Block`. `ask` exists only to check that `default` is one of `options`, which belongs in a schema. `diff` is a real producer (it calls difftastic) and earns a command, but it should emit a component rather than own the write.
- **Renderer, multiplexer and dev commands in the same namespace as the AI API:** `view render select session demo daemon panes hook gc`. guide.md "Limits today" records the cost: "`margin session --help` starts a Zellij session … `margin daemon --help` starts a daemon".
- **Synonyms:** `mg:todo` and `mg:tick`, plus two status vocabularies for one field.

## 2. Generality

**Without Rust you can build:** a CI dashboard (rows or `status`, a `--every` feed; I registered one, `ci.sh`, and it landed as `rev 15 by feed:ci`), a reading list or game tracker that Claude maintains (todos, rows, bullets), and an image of a chart rendered elsewhere.

**Without Rust you can't build:**

- **Anything a script reacts to.** `store.rs` `decide` rejects sulin's action on feed-owned ids: "`{id}` is kept current by feed …; its components are read-only". Events reach only Claude, through `hook.rs`. So a reading list kept by a feed can't take "mark read", and a CI panel can't offer "rerun". Interactivity is a closed loop between the pane and Claude, and that limits the most use cases.
- **Tables, key/value grids, charts.** design.md's vellum table lists `table`, `gauge` and `sparkline` as parts, but the only data component is `row`, a fixed slot layout (glyph, tag, text, aside, since). A chart needs an external PNG.
- **A custom component.** guide.md "A new component type" says "a type is a name, not a schema: any `{"type":"x"}` already stores fine". That isn't true: `doc.rs` `normalize` rejects unknown kinds (`margin add: unknown component type "widget" (known: …)`). It's still 6 edit sites in Rust.

**The op model** is general in shape, close to a JSON-Patch subset keyed by id instead of by path, and that's the right choice: ids survive reorders, paths don't. It isn't bespoke per component. What it lacks:

- **No upsert on the CLI.** `add` fails on an existing id ("already exists; use set"). Claude has to know whether a thing exists before writing it.
- **No multi-field set.** `set` writes one field per op.
- **No delete-field verb.** `value: null` does it, but only through `patch`.

## 3. Consistency (with evidence)

| issue | evidence |
|---|---|
| **Values guess their type and can lose content.** `set` and `k=v` parse as JSON first, so `set plan t2 text 2026` stores the number 2026 and the todo renders blank (`b.s("text")` returns `""` for non-strings). `true` and `null` do the same. | ran: `show --json` printed `"text": 2026`; `render` drew an empty todo |
| **Extra positional words vanish.** `add plan - text x2 hello world` keeps only `"world"`: each bare word overwrites `text`. | ran: `show` printed `text x2  world` |
| **No field or value validation.** `set t1 status banana`, `set t1 colour red`, `add … progress of=nosuch` and `{"op":"set","field":"txt"}` all exit 0. | ran |
| **Generated ids are positional, so decisions migrate.** `put` assigns `t1, t2…` by order. I ticked `t2` (beta), re-put with a todo inserted first, and sulin's `done` moved onto "alpha". | ran: `todo t2 [done] alpha` |
| **Status writes don't bump `rev`.** A todo `status` goes to `state.json` (`store.rs` `patch` `status_writes`), so the printed rev stays the same and `--base-rev` doesn't cover it. Claude's own writes also land in "decisions" with `by: claude`, which blurs the three-owner model. | ran: `set plan t1 status doing` printed `plan rev 1` twice |
| **The feed grammar and `patch` differ.** A feed line accepts an op, an array or a whole component. `patch` accepts only an array: a single op gives `invalid type: map, expected a sequence`. | ran |
| **Ownership is a raw string prefix.** `--owns ci` also claims Claude's `city`, `circle` and `cite`. | `store.rs` `owns`: `id.starts_with(p)` |
| **Addressing has three styles:** `<art> <id>` (positional), `<art>.<id>` (`wait`, tokens), and `--parent P`. design.md writes tokens as `mg:pick/Q1.mcp`, but `select.rs` `parse_token` uses `=`. | read |
| **Positional and flag usage is mixed.** `add` puts parent and type in positions and placement in flags. `ask` puts options in positions and the default in a flag. `diff` takes `--rev` with a positional file. `feed add --watch` takes extra paths as positionals after the name. | `main.rs` |
| **Machine output is patchy.** Writes print only `art rev N`. `show --json` and `events --json` exist; `ls`, `feed ls` and write results have no JSON. | read |
| **`wait` on a missing id waits out the timeout** and exits 2 instead of failing fast. | ran: `wait plan.zz --timeout 1s` → exit 2 |
| **Exit codes:** 0 ok, 1 any error, 2 for `wait` timeout. That's fine; keep it and document it. | read |

**Error messages are the strongest part.** `stale base: you edited rev 1 but the artifact is at rev 5; run margin show and redo the change on rev 5` and the feed-owned message (names the feed, the prefix and the two ways out) are models of actionable errors. The problem is the errors that *don't* fire.

## 4. AI ergonomics

- **Per-write cost is already low.** `margin set plan t1 status done` is about 12 tokens in and `plan rev 2` about 4 tokens out. Chaining it onto a real command, as SKILL.md teaches, makes it nearly free. The real costs are elsewhere:
  - **Loading the skill:** about 2.3k tokens, half of it a hand-kept component table and CLI cookbook that a `margin schema` could print on demand.
  - **Knowing what exists:** add-vs-set forces a `show` first (another call) or an error round-trip.
  - **Silent bad writes:** a wrong field name costs a turn later, when sulin says "the pane is blank".
- **The declarative path exists but is hidden.** `put` is whole-doc and clobbers without `--base-rev`. Upsert by id (`replace`) is reachable only as `{"op":"replace","block":…}` inside a `patch` array, and SKILL.md never mentions it. The feeds prove it's the natural write: `feeds/jj` and `feeds/agents` print one whole `section` per run and never emit a fine-grained op.
- **SKILL.md teaches the right principles:** content vs decisions, base-rev, chain writes, don't poll. It teaches too many commands. A model that knows "write components by id; `apply` upserts them" needs one shape, not eight commands.

## 5. Integration: CLI vs MCP vs `apply`

- **Don't build an MCP server for Claude Code.** The cost rule in SKILL.md ("never spend a tool call on margin alone … est. 1.1× chained vs 3× standalone") depends on the write riding inside a Bash call. An MCP tool call is always its own call, so it re-reads the context and costs 3× by the skill's own estimate. MCP only pays for clients without a shell; build it later as a thin wrapper over `apply`.
- **Do build `margin apply`:** stdin JSON in the feed line grammar, plus an envelope for a whole doc. One parser (`feed.rs` `parse_line`) then serves Claude, feeds and any other client. JSON on stdin also removes the shell-quoting and type-guessing traps from §3.
- **Versioning: there is none.** `doc.json`, ops and events carry no version field (grep for `version`/`schema` found nothing relevant). Add `"v": 1` to doc.json and to `apply` envelopes now, while there are two clients (the two feeds). Tolerate a missing `v` as 1.

## 6. Proposals, ranked by value per cost

| # | change | value | cost |
|---|---|---|---|
| 1 | **`margin apply <art> [--base-rev N] [--json]`**: stdin is one op, an op array, a component (upsert), or `{"title":…, "blocks":[…]}` (whole doc). This is exactly `feed.rs` `parse_line` plus the doc envelope. `put` and `patch` become aliases. SKILL.md teaches `apply` with heredocs and `set` as the one-liner. | one grammar for every writer; upsert ends add-vs-set; JSON ends the type guessing | small: `parse_line` exists |
| 2 | **Component schema in one Rust table** (fields, types, enums, required, which fields sulin owns), checked in `normalize`. Errors suggest fixes: `unknown field "txt" on todo (did you mean text?)`, `status "banana": one of todo doing done blocked skipped next`. `margin schema [type] [--json]` prints it, and SKILL.md's component table shrinks to one line. Strings stay strings, because the schema says which fields are text. | kills the silent-bad-write class; fixes the guide drift by generating docs from code | medium |
| 3 | **Route sulin's actions to the owner.** For a feed-owned component, append the event as usual, deliver it on a stream feed's stdin (or as `MARGIN_EVENT` on a rerun), and let the feed declare actions per row (`"actions":["read","drop"]` → `mg:act/<id>=read`). Add `margin events --follow --json` so any script can subscribe. | the generality unlock: reading list, CI rerun, game tracker, approvals without Claude in the loop | medium |
| 4 | **Stable ids:** warn, or refuse, on `put` without ids for interactive kinds (todo, question, claim, diff), or derive ids from content (a slug of `text`) instead of position. Make `--owns` match `prefix` or `prefix-*` only. | stops decisions migrating onto the wrong component (reproduced) and prefixes over-claiming | small |
| 5 | **Generic data components:** `table` (`columns`, `rows` as arrays or objects, `sort`), `kv` (pairs), `chart` (`series` of numbers, `kind: spark/bar/line`, thresholds). vellum's design already lists table, gauge and sparkline parts. `row` stays as the ledger line. | CI, metrics and trackers without image files; ends field creep on `row` | medium |
| 6 | **Split the namespace:** the AI and script API at the top (`apply set rm show events wait open close ls feed`). The plumbing moves under `margin x …` or gets hidden: `view render select session daemon hook demo gc panes`. Every subcommand answers `--help` without acting. | a smaller cognitive surface; ends `--help` side effects | small |
| 7 | **One status vocabulary** (`todo doing done blocked skipped next`) for both writers, and one todo token (drop `tick`). Bump `rev` on status writes, or say plainly that status isn't base-rev guarded. | removes two "which word?" questions | small |
| 8 | **`--json` on every command** (writes: `{"artifact","rev","ids":[…]}`; `ls`; `feed ls`), and `"v":1` in doc, envelope and events. `wait` fails fast on an unknown id. | scripts and future clients; protocol evolution | small |

**Cut:** `title` (an `apply` envelope field), `ask` (becomes `apply` with a `question` and schema validation), `image` as a writer (schema checks `path` is a PNG), `move` as a command (keep the op; `set` can't express it, but it's rare), `mg:tick`, and the `--json`-means-two-things hack in `Args::parse`.

## Sketch: the improved API

```text
margin apply <art> [--base-rev N] [--json] < input        # the one write
margin set   <art> <id> <field>=<value>... [--base-rev N]  # sugar: one set op per pair, typed by schema
margin rm    <art> <id>
margin show  <art> [--json]     margin ls [--json]
margin events [--unread|--follow] [--artifact A] [--since N] [--json]
margin wait  <art>.<id> [--timeout 10m]
margin open  <art> [--float|--split|--stacked]   margin close <art>
margin schema [type] [--json]
margin diff  <art> <id> (--old A --new B | --rev R [--to R2] <file>)   # a producer: computes, then applies
margin feed  add|rm|ls|logs|pause|resume|run …
margin x     view|render|select|session|daemon|hook|demo|gc|panes      # plumbing
```

**One input schema for `apply`:** the same grammar a feed prints, one value per line or one document per call.

```jsonc
// a whole doc (create or replace): the `put` case
{"v":1, "title":"fix recs flake", "blocks":[ {"type":"todo","id":"t1","text":"…"} ]}

// a component: upsert by id (replace in place, else append to `parent` or top level)
{"type":"section","id":"steps","title":"Steps","children":[…], "parent":null}

// an op, or an array of ops (all or none)
{"op":"add",  "parent":"steps", "after":"t1", "block":{"type":"todo","id":"t3","text":"…"}}
{"op":"set",  "id":"t1", "fields":{"status":"done","note":"200 runs"}}   // multi-field; null deletes
{"op":"rm",   "id":"t3"}
{"op":"move", "id":"t3", "before":"t2"}          // or "parent":"P"
```

Changes from today's ops: `set` takes `fields` (keep `field`/`value` as an accepted alias), `title` disappears into the envelope, and `replace` becomes implicit for a bare component. Ownership, base-rev and the all-or-nothing rule stay as they are.

## Migration path

1. **Add without breaking.** Ship `apply` as a superset of `patch`. Accept a single op in `patch`. Add `margin schema`. Validate in **warn mode** (stderr, exit 0) for a week, so existing writers surface their bad fields.
2. **Repoint the skill.** Rewrite SKILL.md around `apply` and `set k=v`, and replace its component table with "run `margin schema`". The feeds need no change, because their output already is `apply` input.
3. **Enforce.** Turn validation into errors and add `"v":1`. Make `put`, `patch`, `add`, `ask`, `title` and `image` print a one-line deprecation pointing at `apply`, then remove them.
4. **Generalise.** Event routing to feeds and `events --follow` come first, because they unblock use cases. Then `table`, `kv` and `chart`.

## The minimal core

These pieces carry the design, and everything else is a convenience on top:

- **A document:** a tree of `{type, id, children, …fields}`, with a schema per type.
- **Writes:** one write, `apply`, built from `add`, `set` (multi-field), `rm`, `move` and upsert-by-component, all-or-nothing, with an optional base rev.
- **Owners:** Claude writes content, feeds write their own id namespace, and sulin writes decisions into `state.json`.
- **Events:** one log, a cursor per reader, delivered to the component's owner.
- **Reads:** `show` and `events` as JSON.

That core is about 8 commands, against today's 32 commands and subcommands. Feeds and Claude become the same kind of client: one speaks through a script's stdout, the other through Bash.
