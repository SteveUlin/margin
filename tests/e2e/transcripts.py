"""Transcript providers (providers/queue, providers/turns): synthetic Claude Code transcripts, then a throwaway
headless Zellij session.

Part 1, the scripts alone (QUEUE_ONCE / TURNS_ONCE, no daemon):
  queue: waiting -> absorbed (turn n) -> answered; typed-between-turns never shows; waited for the
         turn's end -> "next turn" -> answered; popAll and a remove without delivery -> dropped;
         notifications sharing the queue never show; text is one line, at most 60 chars.
  turns: one section per prompt (tool results, meta and compact summaries start none), tool and
         file counts, only the latest unfolded, a running turn, and the jj change at the turn's end
         read with --at-op from a throwaway jj repo.
Part 2, live: both registered as handlers of one refresh event on one board; appending to the
  transcript and emitting the event moves a queued message through the board in the pane, and a
  new turn shows up, with no AI op.

usage: python3 tests/e2e/transcripts.py      (inside `nix develop`, after `cargo build`)
Never touches sulin's sessions, transcripts or config: its own session name, --config, MARGIN_HOME,
CLAUDE_PROJECTS.
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from datetime import datetime, timedelta, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "../.."))
sys.path.insert(0, HERE)
from ptyhost import Host  # noqa: E402

MARGIN = os.environ.get("MARGIN_BIN", os.path.join(ROOT, "target/debug/margin"))
QUEUE = os.path.join(ROOT, "providers/queue")
TURNS = os.path.join(ROOT, "providers/turns")
T = tempfile.mkdtemp(prefix="margin-transcripts-")
S = f"margin-tr-{os.getpid()}"
HOME = os.path.join(T, "home")
REPO = os.path.join(T, "repo")
PROJECTS = os.path.join(T, "projects")
ENV = dict(os.environ)
ENV.update({
    "MARGIN_HOME": HOME,
    "MARGIN_PROJECT": "e2e",
    "MARGIN_CONFIG": os.path.join(T, "margin.json"),
    "CLAUDE_PROJECTS": PROJECTS,
    "PATH": os.path.dirname(MARGIN) + ":" + os.environ["PATH"],
})
for k in ("ZELLIJ", "ZELLIJ_SESSION_NAME", "ZELLIJ_PANE_ID", "WAYLAND_DISPLAY", "MARGIN_HOST", "CLAUDE_CODE_SESSION_ID"):
    ENV.pop(k, None)
results = []


def check(name, ok, detail=""):
    results.append((name, bool(ok), detail))
    print(("PASS " if ok else "FAIL ") + name + (f"  ({detail})" if detail else ""), flush=True)
    return ok


def sh(*args, env=None, inp=None, cwd=None):
    return subprocess.run(args, capture_output=True, text=True, env=env or ENV, input=inp, cwd=cwd or T)


def m(*args, env=None, inp=None):
    return sh(MARGIN, *args, env=env, inp=inp)


class Transcript:
    """A synthetic session transcript; each helper appends one entry at a clock that only moves forward."""

    def __init__(self, path, cwd, start):
        self.path, self.cwd, self.t = path, cwd, start
        os.makedirs(os.path.dirname(path), exist_ok=True)
        open(path, "w").close()

    def ts(self, dt=0.5):
        self.t += timedelta(seconds=dt)
        return self.t.strftime("%Y-%m-%dT%H:%M:%S.") + f"{self.t.microsecond // 1000:03d}Z"

    def put(self, e):
        with open(self.path, "a") as f:
            f.write(json.dumps(e, separators=(",", ":")) + "\n")  # compact, as Claude Code writes it

    def prompt(self, text, **kw):
        self.put({"type": "user", "isSidechain": False, "cwd": self.cwd, "timestamp": self.ts(), "message": {"role": "user", "content": text}, **kw})

    def tool(self, name, **inp):
        uid = f"toolu_{len(open(self.path).read())}"
        self.put({"type": "assistant", "isSidechain": False, "timestamp": self.ts(), "message": {"stop_reason": "tool_use", "content": [{"type": "tool_use", "id": uid, "name": name, "input": inp}]}})
        self.put({"type": "user", "isSidechain": False, "timestamp": self.ts(), "message": {"content": [{"type": "tool_result", "tool_use_id": uid, "content": "ok"}]}})

    def end(self):
        self.put({"type": "assistant", "isSidechain": False, "timestamp": self.ts(), "message": {"stop_reason": "end_turn", "content": [{"type": "text", "text": "done"}]}})

    def q(self, op, content=None, dt=0.5):
        e = {"type": "queue-operation", "operation": op, "timestamp": self.ts(dt)}
        if content is not None:
            e["content"] = content
        self.put(e)

    def absorbed(self, content):
        self.put({"type": "attachment", "isSidechain": False, "timestamp": self.ts(), "attachment": {"type": "queued_command", "prompt": content, "origin": {"kind": "human"}, "humanTurn": True}})


def once(script, path, env_key):
    r = sh(script, path, env=dict(ENV, **{env_key: "1"}))
    try:
        return json.loads(r.stdout.strip().splitlines()[-1])
    except Exception:
        print(r.stdout, r.stderr)
        return {"children": []}


def rows(section):
    return {c["id"]: c for c in section.get("children", [])}


LONG = "please also check that the flake still builds on the laptop before you push anything at all\nsecond line"


def part1():
    old = datetime.now(timezone.utc) - timedelta(minutes=30)
    tr = Transcript(os.path.join(PROJECTS, "-x", "s1.jsonl"), REPO, old)
    tr.prompt("first prompt")
    tr.q("enqueue", LONG)                       # q-0: waits while turn 1 works
    tr.q("enqueue", "<task-notification>\n<task-id>a1</task-id>")  # q-1: not sulin's
    s = once(QUEUE, tr.path, "QUEUE_ONCE")
    r = rows(s)
    check("queue: a typed message shows as waiting", r.get("q-0", {}).get("tag") == "waiting", json.dumps(r.get("q-0")))
    t = r.get("q-0", {}).get("text", "")
    check("queue: only the first line, at most 60 chars, cut with an ellipsis", len(t) <= 60 and t.endswith("…") and "\n" not in t and "second" not in t, repr(t))
    check("queue: notifications sharing the queue never show", "q-1" not in r, str(list(r)))
    check("queue: the header counts waiting", "1 waiting" in s.get("note", ""), s.get("note"))

    tr.tool("Edit", file_path=f"{REPO}/a.rs")
    tr.q("remove", LONG)
    tr.absorbed(LONG)
    tr.q("remove", "<task-notification>\n<task-id>a1</task-id>")
    r = rows(once(QUEUE, tr.path, "QUEUE_ONCE"))
    check("queue: queued_command + remove -> absorbed in turn 1", r.get("q-0", {}).get("tag") == "absorbed" and r["q-0"].get("aside") == "turn 1", json.dumps(r.get("q-0")))
    tr.end()
    r = rows(once(QUEUE, tr.path, "QUEUE_ONCE"))
    check("queue: the turn's end -> answered", r.get("q-0", {}).get("tag") == "answered", json.dumps(r.get("q-0")))

    tr.q("enqueue", "typed between turns")      # q-2: dequeued at once: an ordinary prompt
    tr.q("dequeue", dt=0.01)
    tr.prompt("typed between turns")
    tr.q("enqueue", "wait for the end please")  # q-3: waits for turn 2 to end, becomes turn 3
    tr.tool("Bash", command="true", description="noop")
    tr.end()
    tr.q("dequeue", dt=4)
    tr.prompt("wait for the end please")
    r = rows(once(QUEUE, tr.path, "QUEUE_ONCE"))
    check("queue: typed between turns never shows", "q-2" not in r, str(list(r)))
    check("queue: waited for the turn's end -> next turn (turn 3)", r.get("q-3", {}).get("tag") == "next turn" and r["q-3"].get("aside") == "turn 3", json.dumps(r.get("q-3")))
    tr.q("enqueue", "pulled back")              # q-4: popAll
    tr.q("popAll")
    tr.q("enqueue", "removed, never delivered")  # q-5: remove without an attachment, long ago
    tr.q("remove", "removed, never delivered")
    tr.end()
    s = once(QUEUE, tr.path, "QUEUE_ONCE")
    r = rows(s)
    check("queue: next turn -> answered at its end", r.get("q-3", {}).get("tag") == "answered", json.dumps(r.get("q-3")))
    check("queue: popAll -> dropped", r.get("q-4", {}).get("tag") == "dropped", json.dumps(r.get("q-4")))
    check("queue: removed without delivery -> dropped", r.get("q-5", {}).get("tag") == "dropped", json.dumps(r.get("q-5")))
    check("queue: header says all answered", s.get("note") == "all answered", s.get("note"))

    # turns, over the same transcript plus a running turn; REPO is a jj repo with two changes
    sh("jj", "git", "init", REPO)
    sh("jj", "describe", "-m", "first", cwd=REPO)
    first = sh("jj", "log", "--no-graph", "-r", "@", "-T", "change_id.shortest(4)", cwd=REPO).stdout.strip()
    time.sleep(1.1)
    tr.t = datetime.now(timezone.utc)            # turn 4 starts after the first change's op
    tr.put({"type": "user", "isSidechain": False, "isCompactSummary": True, "timestamp": tr.ts(), "message": {"content": "This session is being continued"}})
    tr.prompt("<command-name>/tidy</command-name>")
    tr.put({"type": "user", "isMeta": True, "timestamp": tr.ts(), "message": {"content": "meta"}})
    tr.tool("Write", file_path=f"{REPO}/b.rs")
    tr.tool("Edit", file_path=f"{REPO}/b.rs")
    tr.tool("Read", file_path=f"{REPO}/c.rs")
    tr.end()
    time.sleep(max(0.0, (tr.t - datetime.now(timezone.utc)).total_seconds()) + 1.1)  # the synthetic clock runs ahead
    sh("jj", "new", "-m", "second", cwd=REPO)
    second = sh("jj", "log", "--no-graph", "-r", "@", "-T", "change_id.shortest(4)", cwd=REPO).stdout.strip()
    tr.t = datetime.now(timezone.utc)
    tr.prompt("still going")
    tr.tool("Bash", command="sleep 1", description="wait")
    s = once(TURNS, tr.path, "TURNS_ONCE")
    r = rows(s)
    check("turns: one section per prompt (tool results, meta, compact summary start none)", list(r) == [f"tn-{i}" for i in range(1, 6)], str(list(r)))
    t4 = r.get("tn-4", {})
    check("turns: tool and file counts", "3 tools" in t4.get("note", "") and "1 file" in t4.get("note", ""), t4.get("note"))
    f = [c for c in t4.get("children", []) if c.get("glyph") == "file"]
    check("turns: the written file listed relative to the repo, Read not counted", [c["text"] for c in f] == ["b.rs"] and f[0].get("aside") == "×2", json.dumps(f))
    check("turns: only the latest is unfolded", [x.get("folded") for x in r.values()] == [True] * 4 + [False], str([x.get("folded") for x in r.values()]))
    t5 = r.get("tn-5", {})
    check("turns: the running turn says so", "so far" in t5.get("note", "") and "#5 running" in s.get("note", ""), f"{t5.get('note')} | {s.get('note')}")
    check("turns: jj change at each turn's end (--at-op)", f"@{first}" in t4.get("title", "") and f"@{second}" in t5.get("title", ""), f"{first}/{second}: {t4.get('title')} | {t5.get('title')}")
    check("turns: the script recorded no jj operation", sh("jj", "op", "log", "--no-graph", "-T", "description ++ \"\\n\"", cwd=REPO).stdout.count("\n") == sh("jj", "op", "log", "--no-graph", "-T", "description ++ \"\\n\"", "--ignore-working-copy", cwd=REPO).stdout.count("\n"))
    return tr


def part2():
    with open(os.path.join(T, "config.kdl"), "w") as f:
        f.write("show_startup_tips false\nshow_release_notes false\npane_frames true\n")
    with open(os.path.join(T, "layout.kdl"), "w") as f:
        f.write('layout {\n  tab name="work" {\n    pane\n  }\n}\n')
    with open(ENV["MARGIN_CONFIG"], "w") as f:
        json.dump({"clipboard": "none", "opener": "true", "motion": "off"}, f)
    argv = ["zellij", "--config", os.path.join(T, "config.kdl"), "--config-dir", T, "-n", os.path.join(T, "layout.kdl"), "-s", S]
    h = Host(argv, cols=150, rows=44, env=ENV)

    def z(*a):
        return sh("zellij", "-s", S, "action", *a)

    def zj(*a):
        try:
            return json.loads(z(*a).stdout)
        except Exception:
            return None

    def wait(pred, timeout=8.0):
        end = time.time() + timeout
        while time.time() < end:
            v = pred()
            if v:
                return v
            time.sleep(0.1)
        return None

    try:
        tabs = wait(lambda: zj("list-tabs", "-j"), 10)
        if not check("zellij session up", tabs):
            return
        chat = wait(lambda: next((p for p in zj("list-panes", "-a", "-j") or [] if not p["is_plugin"]), None), 8)
        cenv = dict(ENV, ZELLIJ_SESSION_NAME=S, ZELLIJ_PANE_ID=str(chat["id"]))
        tr = Transcript(os.path.join(PROJECTS, "-live", "live1.jsonl"), REPO, datetime.now(timezone.utc))
        tr.prompt("start the live test")
        m("new", "live", "--title", "transcripts e2e", inp=json.dumps({"blocks": []}))
        m("open", "live", env=cenv)
        mp = wait(lambda: next((p for p in zj("list-panes", "-a", "-j") or [] if p["title"] == "margin:live"), None), 5)

        def pane():
            return z("dump-screen", "-p", f"terminal_{mp['id']}").stdout

        r1 = m("handler", "add", "queue-refresh", "--on", "live.refresh", "--board", "live", "--owns", "q", "--", "env", "QUEUE_ONCE=1", QUEUE, tr.path)
        r2 = m("handler", "add", "turns-refresh", "--on", "live.refresh", "--board", "live", "--owns", "tn", "--", "env", "TURNS_ONCE=1", TURNS, tr.path)
        check("both handlers register", r1.returncode == 0 and r2.returncode == 0, (r1.stderr + r2.stderr).strip())

        def refresh():
            # margin has no timer: whatever owns the schedule emits the event (here, the test)
            m("emit", "live.refresh")

        refresh()
        check("pane shows the empty board and turn 1", wait(lambda: "nothing queued" in pane() and "#1" in pane(), 10), pane()[-400:])
        tr.q("enqueue", "also bump the version")
        refresh()
        check("live: waiting", wait(lambda: "waiting" in pane() and "also bump the version" in pane(), 6))
        tr.tool("Bash", command="true", description="x")
        tr.q("remove", "also bump the version")
        tr.absorbed("also bump the version")
        refresh()
        check("live: absorbed, turn 1", wait(lambda: "absorbed" in pane() and "turn 1" in pane(), 6))
        tr.end()
        refresh()
        check("live: answered", wait(lambda: "answered" in pane() and "all answered" in pane(), 6))
        tr.prompt("second turn now")
        refresh()
        check("live: a new turn shows", wait(lambda: "#2" in pane() and "second turn now" in pane(), 8))
        with open(os.path.join(T, "pane.txt"), "w") as f:
            f.write(pane())
        doc = json.load(open(os.path.join(HOME, "e2e", "live", "doc.json")))
        check("revisions after the first are all the handlers'", str(doc.get("by", "")).startswith("handler:"), doc.get("by"))
        for n in ("queue-refresh", "turns-refresh"):
            m("handler", "rm", n)
        m("close", "live", env=cenv)
    finally:
        h.close()
        sh("zellij", "kill-session", S)
        sh("zellij", "delete-session", S)


def main():
    os.makedirs(REPO, exist_ok=True)
    try:
        part1()
        part2()
    finally:
        for n in ("queue-refresh", "turns-refresh"):
            m("handler", "rm", n)
        if os.environ.get("KEEP"):
            print("kept", T)
        else:
            shutil.rmtree(T, ignore_errors=True)
    bad = [r for r in results if not r[1]]
    print(f"\n{len(results) - len(bad)}/{len(results)} checks passed")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
