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
    /// More steps, for the parts of the board that no script writes (a plot fed by a dataset).
    pub more: &'static str,
}

macro_rules! example {
    ($name:literal, $owns:literal, $trigger:literal, $about:literal) => {
        example!($name, $owns, $trigger, $about, "")
    };
    ($name:literal, $owns:literal, $trigger:literal, $about:literal, $more:expr) => {
        Example {
            name: $name,
            about: $about,
            artifact: include_str!(concat!("../../../skill/examples/", $name, ".json")),
            feed: include_str!(concat!("../../../skill/examples/", $name, ".feed.sh")),
            data: include_str!(concat!("../../../skill/examples/", $name, ".data.json")),
            out: include_str!(concat!("../../../skill/examples/", $name, ".feed.out")),
            owns: $owns,
            trigger: $trigger,
            more: $more,
        }
    };
}

pub const EXAMPLES: &[Example] = &[
    example!("reviews", "pr", "--watch reviews.data.json", "pull requests to review: a table (id, author badge, title, CI run strip, age), rules, hints"),
    example!("services", "svc", "--every 30s", "background services: a latency plot fed by a dataset; tabs Failing / All; rows with the error, run strips with a failure count, a table with state glyphs", SERVICES_DATA),
    example!("tracker", "wk", "--watch tracker.data.json", "a work tracker: sections Now / Next / Waiting (todos, rows, owner badges) kept by a feed, beside a Decided section that is yours"),
];

/// The services example's plot reads the dataset `latency`, which no script of the example
/// writes: these steps register it and append to it from a shell loop. The numbers are not in
/// the board, so appending one redraws the plot and makes no revision.
const SERVICES_DATA: &str = "#
# The latency plot at the top reads a dataset, not the document (margin help dashboards, \"Numbers that
# change often\"). Until the dataset is registered the plot's place says so. Register it, then append to it
# from whatever has the numbers; here a loop with made-up ones. Each append redraws the plot, and the
# board keeps its revision: nothing is marked as changed
#        margin data add latency --cap 600 --expect-every 30s
#        while sleep 5; do
#          margin data append latency \"{\\\"t\\\":$(date +%s),\\\"p50\\\":$((30 + RANDOM % 20)),\\\"p95\\\":$((120 + RANDOM % 150))}\"
#        done
";

/// The fourth example is not a board with a fetcher: it is an outside tool. A run queue that
/// takes submissions as events, runs them one at a time and owns its rows, as a program of its
/// own that margin neither starts nor schedules (`margin help integrate`).
pub struct ToolExample {
    pub name: &'static str,
    pub about: &'static str,
    pub artifact: &'static str,
    /// The tool: one self-contained script.
    pub tool: &'static str,
}

pub const RUNS: ToolExample = ToolExample {
    name: "runs",
    about: "a run queue kept by an outside tool: sections Running / Waiting / Finished, actions cancel and drop; margin runs nothing",
    artifact: include_str!("../../../skill/examples/runs.json"),
    tool: include_str!("../../../skill/examples/runs.tool.sh"),
};

impl ToolExample {
    /// The steps beside the artifact, on stderr.
    pub fn notes(&self) -> String {
        let n = self.name;
        format!(
            "# {n}: {about}
# stdout is the artifact, a whole document. This example shows a SEPARATE TOOL working with a board:
# it follows the event log, makes every decision itself, and writes the rows it owns. margin runs
# nothing for it, and no margin daemon or handler is involved.
#   1. create the board
#        margin example {n} | margin new {n}
#   2. take the tool (bash + jq) and start it; it registers itself as the owner of the ids run and run-...
#        margin example {n} --tool > {n}.tool.sh && chmod +x {n}.tool.sh
#        ./{n}.tool.sh &                 # its state (queue, logs) goes to ./runs-state, or $RUNS_DIR
#   3. submit work from anywhere (another directory or cron: add --project <project>)
#        margin emit sim.requested '{{\"label\": \"sim A\", \"cmd\": [\"sleep\", \"30\"]}}'
#   4. act on it: X on the running row cancels it, D on a waiting row drops it; from a script
#        margin act {n} run-<seq> cancel        |        margin act {n} run-<seq> drop
#   5. show it
#        margin open {n}
# The tool is yours to replace: order, one-at-a-time, cancel and restart behavior are all in that file.
# What it uses of margin: owner add, events --follow --cursor ... --no-commit / --commit, apply --as.
",
            about = self.about
        )
    }
}

pub fn get(name: &str) -> Option<&'static Example> {
    EXAMPLES.iter().find(|e| e.name == name)
}

/// `margin example`: the names, and how to use one.
pub fn list() -> String {
    let mut s = String::from("examples: complete dashboards to copy from (margin help dashboards explains the parts)\n");
    for e in EXAMPLES {
        s += &format!("  {:<9} {}\n", e.name, e.about);
    }
    s += &format!("  {:<9} {}\n", RUNS.name, RUNS.about);
    s += "\n  margin example <name>          the artifact: a whole document, ready for `margin new <art>`\n";
    s += "  margin example <name> --feed   the script that keeps it current (bash + jq)\n";
    s += "  margin example <name> --data   a sample of the data that script reads\n";
    s += "  margin example runs --tool     the outside tool of the runs example (it has no --feed or --data)\n";
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
{more}",
            more = self.more,
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

    /// The services board's plot is bound to a dataset: the artifact says which, the steps
    /// register it and append to it, and with records there the plot draws them (before that,
    /// its place names the command that registers the dataset).
    #[test]
    fn the_services_plot_reads_a_dataset_the_steps_register_and_append_to() {
        let e = get("services").unwrap();
        let d = doc(e);
        let plot = d.find("latency").expect("the latency plot");
        assert_eq!((plot.kind.as_str(), plot.opt("data"), plot.strs("y")), ("plot", Some("latency"), vec!["p50".to_string(), "p95".to_string()]));
        assert!(plot.f.get("series").is_none(), "its numbers are not in the document");
        let notes = e.notes();
        let lines: Vec<&str> = notes.lines().map(|l| l.trim_start_matches('#').trim()).collect();
        let at = lines.iter().position(|l| *l == "margin data add latency --cap 600 --expect-every 30s").expect("the step that registers the dataset");
        assert!(lines[at + 1].starts_with("while ") && lines[at + 2].starts_with("margin data append latency ") && lines[at + 3] == "done", "a three-line loop: {:?}", &lines[at + 1..at + 4]);
        for other in EXAMPLES.iter().filter(|x| x.name != "services") {
            assert!(!other.notes().contains("margin data"), "{}", other.name);
        }
        // the board as the pane draws it: without the dataset, then with records of the loop's shape
        let st = temp_store();
        st.put(d.clone(), None).unwrap();
        let frame = |st: &crate::store::Store| {
            let raw = st.load("services").unwrap();
            let snap = std::rc::Rc::new(crate::data::Snapshot::load(st, &crate::data::wants(&raw, 100), &mut crate::data::Cache::default()));
            crate::data::install(snap.clone());
            let shown = crate::data::expand(&raw, &snap);
            let (cfg, state, view) = (crate::config::Config::default(), crate::store::State::default(), crate::store::View::default());
            let mut sc = crate::scene::Scene::new(&cfg);
            let inp = crate::scene::Input { doc: &shown, state: &state, view: &view, delivered: 0, unread: 0, now: 1.0, wall: 1_794_000_057.0, cfg: &cfg, kitty: false, cell: (10, 20), focused_pane: false, mouse: false, project: "p", w: 100, h: 40, activity: &[] };
            sc.frame(&inp).grid.to_text()
        };
        let before = frame(&st);
        assert!(before.contains("no dataset \"latency\" (margin data add latency)") && before.contains("Failing (2)"), "{before}");
        st.data_add("latency", crate::data::Dataset { cap: Some(600), expect_every: Some(30.0), added: crate::util::now_rfc3339(), ..Default::default() }).unwrap();
        for i in 0..12 {
            st.data_append("latency", None, &[serde_json::json!({"t": 1_794_000_000 + i * 5, "p50": 30 + i % 7, "p95": 120 + (i * 37) % 150})]).unwrap();
        }
        let after = frame(&st);
        assert!(!after.contains("no dataset") && after.contains("● p50") && after.contains("● p95") && after.contains("-57s") && after.contains("now"), "{after}");
        assert_eq!(st.load("services").unwrap().rev, 1, "twelve appends, the same revision");
        let _ = std::fs::remove_dir_all(&st.home);
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
