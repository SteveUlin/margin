//! The pinned build: `<home>/bin/margin`, a symlink to a copy of one margin binary kept beside
//! it (`<home>/bin/margin-<content hash>`). `margin pin` makes it from the build it is run from.
//!
//! While a pin exists, panes and the daemon follow it and not the newest build: `margin open`
//! launches panes with the link, a running pane re-execs the link when its target changes
//! (app.rs), and so does the daemon (daemon.rs). A `cargo build` then restarts nothing. With no
//! pin everything is as it was: a pane follows its own executable.
//!
//! The link is replaced by a rename, so a reader sees the old target or the new one, and the
//! copy it names is complete before the link moves.

use std::path::{Path, PathBuf};

/// The directory under the store's home. Not a project: `store::projects` leaves it out.
pub const DIR: &str = "bin";

pub fn dir(home: &Path) -> PathBuf {
    home.join(DIR)
}

/// The link panes and the daemon run: `<home>/bin/margin`.
pub fn link(home: &Path) -> PathBuf {
    dir(home).join("margin")
}

/// The file the pin names now, when there is a pin and that file exists.
pub fn target(home: &Path) -> Option<PathBuf> {
    let t = std::fs::read_link(link(home)).ok()?;
    let t = if t.is_absolute() { t } else { dir(home).join(t) };
    t.is_file().then_some(t)
}

/// The binary to start a pane or a daemon with: the pin's link when there is a pin, else the
/// running executable.
pub fn launcher(home: &Path) -> std::io::Result<PathBuf> {
    match target(home) {
        Some(_) => Ok(link(home)),
        None => std::env::current_exe(),
    }
}

/// The first 12 hex digits of a 64-bit hash of the binary: enough to tell builds apart.
fn short_hash(bytes: &[u8]) -> String {
    format!("{:016x}", crate::util::fnv64(bytes))[..12].to_string()
}

/// Pin `exe`: copy it to `<home>/bin/margin-<hash>` (unless that copy is there already) and
/// move the link onto it. The copy the link named before is kept, so a process still running
/// it keeps its file; older copies are removed. Returns the copy's path.
pub fn pin(home: &Path, exe: &Path) -> Result<PathBuf, String> {
    use std::os::unix::fs::PermissionsExt;
    let bytes = std::fs::read(exe).map_err(|e| format!("read {}: {e}", exe.display()))?;
    let d = dir(home);
    std::fs::create_dir_all(&d).map_err(|e| format!("create {}: {e}", d.display()))?;
    let name = format!("margin-{}", short_hash(&bytes));
    let dest = d.join(&name);
    let before = target(home);
    let tmp = |what: &str| d.join(format!(".{what}.{}.tmp", std::process::id()));
    if !dest.is_file() {
        let t = tmp("copy");
        std::fs::write(&t, &bytes).and_then(|_| std::fs::set_permissions(&t, std::fs::Permissions::from_mode(0o755))).and_then(|_| std::fs::rename(&t, &dest)).map_err(|e| {
            let _ = std::fs::remove_file(&t);
            format!("write {}: {e}", dest.display())
        })?;
    }
    if before.as_deref() != Some(dest.as_path()) {
        let t = tmp("link");
        let _ = std::fs::remove_file(&t);
        // a relative target: the store can be moved, or seen under another mount point
        std::os::unix::fs::symlink(&name, &t).and_then(|_| std::fs::rename(&t, link(home))).map_err(|e| {
            let _ = std::fs::remove_file(&t);
            format!("link {}: {e}", link(home).display())
        })?;
    }
    for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
        let p = e.path();
        if e.file_name().to_string_lossy().starts_with("margin-") && p != dest && Some(&p) != before.as_ref() {
            let _ = std::fs::remove_file(p);
        }
    }
    Ok(dest)
}

/// `margin pin --show`, and the doctor's first pin line.
pub fn show(home: &Path) -> String {
    match target(home) {
        Some(t) => format!("{} -> {}", link(home).display(), t.display()),
        None => "nothing pinned".to_string(),
    }
}

/// The binary process `pid` runs (`/proc/<pid>/exe`), without the kernel's mark for a file that
/// was replaced since.
pub fn exe_of(pid: u32) -> Option<PathBuf> {
    let p = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    let s = p.to_string_lossy();
    Some(PathBuf::from(s.strip_suffix(" (deleted)").unwrap_or(&s)))
}

/// Does process `pid` run the pinned copy? None when it cannot be told (no pin, no /proc).
pub fn runs_pin(home: &Path, pid: u32) -> Option<bool> {
    let t = target(home)?;
    let e = exe_of(pid)?;
    Some(std::fs::canonicalize(&t).unwrap_or(t) == e)
}

/// The panes and daemons of this store that are running, and whether each runs the pin: for
/// `margin doctor`. Found through /proc: a margin process whose first argument is `view` or
/// `daemon`, with this store's `--home` (or none, when this store is the default one).
pub fn report(home: &Path) -> String {
    let Some(t) = target(home) else { return String::new() };
    let mut out = format!("pin: {} -> {}\n", link(home).display(), t.file_name().unwrap_or_default().to_string_lossy());
    let me = std::process::id();
    let default_home = crate::store::default_home() == home;
    let mut pids: Vec<u32> = std::fs::read_dir("/proc").into_iter().flatten().flatten().filter_map(|e| e.file_name().to_string_lossy().parse().ok()).collect();
    pids.sort();
    for pid in pids.into_iter().filter(|p| *p != me) {
        let Ok(raw) = std::fs::read(format!("/proc/{pid}/cmdline")) else { continue };
        let args: Vec<String> = raw.split(|b| *b == 0).filter(|a| !a.is_empty()).map(|a| String::from_utf8_lossy(a).into_owned()).collect();
        let is_margin = args.first().and_then(|a| Path::new(a).file_name()).map(|n| n.to_string_lossy().starts_with("margin")).unwrap_or(false);
        if !is_margin || args.len() < 2 || !matches!(args[1].as_str(), "view" | "daemon") {
            continue;
        }
        let its_home = args.iter().position(|a| a == "--home").and_then(|i| args.get(i + 1));
        if !its_home.map(|h| Path::new(h) == home).unwrap_or(default_home) {
            continue;
        }
        let what = if args[1] == "view" { format!("pane {}", args.get(2).cloned().unwrap_or_default()) } else { "daemon".to_string() };
        let how = match (runs_pin(home, pid), exe_of(pid)) {
            (Some(true), _) => "runs the pin".to_string(),
            (_, Some(e)) => format!("runs {} (not the pin: it moves to the pin when the pin next changes)", e.display()),
            _ => "runs a binary that cannot be read".to_string(),
        };
        out += &format!("  {what} (pid {pid}): {how}\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pin is a copy named by its content and a link to it; pinning another build moves the
    /// link, keeps the copy before it and removes the ones older than that.
    #[test]
    fn pin_copies_the_binary_and_moves_the_link_keeping_one_before() {
        let home = std::env::temp_dir().join(format!("margin-pin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        assert_eq!((target(&home), show(&home).as_str()), (None, "nothing pinned"));
        assert_eq!(launcher(&home).unwrap(), std::env::current_exe().unwrap(), "no pin: the running binary");
        let build = |n: &str| {
            let p = home.join(format!("build-{n}"));
            std::fs::write(&p, format!("#!/bin/sh\necho {n}\n")).unwrap();
            p
        };
        let a = pin(&home, &build("a")).unwrap();
        assert_eq!(target(&home), Some(a.clone()));
        assert_eq!(launcher(&home).unwrap(), link(&home));
        assert!(std::fs::symlink_metadata(link(&home)).unwrap().file_type().is_symlink());
        let out = std::process::Command::new(link(&home)).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "a\n", "the copy runs");
        assert_eq!(pin(&home, &build("a")).unwrap(), a, "the same build again: the same copy");
        let b = pin(&home, &build("b")).unwrap();
        assert!(a != b && target(&home) == Some(b.clone()) && a.is_file(), "the one before is kept");
        let c = pin(&home, &build("c")).unwrap();
        assert!(target(&home) == Some(c.clone()) && b.is_file() && !a.is_file(), "older ones go");
        assert!(show(&home).ends_with(&format!("-> {}", c.display())));
        // the directory is not a project
        assert!(!crate::store::projects(&home).contains(&DIR.to_string()));
        std::fs::remove_dir_all(&home).ok();
    }
}
