"""Draw margin's design mockups into docs/img/mock/ (PNG stills, animated WebP).

Run inside the flake's shell:
    nix develop --command python3 docs/tools/mockups.py [name ...]

Every picture uses the components in kit.py, so changing a component changes
it in every mockup, the way the real toolkit will behave. Phone mockups are
62 columns, so a phone shows them without zooming; the hero is desktop-wide.
Each phone mockup is laid out twice: once on a tall canvas to measure, then
at the exact height, so no pane carries dead rows.
"""
import os
import subprocess
import sys
import tempfile
from termshot import Canvas, P, mix, animate
from kit import (I, SPIN, FADE, Pane, pill, tag, token, help_line, grad_bar, spark, section, todo, progress,
                 timer, since_mark, kv, text, rich, warrant, syntax, diff_inline, diff_side, card,
                 decision_card, assumption, decided, agent_row, ctx_gauge, finding, ledger, queue_item,
                 hypothesis, table, tree, zellij_bar)

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "..", "img", "mock")
PW = 62  # phone width in columns


def status_bar(cv: Canvas, y, name, what, right="", right_bg="aqua", x=0, w=None):
    """margin's status bar: app badge, the artifact, a state chip for Claude on the right."""
    w = w or cv.w
    cv.fill(x, y, w, 1, "bg_dim")
    e = pill(cv, x + 1, y, f" {I['leaf']} margin ", bg="purple", ground="bg_dim")
    e = cv.put(e + 1, y, name, "fg", "bg_dim", b=True)
    cv.put(e + 1, y, what, "grey1", "bg_dim")
    if right:
        pill(cv, x + w - len(right) - 3, y, right, bg=right_bg, ground="bg_dim")


def measure(build, w=PW):
    """Rows a pane needs: lay it out on a tall canvas and read where it ended."""
    return build(Canvas(w, 200), 200).row + 3          # blank row, help line, border


def phone(build, title, status, w=PW, h=None):
    """Lay out build(cv, h) -> Pane at its measured height (or a fixed one, for animation frames)."""
    h = h or measure(build, w)
    cv = Canvas(w, h + 1, title=title)
    build(cv, h)
    status(cv, h)
    return cv


def save(cv, name):
    cv.png(os.path.join(OUT, name + ".png"))


# ---------------------------------------------------------------------------
# 1. plan
def plan_pane(cv, x, y, w, h, spin=2, secs=842, prog=.5, ran=18, step3="doing", step3_age=0,
              decided_age=1, focused=True, extra_step=None, checks_open=True, changed_open=False,
              changed_rows=None, since=True, focus_on="t3", rev=9):
    p = Pane(cv, x, y, w, h, "fix recs flake", icon=I["plan"], meta=f"rev {rev}  3s ago", focused=focused,
             help=[("j/k", "move"), ("space", "tick"), ("⏎", "fold"), ("?", "more")])
    yy = section(p, "Steps", 4 + (1 if extra_step is not None else 0))
    timer(cv, p.ix + p.iw - 8, yy, secs)
    todo(p, "done", "Reproduce the flaky test", aside="1 in 20 runs", aside_fg="grey0")
    todo(p, "done", "Find the unseeded draw", aside="sampleBeta", aside_fg="grey0")
    if step3 == "done":
        todo(p, "done", "Seed RNG per test name", changed=step3_age, focused=focus_on == "t3")
    else:
        todo(p, "doing", "Seed RNG per test name", tok="mg:todo/t3", focused=focus_on == "t3", spin=spin)
    if extra_step is not None:
        todo(p, "todo", "Pin the seed list in CI", tok="mg:todo/t5", changed=extra_step)
    todo(p, "todo", "Regression test: 200 seeded runs", tok="mg:todo/t4", focused=focus_on == "t4")
    p.gap()
    done = 3 if prog >= .74 else 2
    progress(p, prog, done, 4, extra=f"{round(prog * 100)}%")
    p.gap()
    if checks_open:
        section(p, "Checks", 3)
        x0, yy = p.line(2)
        if ran < 20:
            cv.put(x0, yy, SPIN[spin % 10], "yellow", b=True)
            cv.put(x0 + 2, yy, "deno test apps/web", "fg")
            cv.put(x0 + 24, yy, f"{ran}/20", "yellow")
        else:
            cv.put(x0, yy, I["done"], "green", b=True)
            cv.put(x0 + 2, yy, "deno test apps/web", "fg")
            cv.put(x0 + 24, yy, "20/20", "green")
        vals = [3, 4, 3, 5, 6, 5, 7, 6, 8, 7, 9, 8]
        spark(cv, p.ix + p.iw - 12, yy, vals[: max(3, min(12, ran - 8))])
        x0, yy = p.line(2)
        cv.put(x0, yy, I["done"], "green", b=True)
        cv.put(x0 + 2, yy, "deno lint", "fg")
        cv.put(x0 + 24, yy, "clean", "grey1")
        x0, yy = p.line(2)
        cv.put(x0, yy, I["blocked"], "red", b=True)
        cv.put(x0 + 2, yy, "check.py recs", "fg")
        cv.put(x0 + 24, yy, "2 tells", "red", b=True)
        token(cv, p.ix + p.iw - 13, yy, "mg:open/check")
    else:
        yy = section(p, "Checks", 3, open=False)
        p.right(yy, f"{I['user']} folded by you", "purple")
    p.gap()
    if since:
        since_mark(p, "2 new since you looked, 19:40")
    section(p, "Decisions", 2, note="strike to correct")
    decided(p, "D1", "Seed from the test name", "19:44")
    p.change(p.row - 1, decided_age)
    assumption(p, "A1", "CI runs tests in one process", changed=decided_age)
    yy = section(p, "Changed this turn", open=changed_open, note="recs.ts +3 -1",
                 focused=focus_on == "changed")
    if changed_open and changed_rows:
        rows = [("-", 41, "  const d = items.map(sampleBeta);", ["sampleBeta"]),
                ("+", 41, "  const d = items.map((i) => sampleBeta(i, rng));", ["(i) =>", "(i, rng)"]),
                ("+", 42, "  // seeded per test name, see rng.ts", [])]
        diff_inline(p, rows[:changed_rows], indent=2)
    yy = section(p, "PR #412 notes", open=False, note="mg:open/pr-412", note_fg="aqua")
    return p


def plan():
    cv = phone(lambda cv, h: plan_pane(cv, 0, 0, PW, h), "margin: plan",
               lambda cv, y: status_bar(cv, y, "plan", "~/webapp", f" {I['send']} 1 event for Claude "))
    save(cv, "plan")


# ---------------------------------------------------------------------------
# 2. decisions: the defaults board
def decisions_pane(cv, h, q1=None, flash=None, sent=False, resolved=False, ack=False, a3_age=0):
    p = Pane(cv, 0, 0, PW, h, "what we've settled", icon=I["scale"], meta="rev 23  8s ago", focused=True,
             help=[("j/k", "move"), ("x", "strike"), ("1-3", "pick"), ("⏎", "why")])
    section(p, "Open questions", 1 if resolved else 2, note="Claude goes on with the default")
    p.gap()
    if not resolved:
        decision_card(p, "Q1", "MCP server in v0, or CLI only?", ["cli", "mcp", "both"], "cli",
                      picked=q1, applies="4m", focused=True, flash=flash, sent=sent)
        p.gap()
    decision_card(p, "Q2", "Keep artifacts after a session?", ["keep", "30d", "drop"], "30d",
                  applies="9m", focused=resolved)
    p.gap()
    section(p, "Assumptions Claude is making", 3)
    assumption(p, "A1", "Zellij runs over ssh, not locally")
    assumption(p, "A2", "Ghostty is 1.2 or newer everywhere", struck=True,
               reason="the old laptop is on 1.1")
    rich(p, [(I["robot"], "blue"), (" Claude ", "blue", {"b": True}), ("will probe the version", "grey2")], indent=4)
    assumption(p, "A3", "One chat per tab", changed=a3_age)
    p.gap()
    section(p, "Decided", 4 if resolved else 3)
    if resolved:
        y = decided(p, "Q1", "MCP server in v0 too", "now", by="you")
        p.change(y, 0)
        if ack:
            rich(p, [(I["robot"], "blue"), (" Claude ", "blue", {"b": True}),
                     ("adding a margin-mcp crate to the plan", "grey2")], indent=4)
            p.change(p.row - 1, 0)
    decided(p, "D1", "Rust for toolkit, app and plugin", "19:31", by="you")
    decided(p, "D2", "Own the UI; use difftastic, graphviz", "19:52")
    decided(p, "D3", "Selection tokens trigger actions", "19:58")
    return p


def decisions_status(cv, y, right=f" {I['user']} A2 struck ", bg="purple"):
    status_bar(cv, y, "decisions", "~/margin", right, right_bg=bg)


def decisions():
    save(phone(decisions_pane, "margin: decisions", decisions_status), "decisions")


# ---------------------------------------------------------------------------
# 3. hero: the chat, margin beside it, a pinned float
def hero():
    W, H = 150, 38
    cv = Canvas(W, H, title="zellij: main")
    zellij_bar(cv, 0, [("webapp", I["half"], "yellow"), ("api", I["half"], "yellow"),
                       ("margin", I["bell"], "purple"), ("home", "", None)], 0)
    cw = 80
    cv.box(0, 1, cw, H - 2, fg="bg3")
    pill(cv, 2, 1, f" {I['term']} claude  ~/webapp ", bg="bg3", fg="fg")
    y = 3

    def say(parts, x=3):
        nonlocal y
        for t, fg, *o in parts:
            x = cv.put(x, y, t, fg, **(o[0] if o else {}))
        y += 1

    cv.tint(1, y, cw - 2, 1, "bg1")
    say([("> ", "grey1"), ("the recs flake is red again, fix it and keep the PR notes", "fg")])
    y += 1
    say([("● ", "fg"), ("Reproducing first. The plan is in margin; I tick it as I go.", "fg")])
    say([("  └ ", "grey0"), ("margin open plan --split", "grey1"), ("   tab 1, focus unchanged", "grey0")])
    y += 1
    say([("● ", "green"), ("Bash", "fg", {"b": True}), ("(deno test apps/web --filter thompson)", "grey2")])
    say([("  └ ", "grey0"), ("FAILED", "red", {"b": True}), (" thompson picks argmax (1 of 20 runs)", "red")])
    say([("    ", "grey0"), ("seed differs per run: Math.random in sampleBeta", "grey1")])
    y += 1
    say([("● ", "fg"), ("The prior draw is unseeded, so 1 in 20 runs flips the ranking.", "fg")])
    say([("  ", "fg"), ("Seeding from the test name fixes it without hiding real bugs.", "fg")])
    y += 1
    say([("● ", "green"), ("Update", "fg", {"b": True}), ("(apps/web/recs.ts)", "grey2")])
    say([("  └ ", "grey0"), ("+3", "green"), (" -1", "red")])
    for no, kind, src, mk in ((41, "-", "  const draws = items.map(sampleBeta);", ["sampleBeta"]),
                              (41, "+", "  const draws = items.map((i) => sampleBeta(i, rng));", ["(i) =>", "(i, rng)"])):
        bg = "bg_red" if kind == "-" else "bg_green"
        cv.tint(6, y, cw - 9, 1, bg)
        cv.put(6, y, f"{no} {kind}", "red" if kind == "-" else "green", bg, b=True)
        syntax(cv, 11, y, src, bg)
        for m in mk:
            s = src.find(m)
            cv.tint(11 + s, y, len(m), 1, "hl_red" if kind == "-" else "hl_green")
        y += 1
    y += 1
    cv.tint(1, y, cw - 2, 1, "bg1")
    say([("> ", "grey1"), ("why seed from the test name and not a constant?", "fg")])
    y += 1
    say([("● ", "fg"), ("A constant seed makes every test share one stream, so adding", "fg")])
    say([("  ", "fg"), ("a test reshuffles the others' draws. A seed per test name", "fg")])
    say([("  ", "fg"), ("keeps each test stable on its own. Added as PR finding 2.", "fg")])
    y += 1
    say([("  ", "fg"), (f"{I['user']} you ticked t2 in margin, 12s ago", "purple", {"i": True})])
    say([("● ", "fg"), ("Saw it. Moving on to the regression test.", "fg")])
    y += 1
    say([(SPIN[4] + " ", "orange", {"b": True}), ("Writing the regression test… ", "orange"),
         ("38s  esc to interrupt", "grey0")])
    cv.box(2, H - 6, cw - 4, 3, fg="bg4")
    cv.put(4, H - 5, ">", "grey1")
    cv.put(6, H - 5, "▎", "fg")
    cv.put(4, H - 3, "? for shortcuts", "grey0")
    cv.put(cw - 22, H - 3, "71% context left", "grey0")
    ph = measure(lambda c, h: plan_pane(c, 0, 0, W - cw, h), W - cw)
    plan_pane(cv, cw, 1, W - cw, ph)
    q = Pane(cv, cw, 1 + ph, W - cw, H - 2 - ph, "questions", icon=I["scale"], meta="1 open",
             help=[("1-3", "pick"), ("x", "strike")])
    decision_card(q, "Q1", "Does CI run tests in one process?", ["yes", "no", "unsure"], "yes",
                  applies="4m", why="decides whether seeds can collide")
    # pinned summary float over the chat
    fx, fy, fw, fh = 38, 4, 40, 8
    cv.fill(fx, fy, fw, fh, "bg1")
    f = Pane(cv, fx, fy, fw, fh, "summary", icon=I["comment"], meta="40s", pinned=True, ground="bg1")
    rich(f, [("Fixing the recs flake, PR #412", "fg", {"b": True})], indent=0)
    rich(f, [(I["done"] + " ", "green"), ("cause: unseeded Beta draw", "grey2")], indent=0)
    rich(f, [(SPIN[2] + " ", "yellow", {"b": True}), ("fix: seed per test name", "grey2")], indent=0)
    rich(f, [(I["question"] + " ", "yellow"), ("open: does CI share a process?", "yellow")], indent=0)
    rich(f, [(I["right"] + " ", "grey1"), ("next: regression test, push", "fg")], indent=0)
    status_bar(cv, H - 1, "plan", "anchored to tab 1, beside the chat", f" {I['send']} 1 event for Claude ")
    save(cv, "hero")


# ---------------------------------------------------------------------------
# 4. review desk
def review():
    def build(cv, h):
        p = Pane(cv, 0, 0, PW, h, "PR #412", icon=I["pr"], meta="rev 6  just now", focused=True,
                 help=[("a", "approve"), ("r", "reject"), ("?", "ask"), ("n", "next hunk")])
        rich(p, [("recs: seed Thompson sampling per test", "fg", {"b": True})])
        x, y = p.line(0)
        e = tag(cv, x, y, f"{I['blocked']} 1 must-fix", "red")
        e = tag(cv, e + 1, y, "2 suggest", "yellow")
        grad_bar(cv, e + 2, y, 14, 2 / 3, a="purple", b="aqua")
        cv.put(e + 17, y, "2/3 hunks", "grey1")
        p.gap()
        finding(p, 1, "must-fix", "RNG leaks across tests", "recs.ts:41", focused=True)
        text(p, "sampleBeta() draws from Math.random, so the ranking", "grey2", indent=2)
        text(p, "depends on which tests ran first.", "grey2", indent=2)
        p.gap()
        rich(p, [(I["ts"] + " ", "blue"), ("apps/web/recs.ts", "fg"), ("   hunk 1 of 2  ", "grey0"),
                 ("difftastic", "grey0", {"i": True})], indent=2)
        diff_inline(p, [
            (" ", 40, "export function rank(items: Item[]) {", []),
            ("-", 41, "  const d = items.map(sampleBeta);", ["sampleBeta"]),
            ("+", 41, "  const rng = seeded(testName());", ["const rng = seeded(testName());"]),
            ("+", 42, "  const d = items.map((i) => sampleBeta(i, rng));", ["(i) =>", "(i, rng)"]),
            (" ", 43, "  return argsort(d).reverse();", []),
        ], indent=2, focused=2)
        x, y = p.line(2)
        x = token(cv, x, y, "mg:approve/412.h1") + 2
        x = token(cv, x, y, "mg:reject/412.h1") + 2
        token(cv, x, y, "mg:ask/412.h1")
        p.gap()
        finding(p, 2, "suggest", "name the seed source", "recs_test.ts:12", open=False)
        y = finding(p, 3, "suggest", "200 runs is slow (4.1 s)", "recs_test.ts:88", open=False)
        x, y = p.line(6)
        e = pill(cv, x, y, f" {I['user']} approved ", bg="purple")
        cv.put(e + 1, y, "Claude cut it to 50 runs", "grey1")
        p.change(y, 1)
        p.gap()
        section(p, "Files", 3)
        table(p, [("file", 26, "<"), ("+", 4, ">"), ("-", 3, ">"), ("review", 12, "<")], [
            ["apps/web/recs.ts", ("+3", "green"), ("-1", "red"), (f"{I['done']} done", "green")],
            ["apps/web/recs_test.ts", ("+41", "green"), ("-2", "red"), (f"{I['doing']} 1 open", "yellow")],
            ["apps/web/rng.ts", ("+18", "green"), ("-0", "grey0"), (f"{I['todo']} new", "grey1")],
        ], indent=2)
        return p
    save(phone(build, "margin: review",
               lambda cv, y: status_bar(cv, y, "review", "pr-412", f" {I['send']} 2 verdicts for Claude ")),
         "review")


# ---------------------------------------------------------------------------
# 5. agents
def agents():
    def build(cv, h):
        p = Pane(cv, 0, 0, PW, h, "agents", icon=I["robot"], meta="live", focused=True,
                 help=[("j/k", "move"), ("⏎", "expand"), ("a", "attach"), ("/", "filter")])
        x, y = p.line(0)
        e = tag(cv, x, y, "5 running", "green")
        e = tag(cv, e + 1, y, f"{I['bell']} 1 needs you", "purple")
        e = tag(cv, e + 1, y, "1 failed", "red")
        spark(cv, p.ix + p.iw - 12, y, [3, 4, 2, 6, 8, 5, 7, 9, 6, 4, 5, 8])
        p.gap()
        agent_row(p, "waiting", "scout", "sub", "asks you", "1m", act=None, focused=True)
        g = "bg1"
        for line in ("“pyte is in nixpkgs; OK to add it to", "margin's devShell to render captures?”"):
            x, y = p.line(2)
            cv.tint(p.ix - 1, y, p.iw + 2, 1, g)
            cv.put(x, y, line, "fg", g, i=True)
        x, y = p.line(2)
        cv.tint(p.ix - 1, y, p.iw + 2, 1, g)
        for t in ("mg:yes/scout", "mg:no/scout", "mg:attach/scout"):
            x = token(cv, x, y, t, bg=g) + 2
        agent_row(p, "working", "webapp", "peer", "Edit server.ts", "4m", act=[2, 3, 5, 4, 6, 7, 5])
        agent_row(p, "running", "guide-next-step", "worker", "reading guide", "40s", act=[1, 2, 2, 4, 3])
        agent_row(p, "done", "job-failed-diag", "worker", "posted", "9m")
        y = agent_row(p, "failed", "log-recorder", "unit", "exit 1", "2h")
        p.change(y, 0)
        p.gap()
        section(p, "Context", 3, note="notch: compaction at 80%")
        for name, frac in (("webapp", .62), ("scout", .31), ("job-failed-diag", .86)):
            x, y = p.line(2)
            cv.put(x, y, name, "grey2")
            ctx_gauge(cv, x + 18, y, frac, width=30)
        p.gap()
        section(p, "Live tail", note="guide-next-step")
        for t, tool, rest in (("19:42:31", "Read", "guides/getting-started.md"),
                              ("19:42:33", "Grep", "\"next step\"  3 hits"),
                              ("19:42:36", SPIN[5], "thinking")):
            x, y = p.line(2)
            cv.put(x, y, t, "grey0")
            cv.put(x + 10, y, tool, "blue" if len(tool) > 1 else "yellow", b=True)
            cv.put(x + 16, y, rest, "grey2")
        return p
    save(phone(build, "margin: agents",
               lambda cv, y: status_bar(cv, y, "agents", "5 sessions", f" {I['bell']} 1 needs you ", "purple")),
         "agents")


# ---------------------------------------------------------------------------
# 6. explainer: a graphviz picture placed as a kitty image
GRAPH_DOT = """
digraph G {
  graph [rankdir=LR, bgcolor="transparent", pad="0.25", nodesep="0.35", ranksep="0.42", dpi=192];
  node  [shape=box, style="rounded,filled", fontname="JetBrainsMono Nerd Font Mono", fontsize=13,
         color="#56635f", fillcolor="#343f44", fontcolor="#d3c6aa", penwidth=1.4, margin="0.18,0.10"];
  edge  [color="#7a8478", arrowsize=0.7, penwidth=1.4];
  event   [label="  event\\nevent log"];
  hook    [label="  hook\\nprompt"];
  matcher [label="  matcher\\nmatchJob()", color="#d699b6", fillcolor="#4c3f4c", fontcolor="#f0d0e0", penwidth=2.6];
  worker  [label="\U000f06a9  worker\\nclaude -p"];
  you     [label="  you\\nassistant", color="#83c092", fontcolor="#83c092"];
  event -> hook -> matcher -> worker -> you;
  matcher -> you [style=dashed, color="#d699b6", label=" needs-you ", fontcolor="#d699b6",
                  fontname="JetBrainsMono Nerd Font Mono", fontsize=11, constraint=false];
}
"""


def graph_png():
    path = os.path.join(OUT, "explainer-graph.png")
    d = tempfile.mkdtemp()
    conf = os.path.join(d, "fonts.conf")
    open(conf, "w").write(f'<?xml version="1.0"?><fontconfig><dir>{os.environ["MARGIN_FONT_DIR"]}</dir>'
                          f'<cachedir>{d}</cachedir><include ignore_missing="yes">/etc/fonts/fonts.conf</include></fontconfig>')
    subprocess.run(["dot", "-Tpng", "-o", path], input=GRAPH_DOT.encode(), check=True,
                   env={**os.environ, "FONTCONFIG_FILE": conf})
    return path


def explainer():
    g = graph_png()

    def build(cv, h):
        p = Pane(cv, 0, 0, PW, h, "how the matcher reaches you", icon=I["graph"], meta="step 3 of 5",
                 focused=True, help=[("←/→", "step"), ("⏎", "open code"), ("g", "full size")])
        text(p, "An event on disk becomes a message to you in", "fg", indent=0)
        text(p, "three hops. The lit node is the step below.", "fg", indent=0)
        p.gap()
        x, y = p.line(0)
        cv.image(x - 1, y, p.iw + 2, 6, g)
        cv.put(p.ix + p.iw - 12, y + 6, f"{I['image']} kitty image", "grey0")
        p.gap(6)
        p.gap()
        x, y = p.line(0)
        for i, lab in enumerate(["event", "hook", "matcher", "worker", "you"]):
            if i == 2:
                x = pill(cv, x, y, f" 3 {lab} ", bg="purple")
            elif i < 2:
                x = tag(cv, x, y, f"{I['check']} {lab}", "green")
            else:
                x = cv.put(x + 1, y, f"{i + 1} {lab}", "grey1") + 1
            if i < 4:
                x = cv.put(x, y, "─", "bg4")
        p.gap()
        section(p, "matcher: the job matcher", icon=I["cog2"])
        text(p, "Tails the event log and matches each event", "grey2", indent=2)
        text(p, "against the enabled jobs. A match starts a", "grey2", indent=2)
        text(p, "claude -p worker with the job's prompt.", "grey2", indent=2)
        rich(p, [(I["code"] + " ", "blue"), ("@tok", "apps/matcher/match.ts"), ("  matchJob()", "grey1")], indent=2)
        p.gap()
        section(p, "Check yourself", icon=I["question"])
        text(p, "What stops a worker's own line from", "fg", indent=2)
        text(p, "re-triggering the matcher?", "fg", indent=2)
        for i, (opt, sel) in enumerate((("the job's source filter skips claude", True),
                                        ("the 5-a-day message cap", False),
                                        ("the hook dedups by id", False))):
            x, y = p.line(2)
            if sel:
                p.focus(y)
            gnd = cv.cells[y][x].bg
            cv.put(x, y, "" if sel else "", "purple" if sel else "grey1", gnd, b=sel)
            e = cv.put(x + 2, y, opt, "fg" if sel else "grey2", gnd)
            if sel:
                tag(cv, p.ix + p.iw - 8, y, f"{I['check']} right", "green", gnd)
        return p
    save(phone(build, "margin: explain",
               lambda cv, y: status_bar(cv, y, "explain", "matcher", f" {I['check']} 1 of 1 right ", "green")),
         "explainer")


# ---------------------------------------------------------------------------
# 7. research board with warrant
def research():
    def build(cv, h):
        p = Pane(cv, 0, 0, PW, h, "images through zellij", icon=I["search"], meta="rev 11  2m ago",
                 focused=True, help=[("j/k", "move"), ("p", "pin"), ("⏎", "source"), ("e", "evidence")])
        rich(p, [("Can margin show images in Ghostty through Zellij?", "fg", {"b": True})], indent=0)
        x, y = p.line(0)
        e = pill(cv, x, y, f" {I['check']} yes ", bg="green")
        cv.put(e + 1, y, "kitty graphics, placed at the cursor", "green")
        x, y = p.line(0)
        cv.put(x, y, "confidence", "grey1")
        grad_bar(cv, x + 11, y, 24, .86, a="yellow", b="green")
        cv.put(x + 37, y, "high", "green", b=True)
        p.gap()
        section(p, "Pinned", 3, note=f"{I['user']} you pinned 2", note_fg="purple")
        for pinned, claim, w in ((True, "Zellij re-emits kitty images", "ran"),
                                 (True, "no unicode placeholders, no shm", "read"),
                                 (False, "Ghostty has no sixel", "read")):
            x, y = p.line(2)
            cv.put(x, y, I["pin"], "purple" if pinned else "bg4", b=pinned)
            cv.put(x + 2, y, claim, "fg")
            warrant(cv, p.ix + p.iw - 10, y, w)
        p.gap()
        section(p, "Evidence", 4)
        for w, line in (("ran", "fake host answers a=q; zellij sends a=t"),
                        ("ran", "U=1 → ENOTSUPPORTED from zellij"),
                        ("read", "PR #5428: kitty minus animation"),
                        ("inferred", "a float over an image clips it")):
            x, y = p.line(2)
            e = warrant(cv, x, y, w)
            cv.put(x + 13, y, line, "grey2")
        p.gap()
        section(p, "Dead ends", 2, open=False, note="sixel, client-tty writes")
        section(p, "Open", 1)
        rich(p, [(I["question"] + " ", "yellow"), ("Does a float clip an image under it?", "fg")], indent=2)
        x, y = p.line(4)
        token(cv, x, y, "mg:ask/r1")
        return p
    save(phone(build, "margin: research",
               lambda cv, y: status_bar(cv, y, "research", "zellij-images", f" {I['pin']} 2 pins ", "purple")),
         "research")


# ---------------------------------------------------------------------------
# 8. turn ledger
def changes():
    def build(cv, h):
        p = Pane(cv, 0, 0, PW, h, "what changed, turn by turn", icon=I["history"], meta="jj @ mswytvrq",
                 focused=True, help=[("⏎", "fold"), ("d", "diff"), ("R", "revert turn")])
        since_mark(p, "1 turn since you looked")
        ledger(p, 14, "seed RNG per test", [("apps/web/recs.ts", 3, 1), ("apps/web/rng.ts", 18, 0)],
               open=True, focused=True, changed=0)
        rich(p, [(I["ts"] + " ", "blue"), ("apps/web/rng.ts", "fg"), ("  new file", "grey0")], indent=4)
        diff_inline(p, [("+", 1, "export function seeded(name = testName()) {", []),
                        ("+", 2, "  return mulberry32(hash(name));", []),
                        ("+", 3, "}", [])], indent=4)
        x, y = p.line(4)
        e = token(cv, x, y, "mg:diff/14")
        token(cv, e + 2, y, "mg:revert/14")
        p.gap()
        ledger(p, 13, "reproduce flake", [("apps/web/recs_test.ts", 41, 2)])
        ledger(p, 12, "PR notes, finding 2", [("pr-412/notes.md", 6, 0)])
        ledger(p, 11, "rename sampleBeta args", [("apps/web/recs.ts", 4, 4)])
        return p
    save(phone(build, "margin: changes",
               lambda cv, y: status_bar(cv, y, "changes", "from jj, no model calls", "")), "changes")


# ---------------------------------------------------------------------------
# 9. message queue
def inbox():
    def build(cv, h):
        p = Pane(cv, 0, 0, PW, h, "your messages", icon=I["inbox"], meta="this session",
                 focused=True, help=[("j/k", "move"), ("⏎", "jump to reply"), ("d", "drop")])
        x, y = p.line(0)
        e = tag(cv, x, y, "3 done", "green")
        e = tag(cv, e + 1, y, "1 working", "yellow")
        e = tag(cv, e + 1, y, "2 queued", "grey1")
        p.gap()
        queue_item(p, "19:31", "use rust, that's very zellij", "done", "done: workspace and flake")
        queue_item(p, "19:40", "own the toolkit from the ground up", "done", "done: design.md §2")
        queue_item(p, "19:47", "links via selection, not OSC 8", "doing", "working: testing copy_command",
                   focused=True)
        queue_item(p, "19:52", "more use cases, push the idea", "queued", "queued, after the visuals")
        queue_item(p, "19:55", "more interaction than a md file", "queued", "queued: shapes §3")
        queue_item(p, "19:58", "stop after research and design", "changed", "changed the plan: v0 build dropped",
                   changed=0)
        return p
    save(phone(build, "margin: inbox",
               lambda cv, y: status_bar(cv, y, "inbox", "nothing you said is lost", f" 2 open ", "yellow")),
         "inbox")


# ---------------------------------------------------------------------------
# 10. component sheets for the tables in design.md and the guide
def sheet_toolkit():
    def build(cv, h):
        p = Pane(cv, 0, 0, PW, h, "vellum: toolkit parts", icon=I["chip"], meta="v0 to v2", focused=True,
                 help=[("tab", "next part"), ("t", "theme")])

        def head(name):
            if p.row > p.y + 2:
                p.gap()
            x, y = p.line(0)
            cv.put(x, y, name, "purple", b=True)

        head("text")
        rich(p, [("bold", "fg", {"b": True}), ("@sp", 2), ("italic", "fg", {"i": True}), ("@sp", 2),
                 ("dim", "grey0"), ("@sp", 2), ("struck", "grey1", {"s": True}), ("@sp", 2),
                 ("curly", "fg", {"u": "curly", "uc": "red"}), ("@sp", 2), ("@tok", "mg:token")], indent=2)
        x, y = p.line(2)
        cv.grad(x, y, "gradient title, per cell", "purple", "aqua")
        head("pill, tag, glyph")
        x, y = p.line(2)
        x = pill(cv, x, y, " mode ", bg="purple") + 1
        x = tag(cv, x, y, "ran", "green") + 1
        x = tag(cv, x, y, "read", "blue") + 1
        x = tag(cv, x, y, "inferred", "yellow") + 1
        x = tag(cv, x, y, "3", "grey2") + 2
        for g, c in ((I["done"], "green"), (I["doing"], "yellow"), (I["todo"], "grey0"),
                     (I["blocked"], "red"), (I["bell"], "purple"), (I["pin"], "purple")):
            x = cv.put(x, y, g, c, b=True) + 1
        head("progress, gauge, spinner, timer, sparkline")
        x, y = p.line(2)
        grad_bar(cv, x, y, 22, .62)
        cv.put(x + 24, y, "3/5", "fg", b=True)
        ctx_gauge(cv, x + 30, y, .84, width=20)
        x, y = p.line(2)
        for k in range(0, 10, 2):
            x = cv.put(x, y, SPIN[k], "yellow", b=True) + 1
        x = timer(cv, x + 1, y, 842) + 2
        spark(cv, x, y, [2, 3, 5, 4, 6, 8, 7, 9, 6, 7, 8, 10, 9])
        head("box, tree, table")
        x, y = p.line(2)
        cv.box(x, y, 24, 4, fg="bg3")
        pill(cv, x + 2, y, " titled ", bg="bg3", fg="fg")
        cv.put(x + 2, y + 2, "rounded, padded", "grey2")
        cv.box(x + 26, y, 28, 4, grad=("purple", "aqua"))
        pill(cv, x + 28, y, " focused ", bg="purple")
        cv.put(x + 28, y + 2, "gradient ring", "grey2")
        p.gap(3)
        tree(p, [(0, "apps/web", "fg", True, ""), (1, "recs.ts", "fg", None, "+3 -1"),
                 (1, "rng.ts", "green", None, "new"), (1, "tests", "fg", False, "2 files")], sel=1, indent=2)
        table(p, [("agent", 14, "<"), ("state", 10, "<"), ("ctx", 5, ">")],
              [["webapp", ("working", "yellow"), "62%"], ["researcher", ("asks you", "purple"), "31%"]],
              sort=2, indent=2)
        head("diff block (difftastic JSON)")
        diff_inline(p, [("-", 41, "  const d = items.map(sampleBeta);", ["sampleBeta"]),
                        ("+", 41, "  const d = items.map((i) => sampleBeta(i, rng));", ["(i) =>", "(i, rng)"])],
                    indent=2)
        head("change bar, since mark")
        y = text(p, "a line Claude changed since you looked", "fg", indent=2)
        p.change(y, 0)
        y = text(p, "fading as you read it", "fg", indent=2)
        p.change(y, 1)
        since_mark(p, "3 new since 19:40")
        head("help line, status bar")
        x, y = p.line(2)
        help_line(cv, x, y, [("j/k", "move"), ("space", "tick"), ("⏎", "open")])
        return p
    save(phone(build, "vellum: parts",
               lambda cv, y: status_bar(cv, y, "parts", "every state in one place", f" {I['send']} 1 event ")),
         "parts-toolkit")


def sheet_artifacts():
    def build(cv, h):
        p = Pane(cv, 0, 0, PW, h, "margin: artifact parts", icon=I["plan"], meta="what Claude writes",
                 focused=True, help=[("tab", "next part")])

        def head(name):
            if p.row > p.y + 2:
                p.gap()
            x, y = p.line(0)
            cv.put(x, y, name, "purple", b=True)

        head("plan step")
        todo(p, "done", "Reproduce the flaky test", indent=2)
        todo(p, "doing", "Seed RNG per test name", tok="mg:todo/t3", indent=2, spin=3)
        todo(p, "todo", "Regression test", tok="mg:todo/t4", indent=2, by_you=True)
        head("decision card")
        decision_card(p, "Q1", "MCP server in v0?", ["cli", "mcp", "both"], "cli", applies="4m")
        head("assumption, decided")
        assumption(p, "A1", "Zellij runs over ssh")
        assumption(p, "A2", "Ghostty 1.2 everywhere", struck=True)
        decided(p, "D1", "Rust for toolkit and app", "19:31", by="you")
        head("finding, warrant, pin")
        finding(p, 1, "must-fix", "RNG leaks across tests", "recs.ts:41", open=False)
        x, y = p.line(2)
        cv.put(x, y, I["pin"], "purple", b=True)
        cv.put(x + 2, y, "Zellij re-emits kitty images", "fg")
        warrant(cv, p.ix + p.iw - 9, y, "ran")
        head("hypothesis")
        hypothesis(p, "H1", "unseeded draw flips ranking", "holds", "")
        hypothesis(p, "H2", "test order leaks state", "ruled out", "")
        head("turn ledger, queue card")
        ledger(p, 14, "seed RNG per test", [("apps/web/recs.ts", 3, 1), ("apps/web/rng.ts", 18, 0)])
        queue_item(p, "19:47", "links via selection, not OSC 8", "doing", "working: testing copy_command")
        head("agent row")
        agent_row(p, "waiting", "researcher", "sub", "asks you", "1m")
        agent_row(p, "working", "webapp", "peer", "Edit server.ts", "4m", act=[2, 3, 5, 4, 6, 7, 5], spin=4)
        return p
    save(phone(build, "margin: parts",
               lambda cv, y: status_bar(cv, y, "parts", "typed, with ids", "")), "parts-artifacts")


# ---------------------------------------------------------------------------
# animations
def anim_pick():
    """Double-click a pick token: selection, flash, sent, the card resolves, Claude answers."""
    frames = []
    st = lambda r, bg="purple": (lambda cv, y: decisions_status(cv, y, r, bg))
    idle = st(f" {I['hourglass']} Q1 default in 4m ", "bg3")

    H = measure(decisions_pane)

    def add(ms, status=idle, **kw):
        frames.append((phone(lambda cv, h: decisions_pane(cv, h, **kw), "margin: decisions", status, h=H), ms))

    add(1400)
    add(600, flash=(1, 0))                       # Zellij selection highlight on mg:pick/Q1.mcp
    add(150, flash=(1, 1))                       # margin's inverse flash
    add(120, flash=(1, 0))
    add(150, flash=(1, 1))
    add(1300, q1="mcp", sent=True, status=st(f" {I['send']} 1 event for Claude ", "aqua"))
    add(1100, q1="mcp", status=st(f" {I['send']} 1 event for Claude ", "aqua"))
    add(2600, resolved=True, ack=True, status=st(f" {I['check']} Claude read it ", "green"))
    # the page needs the same canvas height in every frame
    animate(frames, os.path.join(OUT, "pick.webp"))


def anim_live():
    """A turn in progress: spinner, timer, test count, the step lands, progress springs, bars fade."""
    frames = []
    st = lambda cv, y: status_bar(cv, y, "plan", "~/webapp", f" {I['send']} 1 event for Claude ")
    # spring for progress .5 → .75 (critically damped, no overshoot)
    import math
    spring = [.5 + .25 * (1 - (1 + t) * math.exp(-t)) for t in [k * .55 for k in range(12)]]
    H = measure(lambda cv, h: plan_pane(cv, 0, 0, PW, h, since=False))
    n = 0
    for k in range(18):                  # tests run
        frames.append((phone(lambda cv, h: plan_pane(cv, 0, 0, PW, h, spin=n, secs=842 + n // 10,
                                                     ran=min(20, 14 + k // 3), since=False), "margin: plan", st, h=H), 100))
        n += 1
    for k, v in enumerate(spring):       # step 3 done; progress springs
        frames.append((phone(lambda cv, h: plan_pane(cv, 0, 0, PW, h, spin=n, secs=842 + n // 10, ran=20,
                                                     step3="done", step3_age=0, prog=v, focus_on="t4",
                                                     since=False), "margin: plan", st, h=H), 70))
        n += 1
    for age, ms in ((0, 900), (1, 500), (2, 500), (3, 1300)):    # the change bar fades as you look
        frames.append((phone(lambda cv, h: plan_pane(cv, 0, 0, PW, h, spin=n, secs=843 + n // 10, ran=20,
                                                     step3="done", step3_age=age, decided_age=3, prog=.75,
                                                     focus_on="t4", since=False), "margin: plan", st, h=H), ms))
        n += 1
    animate(frames, os.path.join(OUT, "live.webp"))


def anim_update():
    """Claude's update lands while your folds and focus stay put; then you open a section."""
    frames = []
    st = lambda r, bg: (lambda cv, y: status_bar(cv, y, "plan", "~/webapp", r, bg))
    base = dict(checks_open=False, since=False, decided_age=3, focus_on="changed", prog=.5, ran=20)

    H = measure(lambda cv, h: plan_pane(cv, 0, 0, PW, h, **{**base, "extra_step": 0, "since": True,
                                                              "changed_open": True, "changed_rows": 3}))

    def add(ms, status, **kw):
        args = {**base, **kw}
        frames.append((phone(lambda cv, h: plan_pane(cv, 0, 0, PW, h, **args), "margin: plan", status, h=H), ms))

    add(1500, st(" rev 9 ", "bg3"))
    add(1600, st(f" {I['refresh']} rev 10 from Claude ", "blue"), extra_step=0, since=True, decided_age=0, rev=10)
    add(1200, st(f" {I['refresh']} rev 10 from Claude ", "blue"), extra_step=1, since=True, decided_age=1, rev=10)
    for k, ms in ((1, 90), (2, 90), (3, 1800)):  # ⏎ on the focused section: rows ease in
        add(ms, st(" rev 10 ", "bg3"), extra_step=2, since=True, decided_age=2, changed_open=True, changed_rows=k, rev=10)
    add(1200, st(" rev 10 ", "bg3"), extra_step=3, since=True, decided_age=3, changed_open=True, changed_rows=3, rev=10)
    animate(frames, os.path.join(OUT, "update.webp"))


def evidence(stream, w, h):
    """Render a captured Zellij client stream (pyte) as a picture: proof, not a mockup."""
    import pyte
    from termshot import from_pyte

    class S(pyte.Screen):
        def report_device_status(self, *a, **k): pass
        def report_device_attributes(self, *a, **k): pass
    s = S(w, h)
    pyte.ByteStream(s).feed(open(stream, "rb").read())
    from_pyte(s).png(os.path.join(OUT, "..", "evidence-pinned-float.png"))


NAMES = {"plan": plan, "decisions": decisions, "hero": hero, "review": review, "agents": agents,
         "explainer": explainer, "research": research, "changes": changes, "inbox": inbox,
         "parts-toolkit": sheet_toolkit, "parts-artifacts": sheet_artifacts,
         "pick": anim_pick, "live": anim_live, "update": anim_update}

if __name__ == "__main__":
    os.makedirs(OUT, exist_ok=True)
    if sys.argv[1:2] == ["evidence"]:          # evidence <stream> <cols> <rows>
        evidence(sys.argv[2], int(sys.argv[3]), int(sys.argv[4]))
        sys.exit()
    for n in (sys.argv[1:] or list(NAMES)):
        NAMES[n]()
        print("drew", n)
