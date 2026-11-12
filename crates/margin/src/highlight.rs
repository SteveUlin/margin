//! Syntax highlighting for code blocks (and the lines of a diff block).
//!
//! The parsing is syntect's, with Sublime Text grammars: its bundled set plus the ones `bat`
//! ships (the `two-face` crate), on the pure-Rust regex engine, so the build needs no C library.
//! Jsonnet is in neither set: its grammar is ours, `syntaxes/Jsonnet.sublime-syntax`. Starlark
//! (Bazel's BUILD and .bzl files) has none either and is read as Python, which it is a subset of.
//!
//! Nothing of syntect's themes is used. A token's scopes map onto a handful of roles
//! ([`Role`]), and the pane's own palette colors each role ([`color`]): comments dim, strings
//! and keywords distinct, most of the text plain.
//!
//! Cost: the grammars load on the first block that names a language (a board without code never
//! loads them), and a block is parsed once per content: [`lines`] caches by a hash of the
//! language and the text, because the pane redraws often.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxDefinition, SyntaxReference, SyntaxSet, SyntaxSetBuilder};
use vellum::color::{Palette, Rgb};

/// What a token is, as far as color goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Plain,
    Keyword,
    Str,
    Comment,
    /// A number or a language constant (`true`, `nullptr`, `None`).
    Number,
    /// A name being defined or a known type: a function, a class, a key of a JSON or YAML map.
    Name,
    Punct,
    /// A diff's added and removed lines.
    Added,
    Removed,
}

/// A role's color in the pane's palette. Plain code is the foreground; punctuation and comments
/// step back; the four accents each mean one thing.
pub fn color(p: &Palette, r: Role) -> Rgb {
    match r {
        Role::Plain => p.fg,
        Role::Keyword => p.orange,
        Role::Str => p.aqua,
        Role::Comment => p.grey0,
        Role::Number => p.purple,
        Role::Name => p.yellow,
        Role::Punct => p.grey2,
        Role::Added => p.green,
        Role::Removed => p.red,
    }
}

static ENABLED: AtomicBool = AtomicBool::new(true);

/// Config `highlight: false` turns highlighting off: every code block draws plain.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

const JSONNET: &str = include_str!("../syntaxes/Jsonnet.sublime-syntax");

struct Sets {
    /// syntect's defaults and bat's extras.
    bundled: SyntaxSet,
    /// The grammars kept in this repository.
    ours: SyntaxSet,
    tags: Tags,
}

/// The scope prefixes the roles are read from, parsed once.
struct Tags {
    comment: Scope,
    string: Scope,
    key: [Scope; 2],
    number: [Scope; 2],
    operator: Scope,
    word_operator: [Scope; 2],
    keyword: [Scope; 3],
    name: [Scope; 5],
    punct: Scope,
    added: Scope,
    removed: Scope,
    heading: Scope,
    raw: Scope,
}

fn sets() -> &'static Sets {
    static SETS: OnceLock<Sets> = OnceLock::new();
    SETS.get_or_init(|| {
        let mut b = SyntaxSetBuilder::new();
        // a grammar of ours that fails to load is a bug the tests catch; the pane draws plain
        if let Ok(def) = SyntaxDefinition::load_from_str(JSONNET, true, Some("Jsonnet")) {
            b.add(def);
        }
        let s = |x: &str| Scope::new(x).unwrap_or_default();
        Sets {
            bundled: two_face::syntax::extra_newlines(),
            ours: b.build(),
            tags: Tags {
                comment: s("comment"),
                string: s("string"),
                key: [s("meta.mapping.key"), s("entity.name.tag")],
                number: [s("constant.numeric"), s("constant.language")],
                operator: s("keyword.operator"),
                word_operator: [s("keyword.operator.word"), s("keyword.operator.logical.python")],
                keyword: [s("keyword"), s("storage"), s("variable.language")],
                name: [s("entity.name"), s("support.type"), s("support.function"), s("support.class"), s("variable.function")],
                punct: s("punctuation"),
                added: s("markup.inserted"),
                removed: s("markup.deleted"),
                heading: s("markup.heading"),
                raw: s("markup.raw"),
            },
        }
    })
}

/// The grammar for a block's label: a language name (`cpp`, `python`, `jsonnet`), an extension
/// (`cc`, `bzl`, `libsonnet`) or a file name (`BUILD.bazel`, `main.rs`). None: draw it plain.
fn syntax_for(label: &str) -> Option<(&'static SyntaxSet, &'static SyntaxReference)> {
    let sets = sets();
    let label = label.trim();
    // a path names its file; a file names its extension
    let file = label.rsplit('/').next().unwrap_or(label);
    let lower = file.to_ascii_lowercase();
    let ext = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    let stem = lower.split('.').next().unwrap_or("");
    let token = if matches!(lower.as_str(), "starlark" | "bazel" | "bzl" | "build" | "workspace" | "module" | "buck" | "tiltfile") || matches!(ext, "bzl" | "bazel" | "star") || (matches!(stem, "build" | "workspace" | "module") && matches!(ext, "bazel" | "bzlmod")) {
        // Starlark reads as Python: the same keywords, strings, comments and calls
        "python"
    } else {
        match lower.as_str() {
            "c++" | "cxx" | "hpp" | "hxx" | "hh" | "h++" => "cpp",
            "libsonnet" => "jsonnet",
            "shell" | "zsh" | "console" => "sh",
            "jsonc" | "json5" => "json",
            "yml" => "yaml",
            "rs" => "rust",
            "py" => "python",
            _ => "",
        }
    };
    let find = |t: &str| -> Option<(&'static SyntaxSet, &'static SyntaxReference)> {
        if t.is_empty() {
            return None;
        }
        sets.ours.find_syntax_by_token(t).map(|s| (&sets.ours, s)).or_else(|| sets.bundled.find_syntax_by_token(t).map(|s| (&sets.bundled, s)))
    };
    find(token).or_else(|| find(&lower)).or_else(|| find(ext)).filter(|(_, s)| s.name != "Plain Text")
}

/// The role of a token under these scopes (outermost first).
fn role(stack: &[Scope], t: &Tags) -> Role {
    let has = |p: Scope| stack.iter().any(|s| p.is_prefix_of(*s));
    if has(t.comment) {
        return Role::Comment;
    }
    if has(t.added) {
        return Role::Added;
    }
    if has(t.removed) {
        return Role::Removed;
    }
    // a map's key is a name, though the grammar also calls it a string
    if t.key.iter().any(|k| has(*k)) {
        return Role::Name;
    }
    if has(t.string) || has(t.raw) {
        return Role::Str;
    }
    for s in stack.iter().rev() {
        let is = |ps: &[Scope]| ps.iter().any(|p| p.is_prefix_of(*s));
        if is(&t.number) {
            return Role::Number;
        }
        if is(&t.word_operator) {
            return Role::Keyword;
        }
        // + - = and the like stay plain: coloring them tells nothing apart
        if is(&[t.operator]) {
            return Role::Plain;
        }
        if is(&t.keyword) || is(&[t.heading]) {
            return Role::Keyword;
        }
        if is(&t.name) {
            return Role::Name;
        }
        if is(&[t.punct]) {
            return Role::Punct;
        }
    }
    Role::Plain
}

/// One highlighted line: its text in runs, each with its role.
pub type Line = Vec<(String, Role)>;

thread_local! {
    static CACHE: RefCell<HashMap<u64, Option<Rc<Vec<Line>>>>> = RefCell::new(HashMap::new());
}

/// The most blocks kept parsed; past it the cache starts over (a pane shows far fewer).
const CACHE_MAX: usize = 256;
/// A text longer than this draws plain: parsing it would stall a frame.
const TEXT_MAX: usize = 256 * 1024;

/// `text` highlighted as `label`'s language, a [`Line`] per line of it (split at `\n`). None
/// when highlighting is off, the label names no grammar, or the text is too long: the caller
/// draws it plain. Parsed once per label and text.
pub fn lines(label: &str, text: &str) -> Option<Rc<Vec<Line>>> {
    if label.trim().is_empty() || text.len() > TEXT_MAX || !ENABLED.load(Ordering::Relaxed) {
        return None;
    }
    let key = crate::util::fnv64(format!("{label}\0{text}").as_bytes());
    if let Some(hit) = CACHE.with(|c| c.borrow().get(&key).cloned()) {
        return hit;
    }
    let out = parse(label, text).map(Rc::new);
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() >= CACHE_MAX {
            c.clear();
        }
        c.insert(key, out.clone());
    });
    out
}

fn parse(label: &str, text: &str) -> Option<Vec<Line>> {
    let (set, syntax) = syntax_for(label)?;
    let tags = &sets().tags;
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut out = Vec::new();
    for raw in text.split('\n') {
        // the grammars expect each line to end in its newline
        let line = format!("{raw}\n");
        // a grammar that fails on a line (a regex the engine refuses) leaves the block plain
        let ops = state.parse_line(&line, set).ok()?;
        let mut runs: Line = Vec::new();
        let mut push = |from: usize, to: usize, stack: &ScopeStack| {
            let s = line[from..to].trim_end_matches('\n');
            if s.is_empty() {
                return;
            }
            let r = role(stack.as_slice(), tags);
            match runs.last_mut() {
                Some((t, lr)) if *lr == r => t.push_str(s),
                _ => runs.push((s.to_string(), r)),
            }
        };
        let mut at = 0;
        for (i, op) in ops {
            let i = i.min(line.len());
            if i > at {
                push(at, i, &stack);
                at = i;
            }
            stack.apply(&op).ok()?;
        }
        push(at, line.len(), &stack);
        out.push(runs);
    }
    Some(out)
}

/// A line's role per character, for a caller that cuts or wraps the line itself.
pub fn per_char(line: &Line) -> Vec<Role> {
    line.iter().flat_map(|(s, r)| s.chars().map(move |_| *r)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The role of the first run of `line` that holds `word`.
    fn role_of(lines: &[Line], word: &str) -> Role {
        lines.iter().flatten().find(|(s, _)| s.contains(word)).map(|(_, r)| *r).unwrap_or_else(|| panic!("{word:?} is in no run: {lines:#?}"))
    }

    #[test]
    fn the_short_list_of_languages_has_grammars() {
        for label in ["cpp", "c++", "cc", "h", "python", "py", "json", "jsonnet", "libsonnet", "starlark", "bazel", "bzl", "BUILD", "BUILD.bazel", "WORKSPACE", "defs.bzl", "rust", "rs", "sh", "bash", "toml", "yaml", "yml", "markdown", "md", "diff", "src/main.cc", "lib/x.libsonnet"] {
            assert!(syntax_for(label).is_some(), "{label}");
        }
        assert_eq!(syntax_for("BUILD").unwrap().1.name, "Python", "Starlark reads as Python");
        assert_eq!(syntax_for("x.libsonnet").unwrap().1.name, "Jsonnet");
        for label in ["", "text", "nope", "output"] {
            assert!(lines(label, "x = 1").is_none(), "{label:?} draws plain");
        }
    }

    #[test]
    fn tokens_take_their_roles() {
        let cpp = lines("cpp", "// the cap\nstatic const int kCap = 5;\nstd::string name(\"retry\");\nreturn nullptr;").unwrap();
        assert_eq!(cpp.len(), 4);
        assert_eq!((role_of(&cpp, "the cap"), role_of(&cpp, "const"), role_of(&cpp, "5"), role_of(&cpp, "retry"), role_of(&cpp, "return"), role_of(&cpp, "nullptr")), (Role::Comment, Role::Keyword, Role::Number, Role::Str, Role::Keyword, Role::Number));
        assert_eq!(role_of(&cpp, "kCap"), Role::Plain, "most of the text stays plain");
        // every line comes back whole
        assert_eq!(cpp[1].iter().map(|(s, _)| s.as_str()).collect::<String>(), "static const int kCap = 5;");

        let py = lines("BUILD.bazel", "# a rule\ncc_library(\n    name = \"retry\",\n    deps = [\":clock\"] if True else [],\n)\ndef f(x):\n    return not x").unwrap();
        assert_eq!((role_of(&py, "a rule"), role_of(&py, "retry"), role_of(&py, "if"), role_of(&py, "def"), role_of(&py, "not")), (Role::Comment, Role::Str, Role::Keyword, Role::Keyword, Role::Keyword));

        let js = lines("jsonnet", "local lib = import 'lib.libsonnet';\n// a comment\n{\n  name: \"x\",\n  replicas: 3,\n  on: true,\n  text: |||\n    block\n  |||,\n  f(x):: if x then std.length(x) else self.name,\n}").unwrap();
        assert_eq!((role_of(&js, "local"), role_of(&js, "import"), role_of(&js, "lib.libsonnet"), role_of(&js, "a comment"), role_of(&js, "3"), role_of(&js, "true"), role_of(&js, "block"), role_of(&js, "if"), role_of(&js, "self")), (Role::Keyword, Role::Keyword, Role::Str, Role::Comment, Role::Number, Role::Number, Role::Str, Role::Keyword, Role::Keyword));

        let json = lines("json", "{\"name\": \"x\", \"n\": 3, \"ok\": null}").unwrap();
        assert_eq!((role_of(&json, "name"), role_of(&json, "x"), role_of(&json, "3"), role_of(&json, "null")), (Role::Name, Role::Str, Role::Number, Role::Number));

        let diff = lines("diff", "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new\n same").unwrap();
        assert_eq!((role_of(&diff, "old"), role_of(&diff, "new"), role_of(&diff, "same")), (Role::Removed, Role::Added, Role::Plain));
    }

    #[test]
    fn a_block_is_parsed_once_and_off_means_plain() {
        let a = lines("python", "x = 1  # once").unwrap();
        let b = lines("python", "x = 1  # once").unwrap();
        assert!(Rc::ptr_eq(&a, &b), "the second draw reads the cache");
        assert_eq!(per_char(&a[0]).len(), "x = 1  # once".chars().count());
    }
}
