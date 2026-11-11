//! Owners: programs margin does not run, registered as the writers of an id prefix on a board.
//!
//! `margin owner add NAME --board B --owns PREFIX` is a claim and nothing else: the ids PREFIX
//! and PREFIX-… on B are the owner's, exactly as a handler's are its own (one claims
//! table: `Store::claims`), and the program writes them with `margin apply B --as NAME`, as often
//! as it likes. margin cannot say whether a program it does not run is healthy; it can say how
//! long ago the owner last wrote. With `--expect-every` an owner whose last write is older than
//! that is stale: the pane marks its rows, the tab gets the failure mark, `owner ls` says so.

use crate::store::{Claim, Store};
use crate::util::valid_id;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Owner {
    pub name: String,
    pub board: String,
    /// The id prefix it owns in `board`; `*` = the whole board.
    pub owns: String,
    /// How often it means to write, in seconds. Past it, its rows show as stale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_every: Option<f64>,
    #[serde(default)]
    pub added: String,
    /// Its last `margin apply --as` (RFC 3339), changed content or not: a write that says "still
    /// the same" is news too.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub last_write: String,
}

impl Owner {
    /// Why it is stale, or None: `last updated 3h ago, expected every 10m`. An owner that has
    /// never written is measured from its registration.
    pub fn stale(&self, now: f64) -> Option<String> {
        overdue(self.expect_every, crate::util::parse_rfc3339(&self.last_write), crate::util::parse_rfc3339(&self.added), now)
    }
}

/// The staleness rule, for an owner's rows and for a dataset (data.rs) alike: with an expected
/// interval `every`, a writer whose last write (`last`, epoch seconds) is older than that is
/// overdue, and one that has never written is measured from its registration (`added`). Returns
/// why, in the words the pane and the `ls` commands show.
pub fn overdue(every: Option<f64>, last: Option<f64>, added: Option<f64>, now: f64) -> Option<String> {
    let every = every?;
    let (at, wrote) = match last {
        Some(t) => (t, true),
        None => (added?, false),
    };
    if now - at <= every {
        return None;
    }
    let every = fmt_secs(every);
    Some(if wrote { format!("last updated {}, expected every {every}", crate::util::ago(now - at)) } else { format!("never updated, expected every {every}") })
}

/// A duration in the fewest words: `90s`, `10m`, `2h`, `0.3s`.
pub fn fmt_secs(s: f64) -> String {
    if s >= 3600.0 && s % 3600.0 == 0.0 {
        format!("{}h", s / 3600.0)
    } else if s >= 60.0 && s % 60.0 == 0.0 {
        format!("{}m", s / 60.0)
    } else if s.fract() == 0.0 {
        format!("{}s", s as i64)
    } else {
        format!("{s:.1}s")
    }
}

/// Do two prefixes claim any common id? (`*` overlaps everything.)
pub fn overlaps(a: &str, b: &str) -> bool {
    a == "*" || b == "*" || a.starts_with(b) || b.starts_with(a)
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Registry {
    #[serde(default)]
    pub owners: Vec<Owner>,
}

/// The one message for an owner that isn't registered ("no owner …": exit code 3).
pub fn missing(name: &str) -> String {
    format!("no owner {name:?}: margin owner ls lists them, margin owner add {name} --board B --owns PREFIX registers one")
}

impl Registry {
    pub fn path(dir: &Path) -> PathBuf {
        dir.join("owners.json")
    }
    pub fn load(dir: &Path) -> Registry {
        Self::load_strict(dir).unwrap_or_default()
    }
    /// The registry as owners.json holds it.
    fn read(dir: &Path) -> Result<Registry, String> {
        match std::fs::read_to_string(Self::path(dir)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Registry::default()),
            Err(e) => Err(format!("{}: {e}", Self::path(dir).display())),
            Ok(s) => serde_json::from_str(&s).map_err(|e| format!("{} is corrupt ({e}); fix or remove it", Self::path(dir).display())),
        }
    }
    /// The registry, with each owner's `last_write` taken from its file on its board
    /// (`<board>/owned/owner.<name>.json`): that file's mtime is the one record of when the
    /// owner last wrote. owners.json holds a `last_write` only for an owner from before owners
    /// had files, until its rows move into one.
    pub fn load_strict(dir: &Path) -> Result<Registry, String> {
        let mut r = Self::read(dir)?;
        for o in &mut r.owners {
            if let Some(t) = wrote(dir, o) {
                o.last_write = crate::util::rfc3339(t);
            }
        }
        Ok(r)
    }
    /// One writer per file: an owner's write no longer comes here. Saved on a registration
    /// change only, without the `last_write` of an owner whose file records it.
    pub fn save(&self, dir: &Path) -> Result<(), String> {
        let mut r = self.clone();
        for o in &mut r.owners {
            if wrote(dir, o).is_some() {
                o.last_write.clear();
            }
        }
        crate::store::write_durable(&Self::path(dir), serde_json::to_string_pretty(&r).unwrap().as_bytes()).map_err(|e| e.to_string())
    }
    /// When owner `name` last wrote as owners.json records it (an owner from before owners had
    /// files), else when it was registered: what its new file's mtime starts from.
    pub fn recorded_write(dir: &Path, name: &str) -> Option<f64> {
        let r = Self::read(dir).ok()?;
        let o = r.get(name)?;
        crate::util::parse_rfc3339(&o.last_write).or_else(|| crate::util::parse_rfc3339(&o.added))
    }
    /// Every claim in the registry, with its board.
    pub fn all_claims(&self) -> Vec<(String, Claim)> {
        self.owners.iter().map(|o| (o.board.clone(), Claim { feed: o.name.clone(), owns: o.owns.clone(), kind: "owner" })).collect()
    }
    pub fn get(&self, name: &str) -> Option<&Owner> {
        self.owners.iter().find(|o| o.name == name)
    }
    pub fn claims(&self, art: &str) -> Vec<Claim> {
        self.owners.iter().filter(|o| o.board == art).map(|o| Claim { feed: o.name.clone(), owns: o.owns.clone(), kind: "owner" }).collect()
    }
    fn check(o: &Owner) -> Result<(), String> {
        if !valid_id(&o.name) {
            return Err(format!("bad owner name {:?}: letters, digits, - and _", o.name));
        }
        if !valid_id(&o.board) {
            return Err(format!("an owner needs its board: --board B (got {:?})", o.board));
        }
        if o.owns != "*" && !valid_id(&o.owns) {
            return Err(format!("an owner needs the id prefix it writes: --owns PREFIX (letters, digits, - and _; or *), got {:?}", o.owns));
        }
        if o.expect_every.map(|e| e <= 0.0).unwrap_or(false) {
            return Err("--expect-every must be more than 0 (10m, 1h), or `none`".into());
        }
        Ok(())
    }
    pub fn add(&mut self, o: Owner) -> Result<(), String> {
        Self::check(&o)?;
        if self.get(&o.name).is_some() {
            return Err(format!("owner {:?} exists: change it with margin owner set {} …, or margin owner rm {} first", o.name, o.name, o.name));
        }
        self.owners.push(o);
        Ok(())
    }
    pub fn set(&mut self, o: Owner) -> Result<(), String> {
        Self::check(&o)?;
        let i = self.owners.iter().position(|x| x.name == o.name).ok_or_else(|| missing(&o.name))?;
        self.owners[i] = o;
        Ok(())
    }
    pub fn remove(&mut self, name: &str) -> Result<Owner, String> {
        let i = self.owners.iter().position(|o| o.name == name).ok_or_else(|| missing(name))?;
        Ok(self.owners.remove(i))
    }
}

/// When an owner last wrote: the mtime of its file on its board (epoch seconds). None: it has
/// no file yet.
fn wrote(dir: &Path, o: &Owner) -> Option<f64> {
    let p = dir.join(&o.board).join("owned").join(format!("owner.{}.json", o.name));
    std::fs::metadata(p).ok()?.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs_f64())
}

/// Change the registry under the project lock. An owner that goes (or moves to another board or
/// prefix) gives its blocks back to the board's layout first, where they are ordinary content
/// again: nothing it wrote disappears, and no file is left that nobody claims.
pub fn edit_registry<T>(st: &Store, f: impl FnOnce(&mut Registry) -> Result<T, String>) -> Result<T, String> {
    let _l = st.lock().map_err(|e| e.to_string())?;
    let mut r = Registry::load_strict(&st.dir())?;
    let before = r.all_claims();
    let out = f(&mut r)?;
    st.claims_going_locked(&before, &r.all_claims())?;
    r.save(&st.dir())?;
    st.claims_came_locked(&before, &r.all_claims());
    Ok(out)
}

/// The stale owners of a board, for the pane: (the prefix whose rows to mark, the reason).
pub fn stale_on(st: &Store, board: &str, now: f64) -> Vec<(String, String)> {
    Registry::load(&st.dir()).owners.iter().filter(|o| o.board == board).filter_map(|o| o.stale(now).map(|why| (o.owns.clone(), why))).collect()
}

/// When the next owner of `board` that is fresh now goes stale (epoch seconds): the pane wakes
/// then, so the mark appears without anything else happening.
pub fn next_stale(st: &Store, board: &str, now: f64) -> Option<f64> {
    Registry::load(&st.dir())
        .owners
        .iter()
        .filter(|o| o.board == board && o.stale(now).is_none())
        .filter_map(|o| Some(crate::util::parse_rfc3339(&o.last_write).or_else(|| crate::util::parse_rfc3339(&o.added))? + o.expect_every?))
        .min_by(|a, b| a.total_cmp(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::Op;
    use crate::store::tests::temp_store;
    use serde_json::json;

    fn owner(name: &str, owns: &str) -> Owner {
        Owner { name: name.into(), board: "runs".into(), owns: owns.into(), expect_every: None, added: "2026-11-10T10:00:00Z".into(), last_write: String::new() }
    }

    fn block(v: serde_json::Value) -> Vec<Op> {
        crate::doc::parse_line(&v.to_string()).unwrap()
    }

    /// An owner is a claim: it writes its prefix with `apply_as`, nobody else does, it writes
    /// nowhere else, and an unregistered name (or the wrong board) is refused.
    #[test]
    fn an_owner_writes_its_prefix_and_only_it_does() {
        let st = temp_store();
        st.create(crate::doc::Doc { actions: json!([{"key": "X", "name": "cancel"}]), ..crate::doc::Doc::empty("runs") }).unwrap();
        st.create(crate::doc::Doc::empty("other")).unwrap();
        assert!(st.apply_as("runs", "queue", &block(json!({"type": "row", "id": "run-1", "text": "a"}))).unwrap_err().starts_with("no owner \"queue\""));
        st.add_owner(owner("queue", "run")).unwrap();
        assert!(st.add_owner(owner("queue", "zz")).unwrap_err().contains("exists: change it with margin owner set queue"));
        assert!(st.add_owner(owner("second", "run-x")).unwrap_err().contains("overlaps owner queue"), "one id has one owner");
        let layout = std::fs::read(st.doc_path("runs")).unwrap();
        let rev = st.apply_as("runs", "queue", &block(json!({"type": "row", "id": "run-1", "text": "sim A"}))).unwrap();
        let d = st.load("runs").unwrap();
        // the board's rev is its layout's: the owner's write moved its own and not that one
        assert_eq!((rev, d.rev, d.by.as_str(), d.find("run-1").unwrap().s("text")), (1, 1, "owner:queue", "sim A"));
        assert_eq!(d.owned["owner:queue"].rev, 1);
        assert_eq!(std::fs::read(st.doc_path("runs")).unwrap(), layout, "doc.json is the layout writer's file: untouched");
        let file = st.art("runs").join("owned/owner.queue.json");
        let wrote = Registry::load(&st.dir()).get("queue").unwrap().last_write.clone();
        assert!(crate::util::parse_rfc3339(&wrote).is_some(), "the write is recorded: the file's mtime");
        assert!(!std::fs::read_to_string(Registry::path(&st.dir())).unwrap().contains("last_write"), "and owners.json is not written by a write");
        // the same state again: no new rev of either, and still a write
        let age = std::time::SystemTime::now() - std::time::Duration::from_secs(900);
        std::fs::File::options().write(true).open(&file).unwrap().set_modified(age).unwrap();
        let before = std::fs::read(&file).unwrap();
        assert_eq!(st.apply_as("runs", "queue", &block(json!({"type": "row", "id": "run-1", "text": "sim A"}))).unwrap(), 1);
        assert_eq!((st.load("runs").unwrap().owned["owner:queue"].rev, std::fs::read(&file).unwrap()), (1, before));
        assert!(std::fs::metadata(&file).unwrap().modified().unwrap() > age + std::time::Duration::from_secs(800), "the last write moved");
        // outside its prefix, on another board, as someone unregistered: refused, nothing written
        let e = st.apply_as("runs", "queue", &block(json!({"type": "row", "id": "note", "text": "x"}))).unwrap_err();
        assert!(e.contains("owner queue may only write the ids run and run-…"), "{e}");
        let mut two = block(json!({"type": "row", "id": "run-2", "text": "b"}));
        two.extend(block(json!({"type": "row", "id": "elsewhere", "text": "c"})));
        assert!(st.apply_as("runs", "queue", &two).is_err());
        assert!(st.load("runs").unwrap().find("run-2").is_none(), "all or nothing");
        assert!(st.apply_as("other", "queue", &block(json!({"type": "row", "id": "run-1", "text": "a"}))).unwrap_err().contains("owner queue is registered on runs, not other"));
        // everyone else is refused inside the prefix, with a message that names the owner
        let e = st.patch("runs", &[Op::Set { id: "run-1".into(), field: "text".into(), value: json!("mine") }], None).unwrap_err();
        assert!(e.contains("belongs to owner `queue`") && e.contains("margin apply runs --as queue"), "{e}");
        assert!(st.decide("runs", "run-1", "pin", json!(true), "key", None).unwrap_err().contains("kept current by owner `queue`"));
        // the person can still act on its rows: an action changes no content
        let ev = st.act("runs", "run-1", "cancel", None, "you", "act").unwrap();
        assert_eq!((ev.kind.as_str(), ev.value["text"].as_str()), ("runs.cancel", Some("sim A")));
        // removed: the rows go back into the layout, where they are ordinary content
        st.remove_owner("queue").unwrap();
        let d = st.load("runs").unwrap();
        assert!(!file.exists() && d.owned.is_empty() && d.rev == 2 && std::fs::read_to_string(st.doc_path("runs")).unwrap().contains("sim A"));
        st.patch("runs", &[Op::Set { id: "run-1".into(), field: "text".into(), value: json!("mine") }], None).unwrap();
        assert!(st.remove_owner("queue").unwrap_err().starts_with("no owner \"queue\""));
        std::fs::remove_dir_all(&st.home).ok();
    }

    /// Staleness: only with `--expect-every`, measured from the last write (or the registration
    /// when it never wrote), and gone again with the next write.
    #[test]
    fn an_overdue_owner_is_stale_until_it_writes() {
        let at = |s: &str| crate::util::parse_rfc3339(s).unwrap();
        let mut o = owner("queue", "run");
        assert_eq!(o.stale(at("2030-01-01T00:00:00Z")), None, "no expectation, never stale");
        o.expect_every = Some(600.0);
        assert_eq!(o.stale(at("2026-11-10T10:09:00Z")), None);
        assert_eq!(o.stale(at("2026-11-10T10:11:00Z")).as_deref(), Some("never updated, expected every 10m"));
        o.last_write = "2026-11-10T11:00:00Z".into();
        assert_eq!(o.stale(at("2026-11-10T11:10:00Z")), None);
        assert_eq!(o.stale(at("2026-11-10T14:00:01Z")).as_deref(), Some("last updated 3h ago, expected every 10m"));
        let st = temp_store();
        st.create(crate::doc::Doc::empty("runs")).unwrap();
        st.add_owner(Owner { added: "2020-01-01T00:00:00Z".into(), ..o.clone() }).unwrap();
        let now = crate::util::epoch_now();
        assert_eq!(stale_on(&st, "runs", now).len(), 1);
        assert!(stale_on(&st, "other", now).is_empty());
        st.apply_as("runs", "queue", &block(json!({"type": "row", "id": "run-1", "text": "a"}))).unwrap();
        assert!(stale_on(&st, "runs", now).is_empty(), "a write makes it fresh");
        let next = next_stale(&st, "runs", now).unwrap();
        assert!((next - now - 600.0).abs() < 5.0, "the pane wakes when it would go stale: {}", next - now);
        std::fs::remove_dir_all(&st.home).ok();
    }
}
