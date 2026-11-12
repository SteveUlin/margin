//! `margin doc`: markdown in, one section component out. Long-form content (an overview, a
//! proposal, a comparison) belongs beside the chat, where it stays on screen while sulin replies;
//! writing it as JSON blocks cost too much, so Claude writes markdown and this turns it into the
//! components the pane already draws.
//!
//! Headings become nested sections, paragraphs `text`, lists nested `bullet`s (a task item
//! wears its box), fences `code`, tables `table` with `tr` rows, quotes `text` with a quote bar,
//! a thematic break a `rule`. Inline markup passes through as written: the pane's own markup
//! reads `**bold**`, `*italic*`, `~~struck~~`, `` `code` `` and `[label](url)`. A paragraph or
//! bullet with one link takes it as its `target`; with several, each link is a `link` row under
//! it. Ids come from the content (a heading's slug, a paragraph's hash), not its position: a
//! revision that edits one paragraph marks only that row as changed, and an inserted bullet
//! doesn't shift every id after it.

use crate::components;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};

/// Sections nest this deep under the document's own; a heading further in is a bold line, since
/// the pane stops indenting there and a section nobody can tell from its parent helps no one.
const MAX_NEST: usize = 4;

/// The section `id`, titled `title` or the document's first `# heading`.
pub fn to_section(id: &str, title: Option<&str>, md: &str) -> Value {
    let mut p = Parser { ids: HashSet::new(), root: id.to_string(), refs: HashMap::new() };
    p.ids.insert(id.to_string());
    let all: Vec<&str> = md.lines().collect();
    // `[label]: url` lines define reference links; they are read first and leave the text
    let code = fenced(&all);
    let lines: Vec<&str> = all
        .iter()
        .enumerate()
        .map(|(i, l)| match ref_def(l).filter(|_| !code[i]) {
            Some((label, url)) => {
                p.refs.insert(label, url);
                ""
            }
            None => l,
        })
        .collect();
    // a leading `# Title` names the section when no --title was given
    let (mut start, mut title) = (0, title.map(String::from));
    if let Some(i) = lines.iter().position(|l| !l.trim().is_empty()) {
        if let Some(h) = lines[i].strip_prefix("# ") {
            if title.is_none() {
                title = Some(components::unmarked(&p.inline(h.trim())));
            }
            start = i + 1;
        }
    }
    let children = p.blocks(&lines[start..], 0);
    json!({"type": "section", "id": id, "title": title.unwrap_or_else(|| id.to_string()), "prose": true, "children": children})
}

struct Parser {
    ids: HashSet<String>,
    root: String,
    /// Reference link definitions: lowercased label → URL.
    refs: HashMap<String, String>,
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars().flat_map(|c| c.to_lowercase()) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
        if out.len() >= 24 {
            break;
        }
    }
    out.trim_end_matches('-').to_string()
}

fn hash4(s: &str) -> String {
    let mut h: u32 = 0x811c9dc5;
    for b in s.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x01000193);
    }
    format!("{:04x}", h & 0xffff)
}

/// `[label]: url` (a reference link's definition; a footnote's `[^n]: …` is not one).
fn ref_def(l: &str) -> Option<(String, String)> {
    let t = l.trim_start().strip_prefix('[')?;
    let (label, rest) = t.split_once("]:")?;
    let url = rest.split_whitespace().next()?;
    let url = url.strip_prefix('<').and_then(|u| u.strip_suffix('>')).unwrap_or(url);
    (!label.is_empty() && !label.starts_with('^') && !label.contains(['[', ']'])).then(|| (label.to_lowercase(), url.to_string()))
}

/// A thematic break: three or more of one of `-`, `*`, `_`, spaces allowed between.
fn hr(t: &str) -> bool {
    let Some(c) = t.chars().next().filter(|c| matches!(c, '-' | '*' | '_')) else { return false };
    t.chars().all(|x| x == c || x == ' ') && t.chars().filter(|&x| x == c).count() >= 3
}

/// One list item: its indent, its text (an ordered item keeps its number, which the pane draws
/// where the bullet would be), and a task item's box (`[ ]` false, `[x]` true).
fn list_item(l: &str) -> Option<(usize, String, Option<bool>)> {
    let indent = l.len() - l.trim_start().len();
    let t = l.trim_start();
    if hr(t.trim_end()) {
        return None;
    }
    for m in ["- ", "* ", "+ "] {
        if let Some(r) = t.strip_prefix(m) {
            let r = r.trim_start();
            for (mark, done) in [("[ ] ", false), ("[x] ", true), ("[X] ", true)] {
                if let Some(text) = r.strip_prefix(mark) {
                    return Some((indent, text.trim().to_string(), Some(done)));
                }
            }
            return Some((indent, r.to_string(), None));
        }
    }
    let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 && (t[digits..].starts_with(". ") || t[digits..].starts_with(") ")) {
        return Some((indent, t.to_string(), None));
    }
    None
}

fn heading(l: &str) -> Option<(usize, &str)> {
    let n = l.chars().take_while(|&c| c == '#').count();
    // (a closing run of #s is decoration)
    (n >= 1 && n <= 6 && l[n..].starts_with(' ')).then(|| (n, l[n + 1..].trim().trim_end_matches('#').trim_end()))
}

/// A line that opens a code fence: its marker and length (``` or ~~~, three or more).
fn fence_open(t: &str) -> Option<(char, usize)> {
    let c = t.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let n = t.chars().take_while(|&x| x == c).count();
    // a backtick in the info string makes it inline code (```x```), not a fence
    (n >= 3 && !(c == '`' && t[n..].contains('`'))).then_some((c, n))
}

/// Does `t` close the fence `(c, n)` opened: the same marker, at least as long, nothing after.
fn fence_close(t: &str, (c, n): (char, usize)) -> bool {
    let k = t.chars().take_while(|&x| x == c).count();
    k >= n && t[k..].trim().is_empty()
}

/// Per line: is it inside a code fence (its closing line included, its opening line not)? What a
/// fence holds is code, so no scan for where a block ends may read it as markdown: a `# comment`
/// in a shell snippet is not a heading. An unclosed fence runs to the end.
fn fenced(lines: &[&str]) -> Vec<bool> {
    let mut open = None;
    lines
        .iter()
        .map(|l| {
            let t = l.trim();
            match open {
                Some(f) => {
                    if fence_close(t, f) {
                        open = None;
                    }
                    true
                }
                None => {
                    open = fence_open(t);
                    false
                }
            }
        })
        .collect()
}

/// A table row's cells: split at each `|` that is neither escaped nor inside backticks, the
/// outer pipes dropped.
fn split_row(l: &str) -> Vec<String> {
    let (mut cells, mut cur, mut code) = (Vec::new(), String::new(), false);
    let mut chars = l.trim().chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                cur.push('|');
                chars.next();
            }
            '`' => {
                code = !code;
                cur.push(c);
            }
            '|' if !code => cells.push(std::mem::take(&mut cur).trim().to_string()),
            _ => cur.push(c),
        }
    }
    cells.push(cur.trim().to_string());
    if l.trim().starts_with('|') {
        cells.remove(0);
    }
    if l.trim().ends_with('|') && !l.trim().ends_with("\\|") {
        cells.pop();
    }
    cells
}

/// The row under a table's header (`|---|:-:|`): each column's alignment, where its colons say.
fn delim_row(l: &str) -> Option<Vec<Option<&'static str>>> {
    if !l.contains('-') || !l.trim().chars().all(|c| matches!(c, '|' | '-' | ':' | ' ')) {
        return None;
    }
    split_row(l)
        .iter()
        .map(|c| {
            let dashes = c.trim_matches(':');
            (!dashes.is_empty() && dashes.chars().all(|x| x == '-')).then(|| match (c.starts_with(':'), c.ends_with(':')) {
                (true, true) => Some("center"),
                (false, true) => Some("right"),
                (true, false) => Some("left"),
                _ => None,
            })
        })
        .collect()
}

/// Does a cell read as a number: `12`, `1,204.5`, `-3%`, `$40`, `12 ms`?
fn numeric(s: &str) -> bool {
    let t = s.trim().trim_start_matches(['+', '-', '−', '~', '$', '€', '£', '<', '>']).trim_end_matches('%');
    let body = t.trim_end_matches(|c: char| c.is_alphabetic());
    t.len() - body.len() <= 3 && body.starts_with(|c: char| c.is_ascii_digit()) && body.trim_end().chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | ','))
}

/// Does a table start at line `i`: a row with a pipe, over a delimiter row?
fn table_at(lines: &[&str], i: usize) -> bool {
    lines[i].contains('|') && lines.get(i + 1).and_then(|l| delim_row(l)).is_some()
}

/// A paragraph's lines as one text: joined by spaces, or by a line break where a line ends in
/// two spaces or a backslash (a hard break).
fn join_lines(lines: &[&str]) -> String {
    let mut out = String::new();
    for (i, l) in lines.iter().enumerate() {
        let hard = l.ends_with("  ") || l.trim_end().ends_with('\\');
        out.push_str(l.trim().trim_end_matches('\\').trim_end());
        if i + 1 < lines.len() {
            out.push(if hard { '\n' } else { ' ' });
        }
    }
    out
}

impl Parser {
    fn id(&mut self, base: &str) -> String {
        let mut id = format!("{}-{}", self.root, base);
        let mut n = 2;
        while self.ids.contains(&id) {
            id = format!("{}-{}{}", self.root, base, n);
            n += 1;
        }
        self.ids.insert(id.clone());
        id
    }

    fn leaf(&mut self, kind: &str, text: &str, extra: &[(&str, Value)]) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("type".into(), json!(kind));
        m.insert("id".into(), json!(self.id(&hash4(text))));
        m.insert("text".into(), json!(text));
        for (k, v) in extra {
            m.insert((*k).into(), v.clone());
        }
        m
    }

    /// What markdown has inline that the pane's markup lacks: an image becomes its alt text and
    /// path (`alt (path)`), a reference link `[label][ref]` its inline form.
    fn inline(&self, s: &str) -> String {
        let mut out = String::new();
        let mut i = 0;
        while let Some(c) = s[i..].chars().next() {
            let rest = &s[i..];
            if c == '`' {
                // code is kept as written, to its closing run
                let n = rest.chars().take_while(|&x| x == '`').count();
                let len = rest[n..].find(&"`".repeat(n)).map(|e| n + e + n).unwrap_or(n);
                out.push_str(&rest[..len]);
                i += len;
                continue;
            }
            if let Some((alt, path, len)) = rest.strip_prefix('!').and_then(components::link_at) {
                out.push_str(&if alt.trim().is_empty() { format!("`{path}`") } else { format!("{} (`{path}`)", alt.trim()) });
                i += 1 + len;
                continue;
            }
            if c == '[' && !self.refs.is_empty() {
                if let Some((label, url, len)) = self.reference(rest) {
                    out.push_str(&format!("[{label}]({url})"));
                    i += len;
                    continue;
                }
            }
            out.push(c);
            i += c.len_utf8();
        }
        out
    }

    /// `[label][ref]`, `[label][]` or `[label]` at the start of `s`, when the reference is
    /// defined: the label, its URL and the bytes taken.
    fn reference<'a>(&self, s: &'a str) -> Option<(&'a str, &str, usize)> {
        let end = s.find(']')?;
        let label = &s[1..end];
        let after = &s[end + 1..];
        if label.is_empty() || label.contains('[') || after.starts_with('(') || after.starts_with(':') {
            return None;
        }
        let (name, len) = match after.strip_prefix('[').and_then(|a| a.find(']').map(|e| &a[..e])) {
            Some(r) => (if r.is_empty() { label } else { r }, end + 1 + r.len() + 2),
            None => (label, end + 1),
        };
        self.refs.get(&name.to_lowercase()).map(|url| (label, url.as_str(), len))
    }

    /// A paragraph or a bullet from its markdown text, with its links made openable: one link is
    /// the block's `target`; several become `link` rows, returned to go under it.
    fn prose(&mut self, kind: &str, raw: &str, extra: &[(&str, Value)]) -> (Map<String, Value>, Vec<Value>) {
        let text = self.inline(raw);
        let links = components::links(&text);
        let mut m = self.leaf(kind, &text, extra);
        let mut rows = Vec::new();
        match links.as_slice() {
            [] => {}
            [(_, url)] => {
                m.insert("target".into(), json!(url));
            }
            many => {
                for (label, url) in many {
                    let id = self.id(&hash4(&format!("{label}\n{url}")));
                    rows.push(json!({"type": "link", "id": id, "text": if label == url { "" } else { label.as_str() }, "target": url}));
                }
            }
        }
        (m, rows)
    }

    /// Blocks for a run of lines; a heading opens a section holding everything up to the next
    /// heading of its level or above. `depth`: how many sections of this document are open.
    fn blocks(&mut self, lines: &[&str], depth: usize) -> Vec<Value> {
        let mut out = Vec::new();
        let code = fenced(lines);
        let mut i = 0;
        while i < lines.len() {
            let l = lines[i];
            let t = l.trim();
            if t.is_empty() {
                i += 1;
            } else if hr(t) {
                out.push(json!({"type": "rule", "id": self.id("hr")}));
                i += 1;
            } else if let Some((lvl, h)) = heading(t) {
                if depth >= MAX_NEST {
                    // too deep for a section of its own: a bold line, and what follows stays here
                    let text = format!("**{}**", self.inline(h).replace("**", ""));
                    out.push(Value::Object(self.leaf("text", &text, &[])));
                    i += 1;
                    continue;
                }
                let end = (i + 1..lines.len()).find(|&j| !code[j] && heading(lines[j].trim()).map(|(n, _)| n <= lvl).unwrap_or(false)).unwrap_or(lines.len());
                let id = self.id(&slug(h));
                let title = components::unmarked(&self.inline(h));
                let kids = self.blocks(&lines[i + 1..end], depth + 1);
                out.push(json!({"type": "section", "id": id, "title": title, "prose": true, "children": kids}));
                i = end;
            } else if let Some(f) = fence_open(t) {
                let lang = t.trim_start_matches(f.0).split_whitespace().next().unwrap_or("").to_string();
                let end = (i + 1..lines.len()).find(|&j| fence_close(lines[j].trim(), f)).unwrap_or(lines.len());
                let body = lines[i + 1..end].join("\n");
                out.push(Value::Object(self.leaf("code", &body, &[("language", json!(lang))])));
                i = end + 1;
            } else if table_at(lines, i) || t.starts_with('|') {
                let end = (i..lines.len()).find(|&j| !lines[j].contains('|') || lines[j].trim().is_empty()).unwrap_or(lines.len());
                out.push(self.table(&lines[i..end]));
                i = end;
            } else if t.starts_with('>') {
                let end = (i..lines.len()).find(|&j| !lines[j].trim().starts_with('>')).unwrap_or(lines.len());
                let inner: Vec<&str> = lines[i..end].iter().map(|l| l.trim().strip_prefix('>').map(|r| r.strip_prefix(' ').unwrap_or(r)).unwrap_or("")).collect();
                // (a heading inside a quote is a bold line: a quote holds no sections)
                let quoted = self.blocks(&inner, MAX_NEST);
                self.quote(quoted, &mut out);
                i = end;
            } else if list_item(l).is_some() {
                let end = (i..lines.len())
                    .find(|&j| {
                        let lj = lines[j];
                        let tj = lj.trim();
                        // a list runs until a blank line followed by a non-list, unindented line
                        // (never a line of a fence indented under an item, whatever it looks like)
                        !code[j] && !tj.is_empty() && list_item(lj).is_none() && lj.len() == lj.trim_start().len() && (j == 0 || lines[j - 1].trim().is_empty() || heading(tj).is_some() || fence_open(tj).is_some() || hr(tj))
                    })
                    .unwrap_or(lines.len());
                out.extend(self.list(&lines[i..end]));
                i = end;
            } else {
                let end = (i + 1..lines.len())
                    .find(|&j| {
                        let tj = lines[j].trim();
                        tj.is_empty() || hr(tj) || heading(tj).is_some() || fence_open(tj).is_some() || tj.starts_with('|') || table_at(lines, j) || tj.starts_with('>') || list_item(lines[j]).is_some()
                    })
                    .unwrap_or(lines.len());
                let (m, rows) = self.prose("text", &join_lines(&lines[i..end]), &[]);
                out.push(Value::Object(m));
                out.extend(rows);
                i = end;
            }
        }
        out
    }

    /// A table: columns from the header row (aligned by the delimiter row's colons, a column of
    /// numbers to the right), one `tr` a row, cells as text with their markup. A row with one
    /// link opens it. Rows that don't have the header's number of cells make it a code block of
    /// the lines as written: columns nobody can trust are better read raw.
    fn table(&mut self, lines: &[&str]) -> Value {
        let raw = || lines.iter().map(|l| l.trim()).collect::<Vec<_>>().join("\n");
        let (Some(aligns), true) = (lines.get(1).and_then(|l| delim_row(l)), lines.len() >= 2) else {
            let text = raw();
            return Value::Object(self.leaf("code", &text, &[]));
        };
        let head = split_row(lines[0]);
        let rows: Vec<Vec<String>> = lines[2..].iter().map(|l| split_row(l)).collect();
        if aligns.len() != head.len() || rows.iter().any(|r| r.len() != head.len()) {
            let text = raw();
            return Value::Object(self.leaf("code", &text, &[]));
        }
        let key = |i: usize| format!("c{}", i + 1);
        let columns: Vec<Value> = head
            .iter()
            .enumerate()
            .map(|(i, h)| {
                let mut c = Map::new();
                c.insert("key".into(), json!(key(i)));
                c.insert("title".into(), json!(components::unmarked(&self.inline(h))));
                let cells: Vec<&str> = rows.iter().map(|r| r[i].as_str()).filter(|c| !c.is_empty()).collect();
                if !cells.is_empty() && cells.iter().all(|c| numeric(c)) {
                    c.insert("kind".into(), json!("num"));
                }
                if let Some(a) = aligns[i] {
                    c.insert("align".into(), json!(a));
                }
                Value::Object(c)
            })
            .collect();
        let id = self.id(&format!("tbl-{}", hash4(lines[0].trim())));
        let children: Vec<Value> = rows
            .iter()
            .map(|r| {
                let cells: Map<String, Value> = r.iter().enumerate().map(|(i, c)| (key(i), json!(self.inline(c)))).collect();
                let line = cells.values().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(" | ");
                let mut row = Map::new();
                row.insert("type".into(), json!("tr"));
                row.insert("id".into(), json!(self.id(&hash4(&line))));
                match components::links(&line).as_slice() {
                    [] => {}
                    [(_, url)] => {
                        row.insert("target".into(), json!(url));
                    }
                    // several: the detail panel lists them (a table holds only rows)
                    many => {
                        row.insert("detail".into(), json!(many.iter().map(|(l, u)| if l == u { u.clone() } else { format!("{l}: {u}") }).collect::<Vec<_>>().join("\n")));
                    }
                }
                row.insert("cells".into(), Value::Object(cells));
                Value::Object(row)
            })
            .collect();
        json!({"type": "table", "id": id, "columns": columns, "children": children})
    }

    /// Mark the blocks a quote holds as quoted and append them to `out`: a paragraph takes the
    /// `note` style and one more quote bar; a list becomes quoted lines that keep their marks
    /// (a bullet draws no bar); anything else (code, a table) stands as it is.
    fn quote(&mut self, blocks: Vec<Value>, out: &mut Vec<Value>) {
        fn line(b: &Value, level: usize, out: &mut Vec<Value>) {
            let text = b["text"].as_str().unwrap_or("");
            let mark = match b["task"].as_str() {
                Some("done") => "☑ ",
                Some(_) => "☐ ",
                None if components::numbered(text).is_some() => "",
                None => "• ",
            };
            let mut t = json!({"type": "text", "id": b["id"], "text": format!("{}{mark}{text}", "\u{a0}\u{a0}".repeat(level)), "style": "note", "quote": 1});
            if !b["target"].is_null() {
                t["target"] = b["target"].clone();
            }
            out.push(t);
            for c in b["children"].as_array().into_iter().flatten() {
                if c["type"] == "bullet" {
                    line(c, level + 1, out);
                } else {
                    out.push(c.clone());
                }
            }
        }
        for mut b in blocks {
            match b["type"].as_str() {
                Some("text") => {
                    b["quote"] = json!(b["quote"].as_u64().unwrap_or(0) + 1);
                    if b["style"].is_null() {
                        b["style"] = json!("note");
                    }
                    out.push(b);
                }
                Some("bullet") => line(&b, 0, out),
                _ => out.push(b),
            }
        }
    }

    /// Nested bullets from indentation; a continuation line joins the bullet above it, and a
    /// fence under an item is a code block inside that bullet.
    fn list(&mut self, lines: &[&str]) -> Vec<Value> {
        // (indent, text, task, code blocks) flattened first, then nested
        let mut items: Vec<Item> = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            let l = lines[i];
            let t = l.trim();
            if let Some(f) = fence_open(t).filter(|_| !items.is_empty()) {
                let ind = l.len() - l.trim_start().len();
                let end = (i + 1..lines.len()).find(|&j| fence_close(lines[j].trim(), f)).unwrap_or(lines.len());
                let body: Vec<&str> = lines[i + 1..end].iter().map(|x| &x[x.len().min(ind).min(x.len() - x.trim_start().len())..]).collect();
                let lang = t.trim_start_matches(f.0).split_whitespace().next().unwrap_or("").to_string();
                let code = self.leaf("code", &body.join("\n"), &[("language", json!(lang))]);
                items.last_mut().unwrap().kids.push(Value::Object(code));
                i = end + 1;
                continue;
            }
            if let Some((indent, text, task)) = list_item(l) {
                items.push(Item { indent, text, task, kids: Vec::new() });
            } else if !t.is_empty() {
                if let Some(last) = items.last_mut() {
                    last.text.push(' ');
                    last.text.push_str(t);
                }
            }
            i += 1;
        }
        let mut pos = 0;
        self.nest(&items, &mut pos, 0)
    }

    fn nest(&mut self, items: &[Item], pos: &mut usize, indent: usize) -> Vec<Value> {
        let mut out = Vec::new();
        while *pos < items.len() && items[*pos].indent >= indent {
            let it = &items[*pos];
            *pos += 1;
            let extra: Vec<(&str, Value)> = it.task.map(|done| ("task", json!(if done { "done" } else { "open" }))).into_iter().collect();
            let (mut b, mut kids) = self.prose("bullet", &it.text, &extra);
            kids.extend(it.kids.iter().cloned());
            if *pos < items.len() && items[*pos].indent > it.indent {
                let deeper = items[*pos].indent;
                kids.extend(self.nest(items, pos, deeper));
            }
            if !kids.is_empty() {
                b.insert("children".into(), Value::Array(kids));
            }
            out.push(Value::Object(b));
        }
        out
    }
}

/// One list item before nesting.
struct Item {
    indent: usize,
    text: String,
    task: Option<bool>,
    kids: Vec<Value>,
}

/// Paragraphs that would read better as bullets: more than two sentences in one row. sulin likes
/// bullets, and a bullet is a row he can react to on its own (2026-11-08).
pub fn long_paragraphs(sec: &Value) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    fn walk(v: &Value, out: &mut Vec<(String, usize)>) {
        if v["type"] == "text" && v["style"] != "note" {
            // (a link reads as its label, and a dot inside a word or an address ends nothing)
            let t = components::unmarked(v["text"].as_str().unwrap_or(""));
            let n = t.split_inclusive(". ").flat_map(|s| s.split_inclusive(['?', '!'])).filter(|s| s.trim().len() > 2).count();
            if n > 2 {
                out.push((v["id"].as_str().unwrap_or("").to_string(), n));
            }
        }
        for c in v["children"].as_array().into_iter().flatten() {
            walk(c, out);
        }
    }
    walk(sec, &mut out);
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn three_sentences_earn_a_hint() {
        let s = super::to_section("t", Some("x"), "One. Two. Three.\n\nShort lead. Fine.\n");
        let l = super::long_paragraphs(&s);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].1, 3);
    }

    use super::*;

    fn kinds(v: &Value) -> Vec<String> {
        v["children"].as_array().unwrap().iter().map(|c| c["type"].as_str().unwrap().to_string()).collect()
    }

    #[test]
    fn markdown_becomes_components() {
        let md = "# Toasts\n\nWhere a toast goes.\nIt wraps.\n\n## Options\n\n- anchored to the row\n  - falls back to overlay\n- in the strip\n\n```rust\nfn main() {}\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n> a note\n";
        let s = to_section("t", None, md);
        assert_eq!(s["title"], "Toasts");
        assert_eq!(kinds(&s), ["text", "section"]);
        assert_eq!(s["children"][0]["text"], "Where a toast goes. It wraps.");
        let opts = &s["children"][1];
        assert_eq!((opts["id"].as_str().unwrap(), opts["title"].as_str().unwrap()), ("t-options", "Options"));
        assert_eq!(kinds(opts), ["bullet", "bullet", "code", "table", "text"]);
        assert_eq!(opts["children"][0]["children"][0]["text"], "falls back to overlay");
        assert_eq!(opts["children"][2]["language"], "rust");
        assert_eq!(opts["children"][4]["style"], "note");
    }

    #[test]
    fn ids_follow_content_not_position() {
        let a = to_section("t", Some("x"), "- one\n- two\n- three\n");
        let b = to_section("t", Some("x"), "- zero\n- one\n- two\n- three\n");
        let ids = |v: &Value| v["children"].as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap().to_string()).collect::<Vec<_>>();
        // inserting a bullet at the top keeps every other id
        assert_eq!(ids(&a), ids(&b)[1..]);
        // duplicates get a suffix, not a clash
        let d = to_section("t", Some("x"), "- same\n- same\n");
        let di = ids(&d);
        assert_ne!(di[0], di[1]);
    }

    /// What a fence holds is code: a `# comment` in a shell snippet opened a section and swallowed
    /// the real heading after it into a code block.
    #[test]
    fn a_fence_hides_its_lines_from_headings() {
        let md = "## Setup\n\nRun this.\n\n```bash\n# install the tool\ncargo install margin\n```\n\n## Next\n\nThen that.\n";
        let s = to_section("t", Some("x"), md);
        let titles: Vec<&str> = s["children"].as_array().unwrap().iter().map(|c| c["title"].as_str().unwrap_or("")).collect();
        assert_eq!(titles, ["Setup", "Next"], "{s:#}");
        let setup = &s["children"][0];
        assert_eq!(kinds(setup), ["text", "code"]);
        assert_eq!((setup["children"][1]["text"].as_str().unwrap(), setup["children"][1]["language"].as_str().unwrap()), ("# install the tool\ncargo install margin", "bash"));
        assert_eq!(kinds(&s["children"][1]), ["text"]);
    }

    /// ~~~ fences count, a fence closes only on its own marker (so one kind can quote the other,
    /// and four backticks can hold three), and a paragraph stops at either kind.
    #[test]
    fn tilde_and_longer_fences() {
        let md = "## A\n\nlead\n~~~sh\n# not a heading\n```\n## nor this\n~~~\n\n````md\n```\n# inside\n```\n````\n\n## B\n";
        let s = to_section("t", Some("x"), md);
        assert_eq!(kinds(&s), ["section", "section"], "{s:#}");
        let a = &s["children"][0];
        assert_eq!(kinds(a), ["text", "code", "code"], "{a:#}");
        assert_eq!(a["children"][0]["text"], "lead");
        assert_eq!((a["children"][1]["text"].as_str().unwrap(), a["children"][1]["language"].as_str().unwrap()), ("# not a heading\n```\n## nor this", "sh"));
        assert_eq!(a["children"][2]["text"], "```\n# inside\n```");
        assert_eq!(s["children"][1]["title"], "B");
    }

    /// An unclosed fence runs to the end of the input: everything after it is code, no sections.
    #[test]
    fn an_unclosed_fence_runs_to_the_end() {
        let s = to_section("t", Some("x"), "## A\n\n```\n# still code\n\n## and this\n- not a list\n");
        assert_eq!(kinds(&s), ["section"], "{s:#}");
        let a = &s["children"][0];
        assert_eq!(kinds(a), ["code"], "{a:#}");
        assert_eq!(a["children"][0]["text"], "# still code\n\n## and this\n- not a list");
    }

    /// A fence indented under a list item doesn't end the list at an unindented line inside it,
    /// and the heading after the list is still a heading.
    #[test]
    fn a_fence_inside_a_list_keeps_the_list_whole() {
        let md = "- step one\n\n  ```bash\n# a comment at column 0\n\necho hi\n  ```\n- step two\n\n## After\n";
        let s = to_section("t", Some("x"), md);
        assert_eq!(kinds(&s), ["bullet", "bullet", "section"], "{s:#}");
        assert_eq!(s["children"][1]["text"], "step two");
        assert_eq!(s["children"][2]["title"], "After");
    }


    fn sec(md: &str) -> Value {
        let s = to_section("t", Some("x"), md);
        // what the converter writes must be what the schema takes
        crate::doc::check_block(&serde_json::from_value(s.clone()).unwrap()).unwrap();
        s
    }

    /// Item 1: a table is a `table` with `tr` rows: titles from the header, alignment from the
    /// delimiter row's colons, a column of numbers a `num`. Ragged rows stay raw, in a code block.
    #[test]
    fn a_table_becomes_a_table_with_rows() {
        let s = sec("| Name | Count | Note |\n|:--|--:|:-:|\n| `a|b` | 1,204 | see [x](https://x.example) |\n| beta \\| pipe | 12 | *it* |\n\n| ragged | row |\n|---|---|\n| one |\n");
        assert_eq!(kinds(&s), ["table", "code"], "{s:#}");
        let t = &s["children"][0];
        assert_eq!(t["columns"], json!([{"key": "c1", "title": "Name", "align": "left"}, {"key": "c2", "title": "Count", "kind": "num", "align": "right"}, {"key": "c3", "title": "Note", "align": "center"}]));
        assert_eq!(t["children"][0]["cells"], json!({"c1": "`a|b`", "c2": "1,204", "c3": "see [x](https://x.example)"}));
        assert_eq!(t["children"][0]["target"], "https://x.example");
        assert_eq!(t["children"][1]["cells"]["c1"], "beta | pipe");
        assert_eq!(s["children"][1]["text"], "| ragged | row |\n|---|---|\n| one |");
    }

    /// Item 2: a thematic break is a rule, and `- - -` is not a list.
    #[test]
    fn a_thematic_break_is_a_rule() {
        let s = sec("above\n\n---\n\nbetween\n\n- - -\n- item\n***\n");
        assert_eq!(kinds(&s), ["text", "rule", "text", "rule", "bullet", "rule"], "{s:#}");
    }

    /// Item 3: one link is the block's target; several are link rows under it; a bare URL counts.
    #[test]
    fn links_open_through_a_target_or_their_own_rows() {
        let s = sec("See [the guide](docs/guide.md).\n\nBoth [a](https://a.example) and https://b.example.\n\n- item [c](https://c.example) or [d](https://d.example)\n");
        assert_eq!(kinds(&s), ["text", "text", "link", "link", "bullet"], "{s:#}");
        let c = &s["children"];
        assert_eq!((c[0]["text"].as_str().unwrap(), c[0]["target"].as_str().unwrap()), ("See [the guide](docs/guide.md).", "docs/guide.md"));
        assert!(c[1]["target"].is_null());
        assert_eq!((c[2]["text"].as_str().unwrap(), c[2]["target"].as_str().unwrap()), ("a", "https://a.example"));
        assert_eq!((c[3]["text"].as_str().unwrap(), c[3]["target"].as_str().unwrap()), ("", "https://b.example"));
        assert_eq!(kinds(&c[4]), ["link", "link"]);
    }

    /// Item 4: a quote is note text with its depth; a quote in a quote is one deeper, and a list
    /// inside keeps its marks.
    #[test]
    fn a_quote_carries_its_depth() {
        let s = sec("> first\n> line\n>\n> > inner\n> - item\n");
        let got: Vec<(&str, u64, &str)> = s["children"].as_array().unwrap().iter().map(|c| (c["text"].as_str().unwrap(), c["quote"].as_u64().unwrap(), c["style"].as_str().unwrap())).collect();
        assert_eq!(got, [("first line", 1, "note"), ("inner", 2, "note"), ("• item", 1, "note")]);
    }

    /// Item 5: a task item is a bullet with a box, never a plan's todo.
    #[test]
    fn a_task_item_is_a_bullet_with_a_box() {
        let s = sec("- [ ] open\n- [x] done\n- plain [ ] brackets\n");
        let got: Vec<(&str, &str, Option<&str>)> = s["children"].as_array().unwrap().iter().map(|c| (c["type"].as_str().unwrap(), c["text"].as_str().unwrap(), c["task"].as_str())).collect();
        assert_eq!(got, [("bullet", "open", Some("open")), ("bullet", "done", Some("done")), ("bullet", "plain [ ] brackets", None)]);
    }

    /// Item 7: without --title the first `# heading` names the section; deeper headings nest,
    /// and one past the nesting the pane draws is a bold line whose text stays in place.
    #[test]
    fn headings_name_nest_and_degrade() {
        let s = to_section("t", None, "# The *Title*\n\n## A\n### B\n#### C\n##### D\n###### E ##\nbody\n");
        assert_eq!(s["title"], "The Title");
        let d = &s["children"][0]["children"][0]["children"][0]["children"][0];
        assert_eq!((d["title"].as_str().unwrap(), d["id"].as_str().unwrap()), ("D", "t-d"));
        assert_eq!(kinds(d), ["text", "text"]);
        assert_eq!((d["children"][0]["text"].as_str().unwrap(), d["children"][1]["text"].as_str().unwrap()), ("**E**", "body"));
        assert_eq!(to_section("t", Some("Given"), "# Swallowed\n\nbody\n")["title"], "Given");
    }

    /// Item 8: an ordered list under a bullet, and bullets under a number, keep their numbers.
    #[test]
    fn mixed_nested_lists_keep_their_numbers() {
        let s = sec("- bullet\n  1. one\n  2) two\n\n1. first\n   - sub\n2. second\n");
        let c = &s["children"];
        assert_eq!(kinds(&s), ["bullet", "bullet", "bullet"]);
        assert_eq!((c[0]["children"][0]["text"].as_str().unwrap(), c[0]["children"][1]["text"].as_str().unwrap()), ("1. one", "2) two"));
        assert_eq!((c[1]["text"].as_str().unwrap(), c[1]["children"][0]["text"].as_str().unwrap(), c[2]["text"].as_str().unwrap()), ("1. first", "sub", "2. second"));
        assert!(crate::components::numbered("2) two").is_some());
    }

    /// Item 10: hard breaks are line breaks; an image is its alt text and path; a reference link
    /// resolves; HTML, a footnote and inline code stay as written; a fence's label is its first
    /// word; a fence under a list item is that bullet's code.
    #[test]
    fn the_rest_of_markdown() {
        let s = sec("one  \ntwo\\\nthree\n\n![a diagram](img/d.png) and [ref link][r] and [r] <br> `a ** [r] b`[^1]\n\n[r]: https://r.example \"Title\"\n[^1]: note\n\n- step\n  ```sh extra\n  ls\n  ```\n");
        let c = &s["children"];
        assert_eq!(c[0]["text"], "one\ntwo\nthree");
        assert_eq!(c[1]["text"], "a diagram (`img/d.png`) and [ref link](https://r.example) and [r](https://r.example) <br> `a ** [r] b`[^1]");
        assert_eq!(c[1]["target"], "https://r.example");
        assert_eq!(c[2]["text"], "[^1]: note");
        let code = &c[3]["children"][0];
        assert_eq!((code["type"].as_str().unwrap(), code["text"].as_str().unwrap(), code["language"].as_str().unwrap()), ("code", "ls", "sh"));
    }

    /// A revision keeps the id of everything it did not touch, so only the edited row is marked.
    #[test]
    fn an_edit_keeps_the_ids_of_what_it_left_alone() {
        let ids = |v: &Value| v["children"].as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap().to_string()).collect::<Vec<_>>();
        let a = ids(&sec("First.\n\nSecond.\n\n- item\n\n| a |\n|---|\n| 1 |\n\n---\n"));
        let b = ids(&sec("First.\n\nSecond, edited.\n\nNew.\n\n- item\n\n| a |\n|---|\n| 1 |\n| 2 |\n\n---\n"));
        assert_eq!((&a[0], &a[2..]), (&b[0], &b[3..]), "{a:?} {b:?}");
        assert_ne!(a[1], b[1]);
    }
}
