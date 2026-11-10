//! `margin doc`: markdown in, one section component out. Long-form content (an overview, a
//! proposal, a comparison) belongs beside the chat, where it stays on screen while sulin replies;
//! writing it as JSON blocks cost too much, so Claude writes markdown and this turns it into the
//! components the pane already draws.
//!
//! Headings become nested sections, paragraphs `text`, lists nested `bullet`s, fences and tables
//! `code`, quotes a `note`. Ids come from the content (a heading's slug, a paragraph's hash), not
//! its position: a revision that edits one paragraph marks only that row as changed, and an
//! inserted bullet doesn't shift every id after it.

use serde_json::{json, Map, Value};
use std::collections::HashSet;

/// The section `id`, titled `title` or the document's first `# heading`.
pub fn to_section(id: &str, title: Option<&str>, md: &str) -> Value {
    let mut p = Parser { ids: HashSet::new(), root: id.to_string() };
    p.ids.insert(id.to_string());
    let lines: Vec<&str> = md.lines().collect();
    // a leading `# Title` names the section when no --title was given
    let (mut start, mut title) = (0, title.map(String::from));
    if let Some(i) = lines.iter().position(|l| !l.trim().is_empty()) {
        if let Some(h) = lines[i].strip_prefix("# ") {
            if title.is_none() {
                title = Some(h.trim().to_string());
            }
            start = i + 1;
        }
    }
    let children = p.blocks(&lines[start..]);
    let mut sec = Map::new();
    sec.insert("type".into(), json!("section"));
    sec.insert("id".into(), json!(id));
    sec.insert("title".into(), json!(title.unwrap_or_else(|| id.to_string())));
    sec.insert("children".into(), Value::Array(children));
    Value::Object(sec)
}

struct Parser {
    ids: HashSet<String>,
    root: String,
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

/// `[text](url)` → `text`: the pane has no inline links, and the url would only crowd the line.
fn inline(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('[') {
        let after = &rest[i + 1..];
        match after.find("](").and_then(|j| after[j + 2..].find(')').map(|k| (j, k))) {
            Some((j, k)) => {
                out.push_str(&rest[..i]);
                out.push_str(&after[..j]);
                rest = &after[j + 2 + k + 1..];
            }
            None => {
                out.push_str(&rest[..=i]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn list_item(l: &str) -> Option<(usize, String)> {
    let indent = l.len() - l.trim_start().len();
    let t = l.trim_start();
    for m in ["- ", "* ", "+ "] {
        if let Some(r) = t.strip_prefix(m) {
            return Some((indent, r.to_string()));
        }
    }
    let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 && t[digits..].starts_with(". ") {
        return Some((indent, t.to_string()));
    }
    None
}

fn heading(l: &str) -> Option<(usize, &str)> {
    let n = l.chars().take_while(|&c| c == '#').count();
    (n >= 1 && n <= 6 && l[n..].starts_with(' ')).then(|| (n, l[n + 1..].trim()))
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

    fn leaf(&mut self, kind: &str, text: &str, extra: &[(&str, Value)]) -> Value {
        let mut m = Map::new();
        m.insert("type".into(), json!(kind));
        m.insert("id".into(), json!(self.id(&hash4(text))));
        m.insert("text".into(), json!(text));
        for (k, v) in extra {
            m.insert((*k).into(), v.clone());
        }
        Value::Object(m)
    }

    /// Blocks for a run of lines; a heading opens a section holding everything up to the next
    /// heading of its level or above.
    fn blocks(&mut self, lines: &[&str]) -> Vec<Value> {
        let mut out = Vec::new();
        let code = fenced(lines);
        let mut i = 0;
        while i < lines.len() {
            let l = lines[i];
            let t = l.trim();
            if t.is_empty() || t == "---" || t == "***" {
                i += 1;
            } else if let Some((lvl, h)) = heading(t) {
                let end = (i + 1..lines.len()).find(|&j| !code[j] && heading(lines[j].trim()).map(|(n, _)| n <= lvl).unwrap_or(false)).unwrap_or(lines.len());
                let id = self.id(&slug(h));
                let kids = self.blocks(&lines[i + 1..end]);
                out.push(json!({"type": "section", "id": id, "title": inline(h), "children": kids}));
                i = end;
            } else if let Some(f) = fence_open(t) {
                let lang = t.trim_start_matches(f.0).trim().to_string();
                let end = (i + 1..lines.len()).find(|&j| fence_close(lines[j].trim(), f)).unwrap_or(lines.len());
                let body = lines[i + 1..end].join("\n");
                out.push(self.leaf("code", &body, &[("language", json!(lang))]));
                i = end + 1;
            } else if t.starts_with('|') {
                // a table keeps its columns as monospace text
                let end = (i..lines.len()).find(|&j| !lines[j].trim().starts_with('|')).unwrap_or(lines.len());
                let body: Vec<&str> = lines[i..end].iter().map(|l| l.trim()).filter(|l| !l.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))).collect();
                out.push(self.leaf("code", &body.join("\n"), &[]));
                i = end;
            } else if t.starts_with('>') {
                let end = (i..lines.len()).find(|&j| !lines[j].trim().starts_with('>')).unwrap_or(lines.len());
                let text = lines[i..end].iter().map(|l| l.trim().trim_start_matches('>').trim()).collect::<Vec<_>>().join(" ");
                out.push(self.leaf("text", &inline(&text), &[("style", json!("note"))]));
                i = end;
            } else if list_item(l).is_some() {
                let end = (i..lines.len())
                    .find(|&j| {
                        let lj = lines[j];
                        let tj = lj.trim();
                        // a list runs until a blank line followed by a non-list, unindented line
                        // (never a line of a fence indented under an item, whatever it looks like)
                        !code[j] && !tj.is_empty() && list_item(lj).is_none() && lj.len() == lj.trim_start().len() && (j == 0 || lines[j - 1].trim().is_empty() || heading(tj).is_some() || fence_open(tj).is_some())
                    })
                    .unwrap_or(lines.len());
                out.extend(self.list(&lines[i..end]));
                i = end;
            } else {
                let end = (i..lines.len())
                    .find(|&j| {
                        let tj = lines[j].trim();
                        tj.is_empty() || heading(tj).is_some() || fence_open(tj).is_some() || tj.starts_with('|') || tj.starts_with('>') || list_item(lines[j]).is_some()
                    })
                    .unwrap_or(lines.len());
                let text = lines[i..end].iter().map(|l| l.trim()).collect::<Vec<_>>().join(" ");
                out.push(self.leaf("text", &inline(&text), &[]));
                i = end;
            }
        }
        out
    }

    /// Nested bullets from indentation; a continuation line joins the bullet above it.
    fn list(&mut self, lines: &[&str]) -> Vec<Value> {
        // (indent, text, children) flattened first, then nested
        let mut items: Vec<(usize, String)> = Vec::new();
        for l in lines {
            if let Some((ind, t)) = list_item(l) {
                items.push((ind, t));
            } else if !l.trim().is_empty() {
                if let Some(last) = items.last_mut() {
                    last.1.push(' ');
                    last.1.push_str(l.trim());
                }
            }
        }
        let mut pos = 0;
        self.nest(&items, &mut pos, 0)
    }

    fn nest(&mut self, items: &[(usize, String)], pos: &mut usize, indent: usize) -> Vec<Value> {
        let mut out = Vec::new();
        while *pos < items.len() && items[*pos].0 >= indent {
            let (ind, text) = items[*pos].clone();
            *pos += 1;
            let mut b = match self.leaf("bullet", &inline(&text), &[]) {
                Value::Object(m) => m,
                _ => unreachable!(),
            };
            if *pos < items.len() && items[*pos].0 > ind {
                let deeper = items[*pos].0;
                b.insert("children".into(), Value::Array(self.nest(items, pos, deeper)));
            }
            out.push(Value::Object(b));
        }
        out
    }
}

/// Paragraphs that would read better as bullets: more than two sentences in one row. sulin likes
/// bullets, and a bullet is a row he can react to on its own (2026-11-08).
pub fn long_paragraphs(sec: &Value) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    fn walk(v: &Value, out: &mut Vec<(String, usize)>) {
        if v["type"] == "text" && v["style"] != "note" {
            let t = v["text"].as_str().unwrap_or("");
            let n = t.split_inclusive(['.', '?', '!']).filter(|s| s.trim().len() > 2).count();
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
        assert_eq!(kinds(opts), ["bullet", "bullet", "code", "code", "text"]);
        assert_eq!(opts["children"][0]["children"][0]["text"], "falls back to overlay");
        assert_eq!(opts["children"][2]["language"], "rust");
        assert_eq!(opts["children"][3]["text"], "| a | b |\n| 1 | 2 |");
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

    #[test]
    fn links_keep_their_text() {
        assert_eq!(inline("see [the guide](docs/guide.md) and [x"), "see the guide and [x");
    }
}
