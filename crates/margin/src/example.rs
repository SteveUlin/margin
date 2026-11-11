//! `margin example`: complete dashboards to copy from, embedded in the binary.
//!
//! Each example is four plain files in `skill/examples/`: the artifact (`<name>.json`, a whole
//! document for `margin new`), the feed script that keeps it current (`<name>.feed.sh`), a
//! sample of the data the script reads (`<name>.data.json`), and what the script prints for that
//! sample (`<name>.feed.out`, which the tests hold the script to). They are the worked half of
//! `margin help dashboards`: an AI that has never seen this repository reads one, pipes it into a
//! store, and changes it into the board it was asked for.

pub struct Example {
    pub name: &'static str,
    /// One line for the list: what the board shows and which components it uses.
    pub about: &'static str,
    pub artifact: &'static str,
    pub feed: &'static str,
    pub data: &'static str,
    /// The script's output for `data`: the feed's parts of `artifact`, line for line.
    pub out: &'static str,
    /// The id prefix the feed owns.
    pub owns: &'static str,
    /// The trigger its `margin feed add` line uses.
    pub trigger: &'static str,
}

macro_rules! example {
    ($name:literal, $owns:literal, $trigger:literal, $about:literal) => {
        Example {
            name: $name,
            about: $about,
            artifact: include_str!(concat!("../../../skill/examples/", $name, ".json")),
            feed: include_str!(concat!("../../../skill/examples/", $name, ".feed.sh")),
            data: include_str!(concat!("../../../skill/examples/", $name, ".data.json")),
            out: include_str!(concat!("../../../skill/examples/", $name, ".feed.out")),
            owns: $owns,
            trigger: $trigger,
        }
    };
}

pub const EXAMPLES: &[Example] = &[
    example!("reviews", "pr", "--watch reviews.data.json", "pull requests to review: a table (id, author badge, title, CI run strip, age), rules, hints"),
    example!("services", "svc", "--every 30s", "background services: tabs Failing / All; rows with the error, run strips with a failure count, a table with state glyphs"),
    example!("tracker", "wk", "--watch tracker.data.json", "a work tracker: sections Now / Next / Waiting (todos, rows, owner badges) kept by a feed, beside a Decided section that is yours"),
];

pub fn get(name: &str) -> Option<&'static Example> {
    EXAMPLES.iter().find(|e| e.name == name)
}

/// `margin example`: the names, and how to use one.
pub fn list() -> String {
    let mut s = String::from("examples: complete dashboards to copy from (margin help dashboards explains the parts)\n");
    for e in EXAMPLES {
        s += &format!("  {:<9} {}\n", e.name, e.about);
    }
    s += "\n  margin example <name>          the artifact: a whole document, ready for `margin new <art>`\n";
    s += "  margin example <name> --feed   the script that keeps it current (bash + jq)\n";
    s += "  margin example <name> --data   a sample of the data that script reads\n";
    s += "\n  margin example reviews | margin new reviews && margin render reviews --width 100\n";
    s
}

impl Example {
    /// What goes to stderr beside the artifact: the steps from this JSON to a live, feed-kept
    /// board. On stderr so that `margin example <name> | margin new <art>` pipes clean JSON.
    pub fn notes(&self) -> String {
        let n = self.name;
        format!(
            "# {n}: {about}
# stdout is the artifact, a whole document: {{title, purpose, hints, blocks}}. From here to a live board:
#   1. create the artifact (prints `{n} rev 1`), and look at one frame of it as text
#        margin example {n} | margin new {n}
#        margin render {n} --width 100 --height 40
#   2. take the script that fetches its rows, and the sample data it reads
#        margin example {n} --feed > {n}.feed.sh && chmod +x {n}.feed.sh
#        margin example {n} --data > {n}.data.json
#   3. run the script by hand: its stdout is `margin apply` input, an update (here it changes nothing, so no new rev)
#        ./{n}.feed.sh {n}.data.json | margin apply {n}
#   4. register it as the handler of a refresh event: whenever `{n}.refresh` is appended, margin's daemon
#      runs it and applies what it prints, with no AI involved
#        margin handler add {n}-refresh --on {n}.refresh --board {n} --owns {owns} -- ./{n}.feed.sh {n}.data.json
#        margin emit {n}.refresh                      # try it: `margin handler ls` shows the run
#   5. make a row's link open: enter (or a click) on a row with a target appends `{n}.open`, and the pane
#      runs nothing itself. This ready-made handler opens it; register it once per project
#        margin handler add open --on '*.open' -- margin handle open
#   6. keep it current: margin has no timer. Whatever runs on a schedule emits the event (from any
#      directory: --project names the board's project; `margin ls` run beside the board prints nothing
#      about it, the pane's bottom line does: MARGIN <artifact> <project>)
#        cron:           */5 * * * *  margin emit {n}.refresh --project <project>
#        systemd timer:  a user timer whose service runs the same line (margin help integrate has both units)
#        a file change:  watchexec -w {n}.data.json -- margin emit {n}.refresh --project <project>
#   7. show it: its own pane, or a tab of a pane that is already open
#        margin open {n}        |        margin tab <that pane's artifact> add {n}
# The handler owns the ids `{owns}` and `{owns}-...`: once it is registered they are read-only for you (test a
# changed script with `margin handler run {n}-refresh --dry-run`). Every other id in the artifact is yours to write.
#
# Optional convenience, instead of steps 4 and 6: the older feed runner schedules the script itself ({trigger_words})
#        margin feed add {n} {trigger} --artifact {n} --owns {owns} -- ./{n}.feed.sh {n}.data.json
",
            trigger_words = if self.trigger.starts_with("--watch") { "here: whenever the data file changes" } else { "here: on an interval" },
            about = self.about,
            trigger = self.trigger,
            owns = self.owns
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::Doc;
    use crate::feed::parse_line;
    use crate::store::tests::temp_store;
    use crate::store::Writer;

    fn doc(e: &Example) -> Doc {
        let mut v: serde_json::Value = serde_json::from_str(e.artifact).unwrap_or_else(|x| panic!("{}.json: {x}", e.name));
        for k in ["title", "purpose", "hints", "blocks"] {
            assert!(v.get(k).is_some(), "{}.json has no {k:?}", e.name);
        }
        v["id"] = serde_json::json!(e.name);
        serde_json::from_value(v).unwrap_or_else(|x| panic!("{}.json: {x}", e.name))
    }

    fn groups(e: &Example, text: &str) -> Vec<Vec<crate::doc::Op>> {
        text.lines().map(|l| parse_line(l).unwrap_or_else(|x| panic!("{}: {x}", e.name))).filter(|g| !g.is_empty()).collect()
    }

    /// The three boards the kit promises, each listed with what it shows.
    #[test]
    fn list_names_every_example() {
        let l = list();
        for n in ["reviews", "services", "tracker"] {
            assert!(get(n).is_some(), "{n}");
        }
        for e in EXAMPLES {
            assert!(l.contains(&format!("  {} ", e.name)) && l.contains(e.about), "{}", e.name);
            assert!(e.notes().contains(&format!("margin example {0} | margin new {0}", e.name)));
            assert!(e.notes().contains(&format!("--owns {} ", e.owns)));
        }
        assert!(get("nope").is_none());
    }

    /// Each artifact is a whole document the store takes: every block passes the schema
    /// (`check_block`, on the way through `Store::put`).
    #[test]
    fn every_artifact_applies() {
        for e in EXAMPLES {
            let st = temp_store();
            let d = doc(e);
            assert!(!d.purpose.is_empty() && !d.hints.is_empty(), "{}: a dashboard states its purpose and hints", e.name);
            assert!(d.blocks.iter().all(|b| b.kind != "section" || b.s("title") != "Goal"), "{}: purpose stands where a Goal section did", e.name);
            st.put(d, None).unwrap_or_else(|x| panic!("{}: {x}", e.name));
            let _ = std::fs::remove_dir_all(&st.home);
        }
    }

    /// The feed's sample output is written as the feed (so every id sits inside the prefix it
    /// owns), into an empty artifact and onto the example artifact. Onto the artifact it changes
    /// nothing: the artifact already holds exactly what the feed prints.
    #[test]
    fn every_feed_output_applies_as_the_feed() {
        for e in EXAMPLES {
            let st = temp_store();
            let w = Writer::Feed { name: e.name.into(), owns: e.owns.into() };
            let g = groups(e, e.out);
            assert!(!g.is_empty(), "{}: the feed prints nothing", e.name);
            st.create(Doc::empty("empty")).unwrap();
            let (rev, errs) = st.feed_write("empty", &g, &w, &[]);
            assert!(rev.is_some() && errs.is_empty(), "{} into an empty artifact: {errs:?}", e.name);
            st.put(doc(e), None).unwrap();
            let (rev, errs) = st.feed_write(e.name, &g, &w, &[]);
            assert!(errs.is_empty(), "{}: {errs:?}", e.name);
            assert_eq!(rev, None, "{}.json and {}.feed.out disagree: regenerate the artifact from the feed", e.name, e.name);
            let _ = std::fs::remove_dir_all(&st.home);
        }
    }

    /// Where bash and jq are installed, the script run on the sample data prints the stored
    /// output. (A build sandbox without jq skips the run; the stored output is still held to the
    /// schema above.)
    #[test]
    fn every_feed_script_prints_its_stored_output() {
        let has = |c: &str| std::process::Command::new(c).arg("--version").output().map(|o| o.status.success()).unwrap_or(false);
        if !has("bash") || !has("jq") {
            eprintln!("skipped: bash or jq is not on PATH");
            return;
        }
        for e in EXAMPLES {
            let dir = std::env::temp_dir().join(format!("margin-example-{}-{}", std::process::id(), e.name));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("feed.sh"), e.feed).unwrap();
            std::fs::write(dir.join("data.json"), e.data).unwrap();
            let o = std::process::Command::new("bash").arg(dir.join("feed.sh")).arg(dir.join("data.json")).output().unwrap();
            assert!(o.status.success(), "{}: {}", e.name, String::from_utf8_lossy(&o.stderr));
            assert_eq!(String::from_utf8_lossy(&o.stdout), e.out, "{}.feed.sh no longer prints {}.feed.out", e.name, e.name);
            // a missing data file is an error on stderr and a non-zero exit, never an empty board
            let o = std::process::Command::new("bash").arg(dir.join("feed.sh")).arg(dir.join("absent.json")).output().unwrap();
            assert!(!o.status.success() && o.stdout.is_empty() && !o.stderr.is_empty(), "{}", e.name);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// The examples are for anyone: no name, no machine path.
    #[test]
    fn examples_name_no_one() {
        for e in EXAMPLES {
            for (what, s) in [("json", e.artifact), ("feed", e.feed), ("data", e.data), ("notes", &e.notes()[..])] {
                let low = s.to_lowercase();
                assert!(!low.contains("sulin") && !low.contains("/home/"), "{}.{what}", e.name);
            }
        }
    }
}
