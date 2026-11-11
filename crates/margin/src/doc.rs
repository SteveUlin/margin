//! The artifact document: a tree of typed components keyed by id, and the id-addressed operations
//! the AI uses to change it. The AI writes meaning (`todo`, `question`, `claim`), never layout.

use crate::util::valid_id;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Block {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Block>,
    #[serde(flatten)]
    pub f: Map<String, Value>,
}

impl Block {
    pub fn s(&self, k: &str) -> &str {
        self.f.get(k).and_then(|v| v.as_str()).unwrap_or("")
    }
    pub fn opt(&self, k: &str) -> Option<&str> {
        self.f.get(k).and_then(|v| v.as_str()).filter(|s| !s.is_empty())
    }
    pub fn num(&self, k: &str) -> Option<f64> {
        self.f.get(k).and_then(|v| v.as_f64())
    }
    pub fn flag(&self, k: &str) -> bool {
        self.f.get(k).and_then(|v| v.as_bool()).unwrap_or(false)
    }
    pub fn strs(&self, k: &str) -> Vec<String> {
        self.f
            .get(k)
            .and_then(|v| v.as_array())
            .map(|a| a.iter().map(|x| x.as_str().map(String::from).unwrap_or_else(|| x.to_string())).collect())
            .unwrap_or_default()
    }
    /// Stable content hash (for reconcile: same id + different hash = changed).
    pub fn hash(&self) -> u64 {
        // children hash separately, so a changed child doesn't mark its whole section changed
        let mut shallow = self.clone();
        shallow.children.clear();
        crate::util::fnv64(serde_json::to_string(&shallow).unwrap_or_default().as_bytes())
    }
}

/// A field's type in the component schema. `Text` fields always store strings (`set … text 2026`
/// keeps "2026"), so a value never guesses its way into a type the renderer can't draw.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ty {
    Text,
    Num,
    Bool,
    /// a list of strings (`options`)
    List,
    /// a list of numbers (`spark`)
    Nums,
    /// one of these strings
    Enum(&'static [&'static str]),
    /// any JSON (machine-written payloads: a diff's rows, a proposal)
    Any,
}

impl Ty {
    pub fn name(&self) -> String {
        match self {
            Ty::Text => "text".into(),
            Ty::Num => "number".into(),
            Ty::Bool => "bool".into(),
            Ty::List => "list of text".into(),
            Ty::Nums => "list of numbers".into(),
            Ty::Enum(v) => format!("one of {}", v.join(" ")),
            Ty::Any => "json".into(),
        }
    }
    fn tag(&self) -> &'static str {
        match self {
            Ty::Text => "text",
            Ty::Num => "number",
            Ty::Bool => "bool",
            Ty::List => "list",
            Ty::Nums => "numbers",
            Ty::Enum(_) => "enum",
            Ty::Any => "json",
        }
    }
}

const TONES: &[&str] = &["green", "ok", "yellow", "warn", "red", "bad", "orange", "changed", "blue", "claude", "aqua", "purple", "you", "grey", "dim", "fg"];
/// One todo status vocabulary for both writers: Claude's `set … status` and sulin's tick menu.
pub const STATUSES: &[&str] = &["todo", "doing", "done", "blocked", "skipped", "next"];

/// One component type: what it means, its fields, whether it holds children, sulin's action.
pub struct Kind {
    pub name: &'static str,
    pub about: &'static str,
    pub fields: &'static [(&'static str, Ty, &'static str)],
    pub children: bool,
    pub action: &'static str,
}

/// What `actions` holds, said once for every type that takes it.
const ACTIONS_NOTE: &str = "list of {key, name, label?}: with this block focused, the key appends the event <board>.<name> carrying the block's id and a snapshot of it; a handler registered for that name does the work (margin help handlers). key: one character the pane doesn't use itself; label: what the hint row shows (default: name). A table's actions apply to each of its rows";

const PROPOSAL: (&str, Ty, &str) = ("proposal", Ty::Any, "written by margin propose");

/// The component schema, checked on every write (CLI, apply, feeds) and printed by `margin
/// schema`, so the skill and the guide point here instead of keeping their own tables. A new
/// type is a row here plus its drawing in components.rs.
pub const SCHEMA: &[Kind] = &[
    Kind { name: "section", about: "a titled group", children: true, action: "fold (click the header)",
        fields: &[("title", Ty::Text, "the header line"), ("note", Ty::Text, "right of the title, shown open"), ("summary", Ty::Text, "right of the title, shown folded"), ("folded", Ty::Bool, "starts folded"), ("tone", Ty::Enum(TONES), "the header's color")] },
    Kind { name: "tabs", about: "views of one dashboard: its children are sections, their titles a tab strip, one section's content shown at a time", children: true, action: "switch (click a tab, ]t / [t)",
        fields: &[] },
    Kind { name: "text", about: "a paragraph", children: false, action: "",
        fields: &[("text", Ty::Text, "the paragraph; wraps; **bold** `code` @[name]"), ("style", Ty::Enum(&["note", "dim", "warn"]), "note: an aside, dim: grey, warn: yellow"), ("tone", Ty::Enum(TONES), "the text's color")] },
    Kind { name: "code", about: "monospace lines: a fenced block or a table (margin doc makes them)", children: false, action: "",
        fields: &[("text", Ty::Text, "lines, kept as written"), ("language", Ty::Text, "the fence's language, as a label")] },
    Kind { name: "bullet", about: "a bullet; children are sub-bullets", children: true, action: "",
        fields: &[("text", Ty::Text, "one idea; wraps; **bold** `code` @[name]"), ("tone", Ty::Enum(TONES), "the text's color")] },
    Kind { name: "todo", about: "a plan step", children: false, action: "tick (mg:todo/<id>, space)",
        fields: &[("text", Ty::Text, "the step; wraps; **bold** `code` @[name]"), ("note", Ty::Text, "dim text after the step: a result, a reason"), ("status", Ty::Enum(STATUSES), "default todo; next: sulin's mark for what to do next"), ("after", Ty::List, "ids of the steps in this section it waits on (drawn as a graph)"), ("rests_on", Ty::List, "ids of the claims this step assumes; striking one flags the step"), ("owner", Ty::Enum(&["you", "claude"]), "you: sulin's own step; Claude stops there and they mark it done"), PROPOSAL] },
    Kind { name: "progress", about: "a bar: a section's done todos, or a value", children: false, action: "",
        fields: &[("of", Ty::Text, "a section id"), ("value", Ty::Num, "0-1"), ("label", Ty::Text, "text before the bar")] },
    Kind { name: "timer", about: "elapsed since, or left until", children: false, action: "",
        fields: &[("since", Ty::Text, "RFC 3339"), ("until", Ty::Text, "RFC 3339"), ("label", Ty::Text, "text before the time")] },
    Kind { name: "status", about: "a check or job", children: false, action: "open",
        fields: &[("text", Ty::Text, "what is checked"), ("state", Ty::Enum(&["running", "pass", "fail"]), "its glyph; fail marks the tab"), ("result", Ty::Text, "a short outcome at the right: exit 2, 14 passed"), ("spark", Ty::Nums, "recent values, drawn as a sparkline"), ("target", Ty::Text, "URL or mg: token"), ("actions", Ty::Any, ACTIONS_NOTE)] },
    Kind { name: "question", about: "a choice sulin makes", children: false, action: "pick (1-9, mg:pick/<id>=<opt>)",
        fields: &[("text", Ty::Text, "the question"), ("options", Ty::List, "the answers, two or more; each is a button"), ("suggest", Ty::Text, "one of options: your recommendation, a line on the card; no option is highlighted until sulin picks"), ("default", Ty::Text, "one of options; only when sulin asked for a default"), ("why", Ty::Text, "why it matters or what each answer costs; a line on the card"), ("applies", Ty::Text, "a step id (the default is taken when that step starts), or prose"), ("takes_at", Ty::Any, "RFC 3339: the default is taken then (a countdown on the card)"), ("applied", Ty::Bool, "you took the default"), ("applied_at", Ty::Text, "set for you when applied goes true"), PROPOSAL] },
    Kind { name: "claim", about: "an assumption sulin can strike", children: false, action: "strike (mg:strike/<id>, x)",
        fields: &[("text", Ty::Text, "the assumption; wraps; **bold** `code` @[name]"), ("ack", Ty::Text, "your reply once it is struck, shown under it"), PROPOSAL] },
    Kind { name: "decided", about: "a settled decision", children: false, action: "",
        fields: &[("text", Ty::Text, "what was decided; wraps; **bold**"), ("by", Ty::Text, "claude or you"), ("at", Ty::Text, "when: RFC 3339, or a short label")] },
    Kind { name: "link", about: "a link", children: false, action: "open",
        fields: &[("text", Ty::Text, "the label shown"), ("target", Ty::Text, "URL or mg: token"), ("actions", Ty::Any, ACTIONS_NOTE)] },
    Kind { name: "image", about: "a PNG", children: false, action: "",
        fields: &[("path", Ty::Text, "PNG"), ("alt", Ty::Text, "shown where images can't draw (default: the path)"), ("rows", Ty::Num, "most rows it takes (default 18)"), ("cols", Ty::Num, "most cells wide (default: the pane)")] },
    Kind { name: "diff", about: "a structural diff (made by margin diff)", children: false, action: "approve / reject / ask",
        fields: &[("path", Ty::Text, "the file's label in the header"), ("language", Ty::Any, "what difftastic detected"), ("adds", Ty::Num, "added lines"), ("dels", Ty::Num, "removed lines"), ("rows", Ty::Any, "the diff's lines, written by margin diff"), ("max_rows", Ty::Num, "rows shown before it is cut"), ("text", Ty::Text, "a plain-text diff when there are no rows"), PROPOSAL] },
    Kind { name: "row", about: "one line of a ledger or monitor", children: true, action: "open (click or enter, when it has a target)",
        fields: &[("text", Ty::Text, "the line; cut at the edge, in full when focused; **bold** `code` @[name]"), ("glyph", Ty::Text, "spin done (ok) fail you (bell) perm (lock) idle ended commit wc file term, or any character; fail marks the tab"), ("tone", Ty::Enum(TONES), "the glyph's color"), ("tag", Ty::Text, "a short pill before the text: a ticket, a kind"), ("tag_tone", Ty::Enum(TONES), "the pill's color"), ("aside", Ty::Text, "dim text right of the text: exit 1, 3 files"), ("aside_tone", Ty::Enum(TONES), "the aside's color"), ("since", Ty::Any, "RFC 3339 or unix seconds"), ("meta", Ty::Text, "dim text at the far right, when there is no since"), ("detail", Ty::Text, "longer text under the row: wraps, three rows at most until the row has the focus"), ("text_tone", Ty::Enum(TONES), "the text's color"), ("bold", Ty::Bool, "the whole text bold"), ("who", Ty::Text, "a name, drawn as its badge (glyph + name in the name's color) before the text"), ("target", Ty::Text, "URL or mg: token, opened on click or enter"), ("actions", Ty::Any, ACTIONS_NOTE)] },
    Kind { name: "feed", about: "a feed's health line (written by the daemon)", children: false, action: "",
        fields: &[("name", Ty::Text, "the feed's name"), ("state", Ty::Text, "not registered, live, restarting, failed, paused, removed, stopped"), ("trigger", Ty::Text, "every 30s, watch <path>, stream"), ("since", Ty::Any, "RFC 3339: when the state began"), ("until", Ty::Any, "RFC 3339: the next restart (a countdown)"), ("last", Ty::Any, "RFC 3339: its last write"), ("restarts", Ty::Num, "restarts so far"), ("note", Ty::Text, "the last error, shown under the line")] },
    Kind { name: "rule", about: "a horizontal line: a break between parts of a dashboard", children: false, action: "",
        fields: &[("text", Ty::Text, "a label inset at the left")] },
    Kind { name: "table", about: "a table: a header and aligned columns; its children are tr rows", children: true, action: "",
        fields: &[("columns", Ty::Any, "list of {key, title?, kind?: text num who id runs age state (default text), align?: left right center, width?: cells, optional?: true to drop it first when the pane is narrow, summary?: true on a runs column to end each strip with its failure count such as 1/6}"), ("zebra", Ty::Bool, "stripe alternate rows (default: config dashboard.zebra, off)"), ("empty", Ty::Text, "shown while it has no rows"), ("actions", Ty::Any, ACTIONS_NOTE)] },
    Kind { name: "tr", about: "one table row, only inside a table; give it a stable id so a rewrite reconciles", children: false, action: "open (click, when it has a target)",
        fields: &[("cells", Ty::Any, "object keyed by column key: text (**bold** `code` @[name]), a number, a name (who), an id, a list of ok fail skip running or null (runs), an RFC 3339 time (age), a row glyph name (state)"), ("target", Ty::Text, "URL or mg: token, opened on click"), ("detail", Ty::Text, "longer text about the row, shown in the detail panel (d)"), ("actions", Ty::Any, ACTIONS_NOTE)] },
    Kind { name: "runs", about: "a run strip: the last n outcomes as small squares, oldest left, a cross for a failure", children: false, action: "",
        fields: &[("label", Ty::Text, "text at the left: what ran"), ("values", Ty::Any, "list of ok fail skip running, or null for no run"), ("summary", Ty::Bool, "end with a count such as 2/20 failed")] },
    Kind { name: "plot", about: "a plot drawn in the pane: lines, an area, bars, columns, a histogram, sparklines or heat strips", children: false, action: "",
        fields: &[("kind", Ty::Enum(&["line", "area", "bars", "columns", "hist", "spark", "heat"]), "which plot (default line)"), ("series", Ty::Any, "list of {label, values, color?}: values are numbers, null for a gap; color is a palette name"), ("labels", Ty::List, "bars and columns: one label per value of the first series"), ("height", Ty::Num, "rows for line, area, columns and hist (default 8)"), ("threshold", Ty::Num, "a reference line at this value"), ("x", Ty::List, "line and area: the labels at the left and right ends of the x axis"), ("legend", Ty::Bool, "line: name each series under the plot")] },
];

/// The fields of the document itself: the envelope a whole-document `margin apply` takes
/// (main.rs `apply_cmd`). `id`, `rev`, `updated` and `by` are margin's and never written.
pub const DOC_FIELDS: &[(&str, Ty, &str)] = &[
    ("title", Ty::Text, "the pane's title line and its tab's name (margin title; default: the artifact id)"),
    ("purpose", Ty::Text, "what the pane is for, one line pinned at the top (margin purpose)"),
    ("hints", Ty::List, "reminders at the pane's bottom, one each; `key  text` (two spaces) draws a key (margin hints)"),
    ("kind", Ty::Text, "a free label for what the artifact is (plan, dashboard); listed by margin ls --json, never drawn"),
    ("actions", Ty::Any, "list of {key, name, label?}: actions of the whole board, offered on every block that has none of its own under that key (see a row's actions)"),
    ("blocks", Ty::Any, "the top-level components, in order: a list of {type, id, fields…, children}"),
    ("v", Ty::Num, "the format version, optional: 1"),
];

/// Kinds that carry sulin's decisions (tick, pick, strike, verdict): a write that leaves their id
/// out gets one derived from the text, never from position.
pub const DECIDABLE: &[&str] = &["todo", "question", "claim", "diff"];

/// An id from text: the first words, lowercased, joined by `-`, at most 24 characters.
pub fn slug(text: &str) -> String {
    let mut out = String::new();
    for w in text.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()) {
        if !out.is_empty() && out.len() + 1 + w.len() > 24 {
            break;
        }
        if !out.is_empty() {
            out.push('-');
        }
        out.push_str(&w.to_ascii_lowercase());
    }
    out.truncate(24);
    out
}

pub fn kind(name: &str) -> Option<&'static Kind> {
    SCHEMA.iter().find(|k| k.name == name)
}

fn lev(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + (a[i - 1] != b[j - 1]) as usize);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// " (did you mean X?)" for the closest candidate within reach, else "".
pub fn did_you_mean<'a>(word: &str, cands: impl IntoIterator<Item = &'a str>) -> String {
    match cands.into_iter().map(|c| (lev(word, c), c)).min() {
        Some((d, c)) if d <= 2.max(word.len() / 3) || (word.len() >= 3 && (c.starts_with(word) || word.starts_with(c))) => format!(" (did you mean {c}?)"),
        _ => String::new(),
    }
}

/// Check one field's value against the schema. `null` always passes: it deletes the field.
pub fn check_field(k: &Kind, field: &str, v: &Value) -> Result<(), String> {
    let Some((_, ty, _)) = k.fields.iter().find(|(f, _, _)| *f == field) else {
        let fields: Vec<&str> = k.fields.iter().map(|f| f.0).collect();
        return Err(format!("unknown field {field:?} on {}{}; its fields: {}", k.name, did_you_mean(field, fields.iter().copied()), fields.join(" ")));
    };
    if v.is_null() {
        return Ok(());
    }
    let ok = match ty {
        Ty::Text => v.is_string(),
        Ty::Num => v.is_number(),
        Ty::Bool => v.is_boolean(),
        Ty::List => v.as_array().map(|a| a.iter().all(|x| x.is_string())).unwrap_or(false),
        Ty::Nums => v.as_array().map(|a| a.iter().all(|x| x.is_number())).unwrap_or(false),
        Ty::Any => true,
        Ty::Enum(opts) => match v.as_str() {
            Some(s) if opts.contains(&s) => true,
            Some(s) => return Err(format!("{} {field} {s:?}: {}{}", k.name, ty.name(), did_you_mean(s, opts.iter().copied()))),
            None => false,
        },
    };
    if ok {
        Ok(())
    } else {
        Err(format!("{} {field} must be {}, got {v}", k.name, ty.name()))
    }
}

/// A CLI word as the field's value: text and enum fields keep the string as typed; other fields
/// parse as JSON (numbers, bools, lists), falling back to the string, which the check then names.
pub fn coerce(kind_name: &str, field: &str, raw: &str) -> Value {
    match kind(kind_name).and_then(|k| k.fields.iter().find(|f| f.0 == field)).map(|f| f.1) {
        Some(Ty::Text) | Some(Ty::Enum(_)) => Value::String(raw.into()),
        _ => serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.into())),
    }
}

/// A table column's kinds: how its cells are read and drawn.
pub const COL_KINDS: &[&str] = &["text", "num", "who", "id", "runs", "age", "state"];
const ALIGNS: &[&str] = &["left", "right", "center"];

/// A run strip's values: a list of outcome names, null where nothing ran.
fn check_runs(v: &Value, what: &str) -> Result<(), String> {
    let a = v.as_array().ok_or_else(|| format!("{what} must be a list of {} or null, got {v}", vellum::widgets::OUTCOMES.join(" ")))?;
    for x in a {
        match x {
            Value::Null => {}
            Value::String(s) if vellum::widgets::OUTCOMES.contains(&s.as_str()) => {}
            Value::String(s) => return Err(format!("{what}: {s:?} isn't a run outcome{}; use {} or null", did_you_mean(s, vellum::widgets::OUTCOMES.iter().copied()), vellum::widgets::OUTCOMES.join(" "))),
            other => return Err(format!("{what}: {other} isn't a run outcome; use {} or null", vellum::widgets::OUTCOMES.join(" "))),
        }
    }
    Ok(())
}

/// A table's columns as (key, kind), checked: a list of objects, each with a unique `key`, a
/// known `kind` and `align`, a positive `width`.
pub fn check_columns(table: &Block) -> Result<Vec<(String, String)>, String> {
    let id = &table.id;
    let Some(v) = table.f.get("columns").filter(|v| !v.is_null()) else { return Ok(Vec::new()) };
    let cols = v.as_array().ok_or_else(|| format!("{id}: table columns must be a list of {{key, title, kind}}, got {v}"))?;
    let mut out: Vec<(String, String)> = Vec::new();
    for c in cols {
        let o = c.as_object().ok_or_else(|| format!("{id}: a column must be an object with a key, got {c}"))?;
        let key = o.get("key").and_then(|k| k.as_str()).filter(|k| !k.is_empty()).ok_or_else(|| format!("{id}: a column needs a key, got {c}"))?;
        if out.iter().any(|(k, _)| k == key) {
            return Err(format!("{id}: two columns share the key {key:?}"));
        }
        for f in o.keys() {
            let known = ["key", "title", "kind", "align", "width", "optional", "summary"];
            if !known.contains(&f.as_str()) {
                return Err(format!("{id}: column {key:?} has an unknown field {f:?}{}; its fields: {}", did_you_mean(f, known.iter().copied()), known.join(" ")));
            }
        }
        let word = |f: &str, opts: &[&'static str], dflt: &str| -> Result<String, String> {
            match o.get(f) {
                None | Some(Value::Null) => Ok(dflt.to_string()),
                Some(Value::String(s)) if opts.contains(&s.as_str()) => Ok(s.clone()),
                Some(Value::String(s)) => Err(format!("{id}: column {key:?} {f} {s:?}: one of {}{}", opts.join(" "), did_you_mean(s, opts.iter().copied()))),
                Some(other) => Err(format!("{id}: column {key:?} {f} must be one of {}, got {other}", opts.join(" "))),
            }
        };
        let kind = word("kind", COL_KINDS, "text")?;
        word("align", ALIGNS, "left")?;
        if let Some(t) = o.get("title").filter(|t| !t.is_null() && !t.is_string()) {
            return Err(format!("{id}: column {key:?} title must be text, got {t}"));
        }
        if let Some(w) = o.get("width").filter(|w| !w.is_null()) {
            if w.as_u64().filter(|n| (1..=500).contains(n)).is_none() {
                return Err(format!("{id}: column {key:?} width must be a number of cells, got {w}"));
            }
        }
        if let Some(x) = o.get("optional").filter(|x| !x.is_null() && !x.is_boolean()) {
            return Err(format!("{id}: column {key:?} optional must be true or false, got {x}"));
        }
        match o.get("summary") {
            None | Some(Value::Null) | Some(Value::Bool(false)) => {}
            Some(Value::Bool(true)) if kind == "runs" => {}
            Some(Value::Bool(true)) => return Err(format!("{id}: column {key:?} summary: only a runs column has a failure count (this one is {kind})")),
            Some(x) => return Err(format!("{id}: column {key:?} summary must be true or false, got {x}")),
        }
        out.push((key.to_string(), kind));
    }
    Ok(out)
}

/// A row's cells against its table's columns: every key names a column, and a runs cell holds
/// run outcomes.
fn check_cells(row: &Block, cols: &[(String, String)]) -> Result<(), String> {
    let Some(v) = row.f.get("cells").filter(|v| !v.is_null()) else { return Ok(()) };
    let cells = v.as_object().ok_or_else(|| format!("{}: tr cells must be an object keyed by column key, got {v}", row.id))?;
    for (k, cell) in cells {
        let Some((_, kind)) = cols.iter().find(|(key, _)| key == k) else {
            let keys: Vec<&str> = cols.iter().map(|(k, _)| k.as_str()).collect();
            return Err(format!("{}: cell {k:?} names no column{}; the table's columns: {}", row.id, did_you_mean(k, keys.iter().copied()), keys.join(" ")));
        };
        if kind == "runs" && !cell.is_null() {
            check_runs(cell, &format!("{}: cell {k:?}", row.id))?;
        } else if cell.is_object() || (cell.is_array() && kind != "runs") {
            return Err(format!("{}: cell {k:?} must be text or a number, got {cell}", row.id));
        }
    }
    Ok(())
}

/// One declared action: the key that fires it, the event name's second half, the hint's words.
#[derive(Clone, Debug, PartialEq)]
pub struct Action {
    pub key: char,
    pub name: String,
    pub label: String,
}

/// An `actions` value, checked: a list of `{key, name, label?}` where each key is one character
/// the pane does not bind itself (`reserved`: app.rs derives it from its own key table, so the
/// two cannot drift), no key or name is used twice in the list, and `name` is an id. `what`
/// names the block in the error.
pub fn parse_actions(v: &Value, what: &str, reserved: &[char]) -> Result<Vec<Action>, String> {
    if v.is_null() {
        return Ok(Vec::new());
    }
    let list = v.as_array().ok_or_else(|| format!("{what}: actions must be a list of {{key, name, label}}, got {v}"))?;
    let mut out: Vec<Action> = Vec::new();
    for a in list {
        let o = a.as_object().ok_or_else(|| format!("{what}: an action must be an object {{key, name, label}}, got {a}"))?;
        if let Some(f) = o.keys().find(|f| !["key", "name", "label"].contains(&f.as_str())) {
            return Err(format!("{what}: an action has an unknown field {f:?}{}; its fields: key name label", did_you_mean(f, ["key", "name", "label"])));
        }
        let name = o.get("name").and_then(|n| n.as_str()).filter(|n| valid_id(n)).ok_or_else(|| format!("{what}: an action needs a name (letters, digits, - and _), got {a}"))?;
        let key = o.get("key").and_then(|k| k.as_str()).unwrap_or("");
        let mut cs = key.chars();
        let k = match (cs.next(), cs.next()) {
            (Some(c), None) if !c.is_whitespace() && !c.is_control() => c,
            _ => return Err(format!("{what}: action {name:?} needs a key of one character, got {key:?}")),
        };
        if reserved.contains(&k) {
            let mut r: Vec<String> = reserved.iter().map(|c| c.to_string()).collect();
            r.sort();
            return Err(format!("{what}: action {name:?} can't use the key {k:?}: the pane uses it (taken: {})", r.join(" ")));
        }
        if let Some(d) = out.iter().find(|x| x.key == k) {
            return Err(format!("{what}: actions {:?} and {name:?} share the key {k:?}", d.name));
        }
        if out.iter().any(|x| x.name == name) {
            return Err(format!("{what}: two actions are named {name:?}"));
        }
        if name == "open" {
            return Err(format!("{what}: open is the action every block with a target has already (enter); name this one something else"));
        }
        let label = match o.get("label") {
            None | Some(Value::Null) => name.to_string(),
            Some(Value::String(l)) if !l.trim().is_empty() => l.clone(),
            Some(l) => return Err(format!("{what}: action {name:?} label must be text, got {l}")),
        };
        out.push(Action { key: k, name: name.to_string(), label });
    }
    Ok(out)
}

/// Check a block and its descendants against the schema, as a top-level block.
pub fn check_block(b: &Block) -> Result<(), String> {
    check_under(b, None)
}

/// Check a block and its descendants against the schema, as a child of `parent` (None = top
/// level). The parent matters to a table: a `tr` lives only in one, and its cells are checked
/// against that table's columns.
pub fn check_under(b: &Block, parent: Option<&Block>) -> Result<(), String> {
    let k = kind(&b.kind).ok_or_else(|| {
        let names: Vec<&str> = SCHEMA.iter().map(|k| k.name).collect();
        format!("unknown component type {:?}{} (known: {})", b.kind, did_you_mean(&b.kind, names.iter().copied()), names.join(", "))
    })?;
    for (f, v) in &b.f {
        check_field(k, f, v).map_err(|e| format!("{}: {e}", b.id))?;
    }
    if !k.children && !b.children.is_empty() {
        return Err(format!("{}: a {} holds no children", b.id, k.name));
    }
    if let Some(v) = b.f.get("actions") {
        parse_actions(v, &b.id, &crate::app::reserved_keys())?;
    }
    if b.kind == "question" {
        let opts = b.strs("options");
        for field in ["default", "suggest"] {
            if let Some(d) = b.opt(field) {
                if !opts.is_empty() && !opts.iter().any(|o| o == d) {
                    return Err(format!("{}: {field} {d:?} isn't one of the options", b.id));
                }
            }
        }
    }
    let in_table = parent.map(|p| p.kind == "table").unwrap_or(false);
    match (b.kind.as_str(), in_table) {
        ("tr", false) => return Err(format!("{}: a tr belongs inside a table (add it with the table as its parent)", b.id)),
        ("tr", true) => check_cells(b, &check_columns(parent.unwrap())?)?,
        (other, true) => return Err(format!("{}: a table holds only tr rows, not a {other}", b.id)),
        _ => {}
    }
    if b.kind == "table" {
        // the columns are parsed once here; each row is checked against them below
        let cols = check_columns(b)?;
        for c in &b.children {
            if c.kind == "tr" {
                check_cells(c, &cols)?;
            }
        }
    }
    if b.kind == "runs" {
        if let Some(v) = b.f.get("values").filter(|v| !v.is_null()) {
            check_runs(v, &format!("{}: runs values", b.id))?;
        }
    }
    b.children.iter().try_for_each(|c| check_under(c, Some(b)))
}

fn pick(only: Option<&str>) -> Result<Vec<&'static Kind>, String> {
    match only {
        Some(n) => Ok(vec![kind(n).ok_or_else(|| format!("unknown type {n:?}{}", did_you_mean(n, SCHEMA.iter().map(|k| k.name))))?]),
        None => Ok(SCHEMA.iter().collect()),
    }
}

/// `document` is not a component type: `margin schema document` prints the envelope's fields.
const DOCUMENT: &str = "document";
const DOCUMENT_ABOUT: &str = "the artifact itself: the envelope of a whole-document `margin apply <art>`; a write without purpose or hints keeps the ones it has";

fn field_json(fields: &[(&str, Ty, &str)]) -> Map<String, Value> {
    fields
        .iter()
        .map(|(f, t, note)| {
            let mut o = serde_json::json!({"type": t.tag()});
            if let Ty::Enum(v) = t {
                o["values"] = serde_json::json!(v);
            }
            if !note.is_empty() {
                o["note"] = serde_json::json!(note);
            }
            (f.to_string(), o)
        })
        .collect()
}

fn field_text(fields: &[(&str, Ty, &str)]) -> String {
    fields.iter().map(|(f, t, note)| format!("  {f:<11} {}{}\n", t.name(), if note.is_empty() { String::new() } else { format!("  ({note})") })).collect()
}

/// `margin schema [type]`, for reading: the document's own fields first, then each component
/// type. Every field has a few words on what it is.
pub fn schema_text(only: Option<&str>) -> Result<String, String> {
    let mut out = String::new();
    if only.is_none() || only == Some(DOCUMENT) {
        out += &format!("{DOCUMENT}: {DOCUMENT_ABOUT}\n{}", field_text(DOC_FIELDS));
        if only.is_some() {
            return Ok(out);
        }
        out += "every component below also takes: type, id (letters, digits, - and _; unique in the artifact), and children where it holds them\n";
    }
    for k in pick(only)? {
        let action = if k.action.is_empty() { String::new() } else { format!("; sulin: {}", k.action) };
        out += &format!("{}: {}{}{action}\n{}", k.name, k.about, if k.children { "; holds children" } else { "" }, field_text(k.fields));
    }
    Ok(out)
}

/// `margin schema [type] --json`.
pub fn schema_json(only: Option<&str>) -> Result<Value, String> {
    let document = serde_json::json!({"about": DOCUMENT_ABOUT, "fields": field_json(DOC_FIELDS)});
    if only == Some(DOCUMENT) {
        return Ok(serde_json::json!({"v": 1, "document": document, "types": {}}));
    }
    let mut m = Map::new();
    for k in pick(only)? {
        m.insert(k.name.into(), serde_json::json!({"about": k.about, "children": k.children, "action": k.action, "fields": field_json(k.fields)}));
    }
    let mut out = serde_json::json!({"v": 1, "types": m});
    if only.is_none() {
        out["document"] = document;
    }
    Ok(out)
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Doc {
    /// format version; a doc written before versions reads as 1
    #[serde(default = "crate::util::v1")]
    pub v: u64,
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub rev: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default)]
    pub blocks: Vec<Block>,
    #[serde(default)]
    pub updated: String,
    /// Claude Code session ids that wrote this artifact.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<String>,
    /// Who wrote this revision: `claude`, or `feed:<name>` (the fourth owner kind).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub by: String,
    /// Reminders the author keeps at the bottom of the pane while this artifact is on screen
    /// (`margin hints`): "r  re-request review", "merge queue freezes Fri 3pm".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<String>,
    /// What this pane is for, in one line (`margin purpose`): the first line of the pinned head.
    /// It stands where a Goal section used to, so the top of the pane goes to what changes.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub purpose: String,
    /// Actions of the whole board (`{key, name, label?}`): offered on every block that has no
    /// action of its own under that key, and with nothing focused.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub actions: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum Op {
    /// Insert `block` into `parent` (None = top level), before/after a sibling, else at the end.
    Add {
        #[serde(default)]
        parent: Option<String>,
        block: Block,
        #[serde(default)]
        before: Option<String>,
        #[serde(default)]
        after: Option<String>,
    },
    /// Set one field. `value: null` removes it.
    Set { id: String, field: String, value: Value },
    Rm { id: String },
    /// Move a block before/after a sibling, or to the end of `parent`.
    Move {
        id: String,
        #[serde(default)]
        before: Option<String>,
        #[serde(default)]
        after: Option<String>,
        #[serde(default)]
        parent: Option<String>,
    },
    Title { value: String },
    /// Replace the component with `block.id` in place (same position, same parent), or add it to
    /// `parent` (None = top level) when it doesn't exist yet. A feed's usual write: one line of
    /// stdout carries the whole component, and reconcile-by-id keeps folds and focus.
    Replace {
        block: Block,
        #[serde(default)]
        parent: Option<String>,
    },
}

impl Op {
    /// The ids this op writes, judged against `doc` before it applies: the target, every
    /// descendant it replaces or removes, and the parent it inserts into. Ownership is checked
    /// against this list (store.rs), so a removal can't take a feed's components with it.
    pub fn touched(&self, doc: &Doc) -> (Vec<String>, Option<String>) {
        fn with_desc(b: &Block, out: &mut Vec<String>) {
            out.push(b.id.clone());
            for c in &b.children {
                with_desc(c, out);
            }
        }
        let mut ids = Vec::new();
        let parent = match self {
            Op::Title { .. } => return (vec![], None),
            Op::Set { id, .. } => {
                ids.push(id.clone());
                None
            }
            Op::Rm { id } => {
                match doc.find(id) {
                    Some(b) => with_desc(b, &mut ids),
                    None => ids.push(id.clone()),
                }
                None
            }
            Op::Add { parent, block, .. } => {
                with_desc(block, &mut ids);
                parent.clone()
            }
            Op::Move { id, parent, before, after } => {
                match doc.find(id) {
                    Some(b) => with_desc(b, &mut ids),
                    None => ids.push(id.clone()),
                }
                // moving next to a sibling lands in that sibling's parent
                parent.clone().or_else(|| {
                    before.as_ref().or(after.as_ref()).and_then(|s| doc.parent_of(s)).map(|p| p.id.clone())
                })
            }
            Op::Replace { block, parent } => {
                with_desc(block, &mut ids);
                if let Some(old) = doc.find(&block.id) {
                    with_desc(old, &mut ids);
                    doc.parent_of(&block.id).map(|p| p.id.clone())
                } else {
                    parent.clone()
                }
            }
        };
        ids.sort();
        ids.dedup();
        (ids, parent)
    }
}

pub fn walk<'a>(blocks: &'a [Block], f: &mut dyn FnMut(&'a Block, Option<&'a Block>, usize)) {
    fn go<'a>(bs: &'a [Block], parent: Option<&'a Block>, depth: usize, f: &mut dyn FnMut(&'a Block, Option<&'a Block>, usize)) {
        for b in bs {
            f(b, parent, depth);
            go(&b.children, Some(b), depth + 1, f);
        }
    }
    go(blocks, None, 0, f)
}

impl Doc {
    /// An artifact with nothing in it yet, titled by its id: what `margin new <art>` creates
    /// when it is given no document.
    #[cfg(test)]
    pub fn empty(id: &str) -> Doc {
        Doc { v: 1, id: id.into(), title: id.into(), kind: String::new(), rev: 0, icon: None, blocks: vec![], updated: String::new(), sessions: vec![], by: String::new(), hints: vec![], purpose: String::new(), actions: Value::Null }
    }
    /// The actions on offer for block `id` (None or empty: the board with nothing focused): the
    /// block's own, then its table's when it is a table row, then the board's; a nearer one wins
    /// its key and its name. `open` is not in the list: every block with a `target` has it.
    pub fn actions_for(&self, id: Option<&str>) -> Vec<Action> {
        let mut out: Vec<Action> = Vec::new();
        let mut take = |v: Option<&Value>| {
            // checked at write time: what is stored parses
            for a in v.and_then(|v| parse_actions(v, "", &[]).ok()).unwrap_or_default() {
                if !out.iter().any(|x| x.key == a.key || x.name == a.name) {
                    out.push(a);
                }
            }
        };
        if let Some(b) = id.filter(|i| !i.is_empty()).and_then(|i| self.find(i)) {
            take(b.f.get("actions"));
            if b.kind == "tr" {
                take(self.parent_of(&b.id).and_then(|t| t.f.get("actions")));
            }
        }
        take(Some(&self.actions));
        out
    }

    pub fn find(&self, id: &str) -> Option<&Block> {
        let mut out = None;
        walk(&self.blocks, &mut |b, _, _| {
            if b.id == id && out.is_none() {
                out = Some(b)
            }
        });
        out
    }

    pub fn parent_of(&self, id: &str) -> Option<&Block> {
        let mut out = None;
        walk(&self.blocks, &mut |b, p, _| {
            if b.id == id {
                out = p
            }
        });
        out
    }

    pub fn ids(&self) -> Vec<String> {
        let mut v = Vec::new();
        walk(&self.blocks, &mut |b, _, _| v.push(b.id.clone()));
        v
    }

    fn find_mut<'a>(bs: &'a mut Vec<Block>, id: &str) -> Option<&'a mut Block> {
        for b in bs.iter_mut() {
            if b.id == id {
                return Some(b);
            }
            if let Some(x) = Self::find_mut(&mut b.children, id) {
                return Some(x);
            }
        }
        None
    }

    /// The sibling list that holds `id`.
    fn list_of<'a>(bs: &'a mut Vec<Block>, id: &str) -> Option<&'a mut Vec<Block>> {
        if bs.iter().any(|b| b.id == id) {
            return Some(bs);
        }
        for b in bs.iter_mut() {
            if let Some(l) = Self::list_of(&mut b.children, id) {
                return Some(l);
            }
        }
        None
    }

    /// Give every block without an id one (`<kind initial><n>`), and check ids are valid and unique.
    pub fn normalize(&mut self) -> Result<(), String> {
        let mut seen = std::collections::HashSet::new();
        let mut n = 0;
        fn go(bs: &mut [Block], seen: &mut std::collections::HashSet<String>, n: &mut usize) -> Result<(), String> {
            for b in bs.iter_mut() {
                if b.id.is_empty() {
                    loop {
                        *n += 1;
                        let c = b.kind.chars().next().unwrap_or('b');
                        let cand = format!("{c}{n}");
                        if !seen.contains(&cand) {
                            b.id = cand;
                            break;
                        }
                    }
                }
                if !valid_id(&b.id) {
                    return Err(format!("bad id {:?}: use letters, digits, - and _", b.id));
                }
                if !seen.insert(b.id.clone()) {
                    return Err(format!("duplicate id {:?}", b.id));
                }
                // these are the block's own keys; a field of the same name serialised a second
                // `"type"` and left doc.json unloadable
                if let Some(k) = ["type", "id", "children"].iter().find(|k| b.f.contains_key(**k)) {
                    return Err(format!("{:?} can't be a field of {:?}: it's the component's own {k}", k, b.id));
                }
                go(&mut b.children, seen, n)?;
            }
            Ok(())
        }
        // pre-collect explicit ids so generated ones never collide with later explicit ones
        let mut explicit = std::collections::HashSet::new();
        walk(&self.blocks, &mut |b, _, _| {
            if !b.id.is_empty() {
                explicit.insert(b.id.clone());
            }
        });
        let mut taken = explicit.clone();
        fn assign(bs: &mut [Block], taken: &mut std::collections::HashSet<String>, n: &mut usize) -> Result<(), String> {
            for b in bs.iter_mut() {
                if b.id.is_empty() && DECIDABLE.contains(&b.kind.as_str()) {
                    // sulin's decisions are keyed by id: a positional id would carry his tick onto
                    // whatever lands in that position next time (2026-11-06 review), so derive it
                    // from the text, which survives reorders and inserts
                    let base = slug(b.s("text"));
                    if base.is_empty() {
                        return Err(format!("a {} without text needs an id: sulin's decisions on it are kept by id", b.kind));
                    }
                    let mut cand = base.clone();
                    let mut k = 1;
                    while !taken.insert(cand.clone()) {
                        k += 1;
                        cand = format!("{base}-{k}");
                    }
                    b.id = cand;
                } else if b.id.is_empty() {
                    let c = b.kind.chars().next().unwrap_or('b');
                    loop {
                        *n += 1;
                        let cand = format!("{c}{n}");
                        if taken.insert(cand.clone()) {
                            b.id = cand;
                            break;
                        }
                    }
                }
                assign(&mut b.children, taken, n)?;
            }
            Ok(())
        }
        assign(&mut self.blocks, &mut taken, &mut n)?;
        go(&mut self.blocks, &mut seen, &mut n)
    }

    pub fn apply(&mut self, op: &Op) -> Result<(), String> {
        match op {
            Op::Title { value } => self.title = value.clone(),
            Op::Add { parent, block, before, after } => {
                check_under(block, parent.as_ref().and_then(|p| self.find(p)))?;
                let mut block = block.clone();
                if !block.id.is_empty() && self.find(&block.id).is_some() {
                    return Err(format!("id {:?} already exists; use set to change it", block.id));
                }
                let list = match parent {
                    None => &mut self.blocks,
                    Some(p) => {
                        &mut Self::find_mut(&mut self.blocks, p).ok_or_else(|| format!("no parent {p:?}"))?.children
                    }
                };
                if block.id.is_empty() {
                    block.id = String::new();
                }
                let at = match (before, after) {
                    (Some(s), _) => list.iter().position(|b| &b.id == s).ok_or_else(|| format!("no sibling {s:?}"))?,
                    (None, Some(s)) => {
                        list.iter().position(|b| &b.id == s).ok_or_else(|| format!("no sibling {s:?}"))? + 1
                    }
                    _ => list.len(),
                };
                list.insert(at, block);
            }
            Op::Set { id, field, value } => {
                let b = Self::find_mut(&mut self.blocks, id).ok_or_else(|| format!("no component {id:?}"))?;
                match field.as_str() {
                    "id" | "type" | "children" => return Err(format!("field {field:?} can't be set; rm and add instead")),
                    _ => {}
                }
                if let Some(k) = kind(&b.kind) {
                    check_field(k, field, value).map_err(|e| format!("{id}: {e}"))?;
                }
                // fields whose value is checked against the block's surroundings (a row's cells
                // against its table's columns, a table's columns against its rows): check the
                // block as it would be, before it changes
                if matches!((b.kind.as_str(), field.as_str()), ("tr", "cells") | ("table", "columns") | ("runs", "values")) {
                    let mut next = b.clone();
                    next.f.insert(field.clone(), value.clone());
                    check_under(&next, self.parent_of(id))?;
                }
                let b = Self::find_mut(&mut self.blocks, id).unwrap();
                if value.is_null() {
                    b.f.remove(field);
                } else {
                    b.f.insert(field.clone(), value.clone());
                }
            }
            Op::Replace { block, parent } => {
                // in place it keeps its parent; new, it lands in `parent`
                let under = if self.find(&block.id).is_some() { self.parent_of(&block.id) } else { parent.as_ref().and_then(|p| self.find(p)) };
                check_under(block, under)?;
                if block.id.is_empty() {
                    return Err("replace needs the component's id".into());
                }
                if let Some(list) = Self::list_of(&mut self.blocks, &block.id) {
                    let i = list.iter().position(|b| b.id == block.id).unwrap();
                    list[i] = block.clone();
                } else {
                    let list = match parent {
                        None => &mut self.blocks,
                        Some(p) => &mut Self::find_mut(&mut self.blocks, p).ok_or_else(|| format!("no parent {p:?}"))?.children,
                    };
                    list.push(block.clone());
                }
            }
            Op::Rm { id } => {
                let list = Self::list_of(&mut self.blocks, id).ok_or_else(|| format!("no component {id:?}"))?;
                list.retain(|b| &b.id != id);
            }
            Op::Move { id, before, after, parent } => {
                // where it lands decides whether it may: a tr only in a table, a table only trs
                if let Some(b) = self.find(id) {
                    let to = match parent {
                        Some(p) => self.find(p),
                        None => before.as_ref().or(after.as_ref()).and_then(|s| self.parent_of(s)),
                    };
                    if b.kind == "tr" || to.map(|t| t.kind == "table").unwrap_or(false) {
                        check_under(b, to)?;
                    }
                }
                let list = Self::list_of(&mut self.blocks, id).ok_or_else(|| format!("no component {id:?}"))?;
                let i = list.iter().position(|b| &b.id == id).unwrap();
                let b = list.remove(i);
                let target = match parent {
                    Some(p) => {
                        &mut Self::find_mut(&mut self.blocks, p).ok_or_else(|| format!("no parent {p:?}"))?.children
                    }
                    None => {
                        let sib = before.as_ref().or(after.as_ref()).ok_or("move needs --before, --after or --parent")?;
                        Self::list_of(&mut self.blocks, sib).ok_or_else(|| format!("no sibling {sib:?}"))?
                    }
                };
                let at = match (before, after) {
                    (Some(s), _) => target.iter().position(|x| &x.id == s).unwrap_or(target.len()),
                    (None, Some(s)) => target.iter().position(|x| &x.id == s).map(|p| p + 1).unwrap_or(target.len()),
                    _ => target.len(),
                };
                target.insert(at, b);
            }
        }
        self.normalize()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn doc() -> Doc {
        serde_json::from_value(json!({
            "id": "plan", "title": "t", "blocks": [
                {"type": "section", "id": "steps", "title": "Steps", "children": [
                    {"type": "todo", "id": "t1", "text": "one"},
                    {"type": "todo", "id": "t2", "text": "two"}
                ]},
                {"type": "progress", "id": "p", "of": "steps"}
            ]
        }))
        .unwrap()
    }

    fn todo(id: &str) -> Block {
        serde_json::from_value(json!({"type": "todo", "id": id, "text": id})).unwrap()
    }

    #[test]
    fn ops_by_id() {
        let mut d = doc();
        d.apply(&Op::Add { parent: Some("steps".into()), block: todo("t3"), before: Some("t2".into()), after: None }).unwrap();
        assert_eq!(d.ids(), vec!["steps", "t1", "t3", "t2", "p"]);
        d.apply(&Op::Set { id: "t1".into(), field: "note".into(), value: json!("1 in 20") }).unwrap();
        assert_eq!(d.find("t1").unwrap().s("note"), "1 in 20");
        d.apply(&Op::Move { id: "t2".into(), before: Some("t1".into()), after: None, parent: None }).unwrap();
        assert_eq!(d.ids(), vec!["steps", "t2", "t1", "t3", "p"]);
        d.apply(&Op::Rm { id: "t1".into() }).unwrap();
        assert!(d.find("t1").is_none());
        assert_eq!(d.parent_of("t3").unwrap().id, "steps");
    }

    #[test]
    fn ops_refuse_mistakes() {
        let mut d = doc();
        assert!(d.apply(&Op::Add { parent: None, block: todo("t1"), before: None, after: None }).is_err());
        assert!(d.apply(&Op::Rm { id: "nope".into() }).is_err());
        assert!(d.apply(&Op::Set { id: "t1".into(), field: "id".into(), value: json!("x") }).is_err());
        let mut bad = todo("a.b");
        bad.id = "a.b".into();
        assert!(d.clone().apply(&Op::Add { parent: None, block: bad, before: None, after: None }).is_err());
        let mut unk = todo("u");
        unk.kind = "widget".into();
        assert!(d.apply(&Op::Add { parent: None, block: unk, before: None, after: None }).is_err());
        let mut dup = todo("t9");
        dup.f.insert("type".into(), json!("section"));
        assert!(d.apply(&Op::Add { parent: None, block: dup, before: None, after: None }).is_err());
    }

    #[test]
    fn ids_assigned_without_collisions() {
        let mut d: Doc = serde_json::from_value(json!({"id": "x", "blocks": [
            {"type": "todo", "text": "a"}, {"type": "todo", "id": "t1", "text": "b"}
        ]}))
        .unwrap();
        d.normalize().unwrap();
        let ids = d.ids();
        assert_eq!(ids.len(), 2);
        assert_ne!(ids[0], ids[1]);
    }

    #[test]
    fn replace_in_place_or_add() {
        let mut d = doc();
        let mut s: Block = serde_json::from_value(json!({"type": "section", "id": "steps", "title": "New", "children": [
            {"type": "todo", "id": "t9", "text": "nine"}]})).unwrap();
        d.apply(&Op::Replace { block: s.clone(), parent: None }).unwrap();
        assert_eq!(d.ids(), vec!["steps", "t9", "p"], "replaced where it stood, children and all");
        s.id = "fresh".into();
        s.children.clear();
        d.apply(&Op::Replace { block: s, parent: None }).unwrap();
        assert_eq!(d.ids().last().unwrap(), "fresh");
        let (ids, parent) = Op::Rm { id: "steps".into() }.touched(&d);
        assert_eq!((ids, parent), (vec!["steps".to_string(), "t9".to_string()], None));
    }

    /// `margin schema` describes the document's own fields and every field of every type, and
    /// the document section lists exactly the keys an envelope may carry.
    #[test]
    fn schema_describes_the_document_and_every_field() {
        for k in SCHEMA {
            for (f, _, note) in k.fields {
                assert!(note.trim().len() >= 3, "{}.{f} has no description", k.name);
            }
        }
        let all = schema_text(None).unwrap();
        assert!(all.starts_with("document: "), "the document comes first: {}", &all[..80]);
        for f in ["title", "purpose", "hints", "kind", "blocks"] {
            assert!(DOC_FIELDS.iter().any(|d| d.0 == f && !d.2.is_empty()), "document.{f}");
            assert!(all.lines().take(8).any(|l| l.trim_start().starts_with(f)), "{f} is listed under document");
        }
        let doc = schema_text(Some("document")).unwrap();
        assert!(doc.contains("purpose") && !doc.contains("section:"), "{doc}");
        let glyph = schema_text(Some("row")).unwrap();
        for alias in ["(ok)", "(bell)", "(lock)"] {
            assert!(glyph.contains(alias), "row.glyph doesn't name the alias {alias}");
        }
        let j = schema_json(None).unwrap();
        assert_eq!(j["document"]["fields"]["hints"]["type"], "list");
        assert!(j["types"]["feed"]["fields"]["state"]["note"].as_str().unwrap().contains("paused"));
        assert!(schema_json(Some("document")).unwrap()["document"]["fields"]["title"].is_object());
        assert!(schema_json(Some("todo")).unwrap().get("document").is_none());
        // a document with each of the described fields is one the store's envelope takes
        let d: Doc = serde_json::from_value(serde_json::json!({"id": "a", "v": 1, "title": "T", "purpose": "p", "hints": ["k  v"], "kind": "dashboard", "blocks": []})).unwrap();
        assert_eq!((d.title.as_str(), d.purpose.as_str(), d.kind.as_str(), d.hints.len()), ("T", "p", "dashboard", 1));
    }

    #[test]
    fn schema_checks_every_write() {
        let mut d = doc();
        let set = |f: &str, v: Value| Op::Set { id: "t1".into(), field: f.into(), value: v };
        let e = d.clone().apply(&set("txt", json!("x"))).unwrap_err();
        assert!(e.contains("unknown field \"txt\" on todo (did you mean text?)"), "{e}");
        let e = d.clone().apply(&set("status", json!("bloked"))).unwrap_err();
        assert!(e.contains("did you mean blocked?"), "{e}");
        assert!(d.clone().apply(&set("text", json!(2026))).unwrap_err().contains("must be text"));
        d.apply(&set("note", Value::Null)).unwrap();
        let mut bad = todo("t7");
        bad.f.insert("colour".into(), json!("red"));
        assert!(d.clone().apply(&Op::Replace { block: bad.clone(), parent: None }).is_err());
        assert!(d.clone().apply(&Op::Add { parent: None, block: bad, before: None, after: None }).is_err());
        let q: Block = serde_json::from_value(json!({"type": "question", "id": "q", "text": "?", "options": ["a", "b"], "default": "c"})).unwrap();
        assert!(check_block(&q).unwrap_err().contains("isn't one of the options"));
        let mut kid = todo("t8");
        kid.children.push(todo("t9"));
        assert!(check_block(&kid).is_err(), "a todo holds no children");
    }

    #[test]
    fn decidable_ids_come_from_text_not_position() {
        let mk = |texts: &[&str]| {
            let mut d: Doc = serde_json::from_value(json!({"id": "x", "blocks": texts.iter().map(|t| json!({"type": "todo", "text": t})).collect::<Vec<_>>()})).unwrap();
            d.normalize().unwrap();
            d.ids()
        };
        assert_eq!(mk(&["beta", "gamma"]), vec!["beta", "gamma"]);
        assert_eq!(mk(&["alpha", "beta", "gamma"])[1], "beta", "an insert before it leaves its id alone");
        assert_eq!(mk(&["Fix the flake!", "fix the flake"]), vec!["fix-the-flake", "fix-the-flake-2"]);
        let mut d: Doc = serde_json::from_value(json!({"id": "x", "blocks": [{"type": "claim"}]})).unwrap();
        assert!(d.normalize().unwrap_err().contains("needs an id"));
        assert!(slug("a very long sentence that keeps on going").len() <= 24);
    }

    #[test]
    fn coerce_keeps_text_as_text() {
        assert_eq!(coerce("todo", "text", "2026"), json!("2026"));
        assert_eq!(coerce("todo", "text", "true"), json!("true"));
        assert_eq!(coerce("progress", "value", "0.5"), json!(0.5));
        assert_eq!(coerce("question", "options", r#"["a","b"]"#), json!(["a", "b"]));
        assert!(schema_text(Some("todo")).unwrap().contains("status"));
        assert!(schema_text(Some("tod")).unwrap_err().contains("did you mean todo?"));
        assert_eq!(schema_json(None).unwrap()["types"].as_object().unwrap().len(), SCHEMA.len());
    }

    #[test]
    fn tables_rows_and_run_strips_are_checked() {
        let block = |v: Value| -> Block { serde_json::from_value(v).unwrap() };
        let table = json!({"type": "table", "id": "prs", "zebra": true, "columns": [
            {"key": "pr", "title": "PR", "kind": "id"}, {"key": "title", "title": "Title"},
            {"key": "ci", "title": "CI", "kind": "runs", "optional": true}], "children": [
            {"type": "tr", "id": "pr1", "cells": {"pr": "#1", "title": "one", "ci": ["ok", null, "fail"]}, "target": "https://x", "detail": "d"}]});
        check_block(&block(table.clone())).unwrap();
        let tr = |cells: Value| block(json!({"type": "tr", "id": "r", "cells": cells}));
        let e = check_block(&tr(json!({"pr": "#2"}))).unwrap_err();
        assert!(e.contains("belongs inside a table"), "{e}");
        let t = block(table.clone());
        let e = check_under(&tr(json!({"titel": "x"})), Some(&t)).unwrap_err();
        assert!(e.contains("names no column (did you mean title?)"), "{e}");
        let e = check_under(&tr(json!({"ci": ["ok", "flaky"]})), Some(&t)).unwrap_err();
        assert!(e.contains("\"flaky\" isn't a run outcome"), "{e}");
        assert!(check_under(&tr(json!({"title": {"a": 1}})), Some(&t)).is_err(), "a cell is text or a number");
        check_under(&tr(json!({"title": 3, "pr": null})), Some(&t)).unwrap();
        // the table's own shape
        let with = |cols: Value| block(json!({"type": "table", "id": "t", "columns": cols}));
        assert!(check_block(&with(json!([{"title": "no key"}]))).unwrap_err().contains("needs a key"));
        assert!(check_block(&with(json!([{"key": "a"}, {"key": "a"}]))).unwrap_err().contains("share the key"));
        assert!(check_block(&with(json!([{"key": "a", "kind": "nums"}]))).unwrap_err().contains("did you mean num?"));
        assert!(check_block(&with(json!([{"key": "a", "align": "middle"}]))).is_err());
        assert!(check_block(&with(json!([{"key": "a", "widht": 4}]))).unwrap_err().contains("did you mean width?"));
        assert!(check_block(&with(json!("a,b"))).is_err());
        let mut t2 = block(table.clone());
        t2.children.push(todo("t1"));
        assert!(check_block(&t2).unwrap_err().contains("holds only tr rows"));
        // the standalone strip
        check_block(&block(json!({"type": "runs", "id": "n", "label": "nightly", "values": ["ok", "skip", "running", null], "summary": true}))).unwrap();
        assert!(check_block(&block(json!({"type": "runs", "id": "n", "values": ["pass"]}))).unwrap_err().contains("isn't a run outcome"));
        assert!(check_block(&block(json!({"type": "runs", "id": "n", "values": "ok"}))).is_err());
        check_block(&block(json!({"type": "rule", "id": "r", "text": "Reviews"}))).unwrap();
        // through the ops: a row is added under its table, never beside it, and can't move out
        let mut d: Doc = serde_json::from_value(json!({"id": "x", "blocks": [table, {"type": "section", "id": "s", "title": "S"}]})).unwrap();
        let row = tr(json!({"pr": "#2", "title": "two"}));
        assert!(d.clone().apply(&Op::Add { parent: None, block: row.clone(), before: None, after: None }).is_err());
        assert!(d.clone().apply(&Op::Add { parent: Some("s".into()), block: row.clone(), before: None, after: None }).is_err());
        d.apply(&Op::Add { parent: Some("prs".into()), block: row.clone(), before: None, after: None }).unwrap();
        d.apply(&Op::Replace { block: tr(json!({"pr": "#2", "title": "two again"})), parent: None }).unwrap();
        assert_eq!(d.parent_of("r").unwrap().id, "prs", "replaced where it stood");
        assert!(d.clone().apply(&Op::Move { id: "r".into(), before: None, after: None, parent: Some("s".into()) }).is_err());
        assert!(d.clone().apply(&Op::Move { id: "s".into(), before: Some("r".into()), after: None, parent: None }).is_err(), "a section can't move into a table");
        d.apply(&Op::Move { id: "r".into(), before: Some("pr1".into()), after: None, parent: None }).unwrap();
        let set = |id: &str, f: &str, v: Value| Op::Set { id: id.into(), field: f.into(), value: v };
        assert!(d.clone().apply(&set("r", "cells", json!({"nope": 1}))).unwrap_err().contains("names no column"));
        d.apply(&set("r", "cells", json!({"title": "three"}))).unwrap();
        let e = d.clone().apply(&set("prs", "columns", json!([{"key": "pr"}]))).unwrap_err();
        assert!(e.contains("names no column"), "dropping a column its rows still fill is refused: {e}");
        assert!(schema_text(Some("tr")).unwrap().contains("cells") && schema_text(Some("row")).unwrap().contains("who"));
    }

    #[test]
    fn hash_ignores_children() {
        let d = doc();
        let mut e = d.clone();
        e.apply(&Op::Set { id: "t1".into(), field: "text".into(), value: json!("changed") }).unwrap();
        assert_eq!(d.find("steps").unwrap().hash(), e.find("steps").unwrap().hash());
        assert_ne!(d.find("t1").unwrap().hash(), e.find("t1").unwrap().hash());
    }
}
