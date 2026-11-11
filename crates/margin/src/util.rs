//! Small utilities: timestamps without a date crate, slugs, hashing, durations.

use std::time::{SystemTime, UNIX_EPOCH};

pub fn epoch_now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// Days since 1970-01-01 to (y, m, d) — Howard Hinnant's civil_from_days.
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + (m <= 2) as i64, m, d)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

pub fn rfc3339(epoch: f64) -> String {
    let s = epoch.floor() as i64;
    let (y, m, d) = civil(s.div_euclid(86400));
    let t = s.rem_euclid(86400);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", t / 3600, t / 60 % 60, t % 60)
}

pub fn now_rfc3339() -> String {
    rfc3339(epoch_now())
}

/// Parse `YYYY-MM-DDTHH:MM:SS[.frac](Z|±HH:MM)` to epoch seconds.
pub fn parse_rfc3339(s: &str) -> Option<f64> {
    let b = s.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let n = |a: usize, l: usize| s.get(a..a + l)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, se) = (n(0, 4)?, n(5, 2)?, n(8, 2)?, n(11, 2)?, n(14, 2)?, n(17, 2)?);
    let mut i = 19;
    let mut frac = 0.0;
    if b.get(i) == Some(&b'.') {
        let st = i;
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        frac = s[st..i].parse::<f64>().unwrap_or(0.0);
    }
    let off = match b.get(i) {
        Some(b'Z') | Some(b'z') | None => 0,
        Some(&c) if c == b'+' || c == b'-' => {
            let oh = n(i + 1, 2)?;
            let om = n(i + 4, 2).unwrap_or(0);
            let o = oh * 3600 + om * 60;
            if c == b'+' {
                o
            } else {
                -o
            }
        }
        _ => return None,
    };
    let days = days_from_civil(y, mo as u32, d as u32);
    Some((days * 86400 + h * 3600 + mi * 60 + se - off) as f64 + frac)
}

/// Local wall-clock "HH:MM" for an epoch, via the C library's timezone database.
pub fn local_hm(epoch: f64) -> String {
    unsafe {
        let t = epoch as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
    }
}

/// "3s ago", "4m ago", "2h ago", "3d ago".
pub fn ago(secs: f64) -> String {
    let s = secs.max(0.0) as i64;
    match s {
        0..=59 => format!("{s}s ago"),
        60..=3599 => format!("{}m ago", s / 60),
        3600..=86399 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86400),
    }
}

/// "10m", "30s", "2h", "1d", or plain seconds.
pub fn parse_duration(s: &str) -> Option<f64> {
    let s = s.trim();
    let (num, unit) = s.split_at(s.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(s.len()));
    let v: f64 = num.parse().ok()?;
    Some(v * match unit {
        "" | "s" => 1.0,
        "m" => 60.0,
        "h" => 3600.0,
        "d" => 86400.0,
        _ => return None,
    })
}

/// `/home/me/project` → `home-me-project`.
pub fn slug(path: &str) -> String {
    let s: String = path
        .trim_matches('/')
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '_' { c } else { '-' })
        .collect();
    if s.is_empty() {
        "root".into()
    } else {
        s
    }
}

/// Shell-style match of a whole string: `*` is any run of characters, `?` any one. Event names
/// are matched with it (`--name 'reviews.*'`, a job's `--on`).
pub fn glob_match(pat: &str, s: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pat.chars().collect(), s.chars().collect());
    let (mut i, mut j, mut star, mut mark) = (0, 0, None, 0);
    while j < t.len() {
        if i < p.len() && (p[i] == '?' || p[i] == t[j]) {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == '*' {
            star = Some(i);
            mark = j;
            i += 1;
        } else if let Some(st) = star {
            i = st + 1;
            mark += 1;
            j = mark;
        } else {
            return false;
        }
    }
    p[i..].iter().all(|c| *c == '*')
}

pub fn fnv64(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in data {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Valid artifact and component ids: letters, digits, `-` and `_`. No `.`, `/`, `=` or spaces,
/// because those delimit a selection token (`mg:pick/plan.Q1=mcp`).
pub fn valid_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Levenshtein distance between two artifact names (for "did you mean").
pub fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut prev = row[0];
        row[0] = i;
        for j in 1..=b.len() {
            let cur = row[j];
            row[j] = if a[i - 1] == b[j - 1] { prev } else { 1 + prev.min(row[j]).min(row[j - 1]) };
            prev = cur;
        }
    }
    row[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn edit_distance_counts_single_character_edits() {
        for (a, b, d) in [("plan", "plan", 0), ("plan", "paln", 2), ("reviews", "review", 1), ("margin", "margn", 1), ("a", "", 1), ("kitten", "sitting", 3)] {
            assert_eq!(edit_distance(a, b), d, "{a} {b}");
        }
    }
    #[test]
    fn time_roundtrip() {
        assert_eq!(rfc3339(0.0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_793_908_682.0), "2026-11-05T19:58:02Z");
        assert_eq!(parse_rfc3339("2026-11-05T19:58:02Z"), Some(1_793_908_682.0));
        assert_eq!(parse_rfc3339("2026-11-05T11:58:02-08:00"), Some(1_793_908_682.0));
        assert_eq!(parse_rfc3339("2026-11-05T19:58:02.5Z"), Some(1_793_908_682.5));
        assert_eq!(parse_rfc3339("nope"), None);
    }
    #[test]
    fn misc() {
        assert_eq!(slug("/home/me/project"), "home-me-project");
        assert_eq!(parse_duration("10m"), Some(600.0));
        assert_eq!(parse_duration("5"), Some(5.0));
        assert_eq!(ago(130.0), "2m ago");
        assert!(valid_id("Q1") && valid_id("pr-412") && !valid_id("a.b") && !valid_id(""));
        for (p, s, want) in [("reviews.*", "reviews.open", true), ("reviews.*", "reviews", false), ("*", "", true), ("*.open", "reviews.open", true), ("job.*", "jobs.x", false), ("a?c", "abc", true), ("a?c", "ac", false), ("reviews.open", "reviews.open", true), ("*o*n", "reviews.open", true), ("", "x", false)] {
            assert_eq!(glob_match(p, s), want, "{p} {s}");
        }
    }
}

/// The store for a working directory (hooks get `cwd` from Claude Code, not from the process).
pub fn project_store_for(cwd: &std::path::Path) -> crate::store::Store {
    crate::store::Store::new(crate::store::default_home(), crate::store::project_for(cwd))
}

/// How to name this binary in a command sulin runs by hand: `margin` when a `margin` is on PATH,
/// else this executable's absolute path (a dev build outside the repo's direnv printed a
/// `margin view …` that the shell couldn't find).
pub fn self_cmd() -> String {
    let on_path = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).any(|d| d.join("margin").is_file())).unwrap_or(false);
    if on_path {
        return "margin".into();
    }
    std::env::current_exe().map(|e| e.display().to_string()).unwrap_or_else(|_| "margin".into())
}

/// The format version doc.json and events.jsonl carry (`"v": 1`); a missing `v` reads as 1.
pub fn v1() -> u64 {
    1
}
