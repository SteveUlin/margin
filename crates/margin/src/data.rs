//! Datasets: values a board displays, kept outside every document.
//!
//! A board holds layout and authored text, which change rarely. A dataset holds the numbers and
//! records a board draws, which change often: it has a name, belongs to a project, is registered
//! explicitly (`margin data add`), and is READ by the pane when it draws. The pane never runs
//! anything to get data, and a dataset changing is not a document changing: no revision, no
//! change bar, no unseen count.
//!
//! On disk: the registry `data.json` (name → {owner?, cap?, file?, expect_every?}) and, for a
//! dataset margin holds, `data/<name>.jsonl`, one record per line. A record is a JSON object or a
//! bare number. A dataset registered with `--file` is a file its provider already writes (JSON
//! lines, or CSV with a header row): margin reads it in place and never writes it.
//!
//! One writer per file: an append is one `write` with O_APPEND under the project lock, `put`
//! and the cap's trim replace the file by rename, and a reader tolerates a torn last line.
//!
//! Reads are tail-bounded (`read_tail` seeks from the end and stops once it has the records that
//! were asked for) and cached by (path, length, mtime), so a large log costs the pane a few
//! kilobytes a frame at most, and nothing while it has not changed.

use crate::doc::{Block, Doc};
use crate::store::{write_atomic, Store};
use crate::util::{parse_rfc3339, valid_id};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// One registered dataset.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Dataset {
    /// The registered owner (owner.rs) that alone may write it, with `--as`. None: any local writer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// Keep the newest `cap` records (trimmed by rewrite once the file passes twice that).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cap: Option<u64>,
    /// An external file, absolute: read in place, never written by margin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// How often it means to change, in seconds. Past it, what is bound to it shows as stale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_every: Option<f64>,
    #[serde(default)]
    pub added: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Registry {
    #[serde(default)]
    pub datasets: BTreeMap<String, Dataset>,
}

impl Registry {
    pub fn path(dir: &Path) -> PathBuf {
        dir.join("data.json")
    }
    pub fn load(dir: &Path) -> Registry {
        Self::load_strict(dir).unwrap_or_default()
    }
    pub fn load_strict(dir: &Path) -> Result<Registry, String> {
        match std::fs::read_to_string(Self::path(dir)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Registry::default()),
            Err(e) => Err(format!("{}: {e}", Self::path(dir).display())),
            Ok(s) => serde_json::from_str(&s).map_err(|e| format!("{} is corrupt ({e}); fix or remove it", Self::path(dir).display())),
        }
    }
    pub fn save(&self, dir: &Path) -> Result<(), String> {
        crate::store::write_durable(&Self::path(dir), serde_json::to_string_pretty(self).unwrap().as_bytes()).map_err(|e| e.to_string())
    }
}

/// How a dataset's file is read: by the file's extension.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Format {
    Jsonl,
    Csv,
}

pub fn format_of(path: &Path) -> Format {
    match path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("csv") => Format::Csv,
        _ => Format::Jsonl,
    }
}

/// The line a component draws in place of a dataset that is not registered.
pub fn hint(name: &str) -> String {
    format!("no dataset {name:?} (margin data add {name})")
}

/// A record margin stores: a JSON object, or a bare number (one unnamed series).
fn check_record(v: &Value) -> Result<(), String> {
    match v {
        Value::Object(_) | Value::Number(_) => Ok(()),
        other => Err(format!("a record is a JSON object or a number, got {other}")),
    }
}

/// Records from text: one JSON array, or JSON lines (blank lines skipped).
pub fn parse_records(text: &str) -> Result<Vec<Value>, String> {
    let t = text.trim();
    let recs: Vec<Value> = if t.starts_with('[') {
        serde_json::from_str::<Vec<Value>>(t).map_err(|e| format!("not a JSON array of records: {e}"))?
    } else {
        t.lines().filter(|l| !l.trim().is_empty()).enumerate().map(|(i, l)| serde_json::from_str(l.trim()).map_err(|e| format!("line {}: {e}", i + 1))).collect::<Result<_, _>>()?
    };
    recs.iter().try_for_each(check_record)?;
    Ok(recs)
}

/// One record from a command-line word: a number, or a JSON object.
pub fn parse_record(word: &str) -> Result<Value, String> {
    let v: Value = serde_json::from_str(word.trim()).map_err(|_| format!("{word:?} is not a number or a JSON object"))?;
    check_record(&v)?;
    Ok(v)
}

fn lines_of(recs: &[Value]) -> String {
    recs.iter().map(|r| r.to_string() + "\n").collect()
}

fn ends_with_newline(p: &Path) -> bool {
    let mut b = [0u8; 1];
    std::fs::File::open(p).and_then(|mut f| f.seek(SeekFrom::End(-1)).and_then(|_| f.read_exact(&mut b))).map(|_| b[0] == b'\n').unwrap_or(true)
}

impl Store {
    pub fn data_dir(&self) -> PathBuf {
        self.dir().join("data")
    }
    /// Where a dataset's records are: its external file, else `data/<name>.jsonl`.
    pub fn data_path(&self, name: &str, ds: &Dataset) -> PathBuf {
        match &ds.file {
            Some(f) => PathBuf::from(f),
            None => self.data_dir().join(format!("{name}.jsonl")),
        }
    }
    /// The sidecar of a capped dataset: `lines length`, trusted while the file is still that long
    /// (the events log's rule), so an append knows the line count without reading the file.
    fn data_count_path(&self, name: &str) -> PathBuf {
        self.data_dir().join(format!("{name}.n"))
    }

    /// The one message for a dataset this project doesn't have, in the words a missing artifact
    /// gets, with the nearest registered name.
    pub fn missing_data(&self, name: &str) -> String {
        let reg = Registry::load(&self.dir());
        let near = reg.datasets.keys().filter(|n| *n != name).map(|n| (crate::util::edit_distance(name, n), n)).filter(|(d, _)| *d <= 2).min();
        let hint = near.map(|(_, n)| format!(" (did you mean {n:?}?)")).unwrap_or_default();
        format!("no dataset {name:?} in project {}: create it with margin data add {name}{hint}", self.project)
    }

    fn check_dataset(&self, name: &str, ds: &Dataset) -> Result<(), String> {
        if !valid_id(name) {
            return Err(format!("bad dataset name {name:?}: letters, digits, - and _"));
        }
        if ds.cap == Some(0) {
            return Err("--cap must be 1 or more, or `none`".into());
        }
        if ds.expect_every.map(|e| e <= 0.0).unwrap_or(false) {
            return Err("--expect-every must be more than 0 (10s, 5m), or `none`".into());
        }
        if ds.file.is_some() && ds.cap.is_some() {
            return Err("--cap is for a dataset margin holds: a --file dataset is its provider's to trim".into());
        }
        if ds.file.is_some() && ds.owner.is_some() {
            return Err("--owner is for a dataset margin holds: margin never writes a --file dataset".into());
        }
        if let Some(o) = &ds.owner {
            if crate::owner::Registry::load_strict(&self.dir())?.get(o).is_none() {
                return Err(crate::owner::missing(o));
            }
        }
        Ok(())
    }

    /// `margin data add`: register a dataset. One that exists is refused.
    pub fn data_add(&self, name: &str, ds: Dataset) -> Result<(), String> {
        let _l = self.lock().map_err(|e| e.to_string())?;
        let mut r = Registry::load_strict(&self.dir())?;
        if r.datasets.contains_key(name) {
            return Err(format!("dataset {name:?} exists: change it with margin data set {name} …, or margin data rm {name} first"));
        }
        self.check_dataset(name, &ds)?;
        if ds.file.is_none() {
            // an empty file, so a board bound to it reads "no records yet" and not "no dataset"
            std::fs::create_dir_all(self.data_dir()).map_err(|e| e.to_string())?;
            let p = self.data_path(name, &ds);
            if !p.exists() {
                std::fs::write(&p, b"").map_err(|e| format!("{}: {e}", p.display()))?;
            }
        }
        r.datasets.insert(name.to_string(), ds);
        r.save(&self.dir())
    }

    /// `margin data set`: change a registration that exists.
    pub fn data_set(&self, name: &str, change: impl FnOnce(&mut Dataset)) -> Result<Dataset, String> {
        let _l = self.lock().map_err(|e| e.to_string())?;
        let mut r = Registry::load_strict(&self.dir())?;
        let mut ds = r.datasets.get(name).cloned().ok_or_else(|| self.missing_data(name))?;
        let held = ds.file.is_none();
        change(&mut ds);
        self.check_dataset(name, &ds)?;
        if ds.file.is_none() && !held {
            std::fs::create_dir_all(self.data_dir()).map_err(|e| e.to_string())?;
            let p = self.data_path(name, &ds);
            if !p.exists() {
                std::fs::write(&p, b"").map_err(|e| format!("{}: {e}", p.display()))?;
            }
        }
        r.datasets.insert(name.to_string(), ds.clone());
        r.save(&self.dir())?;
        Ok(ds)
    }

    /// `margin data rm`: unregister. Records margin holds move to `.archive/` (the store never
    /// deletes); an external file is left alone.
    pub fn data_rm(&self, name: &str) -> Result<Option<PathBuf>, String> {
        let _l = self.lock().map_err(|e| e.to_string())?;
        let mut r = Registry::load_strict(&self.dir())?;
        let ds = r.datasets.remove(name).ok_or_else(|| self.missing_data(name))?;
        r.save(&self.dir())?;
        let _ = std::fs::remove_file(self.data_count_path(name));
        if ds.file.is_some() {
            return Ok(None);
        }
        let p = self.data_path(name, &ds);
        if !p.exists() {
            return Ok(None);
        }
        let dir = self.dir().join(".archive");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let base = format!("data-{name}-{}", &crate::util::now_rfc3339()[..10]);
        let dest = (1..).map(|n| dir.join(if n == 1 { format!("{base}.jsonl") } else { format!("{base}-{n}.jsonl") })).find(|p| !p.exists()).unwrap();
        std::fs::rename(&p, &dest).map_err(|e| format!("archive {name}: {e}"))?;
        Ok(Some(dest))
    }

    /// The dataset a write goes to, checked: registered, held by margin, and written by the
    /// owner it names (through the owners table) when it names one.
    fn data_writable(&self, name: &str, as_owner: Option<&str>, verb: &str) -> Result<Dataset, String> {
        let ds = Registry::load_strict(&self.dir())?.datasets.get(name).cloned().ok_or_else(|| self.missing_data(name))?;
        if let Some(f) = &ds.file {
            return Err(format!("dataset {name} is the external file {f}: margin reads it and never writes it (its provider does); margin data {verb} is for datasets margin holds"));
        }
        if let Some(o) = as_owner {
            if crate::owner::Registry::load_strict(&self.dir())?.get(o).is_none() {
                return Err(crate::owner::missing(o));
            }
        }
        match (&ds.owner, as_owner) {
            (Some(o), Some(w)) if o == w => {}
            (Some(o), Some(w)) => return Err(format!("dataset {name} belongs to owner `{o}`, not {w}: margin data {verb} {name} --as {o}")),
            (Some(o), None) => return Err(format!("dataset {name} belongs to owner `{o}`: only it writes, with margin data {verb} {name} --as {o}")),
            (None, _) => {}
        }
        Ok(ds)
    }

    /// `margin data put`: replace the content (temp file, then rename). With a cap, the newest
    /// `cap` records are kept.
    pub fn data_put(&self, name: &str, as_owner: Option<&str>, recs: &[Value]) -> Result<usize, String> {
        let _l = self.lock().map_err(|e| e.to_string())?;
        let ds = self.data_writable(name, as_owner, "put")?;
        recs.iter().try_for_each(check_record)?;
        let keep = match ds.cap {
            Some(c) if recs.len() as u64 > c => &recs[recs.len() - c as usize..],
            _ => recs,
        };
        let p = self.data_path(name, &ds);
        let text = lines_of(keep);
        write_atomic(&p, text.as_bytes()).map_err(|e| format!("{}: {e}", p.display()))?;
        self.data_count_save(name, keep.len() as u64, text.len() as u64);
        Ok(keep.len())
    }

    fn data_count_save(&self, name: &str, lines: u64, len: u64) {
        let p = self.data_count_path(name);
        let tmp = p.with_extension("n.tmp");
        let _ = std::fs::write(&tmp, format!("{lines} {len}")).and_then(|_| std::fs::rename(&tmp, &p));
    }

    /// `margin data append`: add records at the end, as one `write` with O_APPEND. With a cap the
    /// file is trimmed to the newest `cap` records once it holds more than twice that, by a
    /// rewrite (temp file, then rename): never on every append. Returns how many records the file
    /// holds afterwards when that is known (a capped dataset), else how many were appended.
    pub fn data_append(&self, name: &str, as_owner: Option<&str>, recs: &[Value]) -> Result<usize, String> {
        if recs.is_empty() {
            return Err("nothing to append".into());
        }
        recs.iter().try_for_each(check_record)?;
        let _l = self.lock().map_err(|e| e.to_string())?;
        let ds = self.data_writable(name, as_owner, "append")?;
        let p = self.data_path(name, &ds);
        std::fs::create_dir_all(self.data_dir()).map_err(|e| e.to_string())?;
        let len = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
        let mut text = lines_of(recs);
        // a crash mid-append leaves a torn last line: end it first, or this record would join it
        if len > 0 && !ends_with_newline(&p) {
            text.insert(0, '\n');
        }
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        f.write_all(text.as_bytes()).map_err(|e| format!("{}: {e}", p.display()))?;
        drop(f);
        let Some(cap) = ds.cap else { return Ok(recs.len()) };
        // the line count: the sidecar while the file was still as long as it says, else a count
        let side = std::fs::read_to_string(self.data_count_path(name)).ok().and_then(|s| {
            let mut it = s.split_whitespace().map(|x| x.parse::<u64>().ok());
            Some((it.next()??, it.next()??))
        });
        let new_len = len + text.len() as u64;
        let lines = match side {
            Some((n, l)) if l == len => n + recs.len() as u64,
            _ => std::fs::read(&p).map(|b| b.iter().filter(|c| **c == b'\n').count() as u64).unwrap_or(0),
        };
        if lines <= cap * 2 {
            self.data_count_save(name, lines, new_len);
            return Ok(lines as usize);
        }
        let all = std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        let text = String::from_utf8_lossy(&all);
        let kept: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        let kept = &kept[kept.len().saturating_sub(cap as usize)..];
        let out: String = kept.iter().map(|l| format!("{l}\n")).collect();
        write_atomic(&p, out.as_bytes()).map_err(|e| format!("{}: {e}", p.display()))?;
        self.data_count_save(name, kept.len() as u64, out.len() as u64);
        Ok(kept.len())
    }

    /// Why a dataset is overdue, or None: the rule an owner's rows follow (owner.rs `overdue`),
    /// measured from the file's last change.
    pub fn data_stale(&self, name: &str, ds: &Dataset, now: f64) -> Option<String> {
        crate::owner::overdue(ds.expect_every, last_change(&self.data_path(name, ds)), parse_rfc3339(&ds.added), now)
    }
}

/// When a dataset's file last changed (epoch seconds); None for a missing or empty one.
pub fn last_change(p: &Path) -> Option<f64> {
    let m = std::fs::metadata(p).ok()?;
    if m.len() == 0 {
        return None;
    }
    m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs_f64())
}

// ------------------------------------------------------------------ reading

/// The newest records of a file, and what reading them cost.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tail {
    pub records: Vec<Value>,
    /// Bytes read from the file for this.
    pub bytes: u64,
}

/// How much of a file one tail read takes at a time, and at most: a file of very long lines
/// yields the records that fit in the limit rather than stalling the pane.
const CHUNK: u64 = 64 * 1024;
const TAIL_MAX: u64 = 16 * 1024 * 1024;

thread_local! {
    /// Bytes read from dataset files on this thread (tests assert on it).
    static BYTES_READ: Cell<u64> = const { Cell::new(0) };
    /// Tail reads that went to the file on this thread (a cache hit is not one).
    static READS: Cell<u64> = const { Cell::new(0) };
    static CURRENT: RefCell<Rc<Snapshot>> = RefCell::new(Rc::new(Snapshot::default()));
}

pub fn bytes_read() -> u64 {
    BYTES_READ.with(|b| b.get())
}
pub fn reads() -> u64 {
    READS.with(|b| b.get())
}

/// A CSV line's fields: commas, double quotes around a field, `""` for a quote inside one.
fn csv_fields(line: &str) -> Vec<String> {
    let (mut out, mut cur, mut quoted) = (Vec::new(), String::new(), false);
    let mut it = line.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '"' if quoted && it.peek() == Some(&'"') => {
                cur.push('"');
                it.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => out.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    out.push(cur);
    out
}

/// A CSV field as a value: a number when it reads as one, null when empty, else text.
fn csv_value(s: &str) -> Value {
    let t = s.trim();
    if t.is_empty() {
        return Value::Null;
    }
    match t.parse::<f64>() {
        Ok(n) if n.is_finite() => serde_json::Number::from_f64(n).map(Value::Number).unwrap_or(Value::Null),
        _ => Value::String(t.to_string()),
    }
}

/// The newest `n` records of the file at `path`, reading from its end: whole chunks backwards
/// until `n` lines are in hand (or the start of the file, or `TAIL_MAX`). A CSV file also costs
/// its header line. Lines that do not parse are skipped. A last line with no newline is a write
/// in progress: it is kept only when it cannot be a torn one (a complete JSON object; a CSV row
/// with a field for every column) and dropped otherwise (`71` may be the start of `71.4`).
pub fn read_tail(path: &Path, n: usize, format: Format) -> std::io::Result<Tail> {
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let mut read = 0u64;
    let mut header: Option<Vec<String>> = None;
    if format == Format::Csv {
        let mut buf = vec![0u8; CHUNK.min(len) as usize];
        f.read_exact(&mut buf)?;
        read += buf.len() as u64;
        let first = String::from_utf8_lossy(&buf);
        header = Some(csv_fields(first.lines().next().unwrap_or("").trim_end_matches('\r')).into_iter().map(|h| h.trim().to_string()).collect());
    }
    // backwards until n + 1 newlines: n whole lines, and the end of the line before them
    let (mut start, mut buf) = (len, Vec::<u8>::new());
    while start > 0 && (len - start) < TAIL_MAX {
        let take = CHUNK.min(start);
        start -= take;
        let mut chunk = vec![0u8; take as usize];
        f.seek(SeekFrom::Start(start))?;
        f.read_exact(&mut chunk)?;
        read += take;
        chunk.extend_from_slice(&buf);
        buf = chunk;
        if buf.iter().filter(|c| **c == b'\n').count() > n {
            break;
        }
    }
    BYTES_READ.with(|b| b.set(b.get() + read));
    READS.with(|b| b.set(b.get() + 1));
    let text = String::from_utf8_lossy(&buf);
    let terminated = text.ends_with('\n') || text.is_empty();
    let mut lines: Vec<&str> = text.lines().collect();
    if start > 0 && !lines.is_empty() {
        // the read began inside a line
        lines.remove(0);
    } else if header.is_some() && !lines.is_empty() {
        lines.remove(0);
    }
    let last = lines.len().saturating_sub(1);
    let mut records = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let l = l.trim_end_matches('\r');
        if l.trim().is_empty() {
            continue;
        }
        let whole = terminated || i < last;
        match &header {
            Some(h) => {
                let fields = csv_fields(l);
                if !whole && fields.len() < h.len() {
                    continue;
                }
                let m: Map<String, Value> = h.iter().zip(fields.iter()).filter(|(k, _)| !k.is_empty()).map(|(k, v)| (k.clone(), csv_value(v))).collect();
                records.push(Value::Object(m));
            }
            None => match serde_json::from_str::<Value>(l) {
                Ok(v @ Value::Object(_)) => records.push(v),
                Ok(v @ Value::Number(_)) if whole => records.push(v),
                _ => {}
            },
        }
    }
    if records.len() > n {
        records.drain(..records.len() - n);
    }
    Ok(Tail { records, bytes: read })
}

/// Tail reads kept by (path, length, mtime, how many were asked for): a dataset that has not
/// changed is not read again.
#[derive(Default)]
pub struct Cache {
    map: HashMap<PathBuf, (u64, Option<std::time::SystemTime>, usize, Result<Rc<Vec<Value>>, String>)>,
}

impl Cache {
    fn tail(&mut self, path: &Path, n: usize) -> Result<Rc<Vec<Value>>, String> {
        let (len, mtime) = match std::fs::metadata(path) {
            Ok(m) => (m.len(), m.modified().ok()),
            Err(e) => {
                self.map.remove(path);
                return Err(e.to_string());
            }
        };
        if let Some((l, t, k, r)) = self.map.get(path) {
            if (*l, *t, *k) == (len, mtime, n) {
                return r.clone();
            }
        }
        let r = read_tail(path, n, format_of(path)).map(|t| Rc::new(t.records)).map_err(|e| e.to_string());
        self.map.insert(path.to_path_buf(), (len, mtime, n, r.clone()));
        r
    }
}

// ------------------------------------------------------------------ what a frame reads

/// One dataset as a frame sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct Set {
    /// The newest records, oldest first: as many as the components bound to it asked for.
    pub records: Rc<Vec<Value>>,
    pub expect_every: Option<f64>,
    /// When the file last changed, and when the dataset was registered (epoch seconds).
    pub changed: Option<f64>,
    pub added: Option<f64>,
}

impl Set {
    pub fn stale(&self, now: f64) -> Option<String> {
        crate::owner::overdue(self.expect_every, self.changed, self.added, now)
    }
}

/// The datasets a board's components are bound to, read once for a frame. `Err` is the one line
/// drawn in the component's place (not registered, or unreadable).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub sets: HashMap<String, Result<Set, String>>,
    /// Changes whenever the content does: what a scene and the table cache compare.
    pub gen: u64,
}

impl Snapshot {
    pub fn get(&self, name: &str) -> Result<&Set, String> {
        match self.sets.get(name) {
            Some(Ok(s)) => Ok(s),
            Some(Err(e)) => Err(e.clone()),
            None => Err(hint(name)),
        }
    }
    /// The datasets that are overdue now: name → why.
    pub fn stale(&self, now: f64) -> HashMap<String, String> {
        self.sets.iter().filter_map(|(n, s)| s.as_ref().ok().and_then(|s| s.stale(now)).map(|why| (n.clone(), why))).collect()
    }
    /// When the next dataset that is fresh now goes stale (epoch seconds).
    pub fn next_stale(&self, now: f64) -> Option<f64> {
        self.sets.values().filter_map(|s| s.as_ref().ok()).filter(|s| s.stale(now).is_none()).filter_map(|s| Some(s.changed.or(s.added)? + s.expect_every?)).min_by(|a, b| a.total_cmp(b))
    }
    /// Read the datasets `wants` names, each at most its count of newest records.
    pub fn load(st: &Store, wants: &[(String, usize)], cache: &mut Cache) -> Snapshot {
        let mut sets = HashMap::new();
        if wants.is_empty() {
            return Snapshot { sets, gen: 0 };
        }
        let reg = Registry::load_strict(&st.dir());
        for (name, n) in wants {
            let set = match &reg {
                Err(e) => Err(e.clone()),
                Ok(r) => match r.datasets.get(name) {
                    None => Err(hint(name)),
                    Some(ds) => {
                        let p = st.data_path(name, ds);
                        match cache.tail(&p, *n) {
                            Ok(records) => Ok(Set { records, expect_every: ds.expect_every, changed: last_change(&p), added: parse_rfc3339(&ds.added) }),
                            Err(e) => Err(format!("dataset {name:?}: cannot read {} ({e})", p.display())),
                        }
                    }
                },
            };
            sets.insert(name.clone(), set);
        }
        Snapshot { sets, gen: 0 }
    }
    /// The same content as `other` (whatever their generations)?
    pub fn same(&self, other: &Snapshot) -> bool {
        self.sets == other.sets
    }
}

/// A generation no snapshot has had.
pub fn next_gen() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(1);
    N.fetch_add(1, Ordering::Relaxed)
}

/// The snapshot components draw from: the pane (and `margin render`) installs the one of the
/// board it is about to draw, as the theme is installed.
pub fn install(s: Rc<Snapshot>) {
    CURRENT.with(|c| *c.borrow_mut() = s);
}
pub fn current() -> Rc<Snapshot> {
    CURRENT.with(|c| c.borrow().clone())
}

// ------------------------------------------------------------------ bindings

/// A table bound to a dataset shows at most this many of its newest records.
pub const TABLE_ROWS: usize = 200;
/// An inline sparkline's cells when it names no width, and the most it takes.
pub const INLINE_W: usize = 12;
pub const INLINE_MAX: usize = 60;

/// The body of an inline `~[@name.field:width|color]`: (dataset, field, cells, color name).
pub fn inline_ref(body: &str) -> Option<(&str, Option<&str>, usize, Option<&str>)> {
    let body = body.strip_prefix('@')?;
    let (body, color) = match body.rsplit_once('|') {
        Some((b, c)) => (b, Some(c)),
        None => (body, None),
    };
    let (body, w) = match body.rsplit_once(':') {
        Some((b, w)) => (b, w.parse::<usize>().ok().filter(|n| *n >= 1)?.min(INLINE_MAX)),
        None => (body, INLINE_W),
    };
    let (name, field) = match body.split_once('.') {
        Some((n, f)) if !f.is_empty() => (n, Some(f)),
        Some(_) => return None,
        None => (body, None),
    };
    valid_id(name).then_some((name, field, w, color))
}

fn inline_wants(v: &Value, out: &mut Vec<(String, usize)>) {
    match v {
        Value::String(s) => {
            let mut rest = s.as_str();
            while let Some(i) = rest.find("~[@") {
                rest = &rest[i + 2..];
                if let Some((name, _, w, _)) = rest.find(']').and_then(|e| inline_ref(&rest[..e])) {
                    out.push((name.to_string(), w));
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|x| inline_wants(x, out)),
        Value::Object(o) => o.values().for_each(|x| inline_wants(x, out)),
        _ => {}
    }
}

/// How many newest records a plot draws: its `last`, else what its kind can show in `cols` cells.
pub fn plot_last(b: &Block, cols: u16) -> usize {
    match b.num("last") {
        Some(n) if n >= 1.0 => (n as usize).min(100_000),
        _ => match b.s("kind") {
            "bars" | "columns" => 12,
            "hist" => 512,
            _ => cols.max(1) as usize,
        },
    }
}

/// What a block is bound to: (dataset, how many newest records it asks for at `cols` cells).
fn binding(b: &Block, cols: u16) -> Option<(String, usize)> {
    match b.kind.as_str() {
        "plot" => b.opt("data").map(|d| (d.to_string(), plot_last(b, cols))),
        "runs" => b.opt("data").map(|d| (d.to_string(), b.num("last").filter(|n| *n >= 1.0).map(|n| n as usize).unwrap_or(cols.max(1) as usize))),
        "table" => b.opt("rows").map(|d| (d.to_string(), TABLE_ROWS)),
        _ => None,
    }
}

/// Every dataset `doc` reads and the most records any of its components asks for, for a pane
/// `cols` cells wide.
pub fn wants(doc: &Doc, cols: u16) -> Vec<(String, usize)> {
    let mut all: Vec<(String, usize)> = Vec::new();
    crate::doc::walk(&doc.blocks, &mut |b, _, _| {
        all.extend(binding(b, cols));
        b.f.values().for_each(|v| inline_wants(v, &mut all));
    });
    let mut out: Vec<(String, usize)> = Vec::new();
    for (name, n) in all {
        match out.iter_mut().find(|(x, _)| *x == name) {
            Some(x) => x.1 = x.1.max(n),
            None => out.push((name, n)),
        }
    }
    out.sort();
    out
}

/// The datasets each block of `doc` reads, by block id (a table's rows included, under their own
/// ids): what the pane marks when a dataset goes stale.
pub fn bound_ids(doc: &Doc) -> Vec<(String, String)> {
    let mut out = Vec::new();
    crate::doc::walk(&doc.blocks, &mut |b, parent, _| {
        if let Some((name, _)) = binding(b, 1) {
            out.push((b.id.clone(), name));
        } else if let Some(name) = parent.filter(|p| p.kind == "table").and_then(|p| p.opt("rows")) {
            out.push((b.id.clone(), name.to_string()));
        } else {
            let mut refs = Vec::new();
            b.f.values().for_each(|v| inline_wants(v, &mut refs));
            if let Some((name, _)) = refs.into_iter().next() {
                out.push((b.id.clone(), name));
            }
        }
    });
    out
}

/// A record's key as part of a block id: letters, digits, - and _ kept, anything else a dash.
fn key_slug(v: &Value) -> Option<String> {
    let s = match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    let s: String = s.chars().take(48).map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
    (!s.is_empty()).then_some(s)
}

/// A bound table's rows: one `tr` per record, its id `<table id>-<key value>` (stable, so the
/// focus, actions and the reconcile's animation follow a record), its cells the record's fields,
/// its `target` and `detail` the fields of those names. Records sharing a key are one row: the
/// newest one's content, at the place the key first appeared. A record without the key field
/// (or a bare number) is no row.
pub fn table_rows(table: &Block, records: &[Value]) -> Vec<Block> {
    let key = table.opt("key").unwrap_or("id");
    let mut rows: Vec<Block> = Vec::new();
    for r in records {
        let Some(o) = r.as_object() else { continue };
        let Some(k) = o.get(key).and_then(key_slug) else { continue };
        let mut f = Map::new();
        f.insert("cells".into(), r.clone());
        for field in ["target", "detail"] {
            if let Some(s) = o.get(field).and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
                f.insert(field.into(), json!(s));
            }
        }
        let row = Block { kind: "tr".into(), id: format!("{}-{k}", table.id), children: Vec::new(), f };
        match rows.iter_mut().find(|x| x.id == row.id) {
            Some(x) => *x = row,
            None => rows.push(row),
        }
    }
    rows
}

/// Does any table of `doc` take its rows from a dataset?
pub fn has_bound_table(doc: &Doc) -> bool {
    let mut any = false;
    crate::doc::walk(&doc.blocks, &mut |b, _, _| any |= b.kind == "table" && b.opt("rows").is_some());
    any
}

/// `doc` as the pane shows it: every table bound to a dataset holds that dataset's records as
/// rows. Nothing else changes (a plot, a strip and an inline sparkline read the snapshot when
/// they draw), and the stored document is never touched.
pub fn expand(doc: &Doc, snap: &Snapshot) -> Doc {
    fn go(bs: &mut [Block], snap: &Snapshot) {
        for b in bs {
            if b.kind == "table" {
                if let Some(name) = b.opt("rows").map(String::from) {
                    b.children = snap.get(&name).map(|s| table_rows(b, &s.records)).unwrap_or_default();
                }
            } else {
                go(&mut b.children, snap);
            }
        }
    }
    let mut d = doc.clone();
    if has_bound_table(doc) {
        go(&mut d.blocks, snap);
    }
    d
}

/// A stored document with its dataset rows, read now: for the commands that address a row by id
/// (`margin act`), which the stored document does not hold.
pub fn with_rows(st: &Store, doc: Doc) -> Doc {
    if !has_bound_table(&doc) {
        return doc;
    }
    let snap = Snapshot::load(st, &wants(&doc, 1), &mut Cache::default());
    expand(&doc, &snap)
}

/// The numbers of one field over `records` (NaN where a record has none): `None` reads bare
/// numbers.
pub fn column(records: &[Value], field: Option<&str>) -> Vec<f64> {
    records
        .iter()
        .map(|r| match field {
            Some(f) => r.get(f).and_then(|v| v.as_f64()).unwrap_or(f64::NAN),
            None => r.as_f64().unwrap_or(f64::NAN),
        })
        .collect()
}

/// The fields a plot with no `y` draws: none when the records are bare numbers (one unnamed
/// series), else every numeric field of the newest record but `skip` (the x field).
pub fn numeric_fields(records: &[Value], skip: Option<&str>) -> Vec<String> {
    match records.iter().rev().find_map(|r| r.as_object()) {
        Some(o) => o.iter().filter(|(k, v)| v.is_number() && Some(k.as_str()) != skip).map(|(k, _)| k.clone()).collect(),
        None => Vec::new(),
    }
}

/// A record's time: RFC 3339 text or epoch seconds in `field`.
pub fn time_of(r: &Value, field: &str) -> Option<f64> {
    let v = r.get(field)?;
    v.as_f64().or_else(|| v.as_str().and_then(parse_rfc3339))
}

/// Frames a pane draws for dataset changes: at most one every `GAP` seconds, however fast a
/// dataset is appended to. `hold` says how long a change that arrives now waits.
pub struct Pace {
    next: f64,
}

impl Pace {
    /// 10 Hz.
    pub const GAP: f64 = 0.1;
    pub fn new() -> Pace {
        Pace { next: 0.0 }
    }
    /// Seconds until a change seen at `t` may be drawn (0: now).
    pub fn hold(&self, t: f64) -> f64 {
        (self.next - t).max(0.0)
    }
    /// A frame that read the datasets was drawn at `t`.
    pub fn drew(&mut self, t: f64) {
        self.next = t + Self::GAP;
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::store::tests::temp_store;

    pub fn held(st: &Store, name: &str) -> Dataset {
        let ds = Dataset { added: crate::util::now_rfc3339(), ..Default::default() };
        st.data_add(name, ds.clone()).unwrap();
        ds
    }

    /// A snapshot holding these records, installed as the one components draw from.
    pub fn install_sets(sets: &[(&str, Vec<Value>)]) -> Rc<Snapshot> {
        let sets = sets.iter().map(|(n, r)| (n.to_string(), Ok(Set { records: Rc::new(r.clone()), expect_every: None, changed: None, added: None }))).collect();
        let snap = Rc::new(Snapshot { sets, gen: next_gen() });
        install(snap.clone());
        snap
    }

    fn owner(st: &Store, name: &str) {
        st.create(Doc::empty("board")).ok();
        st.add_owner(crate::owner::Owner { name: name.into(), board: "board".into(), owns: name.into(), expect_every: None, added: crate::util::now_rfc3339(), last_write: String::new() }).unwrap();
    }

    /// Creating and changing are different commands: `add` refuses a name that exists, `set`
    /// and every write refuse one that is missing, with the hint a missing board gets and the
    /// nearest name.
    #[test]
    fn add_refuses_an_existing_name_and_writes_refuse_a_missing_one() {
        let st = temp_store();
        held(&st, "requests");
        let e = st.data_add("requests", Dataset::default()).unwrap_err();
        assert!(e.contains("exists: change it with margin data set requests"), "{e}");
        let want = "no dataset \"request\" in project proj: create it with margin data add request (did you mean \"requests\"?)";
        assert_eq!(st.data_set("request", |d| d.cap = Some(5)).unwrap_err(), want);
        assert_eq!(st.data_append("request", None, &[json!(1)]).unwrap_err(), want);
        assert_eq!(st.data_put("request", None, &[]).unwrap_err(), want);
        assert_eq!(st.data_rm("request").unwrap_err(), want);
        assert_eq!(st.data_append("zzz", None, &[json!(1)]).unwrap_err(), "no dataset \"zzz\" in project proj: create it with margin data add zzz");
        assert_eq!(st.data_set("requests", |d| d.cap = Some(5)).unwrap().cap, Some(5));
        assert!(st.data_add("bad name", Dataset::default()).unwrap_err().contains("bad dataset name"));
        assert!(st.data_set("requests", |d| d.cap = Some(0)).is_err());
        // removed: the records go to the archive, and the name is free again
        st.data_append("requests", None, &[json!({"rps": 1})]).unwrap();
        let kept = st.data_rm("requests").unwrap().unwrap();
        assert!(kept.starts_with(st.dir().join(".archive")) && std::fs::read_to_string(&kept).unwrap().contains("rps"));
        held(&st, "requests");
        std::fs::remove_dir_all(&st.home).ok();
    }

    /// An append adds lines; with a cap the file is rewritten to the newest `cap` records only
    /// once it holds more than twice that, and stays as it is in between.
    #[test]
    fn append_adds_records_and_the_cap_trims_past_twice_its_size() {
        let st = temp_store();
        held(&st, "lat");
        st.data_set("lat", |d| d.cap = Some(10)).unwrap();
        let p = st.data_dir().join("lat.jsonl");
        for i in 0..20 {
            st.data_append("lat", None, &[json!(i)]).unwrap();
        }
        assert_eq!(std::fs::read_to_string(&p).unwrap().lines().count(), 20, "twice the cap is not yet past it");
        let ino = |p: &Path| {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(p).unwrap().ino()
        };
        let before = ino(&p);
        assert_eq!(st.data_append("lat", None, &[json!(20)]).unwrap(), 10);
        let text = std::fs::read_to_string(&p).unwrap();
        assert_eq!(text.lines().collect::<Vec<_>>(), (11..=20).map(|i| i.to_string()).collect::<Vec<_>>(), "the newest ten");
        assert_ne!(ino(&p), before, "trimmed by a rewrite: a new file renamed into place");
        let after = ino(&p);
        // many at once, objects and numbers; and the next appends do not rewrite
        st.data_append("lat", None, &parse_records("{\"ms\": 3}\n4\n").unwrap()).unwrap();
        assert_eq!(ino(&p), after);
        assert_eq!(std::fs::read_to_string(&p).unwrap().lines().count(), 12);
        // a sidecar that no longer describes the file is not trusted: the lines are counted
        std::fs::write(&p, "1\n2\n").unwrap();
        assert_eq!(st.data_append("lat", None, &[json!(3)]).unwrap(), 3);
        assert!(st.data_append("lat", None, &[json!("text")]).unwrap_err().contains("a record is a JSON object or a number"));
        assert!(st.data_append("lat", None, &[]).is_err());
        std::fs::remove_dir_all(&st.home).ok();
    }

    /// `put` replaces the content, from a JSON array or JSON lines, and keeps to the cap.
    #[test]
    fn put_replaces_the_content() {
        let st = temp_store();
        held(&st, "prs");
        st.data_append("prs", None, &[json!({"id": 1})]).unwrap();
        assert_eq!(st.data_put("prs", None, &parse_records(r#"[{"id": 7, "title": "a"}, {"id": 8, "title": "b"}]"#).unwrap()).unwrap(), 2);
        let p = st.data_dir().join("prs.jsonl");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{\"id\":7,\"title\":\"a\"}\n{\"id\":8,\"title\":\"b\"}\n");
        st.data_put("prs", None, &parse_records("{\"id\": 9}\n\n{\"id\": 10}\n").unwrap()).unwrap();
        assert_eq!(read_tail(&p, 10, Format::Jsonl).unwrap().records, vec![json!({"id": 9}), json!({"id": 10})]);
        st.data_set("prs", |d| d.cap = Some(1)).unwrap();
        assert_eq!(st.data_put("prs", None, &[json!(1), json!(2), json!(3)]).unwrap(), 1);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "3\n");
        st.data_put("prs", None, &[]).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "");
        assert!(parse_records("[1, \"x\"]").is_err() && parse_records("{oops").is_err());
        std::fs::remove_dir_all(&st.home).ok();
    }

    /// A `--file` dataset is read where it is, JSON lines or CSV by its extension, and margin
    /// never writes it: `put` and `append` are refused and the file is byte for byte the same.
    #[test]
    fn an_external_file_is_read_in_place_and_never_written() {
        let st = temp_store();
        std::fs::create_dir_all(&st.home).unwrap();
        let (jl, csv) = (st.home.join("metrics.jsonl"), st.home.join("load.csv"));
        std::fs::write(&jl, "{\"t\": 1, \"rps\": 70}\n{\"t\": 2, \"rps\": 71.5}\n").unwrap();
        std::fs::write(&csv, "t,host,load\n2026-11-10T10:00:00Z,\"a, east\",0.5\n2026-11-10T10:01:00Z,b,\n2026-11-10T10:02:00Z,c,1.25\n").unwrap();
        for (name, p) in [("metrics", &jl), ("load", &csv)] {
            st.data_add(name, Dataset { file: Some(p.display().to_string()), added: crate::util::now_rfc3339(), ..Default::default() }).unwrap();
        }
        let before = (std::fs::read(&jl).unwrap(), std::fs::read(&csv).unwrap());
        let snap = Snapshot::load(&st, &[("metrics".into(), 10), ("load".into(), 2)], &mut Cache::default());
        assert_eq!(*snap.get("metrics").unwrap().records, vec![json!({"t": 1, "rps": 70}), json!({"t": 2, "rps": 71.5})]);
        assert_eq!(*snap.get("load").unwrap().records, vec![json!({"t": "2026-11-10T10:01:00Z", "host": "b", "load": null}), json!({"t": "2026-11-10T10:02:00Z", "host": "c", "load": 1.25})]);
        assert_eq!(read_tail(&csv, 10, Format::Csv).unwrap().records[0]["host"], json!("a, east"));
        for e in [st.data_append("metrics", None, &[json!(1)]).unwrap_err(), st.data_put("load", None, &[]).unwrap_err()] {
            assert!(e.contains("margin reads it and never writes it"), "{e}");
        }
        assert_eq!((std::fs::read(&jl).unwrap(), std::fs::read(&csv).unwrap()), before);
        assert!(!st.data_dir().join("metrics.jsonl").exists(), "nothing of it is copied into the store");
        // unregistering leaves the provider's file where it is
        assert_eq!(st.data_rm("metrics").unwrap(), None);
        assert!(jl.exists());
        // a cap or an owner on a file margin does not write is refused
        assert!(st.data_set("load", |d| d.cap = Some(5)).unwrap_err().contains("its provider's to trim"));
        std::fs::remove_dir_all(&st.home).ok();
    }

    /// `--owner`: only that registered owner writes, with `--as`; without one anyone may, and
    /// `--as` must still name a registered owner.
    #[test]
    fn an_owned_dataset_takes_writes_only_from_its_owner() {
        let st = temp_store();
        assert!(st.data_add("q", Dataset { owner: Some("queue".into()), ..Default::default() }).unwrap_err().starts_with("no owner \"queue\""));
        owner(&st, "queue");
        owner(&st, "other");
        st.data_add("q", Dataset { owner: Some("queue".into()), ..Default::default() }).unwrap();
        let e = st.data_append("q", None, &[json!(1)]).unwrap_err();
        assert!(e.contains("belongs to owner `queue`") && e.contains("margin data append q --as queue"), "{e}");
        assert!(st.data_append("q", Some("other"), &[json!(1)]).unwrap_err().contains("belongs to owner `queue`, not other"));
        assert!(st.data_put("q", Some("nobody"), &[json!(1)]).unwrap_err().starts_with("no owner \"nobody\""));
        st.data_append("q", Some("queue"), &[json!(1)]).unwrap();
        st.data_put("q", Some("queue"), &[json!(2), json!(3)]).unwrap();
        assert_eq!(std::fs::read_to_string(st.data_dir().join("q.jsonl")).unwrap(), "2\n3\n");
        held(&st, "open");
        st.data_append("open", None, &[json!(1)]).unwrap();
        st.data_append("open", Some("other"), &[json!(2)]).unwrap();
        assert!(st.data_append("open", Some("nobody"), &[json!(3)]).is_err());
        std::fs::remove_dir_all(&st.home).ok();
    }

    /// A tail read takes the end of the file, not the file: the newest records of a 40 MB log
    /// cost one or two chunks, and an unchanged file is not read again.
    #[test]
    fn a_tail_read_of_a_large_file_reads_only_its_end() {
        let st = temp_store();
        std::fs::create_dir_all(&st.home).unwrap();
        let p = st.home.join("big.jsonl");
        {
            let mut f = std::io::BufWriter::new(std::fs::File::create(&p).unwrap());
            for i in 0..600_000 {
                writeln!(f, "{{\"t\":{i},\"rps\":{},\"note\":\"padding to make the line longer\"}}", i % 97).unwrap();
            }
        }
        let len = std::fs::metadata(&p).unwrap().len();
        assert!(len > 35_000_000, "{len}");
        let t = read_tail(&p, 60, Format::Jsonl).unwrap();
        assert_eq!(t.records.len(), 60);
        assert_eq!((t.records[0]["t"].as_u64(), t.records[59]["t"].as_u64()), (Some(599_940), Some(599_999)));
        assert!(t.bytes <= 2 * CHUNK, "read {} of {len} bytes", t.bytes);
        // through the cache: one read, then none until the file changes
        st.data_add("big", Dataset { file: Some(p.display().to_string()), ..Default::default() }).unwrap();
        let mut cache = Cache::default();
        let (b0, r0) = (bytes_read(), reads());
        let a = Snapshot::load(&st, &[("big".into(), 60)], &mut cache);
        let b = Snapshot::load(&st, &[("big".into(), 60)], &mut cache);
        assert!(a.same(&b) && reads() - r0 == 1 && bytes_read() - b0 <= 2 * CHUNK, "{} reads, {} bytes", reads() - r0, bytes_read() - b0);
        std::fs::OpenOptions::new().append(true).open(&p).unwrap().write_all(b"{\"t\":600000,\"rps\":1}\n").unwrap();
        let c = Snapshot::load(&st, &[("big".into(), 60)], &mut cache);
        assert_eq!((reads() - r0, c.get("big").unwrap().records[59]["t"].as_u64()), (2, Some(600_000)));
        assert!(bytes_read() - b0 <= 4 * CHUNK);
        std::fs::remove_dir_all(&st.home).ok();
    }

    /// A last line with no newline is a write in progress. It is dropped unless it is a whole
    /// JSON object; the lines before it read as usual, and the next append starts a new line.
    #[test]
    fn a_torn_last_line_is_tolerated() {
        let st = temp_store();
        held(&st, "x");
        let p = st.data_dir().join("x.jsonl");
        std::fs::write(&p, "{\"v\": 1}\n{\"v\": 2}\n{\"v\": 3, \"no").unwrap();
        assert_eq!(read_tail(&p, 10, Format::Jsonl).unwrap().records, vec![json!({"v": 1}), json!({"v": 2})]);
        assert_eq!(read_tail(&p, 1, Format::Jsonl).unwrap().records, vec![json!({"v": 2})]);
        // a bare number without its newline may be the start of a longer one
        std::fs::write(&p, "70\n71").unwrap();
        assert_eq!(read_tail(&p, 10, Format::Jsonl).unwrap().records, vec![json!(70)]);
        std::fs::write(&p, "70\n{\"v\": 4}").unwrap();
        assert_eq!(read_tail(&p, 10, Format::Jsonl).unwrap().records, vec![json!(70), json!({"v": 4})], "a whole object cannot be a torn one");
        // garbage in the middle is skipped, and an append after a torn line starts its own line
        std::fs::write(&p, "1\nnot json\n2\n{\"torn").unwrap();
        st.data_append("x", None, &[json!(3)]).unwrap();
        assert_eq!(read_tail(&p, 10, Format::Jsonl).unwrap().records, vec![json!(1), json!(2), json!(3)]);
        // CSV: a last row short of fields is dropped
        let c = st.home.join("t.csv");
        std::fs::write(&c, "a,b\n1,2\n3").unwrap();
        assert_eq!(read_tail(&c, 10, Format::Csv).unwrap().records, vec![json!({"a": 1.0, "b": 2.0})]);
        assert!(read_tail(&st.home.join("absent.jsonl"), 10, Format::Jsonl).is_err());
        std::fs::remove_dir_all(&st.home).ok();
    }

    /// What a board asks for: the widest request per dataset, from plots, strips, tables and
    /// inline sparklines; and a bound table's rows keyed by the record.
    #[test]
    fn bindings_say_what_to_read_and_records_become_rows() {
        let d: Doc = serde_json::from_value(json!({"id": "b", "blocks": [
            {"type": "plot", "id": "p", "data": "requests", "y": ["rps"], "last": 60},
            {"type": "plot", "id": "p2", "data": "requests"},
            {"type": "runs", "id": "r", "data": "nightly", "y": "outcome"},
            {"type": "section", "id": "s", "children": [
                {"type": "text", "id": "t", "text": "load ~[@load.one:24] and ~[@requests.rps|orange] and ~[1 2 3]"},
                {"type": "table", "id": "prs", "rows": "prs", "key": "id", "columns": [{"key": "title"}]}]}]}))
        .unwrap();
        assert_eq!(wants(&d, 80), vec![("load".to_string(), 24), ("nightly".to_string(), 80), ("prs".to_string(), TABLE_ROWS), ("requests".to_string(), 80)]);
        assert_eq!(inline_ref("@a.b:5|red"), Some(("a", Some("b"), 5, Some("red"))));
        assert_eq!(inline_ref("@a"), Some(("a", None, INLINE_W, None)));
        assert_eq!((inline_ref("a"), inline_ref("@a b"), inline_ref("@a.:3"), inline_ref("@a:0")), (None, None, None, None));
        let table = d.find("prs").unwrap();
        let rows = table_rows(table, &[json!({"id": 12, "title": "old"}), json!({"id": "a/b", "title": "x", "target": "https://example.com/x", "detail": "more"}), json!(5), json!({"title": "no key"}), json!({"id": 12, "title": "new"})]);
        assert_eq!(rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["prs-12", "prs-a-b"]);
        assert_eq!((rows[0].f["cells"]["title"].as_str(), rows[1].opt("target"), rows[1].opt("detail")), (Some("new"), Some("https://example.com/x"), Some("more")));
        let ids: Vec<(String, String)> = bound_ids(&d);
        assert!(ids.contains(&("p".into(), "requests".into())) && ids.contains(&("t".into(), "load".into())) && ids.contains(&("prs".into(), "prs".into())), "{ids:?}");
    }

    /// A burst of appends is drawn at 10 Hz at most: a change waits out what is left of the gap
    /// since the last data frame.
    #[test]
    fn a_burst_of_changes_draws_a_bounded_number_of_frames() {
        let mut pace = Pace::new();
        let (mut frames, mut pending) = (0, None::<f64>);
        // an append every 10 ms for one second, as the loop sees them
        for i in 0..=100 {
            let t = 5.0 + i as f64 * 0.01;
            if pending.map(|due| t >= due).unwrap_or(false) || pace.hold(t) == 0.0 {
                pace.drew(t);
                frames += 1;
                pending = None;
            } else {
                pending.get_or_insert(t + pace.hold(t));
            }
        }
        assert!((9..=11).contains(&frames), "{frames} frames for 101 appends in a second");
        assert_eq!(pace.hold(100.0), 0.0, "a quiet dataset draws at once");
    }
}
