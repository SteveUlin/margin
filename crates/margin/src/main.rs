//! margin: live boards in a terminal pane. One binary, four roles: the CLI a person, a script or
//! an assistant calls, the renderer in a pane (`margin view`), the selection dispatcher Zellij
//! pipes copies into (`margin select`), and the event dispatcher (`margin daemon`).
//!
//! The core is generic: boards, an event log with cursors, handlers, owners, datasets, the pane.
//! `claude` is one consumer of it, the Claude Code adapter: this file dispatches `margin claude …`
//! to it and plugs it into the core's seam (`consumer.rs`) at start-up. No core module names it.

mod app;
mod claude;
mod components;
mod config;
mod consumer;
mod daemon;
mod data;
mod demo;
mod detail;
mod diffblock;
mod doc;
mod doctor;
mod example;
mod graph;
mod handler;
mod help;
mod highlight;
mod host;
mod mdoc;
mod owner;
mod pin;
mod scene;
mod select;
mod store;
mod tabs;
mod theme;
mod toast;
mod util;
mod watch;
mod who;
mod zj;

// The adapter's modules under their old top-level names, for the two core files that still reach
// into it by those paths (store.rs: `link::all` in `stop`, `hook::describe` in its tests;
// doctor.rs: `wake`'s leftover marks). They go when those calls move behind `consumer`.
#[allow(unused_imports)]
use claude::{hook, link, wake};
use config::Config;
use doc::{Block, Doc, Op};
use serde_json::{json, Value};
use std::io::Read;
use store::Store;

const HELP: &str = "margin: live boards in a terminal pane

boards: creating and changing are different commands, for a board and inside one
  create   margin new (a board)    margin add, margin ask (a component; an id that exists is refused)
  change   margin set, move, rm, title, purpose, hints (what they name must exist)
  state from a script   margin apply (components upsert by id; the board must exist)
  margin new <board> [--title T] [--purpose P] [--kind K] [< doc.json]   create a board (empty, or from a
                                                         whole doc); one that exists is refused
  margin apply <board> [--base-rev N] [--json] < input   update: ops, components (upsert by id), or a whole doc
  margin schema [type|document] [--json]                 the document's fields, component types, fields, values
  margin add <board> <parent|-> <type> <id> [text] [k=v ...] [--before X|--after X] [--base-rev N]   create a component
  margin set <board> <id> <field> <value> | <field>=<value>... [--base-rev N]   change one (typed by the schema)
  margin rm <board> <id>        margin move <board> <id> --before X|--after X|--parent P
  margin rm <board> [--yes]                              remove a whole board (to .archive/; without --yes: a preview)
  margin title <board> <title>                           rename the board
  margin hints <board> [text...]                         reminders at the pane's bottom (none: clear)
  margin purpose <board> [\"text\"]                        the pane's one line of orientation (none: clear)
  margin ask <board> <id> <question> <opt>... [--suggest S] [--parent P] [--why W]
  margin doc <board> <id> [--title T] < notes.md         long-form markdown as a section
  margin diff <board> <id> --old A --new B | --rev R [--to R2] <file>   one file's change as a structural diff to approve or reject
  margin toast <board> <text> [--at ID] [--level ok|info|warn|error]   a message that goes away
  margin show <board> [<id>] [--json]    margin ls [--json]    margin gc    margin doctor [--fix]

events (what the person does in the pane, and anything a program appends):
  margin events [--since N | --cursor NAME [--no-commit]] [--name GLOB] [--follow] [--json]   the event log
  margin emit <name> [JSON|-] [--board B] [--id ID]                 append a named event (prints its seq)
  margin act <board> <id> <action> [VALUE]                          what an action's key does, from the CLI

handlers and outside writers (margin runs a command only for an event; scheduling stays outside):
  margin handler add <name> --on <glob>... [--board B --owns PREFIX] [--timeout 60s] [--retries 3] -- <cmd...>
  margin handler set <name> ... | ls [--json] | rm | pause | resume | logs <name> [--follow] | run <name> [--events F] [--dry-run]
  margin handle open            the built-in handler that opens a row's target: margin handler add open --on '*.open' -- margin handle open
  margin owner add <name> --board B --owns PREFIX [--expect-every 10m]   rows an outside program writes: margin apply B --as <name>
  margin owner set <name> ... | ls [--json] | rm <name>
  margin daemon [status | install | uninstall] [--unit-dir D]   the dispatcher (started on demand); status, and a systemd user unit

datasets (values a board displays, outside the document; the pane reads them when it draws):
  margin data add <name> [--cap N] [--file PATH] [--owner OWNER] [--expect-every 10s]   register one (--file: an external JSONL or CSV file, read in place)
  margin data set <name> ... | append <name> <VALUE|JSON|-> | put <name> < records | show <name> [--last N] | ls [--json] | rm <name>

panes and screen (host: zellij or plain; config key host):
  margin open <board> [--float|--split|--stacked] [--tab-id N] [--session S] [--while-pid PID]   beside the tab it is run from
                                --stacked: onto the tab's margin pane
                                plain host: prints the `margin view` command to run in a terminal
  margin close <board>          margin panes <board>   (where it's showing)
  margin view <board>...        the live renderer (what the pane runs); more boards are tabs (gt / gT)
  margin tab <board> [add|rm|show <tab-board>] [--pane P]   the tabs of the pane showing <board>
  margin render <board> [--width W] [--height H] [--section ID] [--focus ID] [--ansi]   one frame, no pane
  margin select [TEXT]          Zellij copy_command target (stdin if no TEXT)
  margin session [NAME]         start a Zellij session wired for margin (copy_command)
  margin pin [--show]           pin this build: panes follow <home>/bin/margin, not the newest build
  margin demo                   write a sample plan board named plan

reference:
  margin example [<name> [--script|--data]]   complete dashboards to copy from: board, fetch script, sample data
  margin help [<command>|<topic>]

parked (not part of the documented set; may be removed):
  margin image <board> <id> <png> [--alt A] [--rows N]                  a PNG in the pane
  margin demo live [--speed X]                                          plays a turn into the demo board

global, before or after the command: --home DIR  --project NAME  --by you|NAME
margin <command> --help for each

Claude Code (one consumer of the above; margin claude --help):
  margin claude init [--user-name NAME] [--claude-dir DIR] [--dry-run]   install the skill and the hooks: once per machine
  margin claude skill | link [<board>] | unlink | unread | stop [<board>] | propose <board> <id> <verb> | hook <event>
";

/// `margin claude --help`: the group's own listing.
const CLAUDE_HELP: &str = "margin claude: the Claude Code adapter
  Hooks hand a Claude session what sulin decides in the pane and tell the pane what the session is
  doing; the skill teaches a session to keep a board. Nothing in the core needs it: boards, events,
  handlers, owners, datasets and the pane work without a session (margin --help).
  margin claude init [--user-name NAME] [--claude-dir DIR] [--bin PATH] [--dry-run]
                                 install the skill and the hooks: one command per machine
  margin claude skill            the skill a session loads, rendered for this machine
  margin claude link [<board>]   the board this session keeps: show it, or set it
  margin claude unlink           drop this session's link
  margin claude unread [--session S] [--json]   sulin's decisions this session has not been handed yet
  margin claude stop [<board>]   refuse the linked session's tool calls until sulin's next message
  margin claude propose <board> <id> <verb> [value] [--why W]   offer a change to one of sulin's decisions
  margin claude hook start|prompt|pre-tool|post-tool|stop|permission|notify|end   a Claude Code hook (stdin JSON)
  margin claude hook rewake [--timeout 58m]     the Stop hook that waits: exit 2 wakes an idle session
  margin claude <subcommand> --help for each. A write made from inside Claude Code ($CLAUDECODE or
  $CLAUDE_CODE_ENTRYPOINT set) is logged as by claude unless --by says otherwise.
";

/// The subcommands of `margin claude`. Each is a `Cmd` of the same name; the spellings from
/// before the group (`margin hook …`, `margin init`) reach the same `Cmd` at the top level,
/// unlisted, for one release (settings.json files written by an earlier `init` hold them).
const CLAUDE_CMDS: &[&str] = &["init", "skill", "link", "unlink", "unread", "stop", "propose", "hook"];
/// Group subcommands that were never top-level commands: refused there.
const CLAUDE_ONLY: &[&str] = &["unread"];

/// One subcommand: its help, the flags it takes and how many positionals. Every flag outside
/// `flags` (and the global ones) is refused before the command runs, so a typo or a `--help`
/// never starts a session, a daemon or a write (2026-11-06: `margin session --help` started
/// Zellij, `daemon --help` a daemon, `select --help` copied an empty selection).
struct Cmd {
    name: &'static str,
    help: &'static str,
    flags: &'static [&'static str],
    /// most positionals it takes (None: free text, like `select TEXT` or `ask`'s options)
    max_pos: Option<usize>,
}

/// Flags every command takes: the store (`--home DIR`, `--project NAME`) and who a write is
/// logged as (`--by`).
const GLOBAL_FLAGS: &[&str] = &["--home", "--project", "--by"];

const GLOBAL_HELP: &str = "
global flags (before or after the command):
  --home DIR       the store root (default $MARGIN_HOME, else ~/.local/state/margin)
  --project NAME   the project inside it (default $MARGIN_PROJECT, else from the working directory)
  --by WHO         who a write is logged as: you, or a name (default: you; a consumer's adapter
                   sets its own name for writes made from inside its session)
";

const CMDS: &[Cmd] = &[
    Cmd { name: "apply", help: "margin apply <board> [--base-rev N] [--json] < input\n  state from a script: update a board that exists. stdin is the line grammar a handler prints (margin help handlers), applied all-or-nothing\n    {\"op\":\"set\",\"id\":\"t1\",\"fields\":{\"status\":\"done\"}}       an op (add set rm move title replace)\n    [ {op}, {op} ]                                          an op array\n    {\"type\":\"todo\",\"id\":\"t1\",\"text\":\"…\",\"parent\":\"steps\"}  a component: upsert by id\n    {\"v\":1,\"title\":\"…\",\"blocks\":[…]}                        a whole document (replaces the one there)\n  several lines are one write. Components upsert by id (a script prints \"this is the state now\"); the\n  board itself must exist: a missing one is an error naming margin new, never created\n  --json prints {\"v\":1,\"artifact\",\"rev\"}; field names and values: margin schema\n  --as OWNER   write as a registered owner (margin owner add): only inside its id prefix, ops and\n               components only, and its last-write time moves even when nothing changed\n", flags: &["--base-rev", "--json", "--title", "--kind", "--as"], max_pos: Some(1) },
    Cmd { name: "new", help: "margin new <board> [--title T] [--purpose P] [--kind K] [--json] [< doc.json]\n  create: the one command that makes a board. With a document on stdin ({\"title\", \"purpose\",\n  \"hints\", \"blocks\"}, or a bare array of blocks) it starts with that content; with a terminal, an\n  empty or a closed stdin it starts empty, titled <board> unless --title says otherwise; it never\n  waits on a stdin that stays open and silent (after 500 ms it starts empty and says so). A board that exists\n  is refused: change it with margin apply/add/set, or remove it with margin rm <board> --yes. Every\n  other writing command refuses a board that does not exist, so a mistyped name is an error\n  (with the nearest name), not a second board\n    margin new plan --title 'Fix flaky CI' --purpose 'test_sync passes 200 runs'\n    margin example reviews | margin new reviews\n", flags: &["--title", "--purpose", "--kind", "--json"], max_pos: Some(1) },
    Cmd { name: "schema", help: "margin schema [type|document] [--json]\n  the document's own fields (title, purpose, hints, kind, blocks), then the component types:\n  each field, its type, its allowed values and what it is (what every write is checked against)\n", flags: &["--json"], max_pos: Some(1) },
    Cmd { name: "add", help: "margin add <board> <parent|-> <type> <id> [text] [k=v ...] [--json '{...}'] [--before X|--after X] [--base-rev N]\n  create: one component under <parent> (- = top level). An id that exists is refused (change it\n  with margin set); the board must exist (margin new)\n", flags: &["--json", "--before", "--after", "--base-rev"], max_pos: None },
    Cmd { name: "set", help: "margin set <board> <id> <field> <value> [--base-rev N]\nmargin set <board> <id> <field>=<value>... [--base-rev N]\n  change: set fields of a component that exists (a missing id is refused; create it with margin add); text fields keep the value as typed, others read it as JSON (margin schema);\n  an empty value (field=) removes the field\n", flags: &["--base-rev", "--json"], max_pos: None },
    Cmd { name: "rm", help: "margin rm <board> <id> [--base-rev N] [--json]\n  change: remove a component that exists, and its children\nmargin rm <board> [--yes]\n  remove a whole board. Without --yes it prints what would go (title, rev, the panes\n  showing it) and removes nothing. With --yes the board moves to\n  the project's .archive/ directory (the store never deletes; margin gc archives the same way,\n  and moving the directory back restores it). Panes are left\n  open: margin close <board>\n", flags: &["--base-rev", "--json", "--yes"], max_pos: Some(2) },
    Cmd { name: "move", help: "margin move <board> <id> --before X|--after X|--parent P [--base-rev N] [--json]\n  change: move a component that exists\n", flags: &["--before", "--after", "--parent", "--base-rev", "--json"], max_pos: Some(2) },
    Cmd { name: "title", help: "margin title <board> <title> [--base-rev N]\n  change: rename a board that exists (its header line)\n", flags: &["--base-rev"], max_pos: Some(2) },
    Cmd { name: "hints", help: "margin hints <board> [text...] [--json]\n  change: the reminders of a board that exists, a line at the bottom of the pane while it is on screen (in a pane with\n  tabs, the tab on screen's). Each text is one reminder; `key  what it does` (two spaces) draws\n  the key as a key. No text clears them. They wrap; a whole-document put keeps them unless it\n  carries its own \"hints\"\n    margin hints reviews 'r  re-request review' 'merge queue freezes Fri 3pm'\n", flags: &["--json"], max_pos: None },
    Cmd { name: "purpose", help: "margin purpose <board> [\"text\"] [--json]\n  change: what the pane of a board that exists is for, in one line: the first line of its pinned head, shown whether or not the\n  content scrolls (zh folds the head). It stands where a Goal section used to. No text clears it;\n  a whole-document put keeps it unless it carries its own \"purpose\"\n    margin purpose plan 'Get CI green: the flaky test passes 200 runs'\n", flags: &["--json"], max_pos: Some(2) },
    Cmd { name: "ask", help: "margin ask <board> <id> <question> <opt>... [--suggest S] [--parent P] [--why W] [--base-rev N]\n  create: a question card; sulin picks with a click, 1-9, or mg:pick/<id>=<opt>. There is no default:\n  --suggest marks your recommendation on the card and nothing is highlighted until they pick.\n  An id that exists is refused (change it with margin set); the board must exist (margin new)\n", flags: &["--default", "--suggest", "--parent", "--why", "--applies", "--base-rev"], max_pos: None },
    Cmd { name: "propose", help: "margin claude propose <board> <id> <verb> [value] [--why W] [--base-rev N]\n  offer a change to one of the person's decisions: tick done|todo|skipped|next, pick <opt>, strike,\n  approve, reject. It shows on the component; their accept applies it as their decision\n", flags: &["--why", "--base-rev"], max_pos: Some(4) },
    Cmd { name: "diff", help: "margin diff <board> <id> --old A --new B [--parent P] [--path LABEL]\nmargin diff <board> <id> --rev R [--to R2] <file> [--parent P]      jj revisions\n  one file's change as a structural diff block the user approves, rejects or asks about;\n  replaces <id> in place if it exists. --path is the label in the header (default: the new\n  file's path). Narrow panes show one column with - and +, panes of 100 cells or more the two\n  files side by side; the focused block wraps its long lines and shows every row.\n    margin diff plan d1 --old before/retry.cc --new src/retry.cc --path src/retry.cc\n    margin diff plan d1 --rev @- src/retry.cc\n  margin runs difftastic for it and does not include it: $MARGIN_DIFFT, else difft on PATH,\n  else the config key difft, else the path a nix build compiled in. margin help diffs\n", flags: &["--old", "--new", "--parent", "--path", "--rev", "--to", "--base-rev"], max_pos: Some(3) },
    Cmd { name: "image", help: "margin image <board> <id> <png> [--alt A] [--parent P] [--rows N] [--base-rev N]\n  a PNG in the pane (kitty graphics, else half-blocks); the document stores the file's path\n  not part of the documented set; may be removed\n", flags: &["--alt", "--parent", "--rows", "--base-rev"], max_pos: Some(3) },
    Cmd { name: "show", help: "margin show <board> [--json]\n  what the person sees: content merged with their decisions\n  --json  {\"v\":1, \"doc\": the whole document, \"state\": {\"decisions\": {id: {kind, value, by, at}}}}\nmargin show <board> <id> [--json]\n  one block as JSON: {\"v\":1, \"artifact\", \"rev\", \"block\": {type, id, fields, children}, \"decision\": the\n  person's decision on it, or null}\n  exit codes, here and everywhere: 0 done, 1 refused or failed, 3 not found (no such board,\n  component, handler or owner)\n", flags: &["--json"], max_pos: Some(2) },
    Cmd { name: "ls", help: "margin ls [--json]\n  this project's boards (archives ones untouched for archive_days)\n", flags: &["--json"], max_pos: Some(0) },
    Cmd { name: "gc", help: "margin gc [--days N]\n  archive boards untouched for N days (default: config archive_days), and remove the cursors of\n  sessions not seen for that long (never a named cursor: a tool's or a handler's)\n", flags: &["--days"], max_pos: Some(0) },
    Cmd { name: "events", help: EVENTS_HELP, flags: &["--unread", "--session", "--board", "--artifact", "--since", "--kind", "--name", "--all", "--json", "--follow", "--cursor", "--no-commit", "--commit"], max_pos: Some(0) },
    Cmd { name: "emit", help: "margin emit <name> [JSON | -] [--board B] [--id ID]\n  append a named event to the project's log and print its seq. Anything can call it: cron, CI,\n  a script, a job. <name> is letters, digits and . _ - : (deploy.finished, ci.red); the value is one\n  JSON value (- reads it from stdin; text that is not JSON is an error; none: null)\n  --board B   the board it is about (must exist); --id ID a block in it\n  The event is logged as by you (or --by NAME), via emit. margin events --name 'deploy.*' reads\n  them, and a job registered with --on 'deploy.*' is run for them (margin help jobs)\n  The project comes from the working directory: from cron or another directory pass --project NAME\n    margin emit deploy.finished '{\"sha\":\"abc123\",\"ok\":true}' --project boards\n    jq -c .summary report.json | margin emit ci.report - --board ci\n", flags: &["--board", "--id"], max_pos: Some(2) },
    Cmd { name: "act", help: "margin act <board> <id> <action> [VALUE]\n  what an action's key does in the pane, from the command line: append the event\n  <board>.<action> with id <id> and a snapshot of that block (a table row's cells, target and\n  detail; any other block's fields), and print its seq. <id> - is the board itself. VALUE (JSON,\n  or text) rides along as value.arg. A handler registered with --on '<board>.<action>' handles it\n  (margin help handlers). Works on rows someone else owns: it changes no content\n  refused, with exit 1 and the reason: a missing board or id, an action the block does not offer\n    margin act reviews pr-4812 open\n", flags: &["--json"], max_pos: Some(4) },
    Cmd { name: "hook", help: "margin claude hook start|prompt|pre-tool|post-tool|stop|permission|notify|end\n  Claude Code hook: reads the hook JSON on stdin; prints sulin's new decisions, the linked pane's\n  one-line state (start, prompt) and a nudge when the pane has gone stale (post-tool)\nmargin claude hook rewake [--timeout 58m]\n  the Stop hook that waits (\"asyncRewake\": true): blocks until sulin is done in the pane, then\n  prints his decisions on stderr and exits 2, which wakes an idle Claude; exits 0 when a newer turn\n  starts, the session has no pane, or the timeout passes\n", flags: &["--timeout"], max_pos: Some(1) },
    Cmd { name: "doc", help: "margin doc <board> <id> [--title T] [--parent P] < notes.md\n  long-form beside the chat: markdown becomes one section <id>, replaced in place on every\n  write. Ids follow the content, so a revision marks only what changed. What it reads:\n    # heading        the first names the section (unless --title); deeper ones nest as sections,\n                     and one more than four levels in is a bold line\n    paragraph        text; two trailing spaces or a backslash break the line\n    - item, 1. item  bullets, nested by indent; a number is kept; - [ ] and - [x] draw a box\n    > quote          dim italic text behind a bar, one bar a level\n    ``` or ~~~       a code block, the first word after the fence its language: colored when\n                     it names one (cpp python json jsonnet starlark rust sh toml yaml\n                     markdown diff and more; an extension or a file name works too: cc,\n                     libsonnet, BUILD, defs.bzl), plain otherwise; a line too long is cut\n                     and wraps while the block has the focus\n    | a | b |        a table with rows (alignment from |:--|--:|, numbers to the right); rows\n                     of unequal length stay a code block\n    ---              a rule\n    inline           **bold** *italic* _italic_ ~~struck~~ `code` [label](url) <url> and a bare\n                     https:// URL; [label][ref] with its [ref]: url line; ![alt](path) shows\n                     alt (path); HTML and footnotes stay as written\n  links: a paragraph or bullet with one link opens it (enter or a click: an open event, like\n  any target); with several, each is a link row under it; a table row opens its one link.\n  The focused block shows the URL.\n", flags: &["--title", "--parent", "--base-rev", "--json"], max_pos: Some(2) },
    Cmd { name: "toast", help: "margin toast <board> <text> [--at ID] [--level ok|info|warn|error] [--ttl SECS]\n  a short message that goes away by itself: on the row --at names (drawn on its right, nothing\n  moves), else floating over the pane's bottom. Its time starts once the pane is seen; an error\n  stays until clicked. Stored beside the doc: no revision.\n", flags: &["--at", "--level", "--ttl", "--countdown", "--border"], max_pos: None },
    Cmd { name: "stop", help: "margin claude stop [<board>]\n  the stop signal: Claude's tool calls in this project are refused until sulin next sends a\n  message (the pane's S key does the same)\n", flags: &[], max_pos: Some(1) },
    Cmd { name: "link", help: "margin claude link [<board>] [--session S]\n  the board this Claude session keeps (its pane): show it, or set it (explicit beats the\n  automatic links made by open and at session start). The prompt hook then carries one line\n  with the pane's state, and the live strip shows only this session.\nmargin claude unlink [--session S]\n", flags: &["--session"], max_pos: Some(1) },
    Cmd { name: "unlink", help: "margin claude unlink [--session S]\n  drop this session's link\n", flags: &["--session"], max_pos: Some(0) },
    Cmd { name: "unread", help: "margin claude unread [--session S] [--json]\n  sulin's decisions in the pane that a Claude session has not been handed yet, one line each\n  (--json: one event per line, as margin events --json prints them). The hooks hand them over\n  at the next prompt or tool call and move the session's cursor; this reads without moving it\n  --session S  that session's (default: the one this runs in)\n", flags: &["--session", "--json"], max_pos: Some(0) },
    Cmd { name: "open", help: "margin open <board> [--tabs B,C] [--float|--split|--stacked] [--tab-id N] [--session S]\n            [--while-pid PID]\n  show the board in a pane beside the tab this is run from (Zellij: a split or a pinned float\n  there; focus stays where it was)\n  --tabs B,C: with those boards as tabs beside it (added to the pane if it is already open);\n  PROJECT:B is a board of another project (margin help tab)\n  --stacked: onto the tab's margin pane\n  a board already on show as a tab of a pane in this tab is not opened again\n  plain host: prints the view command to run in a terminal\n  --while-pid PID: the pane it opens closes itself when that process ends (it is passed to the\n  pane's margin view; a pane already open is left as it is)\n", flags: &["--float", "--split", "--stacked", "--tab-id", "--session", "--tabs", "--while-pid"], max_pos: Some(1) },
    Cmd { name: "tab", help: "margin tab <board> [--pane P] [--session S]\n  the tabs of the pane whose own board is <board>\nmargin tab <board> add|rm|show <tab-board> [--from PROJECT] [--pane P] [--session S]\n  add a board as a tab, remove one, or bring one on screen. The pane follows without a\n  restart: the set lives in its view file. --pane names the pane (zj-12, tty) when it can't be\n  found by its title. In the pane: click a tab, gt / gT, or 3gt for the third\n  --from PROJECT: <tab-board> is a board of another project in the same store (a name as\n  --project takes it); PROJECT:<tab-board> says the same. The list shows such a tab as\n  PROJECT:board, and rm / show also take its bare board name while only one tab has it.\n  The pane reads and watches that board where it is: keys and clicks on it write its\n  project's state and event log, so that project's handlers and daemon act on them. A board\n  or project that is not there shows as a tab saying so, and fills in when it appears\n    margin tab tracker add reviews --from home-me-work-api\n", flags: &["--pane", "--session", "--from"], max_pos: Some(3) },
    Cmd { name: "close", help: "margin close <board> [--session S]\n  close the panes showing it\n", flags: &["--session"], max_pos: Some(1) },
    Cmd { name: "panes", help: "margin panes <board> [--session S]\n  where it's showing (pane ids, or pids under the plain host)\n", flags: &["--session"], max_pos: Some(1) },
    Cmd { name: "view", help: "margin view <board> [<board>...] [--while-pid PID]\n  the live renderer (what a margin pane runs). More boards show as tabs on the title line\n  (PROJECT:board: a board of another project)\n  (click, gt / gT, 3gt); with one board the pane shows the tabs its view file remembers\n  --while-pid PID: the pane exits as q does (its view saved) when that process ends; with a PID\n  that is not running it prints one line and exits\n", flags: &["--while-pid"], max_pos: None },
    Cmd { name: "render", help: RENDER_HELP, flags: &["--width", "--height", "--ansi", "--focused", "--at", "--pane", "--motion", "--tab", "--detail", "--focus", "--section"], max_pos: None },
    Cmd { name: "select", help: "margin select [TEXT]\n  Zellij copy_command target: applies an mg: token, opens a link, or copies (stdin if no TEXT)\n  With the config key select_command set (or $MARGIN_SELECT_COMMAND), text that is not an mg:\n  token goes to that shell command on stdin (sh -c) instead: margin then opens no link and\n  copies nothing. The command's output is discarded; a failure is written to select.log in\n  the store. A selection dragged in the pane goes the same way\n", flags: &[], max_pos: None },
    Cmd { name: "pin", help: PIN_HELP, flags: &["--show"], max_pos: Some(0) },
    Cmd { name: "session", help: "margin session [NAME]\n  start (or attach) a Zellij session wired for margin (copy_command); default name margin\n", flags: &[], max_pos: Some(1) },
    Cmd { name: "daemon", help: DAEMON_HELP, flags: &["--stay", "--unit-dir", "--json"], max_pos: Some(1) },
    Cmd { name: "doctor", help: "margin doctor [--fix] [--days N]\n  what was left behind by a session, a pane or a daemon that went without saying so, and what on\n  a board cannot work yet. Lists, and removes nothing without --fix:\n    stale  rewake and review marks whose process is gone; the cursor, activity entry and pane view\n           of a session or pane untouched for N days (default: config archive_days, else 30); the\n           pid file of a daemon that is not running; a handler recorded as running when none is\n    note   a board whose rows have targets or actions that no handler takes (with the line that\n           registers one); history.jsonl files, which margin no longer writes (delete them by hand)\n           each feed a store still lists in feeds.json (margin no longer runs commands on a timer, a\n           file watch or as a stream): the handler and schedule line, or the owner line, that\n           replaces it, and who owns its rows now\n    stale  the removed feed runner's files (feeds.json, feeds/*.log, feeds/*.status.json): --fix\n           moves them into the project's .archive/ directory instead of deleting them\n  --fix removes the stale items. It never touches a board, an event, a named cursor (a tool's or a\n  handler's) or a registration, and it is safe to run at any time. margin help lifecycle\n", flags: &["--fix", "--days"], max_pos: Some(0) },
    Cmd { name: "handler", help: HANDLER_HELP, flags: &[], max_pos: None },
    Cmd { name: "handle", help: "margin handle open\n  a ready-made handler command: reads events on stdin (one JSON object per line) and opens each\n  event's value.target: a URL with the configured opener (config key opener; default xdg-open, or\n  `opener` over ssh), an mg: token by applying it, mg:open/<board> by showing that board.\n  With $MARGIN_DRY_OPEN=FILE it appends each target to FILE and opens nothing. A target that fails\n  to open is reported on stderr and the run still exits 0 (a retry would open the others twice)\n  The pane never opens anything itself: enter, or a double click on a table row (one click on a\n  link or status row), appends the event <board>.open for a row with a target, and this is the handler for it. Register it once per project:\n    margin handler add open --on '*.open' -- margin handle open\n", flags: &[], max_pos: Some(1) },
    Cmd { name: "data", help: DATA_HELP, flags: &["--cap", "--file", "--owner", "--expect-every", "--as", "--last", "--json"], max_pos: Some(3) },
    Cmd { name: "owner", help: OWNER_HELP, flags: &["--board", "--owns", "--expect-every", "--json"], max_pos: Some(2) },
    Cmd { name: "demo", help: "margin demo [live [--speed X]]\n  create the demo board `plan` (run again, it resets it; a `plan` that is not the demo's is\n  refused)\nmargin demo live [--speed X]\n  plays a turn into the demo board: steps tick, a card resolves\n  live: not part of the documented set; may be removed\n", flags: &["--speed"], max_pos: Some(1) },
    Cmd { name: "init", help: "margin claude init [--user-name NAME] [--claude-dir DIR] [--bin PATH] [--dry-run]\n  make this machine ready: write user_name to margin's config (when given), install the skill\n  to <claude-dir>/skills/margin/SKILL.md, and merge the margin hooks into <claude-dir>/settings.json\n  with this binary's absolute path. Safe to re-run: it updates its own entries in place (an entry\n  written as `margin hook …` by an earlier version becomes `margin claude hook …`), leaves\n  every other hook and key alone, and keeps settings.json.bak from before its first change.\n  --claude-dir  Claude Code's directory (default ~/.claude)\n  --bin         the binary the hooks run (default: this one)\n  --dry-run     print the changes, write nothing\n", flags: &["--user-name", "--claude-dir", "--bin", "--dry-run"], max_pos: Some(0) },
    Cmd { name: "example", help: "margin example\n  list the bundled example dashboards\nmargin example <name> [--script | --data | --tool]\n  print one: the board (a whole document: margin example reviews | margin new reviews), with\n  the steps to make it live on stderr; --script: the script that fetches its rows (a handler runs it); --data: a sample\n  of the data that script reads. margin help dashboards explains the parts\n", flags: &["--script", "--data", "--tool"], max_pos: Some(1) },
    Cmd { name: "skill", help: "margin claude skill\n  print the skill Claude loads, rendered with the configured user_name and this binary's path\n  (margin claude init installs it; margin help <topic> has the reference it points to)\n", flags: &[], max_pos: Some(0) },
];

const PIN_HELP: &str = "margin pin [--show]
  pin the build this is run from: copy this binary to <home>/bin/margin-<hash> and point the link
  <home>/bin/margin at it (<home>: --home, $MARGIN_HOME, else ~/.local/state/margin). Prints the
  copy's path. The link is replaced in one rename; the copy pinned before is kept, older ones go
  While a pin exists, panes and the daemon follow it and not the newest build: margin open starts panes with
  <home>/bin/margin, and a running pane re-execs it in place when the pin moves. A rebuild
  (cargo build) restarts no pane until you run margin pin from the new build. With no pin a
  pane follows the binary it was started from, as before. The daemon moves to the pin after
  the handler runs in flight finish (margin daemon status says which binary it runs)
  --show  print what is pinned (the link and the copy it names), or: nothing pinned
  margin doctor lists the running panes and daemons and whether each runs the pin
";

const EVENTS_HELP: &str = "margin events [--since N | --cursor NAME [--no-commit]] [--name GLOB] [--kind K] [--board B]
              [--follow] [--all] [--json]
margin events --cursor NAME --commit SEQ
  the project's event log. Without a filter: sulin's decisions and actions, in local time (with
  the date when not today)
  --name GLOB    events whose kind matches (quote it: 'reviews.*'), whoever made them
  --kind K       one kind, exactly
  --all          everyone's events and the passive kinds too (view, seen, fold...)
  --board B      one board's
  --json         one JSON object per line: {\"v\", \"seq\", \"at\", \"artifact\" (the board's name), \"id\", \"kind\", \"value\", \"by\", \"via\", \"rev\"}
  --since N      the events after seq N (--since 0: everything still kept)
  --follow       keep running and print each new event as it arrives; ctrl-c or a closed
                 stdout ends it with exit 0
  --cursor NAME  a named position in the log (letters, digits, - and _). Start after it, and
                 move it past each event as that event is printed. A cursor used for the first
                 time starts at the END of the log: it hears what happens from now on. With
                 --since N the read starts after N instead (--since 0 replays), and the cursor
                 then moves as usual
  --no-commit    read from the cursor without moving it
  --commit SEQ   move the cursor to SEQ and print nothing else
  Delivery: a cursor that moves as events are printed is at-most-once in a pipeline (the event
  counts as taken once it is written to the pipe; if the reader dies before acting on it, it is
  not printed again). For at-least-once, read with --no-commit and commit each event after
  handling it; the handler must then be safe to run twice on one event (dedupe on seq):
    margin events --follow --cursor mine --json | while read -r e; do handle \"$e\"; done
    margin events --follow --cursor mine --no-commit --json | while read -r e; do
      handle \"$e\" && margin events --cursor mine --commit \"$(jq .seq <<<\"$e\")\"; done
  A handler does the second for you (margin help handlers). The log rotates past a few MB and
  keeps two older generations; seq goes on counting, so a cursor stays valid across a rotation
";

const DAEMON_HELP: &str = "margin daemon [--stay]
  the supervisor for this project, in the foreground: it dispatches appended events to the
  registered handlers, and nothing else. You rarely start it: registering a handler,
  or appending an event while one is registered, starts it in the background. With nothing
  registered it exits (--stay keeps it up). SIGTERM stops it cleanly: running handlers are
  stopped (not failed), their events stay pending, and the next daemon gives them again
margin daemon status [--json]
  is it running, since when and from which binary (and whether that is the pinned build); each
  handler's state, last run, events still waiting and last error; and how it comes back after a
  reboot (the unit, or the next command that appends an event)
  With a build pinned (margin pin) the daemon follows the pin as panes do: when the pin moves
  it lets the handler runs in flight finish, then re-execs <home>/bin/margin with the same
  arguments; handlers resume from their cursors
margin daemon install [--unit-dir DIR]
  write a systemd user unit for this project's daemon (restart on failure, started at login) and
  print the systemctl line that enables it; it runs nothing itself
  --unit-dir DIR   where the unit goes (default $XDG_CONFIG_HOME/systemd/user)
margin daemon uninstall [--unit-dir DIR]
  remove that unit file and print the systemctl line that stops it
  margin help lifecycle says what survives a reboot with and without the unit
";

const HANDLER_HELP: &str = "handlers: commands the daemon runs when an event they asked for is appended
  margin handler add <name> --on <glob> [--on <glob>]... [--board B --owns PREFIX]
                     [--timeout 60s] [--retries 3] -- <cmd...>
  margin handler set <name> [the same flags] [-- <cmd...>]
  margin handler ls [--json]
  margin handler rm <name>     margin handler pause <name>     margin handler resume <name>
  margin handler logs <name> [--follow] [--lines N]
  margin handler run <name> [--events FILE] [--dry-run]
  margin handler <subcommand> --help for each; margin help handlers has the contract
";

fn handler_sub_help(sub: &str) -> Option<&'static str> {
    Some(match sub {
        "add" | "set" => "margin handler add <name> --on <glob> [--on <glob>]... [--board B --owns PREFIX]
                   [--timeout 60s] [--retries 3] -- <cmd...>
  create: register a command for the events whose name matches a glob. A name that exists is
  refused (change it with margin handler set)
margin handler set <name> [--on <glob>...] [--board B --owns PREFIX | --board none] [--timeout T] [--retries N] [-- <cmd...>]
  change: only what you pass changes (--on replaces the whole list); a missing name is refused
  --on GLOB       an event name or a glob of one, quoted: reviews.open, '*.open', 'ci.*'. Never
                  matches the handler's own emits; handler.* events only via a glob starting
                  with handler.; the pane's passive kinds (seen, view, fold) only by exact name;
                  a board's view events (shown, hidden, detail) also via 'BOARD.*'
  --board B --owns PREFIX   the board its stdout writes and the id prefix it owns there (PREFIX
                  and PREFIX-...; * = all). The board must exist. Without them it writes no board
  --timeout 60s   a run is stopped after this long, and counts as failed
  --retries 3     a failed run is retried this many times (1 s, 2 s, 4 s ... apart); then the
                  events it was given are passed over and a handler.failed event is appended
                  (0: never retry)
  -- <cmd...>     the command: run with no shell, in the directory you are in now, with the PATH
                  you have now. A first word of exactly `margin` runs this binary
  The command gets the pending matching events on stdin, one JSON object per line, and prints
  JSON lines; on exit 0 they are applied and its cursor moves. margin help handlers has it all
",
        "ls" => "margin handler ls [--json]
  each handler: its state (live, retrying, paused, stopped), the events it takes, its board,
  how many events wait for it, and its last run (`ran 2s ago · ok`, `failed 5s ago: <stderr>`)
  --json  {v, daemon: {running, pid}, handlers: [{name, on, cmd, board, owns, paused, state,
          cursor, pending, attempts, last_run, last_outcome, last_error}]}
",
        "rm" => "margin handler rm <name>
  unregister a handler and forget its cursor. What it wrote stays on its board, unowned
",
        "pause" | "resume" => "margin handler pause <name>
margin handler resume <name>
  stop dispatching to a handler without unregistering it (events wait at its cursor), or start again
",
        "logs" => "margin handler logs <name> [--follow] [--lines N]
  a line per run (`run: ok (1 event to seq 12, 1 line, wrote)`, `run: failed: …; retry 1 of 3 in 1s`),
  its stderr, rejected lines
",
        "run" => "margin handler run <name> [--events FILE] [--dry-run]
  run a handler once, in the foreground, as the handler
  --events FILE  give it these events (JSON lines, as margin events --json prints them) instead
                 of the ones waiting at its cursor; its cursor is then left alone
  --dry-run      print what it would commit (ops, components, emits, the cursor move) and write nothing
",
        _ => return None,
    })
}

const OWNER_HELP: &str = "owners: programs margin does not run, registered as the writers of rows on a board
  margin owner add <name> --board B --owns PREFIX [--expect-every 10m]
      create: <name> owns the ids PREFIX and PREFIX-... on B (* = the whole board). The board must
      exist; a name that exists is refused. From then on only `margin apply B --as <name>` writes
      those ids, and anyone else's write there is refused, as with a handler's rows
  margin owner set <name> [--board B] [--owns PREFIX] [--expect-every 10m|none]
      change: only what you pass changes; a missing name is refused
  margin owner ls [--json]
      each owner: its board and prefix, when it last wrote, and whether that is overdue
  margin owner rm <name>
      unregister it; its rows stay on the board, unowned
  --expect-every 10m   how often it means to write. Once its last write is older than that its
      rows show as stale in the pane (`last updated 3h ago, expected every 10m`), the board's tab
      gets the failure mark, and owner ls says so. margin runs nothing and restarts nothing: how
      long ago it wrote is the one thing margin knows about a program it does not run
  The program writes as often as it likes: `... | margin apply B --as <name>` (ops and components,
  all or nothing; a write that changes nothing still counts as a write). margin help integrate
";

const DATA_HELP: &str = "datasets: values a board displays, kept outside the document and read by the pane when it draws
  margin data add <name> [--cap N] [--file PATH] [--owner OWNER] [--expect-every 10s]
      create: register a dataset in this project. A name that exists is refused
  margin data set <name> [--cap N|none] [--file PATH|none] [--owner OWNER|none] [--expect-every 10s|none]
      change: only what you pass changes; a missing name is refused
  margin data append <name> <VALUE | JSON | -> [--as OWNER]
      add one record at the end: a number (71.4) or a JSON object ('{\"t\":\"2026-11-10T19:40:00Z\",\"rps\":71.4}').
      - reads many from stdin, one per line. One write, so a reader sees a record whole or not at all
  margin data put <name> [--as OWNER] < records
      replace the content: a JSON array of records, or JSON lines, on stdin (empty: no records)
  margin data show <name> [--last N] [--json]
      the newest records (default 10), read from the end of the file; --json: {v, name, path,
      records, bytes (the file's size), bytes_read (how much of it this read took), ...}
  margin data ls [--json]
      each dataset: where it is, its size, when it last changed, and STALE when that is overdue
  margin data rm <name>
      unregister it. Records margin holds move to the project's .archive/; an external file stays
  A record is a JSON object or a bare number (a dataset of bare numbers is one unnamed series).
  Writes go to a registered name only: a missing one is an error naming margin data add
  --cap N            keep the newest N records: once the file holds more than 2N it is rewritten to N
  --file PATH        an external file its provider already writes: JSON lines, or CSV with a header
                     row (by the .csv extension). margin reads it in place and never writes it, so
                     put and append are refused. A relative path is resolved here and stored absolute
  --owner OWNER      only that registered owner (margin owner add) may write, with --as OWNER
  --expect-every 10s how often it means to change. Once its last change is older than that, the
                     components bound to it show as stale in the pane and data ls says so
  A board binds to a dataset by name: a plot (\"data\": name), a table (\"rows\": name), a run strip
  (\"data\": name) and ~[@name] in text. margin schema plot|table|runs and margin help dashboards
  (\"Numbers that change often\") have the fields. A dataset changing is not the board changing: no
  revision, no change bar, no unseen count
";

const RENDER_HELP: &str = "margin render <board> [<board>...] [--width W] [--height H] [--section ID]... [--focus ID] [--focused]
              [--detail] [--tab ART] [--ansi] [--at SECS] [--motion on|off] [--pane P]
  one frame to stdout, with no pane and no terminal: what the pane would draw
  --width W      the frame's width in cells (default 62)
  --height H     its height in rows (default 40)
  --section ID   show this section of a `tabs` block instead of its first (repeat it for
                 nested or several tabs blocks); an id that is not such a section is an error
  --focus ID     put the focus on a block; a block inside a hidden section of a `tabs` block
                 brings that section on show. An id the board doesn't have is an error
  --focused      draw as the pane that has the keyboard: the focus ring, the focused row in full
  --detail       open the detail panel (the focused row's `detail` and fields)
  --tab BOARD    with several boards (a tab bar on the title line): the one on screen
  --ansi         ANSI escapes with colors and attributes, for a terminal; without it plain
                 text, where Nerd Font icons are replaced by ordinary characters (a check, a cross)
  --at SECS      the animation clock, seconds since the pane started (default 10: all settled)
  --motion on|off  with or without animation (default: the config's)
  --pane P       draw with pane P's saved view: folds, focus, scroll (default tty)
";

fn wants_help(v: &[String]) -> bool {
    v.iter().any(|x| x == "-h" || x == "--help")
}

/// Refuse flags a command doesn't take and surplus positionals, before it acts.
fn check_args(name: &str, a: &Args, flags: &[&str], max_pos: Option<usize>) -> Result<(), String> {
    if let Some((f, _)) = a.flags.iter().find(|(f, _)| !flags.contains(&f.as_str()) && !GLOBAL_FLAGS.contains(&f.as_str())) {
        return Err(format!("unknown flag {f}; margin {name} --help"));
    }
    if let Some(m) = max_pos {
        if a.pos.len() > m {
            return Err(format!("unexpected argument {:?}; margin {name} --help", a.pos[m]));
        }
    }
    Ok(())
}

/// Who a write is logged as: `--by`, else the adapter's name inside its session (`claude::here`),
/// else the person (`who::HUMAN`). `you` (or the old
/// `sulin`) is stored as `who::HUMAN`, so the pane and `margin show` treat it as theirs.
fn author(a: &Args) -> Result<String, String> {
    match a.get("--by") {
        Some("you") | Some("sulin") => Ok(crate::who::HUMAN.into()),
        Some(n) if util::valid_id(n) => Ok(n.into()),
        Some(n) => Err(format!("bad --by {n:?}: you, or a name (letters, digits, - and _)")),
        None if a.has("--by") => Err("--by needs a value: you, or a name".into()),
        None => Ok(if claude::here() { claude::AUTHOR } else { crate::who::HUMAN }.into()),
    }
}

/// `--while-pid PID` of `view` and `open`: the process the pane lives as long as.
fn while_pid(a: &Args) -> Result<Option<u32>, String> {
    match a.get("--while-pid") {
        None if a.has("--while-pid") => Err("--while-pid needs a process id".into()),
        None => Ok(None),
        Some(p) => p.parse::<u32>().ok().filter(|p| *p > 0).map(Some).ok_or_else(|| format!("--while-pid {p}: not a process id")),
    }
}

struct Args {
    pos: Vec<String>,
    flags: Vec<(String, Option<String>)>,
}

const BOOL_FLAGS: &[&str] = &[
    "--json", "--ansi", "--float", "--split", "--stacked", "--focused", "--detail", "--unread", "--all", "--brief", "--stream", "--follow", "--replace", "--dry-run", "--yes", "--no-commit", "--stay", "--fix", "--tool", "--show",
];

impl Args {
    fn parse(v: &[String]) -> Args {
        let mut pos = Vec::new();
        let mut flags = Vec::new();
        let mut i = 0;
        while i < v.len() {
            let a = &v[i];
            if a.starts_with("--") && a.len() > 2 {
                if let Some((k, val)) = a.split_once('=') {
                    flags.push((k.to_string(), Some(val.to_string())));
                } else if i + 1 >= v.len() || (BOOL_FLAGS.contains(&a.as_str()) && !(a == "--json" && v[i + 1].starts_with('{'))) {
                    // `--json` is a switch on show/events but carries fields on add: an object
                    // after it is its value
                    flags.push((a.clone(), None));
                } else {
                    flags.push((a.clone(), Some(v[i + 1].clone())));
                    i += 1;
                }
            } else {
                pos.push(a.clone());
            }
            i += 1;
        }
        Args { pos, flags }
    }
    fn get(&self, k: &str) -> Option<&str> {
        self.flags.iter().rev().find(|(f, _)| f == k).and_then(|(_, v)| v.as_deref())
    }
    /// Every value of a flag that may repeat (`--section a --section b`), in order.
    fn all(&self, k: &str) -> Vec<&str> {
        self.flags.iter().filter(|(f, _)| f == k).filter_map(|(_, v)| v.as_deref()).collect()
    }
    fn has(&self, k: &str) -> bool {
        self.flags.iter().any(|(f, _)| f == k)
    }
    fn pos(&self, i: usize, name: &str) -> Result<&str, String> {
        self.pos.get(i).map(|s| s.as_str()).ok_or_else(|| format!("missing <{name}>"))
    }
    fn rev(&self) -> Result<Option<u64>, String> {
        self.get("--base-rev").map(|s| s.parse().map_err(|_| format!("bad --base-rev {s}"))).transpose()
    }
}

fn store(a: &Args) -> Store {
    let mut st = Store::open_cwd();
    // check_args validated --by before any command ran
    st.author = author(a).unwrap_or_else(|_| who::HUMAN.into());
    if let Some(h) = a.get("--home") {
        st.home = h.into();
    }
    if let Some(p) = a.get("--project") {
        st.project = p.into();
    }
    st
}

fn json_or_str(s: &str) -> Value {
    serde_json::from_str(s).unwrap_or_else(|_| Value::String(s.to_string()))
}

/// How long `margin new` waits for the first byte of a document on a stdin that is open and
/// silent. A pipeline's producer (`margin example reviews | margin new reviews`) starts beside
/// this process and writes a few milliseconds later, so zero would race it and drop its
/// document; a stdin nobody will ever write to (inherited from a script runner) costs this long
/// once instead of hanging the script.
const NEW_STDIN_GRACE_MS: i32 = 500;

/// The document `margin new` was given on stdin, and whether it gave up on a stdin that stayed
/// open and silent. Never blocks for longer than `grace_ms` unless a document is arriving:
/// a terminal has none to give (reading it would wait for a ctrl-d nobody knows to type); a
/// closed stdin, `/dev/null` and a pipe whose writer left without writing are empty at once; a
/// stdin with data ready (a file, a heredoc, a pipe that has started) is read to its end.
fn stdin_document(grace_ms: i32) -> (String, bool) {
    use std::io::IsTerminal;
    if std::io::stdin().is_terminal() {
        return (String::new(), false);
    }
    let mut p = libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 };
    let n = unsafe { libc::poll(&mut p, 1, grace_ms) };
    if n > 0 && p.revents & libc::POLLIN != 0 {
        return (stdin(), false);
    }
    // POLLHUP alone: the writer closed having written nothing. POLLNVAL: fd 0 is closed.
    // n == 0: someone holds the write end and says nothing
    (String::new(), n == 0)
}

fn stdin() -> String {
    let mut s = String::new();
    let _ = std::io::stdin().read_to_string(&mut s);
    s
}

/// A `set` value typed by the schema; an empty one removes the field (`note=`), since an empty
/// string stored in its place still drew as a field with nothing in it.
fn set_value(kind: &str, field: &str, raw: &str) -> Value {
    if raw.is_empty() {
        Value::Null
    } else {
        doc::coerce(kind, field, raw)
    }
}

/// An event's time for a person: local `HH:MM:SS`, with the date in front when it isn't today.
fn local_stamp(at: &str, now: f64) -> String {
    let Some(t) = util::parse_rfc3339(at) else { return at.to_string() };
    let tm = |epoch: f64| unsafe {
        let t = epoch as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        tm
    };
    let (e, n) = (tm(t), tm(now));
    let time = format!("{:02}:{:02}:{:02}", e.tm_hour, e.tm_min, e.tm_sec);
    if (e.tm_year, e.tm_yday) == (n.tm_year, n.tm_yday) {
        time
    } else {
        format!("{:04}-{:02}-{:02} {time}", e.tm_year + 1900, e.tm_mon + 1, e.tm_mday)
    }
}

fn patch(st: &Store, art: &str, ops: Vec<Op>, a: &Args) -> Result<(), String> {
    let rev = st.patch(art, &ops, a.rev()?)?;
    report(art, rev, a);
    Ok(())
}

/// Print a write's result: `art rev N`, or with `--json` `{"v":1,"artifact","rev"}`.
fn report(art: &str, rev: u64, a: &Args) {
    if a.has("--json") {
        println!("{}", json!({"v": 1, "artifact": art, "rev": rev}));
    } else {
        println!("{art} rev {rev}");
    }
}

/// What a whole-document write does with the artifact it names: `margin new` creates it and
/// refuses one that exists; a whole-document `apply` replaces one that exists and
/// refuse a missing one. No command does whichever applies: a mistyped name once made a
/// second board.
#[derive(Clone, Copy, PartialEq)]
enum Whole {
    Create,
    Replace,
}

/// `margin new <art>`: create the artifact from `input` (a whole document, a bare array of
/// blocks, or nothing: an empty artifact).
fn new_cmd(st: &Store, art: &str, input: &str, a: &Args) -> Result<(), String> {
    let t = input.trim();
    let whole_doc = match serde_json::from_str::<Value>(t) {
        Ok(Value::Array(_)) => true,
        Ok(Value::Object(m)) => m.get("op").is_none() && m.get("type").is_none(),
        _ => false,
    };
    if !t.is_empty() && !whole_doc {
        return Err(format!("stdin is not a whole document ({{\"title\", \"blocks\"}}) or an array of blocks; create it empty with `margin new {art}`, then pipe ops or components to `margin apply {art}`"));
    }
    apply_cmd(st, art, input, a, true, Whole::Create)
}

/// `margin apply`: stdin in the line grammar, or a whole-document
/// envelope `{"title", "blocks"}`. One all-or-nothing write to an artifact that exists. `whole`:
/// `margin new`'s reading, where a bare array is the block list, not an op list. `mode`: `margin new`
/// comes through here too, with `Whole::Create`.
fn apply_cmd(st: &Store, art: &str, input: &str, a: &Args, whole: bool, mode: Whole) -> Result<(), String> {
    let t = input.trim();
    let v: Option<Value> = if t.is_empty() { Some(json!({})) } else { serde_json::from_str(t).ok() };
    let envelope = match &v {
        Some(Value::Object(m)) => m.get("op").is_none() && m.get("type").is_none(),
        Some(Value::Array(_)) => whole,
        _ => false,
    };
    if envelope || (whole && v.is_none()) {
        let mut v = v.ok_or("stdin JSON: not one JSON value")?;
        if v.is_array() {
            v = json!({ "blocks": v });
        }
        if let Some(n) = v.get("v").and_then(|x| x.as_u64()) {
            if n != 1 {
                return Err(format!("envelope version {n}: this margin reads v 1"));
            }
        }
        if let Some(m) = v.as_object_mut() {
            m.remove("v");
            if let Some(k) = m.keys().find(|k| !["title", "kind", "blocks", "icon", "id", "hints", "purpose", "actions"].contains(&k.as_str())) {
                return Err(format!("unknown envelope key {k:?}: an envelope is {{\"title\", \"kind\", \"blocks\", \"hints\", \"purpose\", \"actions\"}}"));
            }
        }
        v["id"] = json!(art);
        if let Some(t) = a.get("--title") {
            v["title"] = json!(t);
        }
        if let Some(k) = a.get("--kind") {
            v["kind"] = json!(k);
        }
        if let Some(p) = a.get("--purpose") {
            v["purpose"] = json!(p.split_whitespace().collect::<Vec<_>>().join(" "));
        }
        if mode == Whole::Create && v.get("title").is_none() {
            v["title"] = json!(art);
        }
        if v.get("blocks").is_none() {
            v["blocks"] = json!([]);
        }
        // the reminders are the artifact's, not this revision's: a whole document that doesn't
        // carry its own keeps the ones there
        // (and so is its purpose)
        if v.get("hints").is_none() || v.get("purpose").is_none() || v.get("title").is_none() || v.get("actions").is_none() {
            if let Ok(old) = st.load(art) {
                // and its title: a document that names none keeps the one it has
                if v.get("title").is_none() && mode == Whole::Replace {
                    v["title"] = json!(old.title);
                }
                if v.get("hints").is_none() && !old.hints.is_empty() {
                    v["hints"] = json!(old.hints);
                }
                if v.get("purpose").is_none() && !old.purpose.is_empty() {
                    v["purpose"] = json!(old.purpose);
                }
                if v.get("actions").is_none() && !old.actions.is_null() {
                    v["actions"] = old.actions.clone();
                }
            }
        }
        let d: Doc = serde_json::from_value(v).map_err(|e| e.to_string())?;
        // which of the two it is, the store checks under its lock
        let rev = match mode {
            Whole::Create => st.create(d)?,
            Whole::Replace => st.replace(d, a.rev()?)?,
        };
        report(art, rev, a);
        return Ok(());
    }
    let ops = doc::parse_input(t)?;
    if ops.is_empty() {
        return Err("nothing to apply".into());
    }
    let rev = st.patch(art, &ops, a.rev()?)?;
    report(art, rev, a);
    Ok(())
}

/// Take the global flags written before the subcommand (`margin --home DIR apply …`) off the
/// front: what is left starts with the command, and the flags are handed back to be read with
/// the ones written after it. A global flag with no value there is an error.
fn leading_globals(argv: &[String]) -> Result<(Vec<String>, Vec<String>), String> {
    let mut globals = Vec::new();
    let mut i = 0;
    while i < argv.len() {
        let a = &argv[i];
        let name = a.split_once('=').map(|(k, _)| k).unwrap_or(a);
        if !GLOBAL_FLAGS.contains(&name) {
            break;
        }
        globals.push(a.clone());
        if !a.contains('=') {
            let v = argv.get(i + 1).ok_or_else(|| format!("{a} needs a value"))?;
            globals.push(v.clone());
            i += 1;
        }
        i += 1;
    }
    Ok((globals, argv[i..].to_vec()))
}

/// `--home` and `--project` reach every code path through the environment the store already
/// reads (`$MARGIN_HOME`, `$MARGIN_PROJECT`): `select`, the hooks and the hosts resolve their
/// store without an `Args` in hand, and `margin select --home DIR` once acted on the real store.
fn export_store_flags(a: &Args) {
    if let Some(h) = a.get("--home") {
        // absolute: a handler or a pane started from here runs in another directory
        let h = std::path::absolute(h).map(|p| p.display().to_string()).unwrap_or_else(|_| h.to_string());
        std::env::set_var("MARGIN_HOME", h);
    }
    if let Some(p) = a.get("--project") {
        std::env::set_var("MARGIN_PROJECT", p);
    }
}

/// A closed stdout (`margin ls | head -3`) ends the command quietly, as it does any Unix tool:
/// Rust starts with SIGPIPE ignored, and `println!` then panics with "failed printing to
/// stdout". The commands that live long or write into a pipe of their own keep it ignored: the
/// pane must restore the terminal on its way out, `select` falls back when a clipboard tool
/// exits early, and the daemon outlives whoever started it.
fn quiet_on_closed_stdout(cmd: &str) {
    if !matches!(cmd, "view" | "select" | "daemon" | "events") {
        unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    }
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let (globals, argv) = match leading_globals(&argv) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("margin: {e}; margin --help");
            std::process::exit(1);
        }
    };
    quiet_on_closed_stdout(argv.first().map(String::as_str).unwrap_or(""));
    export_store_flags(&Args::parse(&globals));
    // help names the person as the messages do (who.rs): the source says "sulin", `say` swaps it
    let overview = || format!("{}\n{}", who::say(HELP), help::topic_list(&help::Ctx::here()));
    if argv.is_empty() {
        print!("{}", overview());
        return;
    }
    let cmd = argv[0].clone();
    if cmd == "help" || cmd == "-h" || cmd == "--help" {
        // `margin help <cmd>`, `margin help <topic>`
        match argv.get(1).map(String::as_str) {
            None => print!("{}", overview()),
            Some("claude") => print!("{}", who::say(CLAUDE_HELP)),
            Some(n) => {
                if let Some(c) = CMDS.iter().find(|c| c.name == n) {
                    print!("{}{GLOBAL_HELP}", who::say(c.help));
                } else if let Some(t) = help::topic(n) {
                    print!("{}", help::render(t, &help::Ctx::here()));
                } else {
                    let topics: Vec<&str> = help::TOPICS.iter().map(|(n, _)| *n).collect();
                    eprintln!("margin help: no command or topic {n:?}; topics: {}", topics.join(", "));
                    std::process::exit(1);
                }
            }
        }
        return;
    }
    // `margin claude <subcommand> …`: the adapter's group. `shown` is the command as typed, for
    // messages; `cmd` the `Cmd` it runs, which the spelling from before the group reaches too
    let (cmd, argv, shown) = if cmd == "claude" {
        match argv.get(1).map(String::as_str) {
            None | Some("help") | Some("-h") | Some("--help") => {
                print!("{}", who::say(CLAUDE_HELP));
                return;
            }
            Some(sub) if CLAUDE_CMDS.contains(&sub) => (sub.to_string(), argv[1..].to_vec(), format!("claude {sub}")),
            Some(sub) => {
                eprintln!("margin claude: unknown subcommand {sub:?}; margin claude --help");
                std::process::exit(1);
            }
        }
    } else {
        (cmd.clone(), argv, cmd)
    };
    let Some(spec) = CMDS.iter().find(|c| c.name == cmd).filter(|_| shown != cmd || !CLAUDE_ONLY.contains(&cmd.as_str())) else {
        eprintln!("margin: unknown command {cmd:?}; margin --help");
        std::process::exit(1);
    };
    claude::install();
    // everything after `--` is a handler's command, untouched by flag parsing
    let (mine, rest) = match argv.iter().position(|x| x == "--") {
        Some(i) => (&argv[1..i], argv[i + 1..].to_vec()),
        None => (&argv[1..], vec![]),
    };
    if wants_help(mine) {
        // `margin handler add --help`: that subcommand's own usage
        let word = mine.iter().find(|x| !x.starts_with('-'));
        let sub = match cmd.as_str() {
            "handler" => word.and_then(|s| handler_sub_help(s)),
            _ => None,
        };
        print!("{}{GLOBAL_HELP}", who::say(sub.unwrap_or(spec.help)));
        return;
    }
    // the global flags read the same on either side of the command; one written after it wins
    let mine: Vec<String> = globals.iter().chain(mine.iter()).cloned().collect();
    let a = Args::parse(&mine);
    export_store_flags(&a);
    let r = if cmd == "handler" {
        handler_cmd(&a, rest)
    } else {
        check_args(&shown, &a, spec.flags, spec.max_pos).and_then(|_| if rest.is_empty() { Ok(()) } else { Err("`--` is only for handler add/set".into()) }).and_then(|_| run(&cmd, &a))
    };
    if let Err(e) = r {
        eprintln!("margin {shown}: {}", who::say(&e));
        std::process::exit(exit_code(&e));
    }
}

fn run(cmd: &str, a: &Args) -> Result<(), String> {
    author(a)?;
    let cfg = Config::load();
    match cmd {
        "schema" => {
            let only = a.pos.first().map(String::as_str);
            if a.has("--json") {
                println!("{}", who::say(&serde_json::to_string_pretty(&doc::schema_json(only)?).unwrap()));
            } else {
                // the schema's notes name the person as every other message does
                print!("{}", who::say(&doc::schema_text(only)?));
            }
        }
        "apply" if a.has("--as") => {
            let st = store(a);
            let (art, name) = (a.pos(0, "board")?, a.get("--as").ok_or("--as needs an owner's name (margin owner ls)")?);
            let input = stdin();
            let whole = matches!(serde_json::from_str::<Value>(input.trim()), Ok(Value::Object(m)) if m.get("op").is_none() && m.get("type").is_none());
            if whole {
                return Err(format!("--as writes ops and components inside {name}'s prefix, not a whole document"));
            }
            let ops = doc::parse_input(input.trim())?;
            if ops.is_empty() {
                return Err("nothing to apply".into());
            }
            let rev = st.apply_as(art, name, &ops)?;
            report(art, rev, a);
        }
        "apply" => {
            let st = store(a);
            apply_cmd(&st, a.pos(0, "board")?, &stdin(), a, false, Whole::Replace)?;
        }
        "new" => {
            let st = store(a);
            let board = a.pos(0, "board")?;
            let (input, silent) = stdin_document(NEW_STDIN_GRACE_MS);
            new_cmd(&st, board, &input, a)?;
            if silent {
                eprintln!("margin new: nothing arrived on stdin within {NEW_STDIN_GRACE_MS} ms, so {board} starts empty; a document that comes later goes in with `margin apply {board} < doc.json`");
            }
        }
        "add" => {
            let st = store(a);
            let art = a.pos(0, "board")?;
            let parent = a.pos(1, "parent")?;
            let kind = a.pos(2, "type")?;
            let id = a.pos(3, "id")?;
            let mut f = serde_json::Map::new();
            if let Some(j) = a.get("--json") {
                if let Value::Object(m) = json_or_str(j) {
                    f.extend(m);
                }
            }
            // bare words are the text, all of them, in order; k=v pairs are fields typed by the schema
            let mut words: Vec<&str> = Vec::new();
            for p in &a.pos[4..] {
                match p.split_once('=') {
                    // a key that names no field still goes in as one, so the schema check names it
                    Some((k, v)) if util::valid_id(k) => {
                        f.insert(k.into(), doc::coerce(kind, k, v));
                    }
                    _ => words.push(p),
                }
            }
            if !words.is_empty() {
                f.insert("text".into(), json!(words.join(" ")));
            }
            let block = Block { kind: kind.into(), id: id.into(), children: vec![], f };
            let parent = (parent != "-").then(|| parent.to_string());
            patch(&st, art, vec![Op::Add { parent, block, before: a.get("--before").map(String::from), after: a.get("--after").map(String::from) }], a)?;
        }
        "set" => {
            let st = store(a);
            let (art, id) = (a.pos(0, "board")?, a.pos(1, "id")?);
            // the value is typed by the schema: a text field keeps "2026" as text
            let k = st.load(art)?.find(id).map(|b| b.kind.clone()).ok_or_else(|| format!("no component {id:?} in {art}"))?;
            let rest = &a.pos[2..];
            let ops = if rest.first().map(|w| w.contains('=')).unwrap_or(false) {
                // set <art> <id> k=v [k=v ...]
                rest.iter()
                    .map(|p| p.split_once('=').map(|(f, v)| Op::Set { id: id.into(), field: f.into(), value: set_value(&k, f, v) }).ok_or_else(|| format!("{p:?}: write field=value")))
                    .collect::<Result<Vec<_>, _>>()?
            } else {
                let (field, val) = (a.pos(2, "field")?, a.pos(3, "value")?);
                if rest.len() > 2 {
                    return Err(format!("unexpected argument {:?}: quote a value with spaces, or use field=value pairs", rest[2]));
                }
                vec![Op::Set { id: id.into(), field: field.into(), value: set_value(&k, field, val) }]
            };
            patch(&st, art, ops, a)?;
        }
        "rm" if a.pos.len() == 1 => {
            let st = store(a);
            let art = a.pos(0, "board")?;
            let panes = host::pick(&cfg, None).find(&st, art).unwrap_or_default();
            print!("{}", rm_artifact(&st, art, a.has("--yes"), &panes)?);
        }
        "rm" => {
            if a.has("--yes") {
                return Err("--yes is for removing a whole board: margin rm <board> --yes".into());
            }
            let st = store(a);
            patch(&st, a.pos(0, "board")?, vec![Op::Rm { id: a.pos(1, "id")?.into() }], a)?;
        }
        "move" => {
            let st = store(a);
            let op = Op::Move {
                id: a.pos(1, "id")?.into(),
                before: a.get("--before").map(String::from),
                after: a.get("--after").map(String::from),
                parent: a.get("--parent").map(String::from),
            };
            patch(&st, a.pos(0, "board")?, vec![op], a)?;
        }
        "title" => {
            let st = store(a);
            patch(&st, a.pos(0, "board")?, vec![Op::Title { value: a.pos(1, "title")?.into() }], a)?;
        }
        "hints" => {
            let st = store(a);
            let art = a.pos(0, "board")?;
            let rev = set_hints(&st, art, a.pos[1..].iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())?;
            report(art, rev, a);
        }
        "purpose" => {
            let st = store(a);
            let art = a.pos(0, "board")?;
            let text = a.pos.get(1).map(|s| s.split_whitespace().collect::<Vec<_>>().join(" ")).unwrap_or_default();
            let rev = set_doc_field(&st, art, |d| if d.purpose == text { false } else { d.purpose = text.clone(); true })?;
            report(art, rev, a);
        }
        "ask" => {
            let st = store(a);
            let (art, id, q) = (a.pos(0, "board")?, a.pos(1, "id")?, a.pos(2, "question")?);
            let opts: Vec<String> = a.pos[3..].to_vec();
            if opts.len() < 2 {
                return Err("ask needs at least two options".into());
            }
            // no default unless asked for one: the card waits for sulin, and `--suggest` only names
            // your recommendation
            for flag in ["--default", "--suggest"] {
                if let Some(v) = a.get(flag) {
                    if !opts.iter().any(|o| o == v) {
                        return Err(format!("{} {v:?} isn't one of the options", &flag[2..]));
                    }
                }
            }
            let mut f = serde_json::Map::new();
            f.insert("text".into(), json!(q));
            f.insert("options".into(), json!(opts));
            for flag in ["--default", "--suggest"] {
                if let Some(v) = a.get(flag) {
                    f.insert(flag[2..].into(), json!(v));
                }
            }
            if let Some(w) = a.get("--why") {
                f.insert("why".into(), json!(w));
            }
            if let Some(w) = a.get("--applies") {
                f.insert("applies".into(), json!(w));
            }
            let block = Block { kind: "question".into(), id: id.into(), children: vec![], f };
            if st.load(art)?.find(id).is_some() {
                return Err(format!("{id} already exists in {art}: change it with `margin set {art} {id} <field>=<value>`, or ask under a new id"));
            }
            patch(&st, art, vec![Op::Add { parent: a.get("--parent").map(String::from), block, before: None, after: None }], a)?;
        }
        "propose" => {
            // a change to one of the person's decisions, offered on the component; his accept applies it
            let st = store(a);
            let (art, id, verb) = (a.pos(0, "board")?, a.pos(1, "id")?, a.pos(2, "verb")?);
            if !crate::select::PROPOSABLE.contains(&verb) {
                return Err(format!("propose takes one of {}, not {verb:?}", crate::select::PROPOSABLE.join(", ")));
            }
            let value = a.pos.get(3).cloned();
            let doc = st.load(art)?;
            let b = doc.find(id).ok_or_else(|| format!("no component {id:?} in {art}"))?;
            let fits = match verb {
                "tick" => b.kind == "todo" && value.as_deref().map(|v| doc::STATUSES.contains(&v)).unwrap_or(false),
                "pick" => b.kind == "question" && value.as_ref().map(|v| b.strs("options").contains(v)).unwrap_or(false),
                "strike" => b.kind == "claim",
                _ => b.kind == "diff",
            };
            if !fits {
                return Err(format!("can't propose {verb}{} on {id} (a {})", value.as_ref().map(|v| format!(" {v}")).unwrap_or_default(), b.kind));
            }
            let pr = json!({"verb": verb, "value": value, "why": a.get("--why").unwrap_or(""), "at": crate::util::now_rfc3339()});
            patch(&st, art, vec![Op::Set { id: id.into(), field: "proposal".into(), value: pr }], a)?;
        }
        "diff" => {
            let st = store(a);
            let (art, id) = (a.pos(0, "board")?, a.pos(1, "id")?);
            // before difft runs: a missing artifact is the first thing wrong
            if !st.exists(art) {
                return Err(st.missing(art));
            }
            let tmp = std::env::temp_dir().join(format!("margin-diff-{}", std::process::id()));
            let (old, new, label) = if let Some(rev) = a.get("--rev") {
                let file = a.pos(2, "file")?;
                std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
                let show = |r: &str, out: &str| -> Result<String, String> {
                    let o = std::process::Command::new("jj").args(["file", "show", "-r", r, file]).output().map_err(|e| e.to_string())?;
                    let p = tmp.join(out).join(std::path::Path::new(file).file_name().unwrap_or_default());
                    std::fs::create_dir_all(p.parent().unwrap()).ok();
                    std::fs::write(&p, &o.stdout).map_err(|e| e.to_string())?;
                    Ok(p.to_string_lossy().into_owned())
                };
                let o = show(rev, "a")?;
                let n = match a.get("--to") {
                    Some(t) => show(t, "b")?,
                    None => file.to_string(),
                };
                (o, n, file.to_string())
            } else {
                let o = a.get("--old").ok_or("--old and --new, or --rev")?.to_string();
                let n = a.get("--new").ok_or("--new")?.to_string();
                let l = a.get("--path").unwrap_or(&n).to_string();
                (o, n, l)
            };
            let block = diffblock::compute(id, &old, &new, Some(&label), 2)?;
            let _ = std::fs::remove_dir_all(&tmp);
            let d = st.load(art)?;
            let ops = if d.find(id).is_some() {
                // replace in place, keeping its position
                block.f.iter().map(|(k, v)| Op::Set { id: id.into(), field: k.clone(), value: v.clone() }).collect()
            } else {
                vec![Op::Add { parent: a.get("--parent").map(String::from), block, before: None, after: None }]
            };
            patch(&st, art, ops, a)?;
        }
        "image" => {
            let st = store(a);
            let (art, id, png) = (a.pos(0, "board")?, a.pos(1, "id")?, a.pos(2, "png")?);
            if !st.exists(art) {
                return Err(st.missing(art));
            }
            let path = std::fs::canonicalize(png).map_err(|e| format!("{png}: {e}"))?;
            vellum::image::Image::load_png(&path)?;
            let mut f = serde_json::Map::new();
            f.insert("path".into(), json!(path.to_string_lossy()));
            if let Some(alt) = a.get("--alt") {
                f.insert("alt".into(), json!(alt));
            }
            if let Some(r) = a.get("--rows") {
                f.insert("rows".into(), json_or_str(r));
            }
            let block = Block { kind: "image".into(), id: id.into(), children: vec![], f };
            patch(&st, art, vec![Op::Add { parent: a.get("--parent").map(String::from), block, before: None, after: None }], a)?;
        }
        "show" => {
            let st = store(a);
            let art = a.pos(0, "board")?;
            let d = st.load(art)?;
            let s = st.state(art);
            if let Some(id) = a.pos.get(1) {
                let b = d.find(id).ok_or_else(|| format!("no component {id:?} in {art}; margin show {art}"))?;
                println!("{}", serde_json::to_string_pretty(&json!({"v": 1, "artifact": art, "rev": d.rev, "block": b, "decision": s.decisions.get(id)})).unwrap());
            } else if a.has("--json") {
                println!("{}", serde_json::to_string_pretty(&json!({"v": 1, "doc": d, "state": s})).unwrap());
            } else {
                print!("{}", crate::who::say(&show_text(&d, &s, &st.claims(art), st.delivered_max())));
            }
        }
        "ls" => {
            let st = store(a);
            for m in st.archive_stale(cfg.archive_days) {
                eprintln!("archived {m} (untouched {} days)", cfg.archive_days);
            }
            let docs = st.list();
            if a.has("--json") {
                let v: Vec<Value> = docs.iter().map(|d| json!({"id": d.id, "rev": d.rev, "updated": d.updated, "title": d.title, "kind": d.kind})).collect();
                println!("{}", json!({"v": 1, "artifacts": v}));
                return Ok(());
            }
            for d in docs {
                println!("{:<16} rev {:<4} {:<20} {}", d.id, d.rev, d.updated, d.title);
            }
        }
        "gc" => {
            let st = store(a);
            let days = a.get("--days").and_then(|d| d.parse().ok()).unwrap_or(cfg.archive_days);
            for m in st.archive_stale(days) {
                println!("archived {m}");
            }
            // and the cursors of sessions that long gone (never a named cursor: a tool's or a handler's)
            for p in st.stale_session_cursors(days) {
                if std::fs::remove_file(&p).is_ok() {
                    println!("removed the cursor of a session not seen for {days} days: {}", p.file_name().unwrap_or_default().to_string_lossy());
                }
            }
        }
        "pin" => {
            let home = store(a).home;
            if a.has("--show") {
                println!("{}", pin::show(&home));
            } else {
                let exe = std::env::current_exe().map_err(|e| e.to_string())?;
                let to = pin::pin(&home, &exe)?;
                println!("{}", to.display());
            }
        }
        "doctor" => {
            let st = store(a);
            let days = match a.get("--days") {
                Some(d) => d.parse().map_err(|_| format!("bad --days {d:?}: a whole number of days"))?,
                None if cfg.archive_days > 0 => cfg.archive_days,
                None => 30,
            };
            print!("{}", doctor::run(&st, a.has("--fix"), days));
            // the pinned build, and which panes and daemons of this store run it (pin.rs);
            // nothing when nothing is pinned
            print!("{}", pin::report(&st.home));
        }
        "events" => events_cmd(&store(a), a)?,
        "emit" => {
            let st = store(a);
            let name = a.pos(0, "name")?;
            let value = match a.pos.get(1).map(String::as_str) {
                None => Value::Null,
                Some("-") => {
                    let t = stdin();
                    serde_json::from_str(t.trim()).map_err(|e| format!("stdin is not one JSON value ({e})"))?
                }
                Some(raw) => serde_json::from_str(raw).map_err(|e| format!("the value is not JSON ({e}): {}; quote text as '\"text\"'", vellum::text::truncate(raw, 60)))?,
            };
            if name.starts_with("handler.") {
                return Err("event names starting with handler. are the runner's own (handler.failed)".into());
            }
            for f in ["--board", "--id"] {
                if a.has(f) && a.get(f).is_none() {
                    return Err(format!("{f} needs a value"));
                }
            }
            let ev = st.emit(name, a.get("--board").unwrap_or(""), a.get("--id").unwrap_or(""), value, &st.author, "emit")?;
            println!("{}", ev.seq);
        }
        "act" => {
            let st = store(a);
            let (board, id, action) = (a.pos(0, "board")?, a.pos(1, "id")?, a.pos(2, "action")?);
            let ev = st.act(board, id, action, a.pos.get(3).map(|v| json_or_str(v)), &st.author, "act")?;
            if a.has("--json") {
                println!("{}", serde_json::to_string(&ev).unwrap());
            } else {
                println!("{}", ev.seq);
            }
        }
        "hook" => match a.pos.first().map(|s| s.as_str()).unwrap_or("prompt") {
            // the one hook that waits (asyncRewake): exit 2 wakes the session with what it printed
            "rewake" => {
                let timeout = a.get("--timeout").and_then(util::parse_duration).unwrap_or(claude::rewake::DEADLINE);
                std::process::exit(claude::rewake::run(a.get("--home"), a.get("--project"), timeout))
            }
            kind => claude::hook::run(kind)?,
        },
        "doc" => {
            let st = store(a);
            let (art, id) = (a.pos(0, "board")?, a.pos(1, "id")?);
            let mut sec = mdoc::to_section(id, a.get("--title"), &stdin());
            // keep what this section holds that doc didn't write (a question or claim posted
            // into it): a revision replaces the prose, never sulin's cards
            if let Some(old) = st.load(art).ok().as_ref().and_then(|d| d.find(id)) {
                let prefix = format!("{id}-");
                let kept: Vec<Value> = old.children.iter().filter(|c| !c.id.starts_with(&prefix)).filter_map(|c| serde_json::to_value(c).ok()).collect();
                if let Some(kids) = sec["children"].as_array_mut() {
                    kids.extend(kept);
                }
                // a revision with no title of its own keeps the one it had
                if sec["title"] == json!(id) && !old.s("title").is_empty() {
                    sec["title"] = json!(old.s("title"));
                }
            }
            if let Some(p) = a.get("--parent") {
                sec["parent"] = json!(p);
            }
            for (pid, n) in mdoc::long_paragraphs(&sec) {
                eprintln!("{}", who::say(&format!("hint: {pid} is a paragraph of {n} sentences; bullets (one idea each) read faster and each is a row sulin can react to")));
            }
            apply_cmd(&st, art, &sec.to_string(), a, false, Whole::Replace)?;
        }
        "toast" => {
            let st = store(a);
            let art = a.pos(0, "board")?;
            let text = a.pos.get(1..).map(|w| w.join(" ")).unwrap_or_default();
            let ttl = a.get("--ttl").map(|t| t.trim_end_matches('s').parse::<f64>().map_err(|_| format!("bad --ttl {t:?}: seconds"))).transpose()?;
            let by = if st.author == crate::who::HUMAN { "you".to_string() } else { st.author.clone() };
            let mut t = toast::send(&st, art, &text, a.get("--level").unwrap_or("info"), a.get("--at").unwrap_or(""), ttl, &by)?;
            let (c, b) = (a.get("--countdown"), a.get("--border"));
            if let Some(c) = c.filter(|c| !toast::COUNTDOWNS.contains(c)) {
                return Err(format!("--countdown {c:?}: one of {}", toast::COUNTDOWNS.join(" ")));
            }
            if let Some(b) = b.filter(|b| !toast::BORDERS.contains(b)) {
                return Err(format!("--border {b:?}: one of {}", toast::BORDERS.join(" ")));
            }
            if c.is_some() || b.is_some() {
                toast::set_style(&st, art, &t.id, c, b);
                t.countdown = c.unwrap_or_default().into();
            }
            println!("toast {}{}", t.id, if t.at.is_empty() { String::new() } else { format!(" on {}", t.at) });
        }
        "stop" => {
            let st = store(a);
            let art = a.pos.first().cloned().or_else(|| st.list().into_iter().next().map(|d| d.id)).ok_or("no board to log the stop in")?;
            st.stop(&art, "cli")?;
            println!("{}", who::say(&format!("stopped: {}'s tool calls are refused until sulin's next message", claude::NAME)));
        }
        "link" | "unlink" => {
            let st = store(a);
            let sess = a.get("--session").map(String::from).or_else(claude::session_here).ok_or("no Claude session here ($CLAUDE_CODE_SESSION_ID unset): pass --session")?;
            if cmd == "unlink" {
                println!("{}", if link::remove(&st, &sess) { "unlinked" } else { "no link" });
            } else if let Some(art) = a.pos.first() {
                if !st.exists(art) {
                    return Err(st.missing(art));
                }
                link::set(&st, &sess, art, "explicit");
                println!("linked {art}");
            } else {
                match link::get(&st, &sess) {
                    Some(l) => println!("{}  ({}, since {})\n{}", l.art, l.how, l.since, link::status_line(&st, &l)),
                    None => println!("no link"),
                }
            }
        }
        "open" => {
            let st = store(a);
            let art = a.pos(0, "board")?;
            // run from inside a consumer's session, the adapter may make this board that
            // session's own; from anywhere else this does nothing
            claude::opened(&st, art, a.has("--stacked"));
            let o = host::OpenOpts {
                float: if a.has("--split") {
                    Some(false)
                } else if a.has("--float") {
                    Some(true)
                } else {
                    None
                },
                tab_id: a.get("--tab-id").and_then(|t| t.parse().ok()),
                session: a.get("--session").map(String::from),
                stacked: a.has("--stacked"),
                while_pid: while_pid(a)?,
            };
            let h = host::pick(&cfg, a.get("--session"));
            // (a tab may be another project's board: project:board)
            let more: Vec<String> = a.get("--tabs").map(|t| t.split(',').map(|x| tabs::normalize(&st.project, x)).filter(|x| !x.is_empty() && x != art).collect()).unwrap_or_default();
            for t in &more {
                tab_exists(&st, t).map_err(|e| format!("--tabs: {e}"))?;
            }
            // on show already as a tab of another pane beside the chat: that pane is its pane
            if more.is_empty() && !a.has("--stacked") {
                if let Some((handle, primary)) = tab_hosts(&st, &*h, art, a, true).into_iter().next() {
                    println!("reused {handle} (a tab of margin:{primary}; `margin tab {primary} show {art}` brings it on screen)");
                    return Ok(());
                }
            }
            match h.open(&st, art, &o, &cfg)? {
                host::Opened::Pane { handle, created } => {
                    // the pane's tab set is in its view file: a new pane starts with what was
                    // asked for (an older pane with the same id may have left a set behind), and
                    // --tabs on a pane already open adds to its set. The pane follows the file
                    let key = tabs::pane_key(&handle);
                    let mut v = st.view(art, &key);
                    let before = v.clone();
                    if created {
                        tabs::store(&mut v, &[art.to_string()], 0);
                    }
                    for t in &more {
                        tabs::edit(&mut v, art, "add", t)?;
                    }
                    if v != before {
                        st.save_view(art, &key, &v);
                    }
                    println!("{} {handle}", if created { "opened" } else { "reused" });
                }
                host::Opened::Run(cmd) => {
                    println!("no multiplexer (host {}): run this in a terminal beside the chat:", h.name());
                    println!("  {}", cmd.replacen(&format!(" view {art}"), &format!(" view {}", std::iter::once(art.to_string()).chain(more.iter().cloned()).collect::<Vec<_>>().join(" ")), 1));
                }
            }
        }
        "panes" => {
            let st = store(a);
            let h = host::pick(&cfg, a.get("--session"));
            let art = a.pos(0, "board")?;
            for p in h.find(&st, art)? {
                println!("{p}");
            }
            for (handle, primary) in tab_hosts(&st, &*h, art, a, false) {
                println!("{handle}  (a tab of margin:{primary})");
            }
        }
        "close" => {
            let st = store(a);
            let art = a.pos(0, "board")?;
            let h = host::pick(&cfg, a.get("--session"));
            let n = h.close(&st, art)?;
            println!("closed {n}");
            // a tab is not a pane: closing it would close the artifacts beside it
            if n == 0 {
                for (_, primary) in tab_hosts(&st, &*h, art, a, false) {
                    println!("{art} is a tab of margin:{primary}: margin tab {primary} rm {art}");
                }
            }
        }
        "tab" => {
            let st = store(a);
            let primary = a.pos(0, "board")?;
            if !st.exists(primary) {
                return Err(st.missing(primary));
            }
            let mut panes: Vec<String> = match a.get("--pane") {
                Some(p) => vec![p.to_string()],
                None => host::pick(&cfg, a.get("--session")).find(&st, primary)?.iter().map(|h| tabs::pane_key(h)).collect(),
            };
            panes.dedup();
            if panes.is_empty() {
                return Err(format!("no pane is showing {primary}: open one with `margin open {primary} --tabs B,C`, or name the pane with --pane"));
            }
            let edit = match (a.pos.get(1), a.pos.get(2)) {
                (None, _) => None,
                (Some(op), Some(name)) => {
                    // the tab as the view file names it: a board of this project, or with
                    // --from (or as project:board) a board of another one
                    let entry = match (a.get("--from"), tabs::split(name)) {
                        (Some(_), (Some(_), _)) => return Err(format!("{name} already names its project: leave out --from")),
                        (Some(p), (None, b)) => tabs::entry(&st.project, p, b),
                        (None, _) => tabs::normalize(&st.project, name),
                    };
                    if op == "add" {
                        tab_exists(&st, &entry)?;
                    }
                    Some((op.clone(), entry))
                }
                (Some(op), None) => return Err(format!("margin tab {primary} {op} <board>")),
            };
            for p in panes {
                let mut v = st.view(primary, &p);
                if let Some((op, art)) = &edit {
                    if tabs::edit(&mut v, primary, op, art)? {
                        st.save_view(primary, &p, &v);
                    }
                }
                let (set, active) = tabs::resolve(primary, &[], &v);
                let shown: Vec<String> = set.iter().enumerate().map(|(i, t)| if i == active { format!("[{t}]") } else { t.clone() }).collect();
                println!("{p}: {}", shown.join(" "));
            }
        }
        "view" => {
            let st = store(a);
            let h = host::pick(&cfg, None);
            let art = a.pos(0, "board")?.to_string();
            let more: Vec<String> = a.pos[1..].iter().map(|x| tabs::normalize(&st.project, x)).collect();
            let until = while_pid(a)?;
            if let Some(pid) = until.filter(|p| !watch::alive(*p)) {
                println!("margin view: process {pid} (--while-pid) is not running; not starting");
                return Ok(());
            }
            if let Some(exe) = app::run(st, art.clone(), more, cfg, h, until)? {
                // the terminal is restored (app::run's Terminal dropped); become the new build.
                // Only the pane's own artifact is named again: the tab set is in the view file by
                // now, and naming the tabs it started with would undo a `margin tab` since
                use std::os::unix::process::CommandExt;
                let mut args = vec!["view".to_string(), art];
                for (k, v) in &a.flags {
                    args.push(k.clone());
                    args.extend(v.clone());
                }
                let err = std::process::Command::new(&exe).args(args).exec();
                return Err(format!("reload {}: {err}", exe.display()));
            }
        }
        "daemon" => daemon_cmd(a)?,
        "owner" => owner_cmd(a)?,
        "data" => data_cmd(a)?,
        "handle" => match a.pos(0, "open")? {
            "open" => handle_open(&stdin(), &cfg)?,
            other => return Err(format!("no built-in handler {other:?}: margin handle open")),
        },
        "render" => {
            let st = store(a);
            // more artifacts: the tab bar, with --tab (else the first) on screen
            let set: Vec<String> = a.pos.iter().fold(Vec::new(), |mut v, x| {
                if !v.contains(x) {
                    v.push(x.clone());
                }
                v
            });
            let tab = a.get("--tab").map(|t| set.iter().position(|x| x == t).ok_or_else(|| format!("--tab {t}: not one of {}", set.join(" ")))).transpose()?.unwrap_or(0);
            let art = set.get(tab).map(|s| s.as_str()).ok_or("missing <board>")?;
            let w: u16 = a.get("--width").and_then(|x| x.parse().ok()).unwrap_or(62);
            // the datasets the board reads, as the pane would read them now (data.rs)
            let d = st.load(art)?;
            let mut snap = data::Snapshot::load(&st, &data::wants(&d, w), &mut data::Cache::default());
            snap.gen = data::next_gen();
            let snap = std::rc::Rc::new(snap);
            let d = data::expand(&d, &snap);
            data::install(snap.clone());
            let s = st.state(art);
            let mut v = st.view(art, a.get("--pane").unwrap_or("tty"));
            v.detail |= a.has("--detail");
            render_view(&d, &mut v, a.all("--section"), a.get("--focus"))?;
            let h: u16 = a.get("--height").and_then(|x| x.parse().ok()).unwrap_or(40);
            let at: f64 = a.get("--at").and_then(|x| x.parse().ok()).unwrap_or(10.0);
            let mut cfg = cfg;
            if let Some(m) = a.get("--motion") {
                cfg.motion = m.into();
            }
            let mut sc = scene::Scene::new(&cfg);
            sc.restore(&v);
            // who is working on the board, through the seam; nobody unless a session keeps it
            let acts = sc.present(consumer::on_board(&st, art));
            sc.stale = owner::stale_on(&st, art, util::epoch_now());
            sc.data_gen = snap.gen;
            let overdue = snap.stale(util::epoch_now());
            sc.stale_data = data::bound_ids(&d).into_iter().filter_map(|(id, set)| overdue.get(&set).map(|why| (id, (set.clone(), why.clone())))).collect();
            sc.toasts = toast::load(&st, art).into_iter().map(|t| (t, 1.0)).collect();
            if set.len() > 1 {
                let docs = set.iter().map(|x| st.load(x)).collect::<Result<Vec<_>, _>>()?;
                let titles = tabs::short_titles(&docs.iter().map(|d| if d.title.is_empty() { d.id.clone() } else { d.title.clone() }).collect::<Vec<_>>());
                sc.tabs = docs
                    .iter()
                    .zip(titles)
                    .map(|(d, title)| {
                        let (needs, failed) = tabs::attention(d, &st.state(&d.id));
                        // an overdue writer is a failure of the board: the same mark
                        let failed = failed || !owner::stale_on(&st, &d.id, util::epoch_now()).is_empty();
                        tabs::TabInfo { art: d.id.clone(), title, unseen: 0, needs, failed, project: String::new() }
                    })
                    .collect();
                sc.tab = tab;
            }
            let inp = scene::Input {
                doc: &d,
                state: &s,
                view: &v,
                delivered: st.delivered_max(),
                unread: 0,
                now: at,
                wall: util::epoch_now(),
                cfg: &cfg,
                kitty: false,
                cell: (10, 20),
                focused_pane: a.has("--focused"),
                mouse: false,
                project: &st.project,
                w,
                h,
                activity: &acts,
            };
            let f = sc.frame(&inp);
            // plain text is read where no Nerd Font can be assumed: icons become ordinary characters
            print!("{}", if a.has("--ansi") { f.grid.to_ansi() } else { components::plain_text(&f.grid.to_text()) });
        }
        "select" => {
            let text = if a.pos.is_empty() { stdin() } else { a.pos.join(" ") };
            match select::dispatch(&text, "select", &cfg) {
                select::Outcome::Applied(e) => select::log_line(&format!("  applied {} {} {}", e.kind, e.id, e.value)),
                select::Outcome::Opened(u) => select::log_line(&format!("  opened {u}")),
                select::Outcome::Copied(how) => select::log_line(&format!("  copied via {how}")),
                select::Outcome::Handed => {}
                // refused: say why and exit non-zero (it used to exit 0 with the reason only
                // in select.log, so a script could not tell a refusal from a success)
                select::Outcome::Ignored(e) => {
                    select::log_line(&format!("  ignored: {e}"));
                    return Err(e);
                }
            }
        }
        "session" => {
            let name = a.pos.first().cloned().unwrap_or_else(|| "margin".into());
            let mut c = cfg.clone();
            if c.host == "auto" {
                c.host = "zellij".into(); // `margin session` starts Zellij, so auto means Zellij here
            }
            host::pick(&c, None).session(&name)?;
        }
        "demo" => {
            let st = store(a);
            match a.pos.first().map(|s| s.as_str()) {
                Some("live") => {
                    let speed = a.get("--speed").and_then(|s| s.parse().ok()).unwrap_or(1.0);
                    demo::live(&st, speed);
                }
                _ => {
                    let rev = demo::setup(&st)?;
                    println!("plan rev {rev} in project {} (the demo's own board: run again, it resets it)", st.project);
                    println!("next: margin open plan    (in Zellij a split; elsewhere it prints the view command)");
                }
            }
        }
        "skill" => print!("{}", claude::skill()),
        "unread" => {
            let st = store(a);
            let sess = a.get("--session").map(String::from).or_else(claude::session_here).unwrap_or_else(|| "default".into());
            let now = util::epoch_now();
            for e in claude::hook::unread(&st, &sess) {
                if a.has("--json") {
                    println!("{}", serde_json::to_string(&e).unwrap());
                } else {
                    println!("{:>4} {} {}", e.seq, local_stamp(&e.at, now), who::say(&claude::hook::describe(&st, &e)));
                }
            }
        }
        "example" => {
            // `--script`/`--data` are switches; written before the name they swallow it as a value
            let script = a.has("--script");
            let name = a.pos.first().map(String::as_str).or(a.get("--script")).or(a.get("--data")).or(a.get("--tool"));
            let Some(name) = name else {
                print!("{}", example::list());
                return Ok(());
            };
            if name == example::RUNS.name {
                match (a.has("--tool"), script || a.has("--data")) {
                    (_, true) => return Err("the runs example is an outside tool: margin example runs --tool".into()),
                    (true, _) => print!("{}", example::RUNS.tool),
                    _ => {
                        eprint!("{}", example::RUNS.notes());
                        print!("{}", example::RUNS.artifact);
                    }
                }
                return Ok(());
            }
            if a.has("--tool") {
                return Err("--tool is the runs example's: margin example runs --tool".into());
            }
            let names = || example::EXAMPLES.iter().map(|e| e.name).chain([example::RUNS.name]).collect::<Vec<_>>().join(", ");
            let e = example::get(name).ok_or_else(|| format!("no example {name:?}; examples: {}", names()))?;
            match (script, a.has("--data")) {
                (true, true) => return Err("--script or --data, one at a time".into()),
                (true, _) => print!("{}", e.script),
                (_, true) => print!("{}", e.data),
                _ => {
                    eprint!("{}", e.notes());
                    print!("{}", e.artifact);
                }
            }
        }
        "init" => {
            let home = || std::env::var("HOME").map(std::path::PathBuf::from).map_err(|_| "$HOME isn't set: pass --claude-dir".to_string());
            let abs = |p: &str| std::path::absolute(p).map_err(|e| format!("{p}: {e}"));
            for f in ["--user-name", "--claude-dir", "--bin"] {
                if a.has(f) && a.get(f).map(|v| v.trim().is_empty()).unwrap_or(true) {
                    return Err(format!("{f} needs a value"));
                }
            }
            let o = claude::init::Opts {
                claude_dir: match a.get("--claude-dir") {
                    Some(d) => abs(d)?,
                    None => home()?.join(".claude"),
                },
                config: config::config_path(),
                bin: match a.get("--bin") {
                    Some(b) => abs(b)?,
                    None => help::bin_path(),
                },
                user_name: a.get("--user-name").map(String::from),
                dry_run: a.has("--dry-run"),
            };
            print!("{}", claude::init::run(&o)?);
        }
        c => return Err(format!("unknown command {c:?}; margin --help")),
    }
    Ok(())
}

/// Wait up to `ms` for nothing in particular: the sleep of a log's `--follow` loop, and with 0
/// the question "should a follow stop". False when it is time to stop: a signal arrived (ctrl-c), or stdout is a pipe whose reader has gone.
fn follow_wait(ms: i32) -> bool {
    let mut p = libc::pollfd { fd: 1, events: 0, revents: 0 };
    let n = unsafe { libc::poll(&mut p, 1, ms) };
    !(daemon::exiting() || (n > 0 && p.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0))
}

/// The wait of `events --follow`: block until the log holds an event past `from`. False when it
/// is time to stop instead (ctrl-c, a reader that has gone). No timer: `w` wakes on the append
/// itself, on the signal's exit pipe and on stdout hanging up, and each wake asks all three again.
fn follow_next(st: &Store, from: u64, w: &mut watch::Watch) -> bool {
    loop {
        if !follow_wait(0) {
            return false;
        }
        if st.last_seq() > from {
            return true;
        }
        w.wait(None);
    }
}

/// `margin events`: read the log, from a seq or a named cursor, once or following.
fn events_cmd(st: &Store, a: &Args) -> Result<(), String> {
    use std::io::Write;
    for f in ["--cursor", "--since", "--name", "--kind", "--board", "--artifact", "--commit"] {
        if a.has(f) && a.get(f).is_none() {
            return Err(format!("{f} needs a value"));
        }
    }
    let cursor = a.get("--cursor");
    // `handler:<name>` is a handler's own cursor: readable (what waits for it), never moved from here
    let handlers = cursor.and_then(|c| c.strip_prefix("handler:")).filter(|n| util::valid_id(n));
    if let Some(c) = cursor.filter(|c| !util::valid_id(c) && handlers.is_none()) {
        return Err(format!("bad cursor name {c:?}: letters, digits, - and _ (or handler:<name>, with --no-commit)"));
    }
    if let Some(n) = handlers {
        if a.has("--commit") || !a.has("--no-commit") {
            return Err(format!("handler:{n} is handler {n}'s own cursor: read it with --no-commit (the daemon moves it when a run succeeds)"));
        }
        if handler::Registry::load(&st.dir()).get(n).is_none() {
            return Err(handler::missing(n));
        }
    }
    let seq = |f: &str| a.get(f).map(|s| s.parse::<u64>().map_err(|_| format!("bad {f} {s:?}: an event seq (a whole number)"))).transpose();
    if let Some(to) = seq("--commit")? {
        let c = cursor.ok_or("--commit moves a cursor: margin events --cursor NAME --commit SEQ")?;
        st.set_cursor(c, to);
        return Ok(());
    }
    let (unread, follow) = (a.has("--unread"), a.has("--follow"));
    if unread && (cursor.is_some() || follow) {
        return Err("--unread reads a session's own cursor: it takes neither --cursor nor --follow".into());
    }
    if a.has("--no-commit") && cursor.is_none() {
        return Err("--no-commit goes with --cursor NAME".into());
    }
    let commit = cursor.filter(|_| !a.has("--no-commit"));
    // a cursor is created on first use, at the end of the log, whether or not this read commits
    let at = cursor.map(|c| st.cursor_or_create(c));
    let mut from = seq("--since")?.or(at).unwrap_or(0);
    let keep = |e: &store::Event| {
        a.get("--board").or(a.get("--artifact")).map(|x| e.artifact == x).unwrap_or(true)
            && a.get("--kind").map(|k| e.kind == k).unwrap_or(true)
            && a.get("--name").map(|g| util::glob_match(g, &e.kind)).unwrap_or(true)
            // by default the person's decisions only: the passive kinds (view, seen, fold…) and
            // other writers' events buried them (2026-11-10)
            && (a.has("--all") || a.has("--kind") || a.has("--name") || (who::is_human(&e.by) && !store::is_passive(&e.kind)))
    };
    // armed before the first read of the log, so nothing appended after that read is missed
    let mut watch = follow.then(|| {
        daemon::trap_signals();
        let w = st.watch_events().interrupt(daemon::exit_fd()).hangup(1);
        if let Some(why) = w.fallback() {
            eprintln!("margin: {why}; checking the event log every {} ms instead", watch::FALLBACK_TICK.as_millis());
        }
        w
    });
    let out = std::io::stdout();
    loop {
        let evs = if unread {
            let sess = a.get("--session").map(String::from).or_else(claude::session_here).unwrap_or("default".into());
            hook::unread(st, &sess).into_iter().filter(|e| e.seq > from).collect()
        } else {
            st.events_since(from)
        };
        let now = util::epoch_now();
        for e in &evs {
            from = from.max(e.seq);
            if keep(e) {
                let line = if a.has("--json") { serde_json::to_string(e).unwrap() } else { format!("{:>4} {} {}", e.seq, local_stamp(&e.at, now), who::say(&hook::describe(st, e))) };
                let mut o = out.lock();
                // a reader that has gone ends the command, quietly, before the cursor moves
                if writeln!(o, "{line}").and_then(|_| o.flush()).is_err() {
                    return Ok(());
                }
                if let Some(c) = commit {
                    st.set_cursor(c, e.seq);
                }
            }
        }
        // past the events the filters left out too, so they are not read again
        if let (Some(c), Some(_)) = (commit, evs.last()) {
            st.set_cursor(c, from);
        }
        let Some(w) = watch.as_mut() else { return Ok(()) };
        if !follow_next(st, from, w) {
            return Ok(());
        }
    }
}

/// The exit code for an error: 3 when the thing named is not there (an artifact, a component, a
/// handler, an owner), 1 for everything else (refused, invalid, failed). A script can
/// tell "not created yet" from "something is wrong" without reading the message.
fn exit_code(e: &str) -> i32 {
    if ["no board ", "no component ", "no handler ", "no owner ", "no dataset "].iter().any(|p| e.starts_with(p)) {
        3
    } else {
        1
    }
}

/// How a duration reads in a flag: `60s`, `10m`.
fn duration_flag(a: &Args, flag: &str) -> Result<Option<f64>, String> {
    match a.get(flag) {
        None if a.has(flag) => Err(format!("{flag} needs a value (60s, 10m)")),
        None => Ok(None),
        Some(v) => util::parse_duration(v).map(Some).ok_or_else(|| format!("bad {flag} {v:?}: a duration such as 60s, 10m, 1h")),
    }
}

/// `margin handler …`: the registry of event handlers, their logs, and a run in the foreground.
fn handler_cmd(a: &Args, cmd: Vec<String>) -> Result<(), String> {
    use handler::{edit_registry, Handler, Registry};
    const SUBS: &str = "add, set, ls, rm, pause, resume, logs, run";
    let sub = a.pos(0, "add|set|ls|rm|pause|resume|logs|run")?;
    let (flags, max_pos): (&[&str], usize) = match sub {
        "add" | "set" => (&["--on", "--board", "--owns", "--timeout", "--retries"], 2),
        "ls" => (&["--json"], 1),
        "rm" | "pause" | "resume" => (&[], 2),
        "logs" => (&["--follow", "--lines"], 2),
        "run" => (&["--events", "--dry-run"], 2),
        s => return Err(format!("unknown handler command {s:?}: {SUBS}")),
    };
    check_args(&format!("handler {sub}"), a, flags, Some(max_pos))?;
    author(a)?;
    if !matches!(sub, "add" | "set") && !cmd.is_empty() {
        return Err("`--` is only for handler add and set".into());
    }
    let st = store(a);
    let name = || a.pos(1, "name");
    match sub {
        "add" | "set" => {
            let name = name()?.to_string();
            for f in ["--on", "--board", "--owns", "--retries"] {
                if a.has(f) && a.all(f).len() != a.flags.iter().filter(|(k, _)| k == f).count() {
                    return Err(format!("{f} needs a value"));
                }
            }
            let old = Registry::load_strict(&st.dir())?.get(&name).cloned();
            let cwd = std::env::current_dir().map_err(|e| e.to_string())?.to_string_lossy().into_owned();
            let mut h = match (sub, old.clone()) {
                ("set", Some(h)) => h,
                ("set", None) => return Err(handler::missing(&name)),
                _ => Handler { name: name.clone(), cmd: Vec::new(), on: Vec::new(), board: String::new(), owns: String::new(), paused: false, cwd: cwd.clone(), path: String::new(), timeout: 60.0, retries: 3, added: util::now_rfc3339() },
            };
            if !a.all("--on").is_empty() {
                h.on = a.all("--on").into_iter().map(String::from).collect();
            }
            match a.get("--board") {
                Some("none") => (h.board, h.owns) = (String::new(), String::new()),
                Some(b) => h.board = b.to_string(),
                None => {}
            }
            if let Some(o) = a.get("--owns") {
                h.owns = o.to_string();
            }
            if let Some(t) = duration_flag(a, "--timeout")? {
                h.timeout = t;
            }
            if let Some(r) = a.get("--retries") {
                h.retries = r.parse().map_err(|_| format!("bad --retries {r:?}: a whole number, 0 for never"))?;
            }
            if !cmd.is_empty() {
                // the command, where it runs and what it finds on PATH belong together
                h.cmd = cmd;
                h.cwd = cwd;
                h.path = std::env::var("PATH").unwrap_or_default();
            }
            if !h.board.is_empty() {
                if !st.exists(&h.board) {
                    return Err(st.missing(&h.board));
                }
                if !h.owns.is_empty() {
                    st.claim_overlap(&h.board, &h.owns, ("handler", &name))?;
                }
            }
            let line = format!("handler {name}: on {} → {}{}", h.on.join(" "), h.cmd.join(" "), if h.board.is_empty() { String::new() } else { format!(" (writes {}, owns {:?})", h.board, h.owns) });
            if sub == "add" {
                edit_registry(&st, |r| r.add(h.clone()))?;
                // it hears what happens from now on: its cursor starts at the end of the log
                st.cursor_or_create(&h.cursor());
            } else {
                edit_registry(&st, |r| r.set(h.clone()))?;
            }
            let started = if h.paused { None } else { daemon::ensure(&st)? };
            println!("{line}{}", if started.is_some() { "; daemon started" } else { "" });
        }
        "rm" => {
            let n = name()?;
            let h = edit_registry(&st, |r| r.remove(n))?;
            let _ = std::fs::remove_file(st.cursor_path(&h.cursor()));
            let _ = std::fs::remove_file(handler::status_path(&st, n));
            println!("removed handler {n}{}", if h.board.is_empty() { String::new() } else { format!("; what it wrote stays in {}, unowned", h.board) });
        }
        "pause" | "resume" => {
            let n = name()?;
            edit_registry(&st, |r| r.set_paused(n, sub == "pause"))?;
            if sub == "resume" {
                daemon::ensure(&st)?;
            }
            println!("{} handler {n}", if sub == "pause" { "paused" } else { "resumed" });
        }
        "ls" => {
            let r = Registry::load_strict(&st.dir())?;
            let running = daemon::running(&st);
            let now = util::epoch_now();
            let rows: Vec<(&Handler, handler::Status, String, usize)> = r
                .handlers
                .iter()
                .map(|h| {
                    let s = handler::read_status(&st, &h.name).unwrap_or_default();
                    let state = if h.paused {
                        "paused".to_string()
                    } else if running.is_none() {
                        "stopped".into()
                    } else if s.state.is_empty() {
                        "starting".into()
                    } else {
                        s.state.clone()
                    };
                    (h, s, state, handler::pending(&st, h).len())
                })
                .collect();
            if a.has("--json") {
                let hs: Vec<Value> = rows
                    .iter()
                    .map(|(h, s, state, pending)| json!({"name": h.name, "on": h.on, "cmd": h.cmd, "board": h.board, "owns": h.owns, "paused": h.paused, "state": state, "cursor": st.cursor(&h.cursor()), "pending": pending, "attempts": s.attempts, "last_run": s.last_run, "last_outcome": s.last_outcome, "last_error": s.last_error}))
                    .collect();
                println!("{}", json!({"v": 1, "daemon": {"running": running.is_some(), "pid": running.unwrap_or(0)}, "handlers": hs}));
                return Ok(());
            }
            match running {
                Some(p) if p > 0 => println!("daemon: running, pid {p}"),
                Some(_) => println!("daemon: starting"),
                None => println!("daemon: not running{}", if r.handlers.iter().any(|h| !h.paused) { " (the next event appended starts it; or: margin daemon)" } else { "" }),
            }
            if r.handlers.is_empty() {
                println!("no handlers in project {}", st.project);
            }
            for (h, s, state, pending) in &rows {
                let board = if h.board.is_empty() { "-".to_string() } else { format!("{}/{}", h.board, h.owns) };
                println!("{:<14} {:<9} {:<24} {:<16} waiting {:<3} {}", h.name, state, vellum::text::truncate(&format!("on {}", h.on.join(" ")), 24), board, pending, s.last_run_text(now));
            }
        }
        "logs" => {
            let n = name()?;
            if Registry::load(&st.dir()).get(n).is_none() && !handler::log_path(&st, n).exists() {
                return Err(handler::missing(n));
            }
            print_log(&handler::log_path(&st, n), a, &format!("handler {n}"))?;
        }
        "run" => {
            let n = name()?;
            let h = Registry::load_strict(&st.dir())?.get(n).cloned().ok_or_else(|| handler::missing(n))?;
            let (events, own) = match a.get("--events") {
                Some(f) => {
                    let text = std::fs::read_to_string(f).map_err(|e| format!("--events {f}: {e}"))?;
                    let evs = text.lines().filter(|l| !l.trim().is_empty()).enumerate().map(|(i, l)| serde_json::from_str::<store::Event>(l).map_err(|e| format!("--events {f}: line {}: {e}", i + 1))).collect::<Result<Vec<_>, _>>()?;
                    (evs, false)
                }
                None if a.has("--events") => return Err("--events needs a file of events (margin events --json > FILE)".into()),
                None => (handler::pending(&st, &h), true),
            };
            if events.is_empty() {
                println!("no events wait for handler {n}{}", if own { " (give it some: --events FILE)" } else { "" });
                return Ok(());
            }
            // the command may exit without reading its stdin
            unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };
            let mut w = handler::Worker::new(store(a), h, std::sync::Arc::new(handler::Ctl::default()), true);
            w.exec(&events, a.has("--dry-run"), own)?;
        }
        _ => unreachable!(),
    }
    Ok(())
}

/// The last lines of a log file, and with `--follow` what is appended to it (until ctrl-c).
fn print_log(p: &std::path::Path, a: &Args, what: &str) -> Result<(), String> {
    let lines: usize = a.get("--lines").and_then(|l| l.parse().ok()).unwrap_or(40);
    let text = std::fs::read_to_string(p).map_err(|_| format!("no log yet for {what} ({})", p.display()))?;
    let all: Vec<&str> = text.lines().collect();
    for l in &all[all.len().saturating_sub(lines)..] {
        println!("{l}");
    }
    if a.has("--follow") {
        use std::io::Write;
        daemon::trap_signals();
        let mut seen = text.len();
        while follow_wait(300) {
            let t = std::fs::read_to_string(p).unwrap_or_default();
            if t.len() < seen {
                seen = 0; // rotated
            }
            print!("{}", &t[seen..]);
            let _ = std::io::stdout().flush();
            seen = t.len();
        }
    }
    Ok(())
}

/// `margin owner …`: who writes which rows from outside.
fn owner_cmd(a: &Args) -> Result<(), String> {
    use owner::{Owner, Registry};
    let st = store(a);
    let sub = a.pos(0, "add|set|ls|rm")?;
    let name = || a.pos(1, "name");
    for f in ["--board", "--owns", "--expect-every"] {
        if a.has(f) && a.get(f).is_none() {
            return Err(format!("{f} needs a value"));
        }
    }
    let expect = |a: &Args| -> Result<Option<Option<f64>>, String> {
        match a.get("--expect-every") {
            None => Ok(None),
            Some("none") => Ok(Some(None)),
            Some(v) => util::parse_duration(v).map(|d| Some(Some(d))).ok_or_else(|| format!("bad --expect-every {v:?}: a duration such as 10m, 1h, or none")),
        }
    };
    match sub {
        "add" => {
            let n = name()?;
            let o = Owner { name: n.into(), board: a.get("--board").unwrap_or("").into(), owns: a.get("--owns").unwrap_or("").into(), expect_every: expect(a)?.flatten(), added: util::now_rfc3339(), last_write: String::new() };
            st.add_owner(o.clone())?;
            println!("owner {n}: owns {:?} in {}; it writes with: margin apply {} --as {n}", o.owns, o.board, o.board);
        }
        "set" => {
            let n = name()?;
            let mut o = Registry::load_strict(&st.dir())?.get(n).cloned().ok_or_else(|| owner::missing(n))?;
            if let Some(b) = a.get("--board") {
                o.board = b.into();
            }
            if let Some(p) = a.get("--owns") {
                o.owns = p.into();
            }
            if let Some(e) = expect(a)? {
                o.expect_every = e;
            }
            st.set_owner(o.clone())?;
            println!("owner {n}: owns {:?} in {}{}", o.owns, o.board, o.expect_every.map(|e| format!(", expected every {}", owner::fmt_secs(e))).unwrap_or_default());
        }
        "rm" => {
            let n = name()?;
            let o = st.remove_owner(n)?;
            println!("removed owner {n}; its rows stay in {}, unowned", o.board);
        }
        "ls" => {
            let r = Registry::load_strict(&st.dir())?;
            let now = util::epoch_now();
            if a.has("--json") {
                let v: Vec<Value> = r.owners.iter().map(|o| json!({"name": o.name, "board": o.board, "owns": o.owns, "expect_every": o.expect_every, "added": o.added, "last_write": o.last_write, "stale": o.stale(now).is_some()})).collect();
                println!("{}", json!({"v": 1, "owners": v}));
                return Ok(());
            }
            if r.owners.is_empty() {
                println!("no owners in project {}", st.project);
            }
            for o in &r.owners {
                let wrote = util::parse_rfc3339(&o.last_write).map(|t| format!("wrote {}", util::ago(now - t))).unwrap_or_else(|| "never wrote".into());
                let health = match (o.stale(now), o.expect_every) {
                    (Some(why), _) => format!("STALE: {why}"),
                    (None, Some(e)) => format!("{wrote} (expected every {})", owner::fmt_secs(e)),
                    (None, None) => wrote,
                };
                println!("{:<14} {:<20} {health}", o.name, format!("{}/{}", o.board, o.owns));
            }
        }
        s => return Err(format!("unknown owner command {s:?}: add, set, ls, rm")),
    }
    Ok(())
}

/// `margin data …`: the registry of datasets, and their records.
fn data_cmd(a: &Args) -> Result<(), String> {
    use data::{Dataset, Registry};
    let st = store(a);
    let sub = a.pos(0, "add|set|put|append|show|ls|rm")?;
    let name = || a.pos(1, "name");
    for f in ["--cap", "--file", "--owner", "--expect-every", "--as", "--last"] {
        if a.has(f) && a.get(f).is_none() {
            return Err(format!("{f} needs a value"));
        }
    }
    let takes: &[&str] = match sub {
        "add" | "set" => &["--cap", "--file", "--owner", "--expect-every"],
        "put" | "append" => &["--as"],
        "show" => &["--last", "--json"],
        "ls" => &["--json"],
        _ => &[],
    };
    if let Some((f, _)) = a.flags.iter().find(|(f, _)| !takes.contains(&f.as_str()) && !GLOBAL_FLAGS.contains(&f.as_str())) {
        return Err(format!("margin data {sub} takes no {f}; margin data --help"));
    }
    let max = match sub {
        "ls" => 1,
        "append" => 3,
        _ => 2,
    };
    if a.pos.len() > max {
        return Err(format!("unexpected argument {:?}; margin data --help", a.pos[max]));
    }
    // each flag as Some(new value), where the word `none` clears it (set only)
    let none = |v: &str| v == "none" && sub == "set";
    let cap = match a.get("--cap") {
        None => None,
        Some(v) if none(v) => Some(None),
        Some(v) => Some(Some(v.parse::<u64>().ok().filter(|n| *n >= 1).ok_or_else(|| format!("bad --cap {v:?}: a number of records, 1 or more"))?)),
    };
    let expect = match a.get("--expect-every") {
        None => None,
        Some(v) if none(v) => Some(None),
        Some(v) => Some(Some(util::parse_duration(v).ok_or_else(|| format!("bad --expect-every {v:?}: a duration such as 10s, 5m, 1h"))?)),
    };
    let file = match a.get("--file") {
        None => None,
        Some(v) if none(v) => Some(None),
        // absolute, from here: the pane and every later command run somewhere else
        Some(v) => Some(Some(std::path::absolute(v).map_err(|e| format!("--file {v}: {e}"))?.display().to_string())),
    };
    let owner = match a.get("--owner") {
        None => None,
        Some(v) if none(v) => Some(None),
        Some(v) => Some(Some(v.to_string())),
    };
    let describe = |n: &str, d: &Dataset| {
        let mut parts = vec![match &d.file {
            Some(f) => format!("reads {f} in place"),
            None => "held by margin".to_string(),
        }];
        parts.extend(d.cap.map(|c| format!("newest {c} kept")));
        parts.extend(d.owner.as_ref().map(|o| format!("written only by owner {o}")));
        parts.extend(d.expect_every.map(|e| format!("expected every {}", owner::fmt_secs(e))));
        format!("dataset {n}: {}", parts.join(", "))
    };
    match sub {
        "add" => {
            let n = name()?;
            let d = Dataset { owner: owner.flatten(), cap: cap.flatten(), file: file.flatten(), expect_every: expect.flatten(), added: util::now_rfc3339() };
            st.data_add(n, d.clone())?;
            println!("{}", describe(n, &d));
            match &d.file {
                Some(f) if !std::path::Path::new(f).exists() => eprintln!("margin data: {f} is not there yet; a board bound to {n} says so until it is"),
                Some(_) => {}
                None => println!("write it with: margin data append {n} <value>{}", d.owner.as_ref().map(|o| format!(" --as {o}")).unwrap_or_default()),
            }
        }
        "set" => {
            let n = name()?;
            if cap.is_none() && expect.is_none() && file.is_none() && owner.is_none() {
                return Err("nothing to change: --cap, --file, --owner or --expect-every (each takes `none` to clear it)".into());
            }
            let d = st.data_set(n, |d| {
                if let Some(c) = cap {
                    d.cap = c;
                }
                if let Some(e) = expect {
                    d.expect_every = e;
                }
                if let Some(f) = file.clone() {
                    d.file = f;
                }
                if let Some(o) = owner.clone() {
                    d.owner = o;
                }
            })?;
            println!("{}", describe(n, &d));
        }
        "append" => {
            let n = name()?;
            let recs = match a.pos.get(2).map(String::as_str) {
                None => return Err("missing the record: a number, a JSON object, or - for JSON lines on stdin".into()),
                Some("-") => data::parse_records(&stdin())?,
                Some(w) => vec![data::parse_record(w)?],
            };
            st.data_append(n, a.get("--as"), &recs)?;
        }
        "put" => {
            let n = name()?;
            let recs = data::parse_records(&stdin())?;
            let kept = st.data_put(n, a.get("--as"), &recs)?;
            println!("dataset {n}: {kept} record{}", if kept == 1 { "" } else { "s" });
        }
        "show" => {
            let n = name()?;
            let r = Registry::load_strict(&st.dir())?;
            let d = r.datasets.get(n).ok_or_else(|| st.missing_data(n))?;
            let last = match a.get("--last") {
                None => 10,
                Some(v) => v.parse::<usize>().ok().filter(|k| *k >= 1).ok_or_else(|| format!("bad --last {v:?}: a number of records"))?,
            };
            let p = st.data_path(n, d);
            let t = data::read_tail(&p, last, data::format_of(&p)).map_err(|e| format!("dataset {n}: cannot read {} ({e})", p.display()))?;
            if a.has("--json") {
                println!("{}", json!({"v": 1, "name": n, "path": p.display().to_string(), "external": d.file.is_some(), "owner": d.owner, "cap": d.cap, "expect_every": d.expect_every, "stale": st.data_stale(n, d, util::epoch_now()).is_some(), "bytes": std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0), "bytes_read": t.bytes, "records": t.records}));
                return Ok(());
            }
            for r in &t.records {
                println!("{r}");
            }
        }
        "ls" => {
            let r = Registry::load_strict(&st.dir())?;
            let now = util::epoch_now();
            let rows: Vec<(&String, &Dataset, std::path::PathBuf, Option<u64>, Option<f64>, Option<String>)> = r
                .datasets
                .iter()
                .map(|(n, d)| {
                    let p = st.data_path(n, d);
                    (n, d, p.clone(), std::fs::metadata(&p).ok().map(|m| m.len()), data::last_change(&p), st.data_stale(n, d, now))
                })
                .collect();
            if a.has("--json") {
                let v: Vec<Value> = rows.iter().map(|(n, d, p, len, at, stale)| json!({"name": n, "path": p.display().to_string(), "external": d.file.is_some(), "owner": d.owner, "cap": d.cap, "expect_every": d.expect_every, "added": d.added, "bytes": len, "changed": at.map(util::rfc3339), "stale": stale.is_some()})).collect();
                println!("{}", json!({"v": 1, "datasets": v}));
                return Ok(());
            }
            if rows.is_empty() {
                println!("no datasets in project {}", st.project);
            }
            for (n, d, p, len, at, stale) in rows {
                let size = match len {
                    None => "missing".to_string(),
                    Some(b) if b < 1024 => format!("{b} B"),
                    Some(b) if b < 1024 * 1024 => format!("{:.1} kB", b as f64 / 1024.0),
                    Some(b) => format!("{:.1} MB", b as f64 / 1048576.0),
                };
                let changed = at.map(|t| format!("changed {}", util::ago(now - t))).unwrap_or_else(|| "no records yet".into());
                let health = match (stale, d.expect_every) {
                    (Some(why), _) => format!("STALE: {why}"),
                    (None, Some(e)) => format!("{changed} (expected every {})", owner::fmt_secs(e)),
                    (None, None) => changed,
                };
                let mut notes = Vec::new();
                notes.extend(d.file.as_ref().map(|_| format!("reads {}", p.display())));
                notes.extend(d.cap.map(|c| format!("cap {c}")));
                notes.extend(d.owner.as_ref().map(|o| format!("owner {o}")));
                println!("{:<14} {:>9}  {health}{}", n, size, if notes.is_empty() { String::new() } else { format!("  ({})", notes.join(", ")) });
            }
        }
        "rm" => {
            let n = name()?;
            match st.data_rm(n)? {
                Some(kept) => println!("removed dataset {n}; its records are kept in {}", kept.display()),
                None => println!("removed dataset {n}"),
            }
        }
        s => return Err(format!("unknown data command {s:?}: add, set, put, append, show, ls, rm")),
    }
    Ok(())
}

/// Where a systemd user unit goes when nobody says: `$XDG_CONFIG_HOME/systemd/user`.
fn default_unit_dir() -> Result<std::path::PathBuf, String> {
    let base = match std::env::var("XDG_CONFIG_HOME") {
        Ok(d) if !d.is_empty() => std::path::PathBuf::from(d),
        _ => std::path::PathBuf::from(std::env::var("HOME").map_err(|_| "$HOME isn't set: pass --unit-dir".to_string())?).join(".config"),
    };
    Ok(base.join("systemd/user"))
}

fn unit_name(project: &str) -> String {
    format!("margin-{project}.service")
}

/// The unit `margin daemon install` writes: this binary, this store, this project, restarted
/// when it fails, started with the user's session.
fn unit_text(st: &Store) -> String {
    let home = std::path::absolute(&st.home).unwrap_or_else(|_| st.home.clone());
    format!(
        "# written by `margin daemon install`; remove with `margin daemon uninstall`\n[Unit]\nDescription=margin daemon for project {p}: dispatches events to handlers\n\n[Service]\nExecStart={bin} daemon --stay --project {p} --home {home}\nRestart=on-failure\nRestartSec=5\n# what handlers find their tools on when a registration carries no PATH of its own\nEnvironment=PATH={path}\n\n[Install]\nWantedBy=default.target\n",
        p = st.project,
        bin = help::bin_path().display(),
        home = home.display(),
        path = std::env::var("PATH").unwrap_or_default()
    )
}

fn has_systemd() -> bool {
    std::path::Path::new("/run/systemd/system").exists() && std::env::var_os("PATH").map(|p| std::env::split_paths(&p).any(|d| d.join("systemctl").is_file())).unwrap_or(false)
}

/// `margin daemon [install|uninstall|status]`.
fn daemon_cmd(a: &Args) -> Result<(), String> {
    let st = store(a);
    let unit_dir = || match a.get("--unit-dir") {
        Some(d) => std::path::absolute(d).map_err(|e| format!("--unit-dir {d}: {e}")),
        None if a.has("--unit-dir") => Err("--unit-dir needs a directory".into()),
        None => default_unit_dir(),
    };
    let unit = unit_name(&st.project);
    if !a.pos.is_empty() {
        // only the running daemon outlives its stdout; its subcommands end quietly on a closed pipe
        unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    }
    match a.pos.first().map(String::as_str) {
        None => daemon::run(st, a.has("--stay")),
        Some("install") => {
            let p = unit_dir()?.join(&unit);
            store::write_atomic(&p, unit_text(&st).as_bytes()).map_err(|e| format!("{}: {e}", p.display()))?;
            println!("wrote {}", p.display());
            if has_systemd() {
                println!("enable it (margin does not run systemctl for you):\n  systemctl --user daemon-reload && systemctl --user enable --now {unit}");
                println!("it then starts when you log in and restarts if it fails; to have it running after a reboot before you log in:\n  loginctl enable-linger \"$USER\"");
            } else {
                println!("systemd is not running here, so nothing will start that unit. Without it the daemon still comes back:");
                println!("  - on demand: the next margin command that appends an event (emit, act, a key in the pane) starts it,\n    and handlers resume from their cursors, so nothing appended in between is lost");
                println!("  - or at boot from cron:  @reboot {} daemon --stay --project {} --home {}", help::bin_path().display(), st.project, std::path::absolute(&st.home).unwrap_or_else(|_| st.home.clone()).display());
            }
            Ok(())
        }
        Some("uninstall") => {
            let p = unit_dir()?.join(&unit);
            match std::fs::remove_file(&p) {
                Ok(()) => println!("removed {}\nstop it if it is running:\n  systemctl --user disable --now {unit}", p.display()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => println!("no unit at {}: nothing to remove", p.display()),
                Err(e) => return Err(format!("{}: {e}", p.display())),
            }
            Ok(())
        }
        Some("status") => {
            let now = util::epoch_now();
            let running = daemon::running(&st);
            let up = daemon::since(&st).filter(|_| running.is_some());
            let dir = unit_dir()?;
            let installed = dir.join(&unit).exists();
            let enabled = dir.join("default.target.wants").join(&unit).exists();
            let hs = handler::Registry::load_strict(&st.dir())?;
            let rows: Vec<Value> = hs
                .handlers
                .iter()
                .map(|h| {
                    let s = handler::read_status(&st, &h.name).unwrap_or_default();
                    let state = if h.paused { "paused".to_string() } else if running.is_none() { "stopped".into() } else { s.state.clone() };
                    json!({"name": h.name, "on": h.on, "state": state, "pending": handler::pending(&st, h).len(), "cursor": st.cursor(&h.cursor()), "last": s.last_run_text(now), "last_error": if matches!(s.last_outcome.as_str(), "failed" | "skipped") { s.last_error.clone() } else { String::new() }})
                })
                .collect();
            // which binary it runs, and whether that is the pinned build (null: nothing pinned)
            let (exe, on_pin) = running.filter(|p| *p > 0).map(|p| daemon::binary(&st, p)).unwrap_or((None, None));
            if a.has("--json") {
                println!("{}", json!({"v": 1, "project": st.project, "running": running.is_some(), "pid": running.unwrap_or(0), "since": up.map(util::rfc3339), "binary": exe.as_ref().map(|e| e.display().to_string()), "pinned": on_pin, "unit": {"name": unit, "installed": installed, "enabled": enabled}, "handlers": rows}));
                return Ok(());
            }
            match (running, up) {
                // it holds the lock and has not written its pid yet: just started
                (Some(0), _) => println!("daemon: starting for project {}", st.project),
                (Some(p), Some(t)) => println!("daemon: running for project {}, pid {p}, up since {} ({})", st.project, util::rfc3339(t), util::ago(now - t)),
                (Some(p), None) => println!("daemon: running for project {}, pid {p}", st.project),
                _ => println!("daemon: not running for project {}", st.project),
            }
            if let Some(e) = &exe {
                println!(
                    "binary: {}{}",
                    e.display(),
                    match on_pin {
                        Some(true) => " (the pinned build)".to_string(),
                        Some(false) => format!(" (not the pinned build, {}: it moves there once its runs in flight finish)", pin::show(&st.home)),
                        None => String::new(),
                    }
                );
            }
            println!(
                "after a reboot: {}",
                match (installed, enabled) {
                    (true, true) => format!("the unit {unit} is installed and enabled: it starts at login"),
                    (true, false) => format!("the unit {unit} is installed but not enabled (systemctl --user enable --now {unit}); until then the next event appended starts the daemon"),
                    _ => "no unit (margin daemon install): the next margin command that appends an event starts it, and handlers resume from their cursors".to_string(),
                }
            );
            if rows.is_empty() {
                println!("handlers: none (margin handler add …)");
            }
            for r in &rows {
                println!("  {:<14} {:<9} {:<24} waiting {:<3} {}", r["name"].as_str().unwrap_or(""), r["state"].as_str().unwrap_or(""), vellum::text::truncate(&format!("on {}", r["on"].as_array().map(|v| v.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(" ")).unwrap_or_default()), 24), r["pending"], r["last"].as_str().unwrap_or(""));
            }
            Ok(())
        }
        Some(s) => Err(format!("unknown daemon command {s:?}: install, uninstall, status (none: run it)")),
    }
}

/// `margin handle open`: the built-in handler for `<board>.open` events. The pane's own code
/// for opening a target lives here now: the pane appends the event and runs nothing.
fn handle_open(input: &str, cfg: &Config) -> Result<(), String> {
    let dry = std::env::var("MARGIN_DRY_OPEN").ok().filter(|f| !f.is_empty());
    for (i, l) in input.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        let e: store::Event = serde_json::from_str(l).map_err(|e| format!("stdin line {}: not an event ({e})", i + 1))?;
        let Some(target) = e.value["target"].as_str().filter(|t| !t.is_empty()) else {
            eprintln!("seq {}: {}.{} has no target to open", e.seq, e.artifact, e.id);
            continue;
        };
        if let Some(f) = &dry {
            use std::io::Write;
            let mut out = std::fs::OpenOptions::new().create(true).append(true).open(f).map_err(|e| format!("$MARGIN_DRY_OPEN {f}: {e}"))?;
            writeln!(out, "{target}").map_err(|e| e.to_string())?;
            continue;
        }
        let r = if target.starts_with("http://") || target.starts_with("https://") {
            select::open_url(target, cfg)
        } else if let Some(t) = select::parse_token(target) {
            let st = Store::open_cwd();
            if t.verb == "open" {
                // mg:open/<artifact>: show that artifact where the host puts panes
                let art = t.artifact.clone().unwrap_or_else(|| t.id.clone());
                if st.exists(&t.id) { host::pick(cfg, None).open(&st, &t.id, &host::OpenOpts::default(), cfg).map(|_| ()) } else { Err(format!("{target}: no board {art:?} to show")) }
            } else {
                let art = t.artifact.clone().unwrap_or_else(|| e.artifact.clone());
                select::apply_in(&st, &art, &t, "handler", cfg).map(|_| ())
            }
        } else {
            Err(format!("{target:?} is not a URL or an mg: token"))
        };
        if let Err(err) = r {
            eprintln!("seq {}: {err}", e.seq);
        }
    }
    Ok(())
}

/// `margin rm <art>`: what would go, and with `yes` the removal. The store never deletes
/// (`Store::archive`, as `margin gc`): the artifact's directory moves to `.archive/`, so a
/// mistaken removal is one `mv` from undone. Without `yes` nothing
/// changes and the command fails, so a script can't take the preview for the removal.
fn rm_artifact(st: &Store, art: &str, yes: bool, panes: &[String]) -> Result<String, String> {
    let d = st.load(art)?;
    let mut out = format!("{art}  \"{}\"  rev {}  updated {}\n", d.title, d.rev, d.updated);
    out += &format!("  components: {}\n", d.ids().len());
    out += &match panes.len() {
        0 => "  panes showing it: none\n".to_string(),
        _ => format!("  panes showing it (left open; margin close {art}): {}\n", panes.join(", ")),
    };
    if !yes {
        eprint!("{out}");
        return Err(format!("nothing removed: `margin rm {art} --yes` moves it to the project's .archive/ directory"));
    }
    let dest = st.archive(art)?;
    Ok(format!("{out}removed {art}: archived as {}\n", dest.strip_prefix(st.dir()).unwrap_or(&dest).display()))
}

/// What sulin sees, as text: content merged with his decisions.
/// A board to add as a tab is there: `entry` is `board`, or `project:board` for another
/// project's. The error names what is missing, the project or the board.
fn tab_exists(st: &Store, entry: &str) -> Result<(), String> {
    let (fst, board) = tabs::store_for(st, entry);
    if fst.project != st.project && !fst.dir().is_dir() {
        let mut have = store::projects(&st.home);
        have.retain(|p| p != "zellij" && !p.starts_with('.'));
        have.sort();
        return Err(format!("no project {:?} in {} (projects there: {})", fst.project, st.home.display(), if have.is_empty() { "none".to_string() } else { have.join(" ") }));
    }
    if !fst.exists(&board) {
        return Err(fst.missing(&board));
    }
    Ok(())
}

/// The live panes that show `art` as a tab of another artifact: (pane handle, the pane's own
/// artifact). A view file outlives its pane, so each is checked against the host. `beside`: only
/// panes in the caller's Zellij tab (where `margin open` would put a new one).
fn tab_hosts(st: &Store, h: &dyn host::Host, art: &str, a: &Args, beside: bool) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (primary, pane) in tabs::hosts_of(st, art) {
        for handle in h.find(st, &primary).unwrap_or_default() {
            if tabs::pane_key(&handle) != pane {
                continue;
            }
            if beside && h.name() == "zellij" {
                let here = (|| {
                    let sess = zj::session(a.get("--session"))?;
                    let tab = a.get("--tab-id").and_then(|t| t.parse().ok()).or_else(|| zj::home_tab(&sess).ok())?;
                    Some(zj::list_panes(&sess).ok()?.iter().any(|p| p.handle() == handle && p.tab_id == tab))
                })();
                if here != Some(true) {
                    continue;
                }
            }
            out.push((handle, primary.clone()));
        }
    }
    out
}

fn show_text(d: &Doc, s: &store::State, claims: &[store::Claim], delivered: u64) -> String {
    let by = if d.by.is_empty() { String::new() } else { format!(" by {}", d.by) };
    let mut out = format!("{} \"{}\" rev {}{by}\n", d.id, d.title, d.rev);
    if !d.purpose.is_empty() {
        out += &format!("purpose: {}\n", d.purpose);
    }
    for c in claims {
        out += &format!("  ({} {} owns the ids {} and {}-…: read-only for you)\n", c.kind, c.feed, c.owns, c.owns);
    }
    doc::walk(&d.blocks, &mut |b, _, depth| {
        let pad = "  ".repeat(depth + 1);
        let dec = s.decisions.get(&b.id);
        let st = match (b.kind.as_str(), dec) {
            ("todo", _) => {
                let rests: Vec<String> = components::rests_on(s, b).into_iter().map(|(c, struck)| if struck { format!("{c} STRUCK") } else { c }).collect();
                let r = if rests.is_empty() { String::new() } else { format!(" [rests on {}]", rests.join(", ")) };
                format!("[{}]{r}", components::status_of(s, b))
            }
            ("question", Some(d)) if d.kind == "pick" => format!("[sulin picked {}]", d.value.as_str().unwrap_or("")),
            ("question", Some(d)) if d.kind == "talk" => "[open; sulin wants to talk about it in chat before picking]".to_string(),
            ("question", Some(d)) if d.kind == "hold" => format!("[open; default {}; HELD by sulin: stop at {} and wait for their pick]", b.s("default"), if b.s("applies").is_empty() { "the point it applies" } else { b.s("applies") }),
            ("question", _) if b.s("default").is_empty() => format!("[open{}]", if b.s("suggest").is_empty() { String::new() } else { format!("; you suggested {}", b.s("suggest")) }),
            ("question", _) => format!(
                "[open; default {}{}{}]",
                b.s("default"),
                if b.s("applies").is_empty() { String::new() } else { format!("; taken at {}", b.s("applies")) },
                if b.flag("applied") { ", applied" } else { "" }
            ),
            ("claim", Some(d)) if d.kind == "strike" => format!("[STRUCK by sulin{}]", d.reason.as_ref().map(|r| format!(": {r}")).unwrap_or_default()),
            ("diff", Some(d)) => format!("[{}]", d.value.as_str().unwrap_or("")),
            ("status", _) => format!("[{} {}]", b.s("state"), b.s("result")),
            ("row", _) => [b.s("tag"), b.s("aside"), b.s("meta")].iter().filter(|x| !x.is_empty()).map(|x| format!("[{x}]")).collect::<Vec<_>>().join(" "),
            _ => String::new(),
        };
        let text = [b.s("title"), b.s("text"), b.s("path")].into_iter().find(|t| !t.is_empty()).unwrap_or("");
        out += &format!("{pad}{} {} {} {}\n", b.kind, b.id, st, text);
        for (_, n) in store::notes_on(d, s, &b.id) {
            let q = n.value["quote"].as_str().unwrap_or("");
            out += &format!("{pad}  NOTE from sulin: {}{}\n", n.value["r"].as_str().unwrap_or("?"), if q.is_empty() { String::new() } else { format!(" on “{q}”") });
        }
        if let Some(pr) = b.f.get("proposal").filter(|v| v.is_object()) {
            let what = format!("{}{}", pr["verb"].as_str().unwrap_or("?"), pr["value"].as_str().map(|v| format!(" {v}")).unwrap_or_default());
            let state = match s.decisions.get(&format!("{}#proposal", b.id)) {
                Some(d) if d.value["at"] == pr["at"] => if d.value["accepted"] == serde_json::json!(true) { "accepted by sulin" } else { "declined by sulin" },
                _ => "open",
            };
            out += &format!("{pad}  proposal: {what} [{state}]\n");
        }
    });
    let ids: std::collections::HashSet<String> = d.ids().into_iter().collect();
    for (k, v) in &s.decisions {
        if !ids.contains(k) && !k.contains('#') && crate::who::is_human(&v.by) && v.seq > delivered {
            out += &format!("  ORPHANED decision on removed {k}: {} {}\n", v.kind, v.value);
        }
    }
    for h in &d.hints {
        out += &format!("hint: {h}\n");
    }
    out
}

/// `margin render --section S… --focus ID`: choose what a frame shows. Each `--section` names a
/// section of a `tabs` block and puts it on show; `--focus` puts the focus on a block and brings
/// the sections that hold it on show. An id the artifact doesn't have is an error that names it:
/// a frame drawn as if nothing was asked reads as "the flag did nothing".
fn render_view(d: &Doc, v: &mut store::View, sections: Vec<&str>, focus: Option<&str>) -> Result<(), String> {
    for sec in sections {
        if d.find(sec).is_none() {
            return Err(format!("--section {sec:?}: no component with that id in {}; margin show {}", d.id, d.id));
        }
        if d.parent_of(sec).map(|p| p.kind != "tabs").unwrap_or(true) {
            return Err(format!("--section {sec:?}: not a section of a tabs block (its children are the sections; margin show {})", d.id));
        }
        scene::show_in_tabs(d, v, sec);
    }
    if let Some(f) = focus {
        if d.find(f).is_none() {
            return Err(format!("--focus {f:?}: no component with that id in {}; margin show {}", d.id, d.id));
        }
        scene::show_in_tabs(d, v, f);
        v.focus = Some(f.to_string());
    }
    Ok(())
}

/// Replace an artifact's reminders (`margin hints`).
fn set_hints(st: &Store, art: &str, hints: Vec<String>) -> Result<u64, String> {
    set_doc_field(st, art, |d| if d.hints == hints { false } else { d.hints = hints.clone(); true })
}

/// Change a document-level field (`hints`, `purpose`): `change` edits the document and says
/// whether anything changed. The document is otherwise written back as it was read; a write that
/// landed in between (a handler's, an owner's) is taken up and the change redone.
fn set_doc_field(st: &Store, art: &str, change: impl Fn(&mut Doc) -> bool) -> Result<u64, String> {
    let mut last = String::new();
    for _ in 0..8 {
        let mut d = st.load(art)?;
        let base = d.rev;
        if !change(&mut d) {
            return Ok(base);
        }
        match st.replace(d, Some(base)) {
            Ok(rev) => return Ok(rev),
            Err(e) if e.starts_with("stale base") => last = e,
            Err(e) => return Err(e),
        }
    }
    Err(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Args {
        Args::parse(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    /// `events --follow` sleeps on the log's watch, not on a clock: an event is seen as it is
    /// appended (the 200 ms poll this replaced saw it 100 ms late on average), and one appended
    /// before the wait starts is not missed.
    #[test]
    fn a_follow_sees_an_event_as_it_lands() {
        let st = store::tests::temp_store();
        let emit = |st: &Store| st.emit("ping", "", "", serde_json::Value::Null, "you", "emit").unwrap().seq;
        let mut w = st.watch_events();
        // appended after the watch was armed and before the wait: seen at once
        let a = emit(&st);
        assert!(follow_next(&st, a - 1, &mut w));
        let st2 = Store::new(st.home.clone(), st.project.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        let t = std::thread::spawn(move || {
            for from in a..a + 3 {
                assert!(follow_next(&st2, from, &mut w));
                tx.send(std::time::Instant::now()).unwrap();
            }
        });
        for _ in 0..3 {
            // long enough for the follower to be asleep again
            std::thread::sleep(std::time::Duration::from_millis(40));
            emit(&st);
            let emitted = std::time::Instant::now();
            let seen = rx.recv_timeout(std::time::Duration::from_secs(5)).expect("the follower never woke");
            let late = seen.saturating_duration_since(emitted);
            assert!(late < std::time::Duration::from_millis(150), "seen {late:?} after the emit returned");
        }
        t.join().unwrap();
        std::fs::remove_dir_all(&st.home).ok();
    }

    /// Side-effecting commands refuse what they don't take instead of acting on it.
    #[test]
    fn unknown_flags_and_surplus_args_are_refused() {
        let spec = |n: &str| CMDS.iter().find(|c| c.name == n).unwrap();
        for (cmd, v) in [("daemon", &["--foo"][..]), ("session", &["a", "b"]), ("set", &["a", "b", "c", "d", "--nope"]), ("daemon", &["status", "x"])] {
            let s = spec(cmd);
            assert!(check_args(cmd, &args(v), s.flags, s.max_pos).is_err(), "{cmd} {v:?}");
        }
        let s = spec("set");
        assert!(check_args("set", &args(&["a", "b", "c", "d", "--by", "you", "--home", "/h"]), s.flags, s.max_pos).is_ok());
        assert!(wants_help(&["x".into(), "-h".into()]) && !wants_help(&["x".into()]));
        assert!(CMDS.iter().all(|c| !c.help.is_empty()));
    }

    /// A store in a throwaway home, and the flags that point a command at it.
    fn tmp_store(name: &str) -> (Store, Vec<String>) {
        let home = std::env::temp_dir().join(format!("margin-main-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let st = Store::new(home.clone(), "t".into());
        (st, vec!["--home".into(), home.display().to_string(), "--project".into(), "t".into(), "--by".into(), "claude".into()])
    }
    fn go(cmd: &str, v: &[&str], flags: &[String]) -> Result<(), String> {
        let mut all: Vec<String> = v.iter().map(|s| s.to_string()).collect();
        all.extend(flags.iter().cloned());
        run(cmd, &Args::parse(&all))
    }

    /// `margin new <art>` with no document (unit tests have no stdin to give).
    fn mk(art: &str, flags: &[String]) {
        let mut all = vec![art.to_string()];
        all.extend(flags.iter().cloned());
        let a = Args::parse(&all);
        new_cmd(&store(&a), art, "", &a).unwrap();
    }

    /// `margin new` is the one command that creates an artifact: empty, or from a document; a
    /// second `new` is refused and leaves what is there alone.
    #[test]
    fn new_creates_an_artifact_and_refuses_one_that_exists() {
        let (st, f) = tmp_store("new");
        let a = |v: &[&str]| Args::parse(&v.iter().map(|s| s.to_string()).chain(f.iter().cloned()).collect::<Vec<_>>());
        new_cmd(&st, "a1", "", &a(&[])).unwrap();
        let d = st.load("a1").unwrap();
        assert_eq!((d.title.as_str(), d.rev, d.blocks.len(), d.by.as_str()), ("a1", 1, 0, "claude"));
        new_cmd(&st, "a2", "", &a(&["--title", "Fix CI", "--purpose", "green  by Friday", "--kind", "plan"])).unwrap();
        let d = st.load("a2").unwrap();
        assert_eq!((d.title.as_str(), d.purpose.as_str(), d.kind.as_str()), ("Fix CI", "green by Friday", "plan"));
        // a whole document, and a bare array of blocks
        new_cmd(&st, "a3", r#"{"v":1,"title":"Doc","hints":["k  key"],"blocks":[{"type":"todo","id":"t1","text":"one"}]}"#, &a(&[])).unwrap();
        let d = st.load("a3").unwrap();
        assert_eq!((d.title.as_str(), d.hints.len(), d.find("t1").is_some()), ("Doc", 1, true));
        new_cmd(&st, "a4", r#"[{"type":"todo","id":"t1","text":"one"}]"#, &a(&[])).unwrap();
        assert_eq!((st.load("a4").unwrap().title.as_str(), st.load("a4").unwrap().ids()), ("a4", vec!["t1".to_string()]));
        // a second time: refused, whatever it carries, and nothing changes
        for input in ["", r#"{"title":"Other","blocks":[]}"#] {
            let e = new_cmd(&st, "a3", input, &a(&[])).unwrap_err();
            assert_eq!(e, "a3 already exists (rev 1); change it with margin apply/add/set, or remove it with margin rm a3 --yes");
        }
        assert_eq!((st.load("a3").unwrap().title.as_str(), st.load("a3").unwrap().rev), ("Doc", 1));
        // ops and single components are updates, not a document to start from; a bad block creates nothing
        for input in [r#"{"type":"todo","id":"t1","text":"one"}"#, r#"{"op":"rm","id":"t1"}"#, "not json"] {
            let e = new_cmd(&st, "a5", input, &a(&[])).unwrap_err();
            assert!(e.contains("not a whole document") && e.contains("margin new a5"), "{e}");
        }
        assert!(new_cmd(&st, "a5", r#"[{"type":"nope","id":"x"}]"#, &a(&[])).is_err() && !st.exists("a5"));
        assert!(new_cmd(&st, "bad name", "", &a(&[])).is_err());
        // the same name again once the first is removed
        go("rm", &["a3", "--yes"], &f).unwrap();
        new_cmd(&st, "a3", "", &a(&[])).unwrap();
        let _ = std::fs::remove_dir_all(&st.home);
    }

    /// The opposite of what it once was (`add_and_ask_create_the_artifact`): `add` and `ask`
    /// create a component in an artifact that exists, and refuse an artifact that doesn't.
    #[test]
    fn add_and_ask_refuse_a_missing_artifact() {
        let (st, f) = tmp_store("create");
        for (cmd, v) in [("add", &["a1", "-", "section", "now", "title=Now"][..]), ("ask", &["a1", "Q1", "Which?", "x", "y"])] {
            assert_eq!(go(cmd, v, &f).unwrap_err(), "no board \"a1\" in project t: create it with margin new a1", "{cmd}");
        }
        assert!(!st.exists("a1") && !st.art("a1").exists());
        mk("a1", &f);
        go("add", &["a1", "-", "section", "now", "title=Now"], &f).unwrap();
        go("add", &["a1", "now", "todo", "t1", "first", "step"], &f).unwrap();
        let d = st.load("a1").unwrap();
        assert_eq!((d.title.as_str(), d.rev, d.find("t1").unwrap().s("text")), ("a1", 3, "first step"));
        go("ask", &["a1", "Q1", "Which?", "x", "y", "--suggest", "y"], &f).unwrap();
        let d = st.load("a1").unwrap();
        let q = d.find("Q1").unwrap();
        assert_eq!((q.kind.as_str(), q.s("suggest"), q.s("default")), ("question", "y", ""), "a suggestion, no default");
        // inside the artifact the same split: create refuses an id that exists, change one that doesn't
        let e = go("ask", &["a1", "Q1", "Again?", "x", "y"], &f).unwrap_err();
        assert!(e.contains("margin set a1 Q1"), "{e}");
        assert!(go("add", &["a1", "now", "todo", "t1", "again"], &f).is_err());
        assert!(go("set", &["a1", "t9", "text=x"], &f).unwrap_err().contains("no component \"t9\""));
        // a near miss names the artifact that was meant, and creates nothing
        let e = go("add", &["a2", "-", "todo", "t1", "x"], &f).unwrap_err();
        assert!(e.ends_with("create it with margin new a2 (did you mean \"a1\"?)") && !st.exists("a2"), "{e}");
        let _ = std::fs::remove_dir_all(&st.home);
    }

    /// Every command that writes to an artifact refuses one that isn't there, with the one
    /// message that names `margin new` and the nearest name, and creates nothing: a mistyped
    /// name used to make a second board.
    #[test]
    fn every_writing_command_refuses_a_missing_artifact_with_the_hint() {
        let (st, f) = tmp_store("missing");
        mk("plan", &f);
        go("add", &["plan", "-", "todo", "t1", "one"], &f).unwrap();
        let want = "no board \"paln\" in project t: create it with margin new paln (did you mean \"plan\"?)";
        let a = |v: &[&str]| Args::parse(&v.iter().map(|s| s.to_string()).chain(f.iter().cloned()).collect::<Vec<_>>());
        let walk: Vec<(&str, Vec<&str>)> = vec![
            ("add", vec!["paln", "-", "todo", "t2", "two"]),
            ("set", vec!["paln", "t1", "text=x"]),
            ("rm", vec!["paln", "t1"]),
            ("rm", vec!["paln", "--yes"]),
            ("move", vec!["paln", "t1", "--after", "t1"]),
            ("ask", vec!["paln", "q1", "Which?", "x", "y"]),
            ("title", vec!["paln", "Plan"]),
            ("purpose", vec!["paln", "why"]),
            ("hints", vec!["paln", "k  key"]),
            ("propose", vec!["paln", "t1", "tick", "done"]),
            ("diff", vec!["paln", "d1", "--old", "/nonexistent-a", "--new", "/nonexistent-b"]),
            ("image", vec!["paln", "i1", "/nonexistent.png"]),
            ("toast", vec!["paln", "hello"]),
            ("show", vec!["paln"]),
            ("link", vec!["paln", "--session", "s"]),
        ];
        for (cmd, v) in &walk {
            assert_eq!(go(cmd, v, &f).unwrap_err(), want, "{cmd}");
        }
        // the commands that read stdin, by the function `run` hands it to
        let whole = r#"{"title":"P","blocks":[]}"#;
        let comp = r#"{"type":"todo","id":"t2","text":"two"}"#;
        let op = r#"{"op":"set","id":"t1","fields":{"text":"x"}}"#;
        for (what, input, bare) in [("apply a whole document", whole, false), ("apply a component", comp, false), ("apply an op", op, false)] {
            assert_eq!(apply_cmd(&st, "paln", input, &a(&[]), bare, Whole::Replace).unwrap_err(), want, "{what}");
        }
        // doc writes a component through apply_cmd
        let sec = mdoc::to_section("notes", None, "# Notes\n\nA line.\n");
        assert_eq!(apply_cmd(&st, "paln", &sec.to_string(), &a(&[]), false, Whole::Replace).unwrap_err(), want, "doc");
        // nothing was created on the way, and the artifact beside it is as it was
        assert!(!st.exists("paln") && !st.art("paln").exists());
        assert_eq!(st.list().len(), 1);
        assert_eq!(st.load("plan").unwrap().rev, 2);
        // a whole-document apply to an artifact that exists is still an update
        apply_cmd(&st, "plan", r#"{"title":"Plan","blocks":[{"type":"todo","id":"t1","text":"one"}]}"#, &a(&[]), false, Whole::Replace).unwrap();
        assert_eq!((st.load("plan").unwrap().title.as_str(), st.load("plan").unwrap().rev), ("Plan", 3));
        let _ = std::fs::remove_dir_all(&st.home);
    }

    /// The demo creates its artifact and resets its own, and never replaces a `plan` that is
    /// someone's work.
    #[test]
    fn demo_resets_its_own_plan_and_refuses_anyone_elses() {
        if diffblock::difft_bin().is_err() {
            eprintln!("skipped: difft is not installed");
            return;
        }
        let (st, f) = tmp_store("demo");
        assert_eq!(demo::setup(&st).unwrap(), 1);
        assert_eq!(demo::setup(&st).unwrap(), 2, "its own: reset");
        go("rm", &["plan", "--yes"], &f).unwrap();
        mk("plan", &f);
        go("add", &["plan", "-", "todo", "t1", "mine"], &f).unwrap();
        let e = demo::setup(&st).unwrap_err();
        assert!(e.starts_with("plan already exists (rev 2) and is not the demo's") && e.contains("margin rm plan --yes"), "{e}");
        assert!(st.load("plan").unwrap().find("t1").is_some());
        let _ = std::fs::remove_dir_all(&st.home);
    }

    /// `set <field>=` (or an empty value) removes the field instead of storing "".
    #[test]
    fn set_with_an_empty_value_removes_the_field() {
        let (st, f) = tmp_store("unset");
        mk("p", &f);
        go("add", &["p", "-", "todo", "t1", "step", "note=200 runs"], &f).unwrap();
        assert_eq!(st.load("p").unwrap().find("t1").unwrap().s("note"), "200 runs");
        go("set", &["p", "t1", "note=", "text=renamed"], &f).unwrap();
        let d = st.load("p").unwrap();
        let t = d.find("t1").unwrap();
        assert!(!t.f.contains_key("note") && t.s("text") == "renamed", "{:?}", t.f);
        go("set", &["p", "t1", "note", "again"], &f).unwrap();
        go("set", &["p", "t1", "note", ""], &f).unwrap();
        assert!(!st.load("p").unwrap().find("t1").unwrap().f.contains_key("note"));
        let _ = std::fs::remove_dir_all(&st.home);
    }

    /// Events show local time, and the date only when the event isn't from today.
    #[test]
    fn event_times_are_local_with_the_date_when_not_today() {
        let now = util::epoch_now();
        let today = local_stamp(&util::rfc3339(now), now);
        assert_eq!((today.len(), today.matches(':').count()), (8, 2), "{today}");
        assert!(today.starts_with(&util::local_hm(now)), "local, not UTC: {today}");
        let old = local_stamp(&util::rfc3339(now - 3.0 * 86400.0), now);
        assert_eq!((old.len(), old.matches('-').count()), (19, 2), "{old}");
        assert_eq!(local_stamp("garbage", now), "garbage");
    }

    /// `margin --help` is the core, then one short section for the Claude Code group; nothing
    /// before that section, and no core command's own help, names Claude. Every subcommand of the
    /// group is a command whose help is spelled `margin claude <name>`, and the group's listing
    /// shows each.
    #[test]
    fn the_help_is_the_core_then_one_claude_section() {
        let (core, group) = HELP.split_once("\nClaude Code (").expect("the section");
        assert!(!core.to_lowercase().contains("claude"), "the core part of --help names Claude");
        assert!(group.lines().count() <= 3 && group.trim_end().ends_with("hook <event>"), "one short section, last: {group}");
        for must in ["boards:", "events (", "handlers and outside writers", "datasets (", "panes and screen"] {
            assert!(core.contains(must), "{must}");
        }
        for name in CLAUDE_CMDS {
            let c = CMDS.iter().find(|c| c.name == *name).unwrap_or_else(|| panic!("{name} is not a command"));
            assert!(c.help.starts_with(&format!("margin claude {name}")), "{name}'s help is spelled the old way: {}", c.help.lines().next().unwrap());
            assert!(CLAUDE_HELP.contains(&format!("margin claude {name}")), "margin claude --help doesn't list {name}");
            assert!(group.contains(name), "the section of --help doesn't list {name}");
            // and no line of it teaches the spelling from before the group
            for old in ["margin hook ", "margin init", "margin skill", "margin link", "margin unlink", "margin stop", "margin propose"] {
                // (init says which spelling it rewrites, in backticks with an ellipsis)
                assert!(!c.help.replace("`margin hook …`", "").contains(old), "{name}'s help says {old:?}");
            }
        }
        for c in CMDS.iter().filter(|c| !CLAUDE_CMDS.contains(&c.name)) {
            assert!(!c.help.to_lowercase().contains("claude"), "margin {} --help names Claude: it is a core command", c.name);
        }
        for (what, text) in [("global flags", GLOBAL_HELP), ("handler", HANDLER_HELP)] {
            assert!(!text.to_lowercase().contains("claude"), "{what}");
        }
        for sub in ["add", "ls", "rm", "pause", "logs", "run"] {
            assert!(!handler_sub_help(sub).unwrap().to_lowercase().contains("claude"), "handler {sub}");
        }
        // the write's default author inside a session is the adapter's, and said in its help
        assert!(CLAUDE_HELP.contains("logged as by claude") && CLAUDE_ONLY.iter().all(|c| CLAUDE_CMDS.contains(c)));
    }

    /// The help Claude reads names no one: every command's help goes out through `who::say`.
    #[test]
    fn init_skill_and_help_are_commands() {
        for c in ["init", "skill"] {
            assert!(CMDS.iter().any(|x| x.name == c), "{c}");
        }
        assert!(HELP.contains("margin claude init") && HELP.contains("margin help [<command>|<topic>]"));
        assert!(args(&["--dry-run", "--claude-dir", "/x"]).has("--dry-run"), "--dry-run is a switch");
        assert_eq!(args(&["--dry-run", "--claude-dir", "/x"]).get("--claude-dir"), Some("/x"));
        // no topic shadows a command
        assert!(help::TOPICS.iter().all(|(t, _)| CMDS.iter().all(|c| c.name != *t)));
    }

    /// `render --section` picks a tabs block's section, `--focus` inside a hidden section brings
    /// it on show, and an id that isn't there is an error naming it.
    #[test]
    fn render_section_and_focus_choose_the_tab_and_refuse_unknown_ids() {
        let d: Doc = serde_json::from_value(json!({"id": "a", "title": "A", "blocks": [
            {"type": "tabs", "id": "views", "children": [
                {"type": "section", "id": "mine", "title": "Mine", "children": [{"type": "row", "id": "m1", "text": "one"}]},
                {"type": "section", "id": "team", "title": "Team", "children": [{"type": "row", "id": "t1", "text": "two"}]}]},
            {"type": "row", "id": "out", "text": "outside"}]})).unwrap();
        let mut v = store::View::default();
        render_view(&d, &mut v, vec!["team"], None).unwrap();
        assert_eq!(v.subtabs.get("views").map(String::as_str), Some("team"));
        render_view(&d, &mut v, vec!["mine"], None).unwrap();
        assert!(v.subtabs.is_empty(), "the first section is the default");
        render_view(&d, &mut v, vec![], Some("t1")).unwrap();
        assert_eq!((v.subtabs.get("views").map(String::as_str), v.focus.as_deref()), (Some("team"), Some("t1")));
        render_view(&d, &mut v, vec![], Some("out")).unwrap();
        assert_eq!(v.subtabs.get("views").map(String::as_str), Some("team"), "a focus outside the tabs leaves them");
        for (sec, focus, names) in [(vec!["nope"], None, "nope"), (vec!["out"], None, "out"), (vec!["m1"], None, "m1"), (vec![], Some("gone"), "gone")] {
            let e = render_view(&d, &mut v, sec, focus).unwrap_err();
            assert!(e.contains(&format!("{names:?}")), "{e}");
        }
        // the frame itself: the section asked for is the one drawn
        let (st, f) = tmp_store("render");
        st.put(d, None).unwrap();
        assert!(go("render", &["a", "--focus", "gone"], &f).unwrap_err().contains("\"gone\""));
        go("render", &["a", "--section", "team", "--focus", "t1"], &f).unwrap();
        assert!(RENDER_HELP.lines().filter(|l| l.starts_with("  --")).count() >= 10, "a line per flag");
        for flag in CMDS.iter().find(|c| c.name == "render").unwrap().flags {
            assert!(RENDER_HELP.contains(&format!("  {flag} ")), "render --help doesn't explain {flag}");
        }
        let _ = std::fs::remove_dir_all(&st.home);
    }

    /// The thing is a board everywhere a person or an assistant reads: every help text, the
    /// schema, the skill, the topics, the examples' steps and the hint for a missing board. The
    /// old word stays only as the name of the JSON field (`"artifact"`, `"artifacts"`,
    /// `.artifact` in a jq filter), which stored logs and outside tools' parsers hold.
    #[test]
    fn nothing_a_reader_sees_says_artifact_except_the_json_field() {
        let mut texts: Vec<(String, String)> = vec![("margin --help".into(), HELP.into()), ("global flags".into(), GLOBAL_HELP.into())];
        texts.extend(CMDS.iter().map(|c| (format!("margin {} --help", c.name), c.help.to_string())));
        for sub in ["add", "set", "ls", "rm", "pause", "resume", "logs", "run"] {
            if let Some(h) = handler_sub_help(sub) {
                texts.push((format!("margin handler {sub} --help"), h.into()));
            }
        }
        texts.push(("margin schema".into(), doc::schema_text(None).unwrap()));
        texts.push(("margin schema --json".into(), doc::schema_json(None).unwrap().to_string()));
        let c = help::Ctx::new("", std::path::Path::new("/opt/margin/bin/margin"));
        texts.push(("margin skill".into(), help::render(claude::SKILL, &c)));
        texts.push(("margin help".into(), help::topic_list(&c)));
        texts.extend(help::TOPICS.iter().map(|(n, t)| (format!("margin help {n}"), help::render(t, &c))));
        texts.push(("margin example".into(), example::list()));
        texts.extend(example::EXAMPLES.iter().map(|e| (format!("margin example {}", e.name), e.notes())));
        texts.extend(example::EXAMPLES.iter().map(|e| (format!("margin example {} --script", e.name), e.script.to_string())));
        texts.push(("margin example runs".into(), example::RUNS.notes()));
        texts.push(("margin example runs --tool".into(), example::RUNS.tool.into()));
        let (st, _) = tmp_store("words");
        texts.push(("the hint for a missing board".into(), st.missing("paln")));
        texts.push(("the hint for a pane's own tab".into(), tabs::edit(&mut st.view("a", "tty"), "a", "rm", "a").unwrap_err()));
        for (what, text) in &texts {
            for line in text.lines() {
                let rest = line.replace("\"artifact\"", "").replace("\"artifacts\"", "").replace("`artifact`", "").replace(".artifact", "");
                assert!(!rest.to_lowercase().contains("artifact"), "{what} says \"artifact\": {line}");
            }
        }
        let _ = std::fs::remove_dir_all(&st.home);
    }

    /// The feed runner is gone: no `feed` command, no help that names one, no `feed` block type.
    /// One way keeps a board current (a handler, an owner or a dataset), so nothing a reader
    /// can print points at a second.
    #[test]
    fn no_help_names_a_feed_command() {
        assert!(CMDS.iter().all(|c| c.name != "feed") && doc::kind("feed").is_none());
        assert!(!HELP.lines().any(|l| l.starts_with("legacy")), "no legacy heading is left");
        let mut texts: Vec<(String, String)> = vec![("margin --help".into(), HELP.into()), ("global flags".into(), GLOBAL_HELP.into())];
        texts.extend(CMDS.iter().map(|c| (format!("margin {} --help", c.name), c.help.to_string())));
        for sub in ["add", "set", "ls", "rm", "pause", "resume", "logs", "run"] {
            texts.extend(handler_sub_help(sub).map(|h| (format!("margin handler {sub} --help"), h.to_string())));
        }
        let c = help::Ctx::new("", std::path::Path::new("/opt/margin/bin/margin"));
        texts.push(("margin skill".into(), help::render(claude::SKILL, &c)));
        texts.push(("margin help".into(), help::topic_list(&c)));
        texts.extend(help::TOPICS.iter().map(|(n, t)| (format!("margin help {n}"), help::render(t, &c))));
        texts.push(("margin schema".into(), doc::schema_text(None).unwrap()));
        texts.push(("margin example".into(), example::list()));
        texts.extend(example::EXAMPLES.iter().map(|e| (format!("margin example {}", e.name), e.notes())));
        texts.extend(example::EXAMPLES.iter().map(|e| (format!("margin example {} --script", e.name), e.script.to_string())));
        texts.push(("margin example runs".into(), example::RUNS.notes()));
        for (what, text) in &texts {
            for line in text.lines() {
                let l = line.to_lowercase();
                assert!(!l.contains("margin feed") && !l.contains("help feeds") && !l.contains("--feed"), "{what} names the feed command: {line}");
                // the word stays only in `margin doctor`'s help, which says what it does with
                // a store that still holds the removed runner's files
                if !what.starts_with("margin doctor") {
                    assert!(!l.split(|c: char| !c.is_alphanumeric()).any(|w| w == "feed" || w == "feeds"), "{what} says \"feed\": {line}");
                }
            }
        }
    }

    /// The parked commands keep an accurate help, each marked, and `margin --help` lists them
    /// apart from the documented set.
    #[test]
    fn parked_commands_are_marked_in_their_help() {
        let mark = "not part of the documented set; may be removed";
        for name in ["image", "demo"] {
            let c = CMDS.iter().find(|c| c.name == name).unwrap();
            assert!(c.help.contains(mark), "margin {name} --help");
        }
        let lines: Vec<&str> = HELP.lines().collect();
        let at = lines.iter().position(|l| *l == format!("parked ({mark}):")).expect("a parked heading");
        for (i, cmd) in ["margin image ", "margin demo live "].iter().enumerate() {
            assert!(lines[at + 1 + i].trim_start().starts_with(cmd), "{}", lines[at + 1 + i]);
            assert_eq!(lines.iter().filter(|l| l.contains(cmd.trim_end())).count(), 1, "{cmd} is listed once, under parked");
        }
    }

    /// `margin rm <art>`: a preview and a refusal without --yes; with it the artifact is archived
    /// (never deleted), and a second one of the same name can follow.
    #[test]
    fn rm_of_an_artifact_previews_then_archives() {
        let (st, f) = tmp_store("rmart");
        mk("scratch", &f);
        mk("keep", &f);
        go("add", &["scratch", "-", "todo", "t1", "one"], &f).unwrap();
        go("title", &["scratch", "Scratch board"], &f).unwrap();
        go("add", &["keep", "-", "todo", "t1", "stays"], &f).unwrap();
        let e = rm_artifact(&st, "scratch", false, &["pid 7".into()]).unwrap_err();
        assert!(e.contains("nothing removed") && e.contains("margin rm scratch --yes"), "{e}");
        assert!(st.exists("scratch"), "the preview changes nothing");
        assert!(go("rm", &["scratch"], &f).is_err() && st.exists("scratch"));
        let out = rm_artifact(&st, "scratch", true, &["pid 7".into()]).unwrap();
        for must in ["\"Scratch board\"", "rev 3", "pid 7", "removed scratch: archived as .archive/scratch-"] {
            assert!(out.contains(must), "{must}: {out}");
        }
        assert!(!st.exists("scratch") && st.exists("keep"));
        let kept: Vec<_> = std::fs::read_dir(st.dir().join(".archive")).unwrap().map(|e| e.unwrap().path()).collect();
        assert!(kept.len() == 1 && kept[0].join("doc.json").exists(), "archived whole: {kept:?}");
        // the same name again on the same day gets its own place in the archive
        mk("scratch", &f);
        go("add", &["scratch", "-", "todo", "t1", "again"], &f).unwrap();
        go("rm", &["scratch", "--yes"], &f).unwrap();
        assert_eq!(std::fs::read_dir(st.dir().join(".archive")).unwrap().count(), 2);
        assert!(rm_artifact(&st, "scratch", true, &[]).unwrap_err().contains("no board"));
        // a component removal is unchanged, and --yes isn't its flag
        assert!(go("rm", &["keep", "t1", "--yes"], &f).is_err() && st.load("keep").unwrap().find("t1").is_some());
        go("rm", &["keep", "t1"], &f).unwrap();
        assert!(st.exists("keep") && st.load("keep").unwrap().find("t1").is_none());
        let _ = std::fs::remove_dir_all(&st.home);
    }

    #[test]
    fn by_names_the_writer() {
        assert_eq!(author(&args(&["--by", "you"])).unwrap(), crate::who::HUMAN);
        assert_eq!(author(&args(&["--by", "claude"])).unwrap(), "claude");
        assert_eq!(author(&args(&["--by", "jess"])).unwrap(), "jess");
        assert!(author(&args(&["--by", "handler:x"])).is_err());
        assert!(author(&args(&["--by"])).is_err());
    }
}
