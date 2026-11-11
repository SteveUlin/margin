"""capture_feeds: record feeds driving the running pane, as animated WebPs and PNG stills.

Real renders, the same way capture.py makes them: `margin view` runs in a pty, its bytes replay
through pyte (a fake terminal, not Ghostty) and termshot draws them cell by cell. No AI op runs
during any recording: every change on screen comes from a feed through the daemon.

  feed-agents.webp     the agents monitor over synthetic sessions a simulator writes (a tool runs,
                       messages queue, a turn ends, a call waits on permission)
  feed-jj.webp         the jj feed while someone describes, edits and commits in a scratch repo
  feed-crash.webp      a flaky stream feed: crash, restart countdown with backoff, recovery
  feed-queue.webp      the queue board and turn ledger over one synthetic session: a message queued
                       mid-turn is absorbed, then answered when the turn ends

usage: python3 tests/e2e/capture_feeds.py [OUTDIR]     (inside `nix develop`, after `cargo build`)
"""
import json
import os
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "../.."))
sys.path.insert(0, HERE)
from capture import Recorder, still  # noqa: E402
import capture  # noqa: E402
from ptyhost import Host  # noqa: E402

MARGIN = os.path.join(ROOT, "target/debug/margin")
OUT = os.path.abspath(sys.argv[1]) if len(sys.argv) > 1 else os.path.join(ROOT, "docs/img/real")
capture.OUT = OUT
T = tempfile.mkdtemp(prefix="margin-capfeeds-")
ENV = dict(os.environ, MARGIN_HOME=os.path.join(T, "home"), MARGIN_PROJECT="demo", MARGIN_IMAGES="blocks",
           MARGIN_CONFIG=os.path.join(T, "cfg.json"), MARGIN_HOST="zellij", JJ_USER="me", JJ_EMAIL="me@example.com")
for k in ("ZELLIJ", "ZELLIJ_SESSION_NAME", "ZELLIJ_PANE_ID"):
    ENV.pop(k, None)
json.dump({"clipboard": "none", "opener": "true"}, open(ENV["MARGIN_CONFIG"], "w"))
COLS = 66


def m(*a, inp=None, cwd=None, env=None):
    return subprocess.run([MARGIN, *a], env=env or ENV, capture_output=True, text=True, input=inp, cwd=cwd or T)


def drain():
    """Wait for the daemon to exit, so the next feed starts a fresh daemon with this script's env."""
    end = time.time() + 6
    while time.time() < end and "not running" not in m("feed", "ls").stdout:
        time.sleep(0.2)


def view(art, rows, needle):
    h = Host([MARGIN, "view", art], cols=COLS, rows=rows, env=ENV)
    h.wait_for(needle, 8)
    time.sleep(0.4)
    return h


def iso(t):
    return time.strftime("%Y-%m-%dT%H:%M:%S", time.gmtime(t)) + ".%03dZ" % int((t % 1) * 1000)


class Sim:
    """Writes transcript lines the way Claude Code does (shapes from research.md §7), no bodies."""

    def __init__(self, root):
        self.root = root

    def path(self, proj, sid):
        d = os.path.join(self.root, proj)
        os.makedirs(d, exist_ok=True)
        return os.path.join(d, sid + ".jsonl")

    def put(self, proj, sid, cwd, entry, at=None):
        e = dict(entry, sessionId=sid, timestamp=iso(at or time.time()))
        if entry.get("type") in ("assistant", "user"):
            e["cwd"] = cwd
            e["isSidechain"] = False
        with open(self.path(proj, sid), "a") as f:
            f.write(json.dumps(e) + "\n")

    def tool(self, proj, sid, cwd, uid, name, inp, at=None):
        self.put(proj, sid, cwd, {"type": "assistant", "message": {"stop_reason": "tool_use", "content": [
            {"type": "tool_use", "id": uid, "name": name, "input": inp}]}}, at)

    def result(self, proj, sid, cwd, uid, at=None):
        self.put(proj, sid, cwd, {"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": uid}]}}, at)

    def end(self, proj, sid, cwd, at=None):
        self.put(proj, sid, cwd, {"type": "assistant", "message": {"stop_reason": "end_turn", "content": [{"type": "text"}]}}, at)

    def prompt(self, proj, sid, cwd, at=None):
        self.put(proj, sid, cwd, {"type": "user", "message": {"content": "…"}}, at)

    def queue(self, proj, sid, op, at=None):
        self.put(proj, sid, "", {"type": "queue-operation", "operation": op}, at)


def agents():
    root = os.path.join(T, "projects")
    sim = Sim(root)
    now = time.time()
    H, ST, SHOP, DOT = "-home-me-project", "-home-me-project-apps-web", "-home-me-shop", "-home-me-dotfiles"
    hc, sc, gc, nc = "/home/me/project", "/home/me/project/apps/web", "/home/me/shop", "/home/me/dotfiles"
    a, b, c, d = "a1b2c3d4-0000", "b2c3d4e5-0000", "c3d4e5f6-0000", "d4e5f6a7-0000"
    sim.prompt(H, a, hc, now - 40)
    sim.tool(H, a, hc, "u1", "Bash", {"description": "Run the feeds e2e suite", "command": "python3 …"}, now - 6)
    sim.prompt(ST, b, sc, now - 200)
    sim.tool(ST, b, sc, "v1", "Agent", {"description": "Sweep sites for the table pattern"}, now - 25)
    sim.prompt(SHOP, c, gc, now - 300)
    sim.tool(SHOP, c, gc, "w1", "Bash", {"description": "Deploy recs to staging", "command": "…"}, now - 95)
    sim.prompt(DOT, d, nc, now - 3000)
    sim.end(DOT, d, nc, now - 2400)
    art = {"blocks": [{"type": "text", "id": "intro", "style": "note",
                       "text": "Claude Code sessions on this machine, from their transcripts. A feed keeps this current; no model calls."}]}
    m("new", "agents", "--title", "agents", inp=json.dumps(art))
    env = dict(ENV, CLAUDE_PROJECTS=root, AGENTS_EVERY="0.5")
    m("feed", "add", "agents", "--stream", "--artifact", "agents", "--owns", "ag", "--", os.path.join(ROOT, "feeds/agents"), env=env)
    h = view("agents", 22, "agents ·")
    h.send("\x1b[O")
    time.sleep(1.5)
    with Recorder(h, fps=12) as r:
        time.sleep(1.5)
        sim.queue(ST, b, "enqueue")  # the user queues a message while the sweep runs
        time.sleep(2.0)
        sim.result(H, a, hc, "u1")
        sim.tool(H, a, hc, "u2", "Edit", {"file_path": "/home/me/project/src/feed.rs"})
        time.sleep(2.0)
        sim.queue(ST, b, "enqueue")
        time.sleep(1.5)
        sim.result(H, a, hc, "u2")
        sim.end(H, a, hc)  # its turn ends: your move
        time.sleep(2.0)
        sim.result(ST, b, sc, "v1")
        sim.queue(ST, b, "dequeue")
        sim.queue(ST, b, "dequeue")
        sim.tool(ST, b, sc, "v2", "Read", {"file_path": "/home/me/project/apps/web/main.ts"})
        time.sleep(2.5)
    r.save("feed-agents.webp", "margin: agents monitor (a feed, no model calls)")
    h.close()
    m("feed", "rm", "agents")
    drain()


def jj():
    repo = os.path.join(T, "shop")
    os.makedirs(repo)
    j = lambda *a: subprocess.run(["jj", *a], cwd=repo, env=ENV, capture_output=True, text=True)
    j("git", "init")
    open(os.path.join(repo, "recs.ts"), "w").write("export const floor = 0;\n")
    j("describe", "-m", "shop: recs read the market floor")
    j("new")
    open(os.path.join(repo, "prior.py"), "w").write("alpha = 1\nbeta = 1\n")
    j("describe", "-m", "shop: Beta prior per item class")
    j("new")
    m("new", "changes", "--title", "changes · shop", inp=json.dumps({"blocks": [
        {"type": "text", "id": "why", "style": "note", "text": "The jj feed rewrites the ledger below whenever jj records an operation."}]}))
    m("feed", "add", "jj", "--watch", ".jj/repo/op_heads/heads", "--artifact", "changes", "--", os.path.join(ROOT, "feeds/jj"), cwd=repo)
    h = view("changes", 24, "jj · shop")
    h.send("\x1b[O")
    time.sleep(1.0)
    with Recorder(h, fps=12) as r:
        time.sleep(1.5)
        open(os.path.join(repo, "recs.ts"), "a").write("export const undercut = (p: number) => p - 1;\n" * 3)
        j("status")  # any jj command snapshots the edit: one op, the feed reruns
        time.sleep(2.0)
        j("describe", "-m", "shop: undercut the floor by one coin")
        time.sleep(2.0)
        open(os.path.join(repo, "events.ts"), "w").write("export const events = [];\n" * 8)
        j("commit", "-m", "shop: event features for Thompson sampling")
        time.sleep(2.5)
    r.save("feed-jj.webp", "margin: the jj feed (watch .jj/repo/op_heads)")
    still(h, "feed-jj.png", "margin: the jj feed")
    h.close()
    m("feed", "rm", "jj")
    drain()


def crash():
    count = os.path.join(T, "attempts")
    flaky = os.path.join(T, "flaky")
    open(flaky, "w").write(f"""#!/usr/bin/env bash
# a stream feed that fails its first three starts, then holds
n=$(( $(cat {count} 2>/dev/null || echo 0) + 1 )); echo $n > {count}
if [ $n -le 3 ]; then
  printf '{{"type":"row","id":"fl-1","glyph":"spin","text":"connecting to the price API (attempt %s)"}}\\n' $n
  sleep 1.2; echo "connection refused (simulated)" >&2; exit 1
fi
i=0
while :; do i=$((i+1))
  printf '{{"type":"row","id":"fl-1","glyph":"done","text":"prices current: %s updates","since":"%s"}}\\n' $i "$(date -u +%FT%TZ)"
  sleep 1
done
""")
    os.chmod(flaky, 0o755)
    m("new", "prices", "--title", "prices", inp=json.dumps({"blocks": [
        {"type": "text", "id": "why", "style": "note", "text": "A flaky stream feed: the daemon restarts it with backoff (1 s, 2 s, 4 s)."}]}))
    h = view("prices", 14, "prices")
    h.send("\x1b[O")
    time.sleep(0.5)
    with Recorder(h, fps=12) as r:
        m("feed", "add", "fl", "--stream", "--artifact", "prices", "--", flaky)
        time.sleep(15.0)
    r.save("feed-crash.webp", "margin: a feed crashes and the daemon restarts it")
    h.close()
    m("feed", "rm", "fl")
    drain()


def transcripts():
    """The queue board and the turn ledger over one synthetic session, side by side in one artifact."""
    root = os.path.join(T, "projects")
    sim = Sim(root)
    P, sid, cwd = "-home-me-project", "e5f6a7b8-0000", "/home/me/project"
    now = time.time()
    sim.put(P, sid, cwd, {"type": "user", "message": {"content": "tidy the web collectors and make the sync job retry"}}, now - 400)
    sim.tool(P, sid, cwd, "x1", "Read", {"file_path": cwd + "/apps/web/collect.ts"}, now - 390)
    sim.result(P, sid, cwd, "x1", now - 389)
    sim.tool(P, sid, cwd, "x2", "Edit", {"file_path": cwd + "/apps/web/collect.ts"}, now - 300)
    sim.result(P, sid, cwd, "x2", now - 299)
    sim.end(P, sid, cwd, now - 240)
    sim.put(P, sid, cwd, {"type": "user", "message": {"content": "now the sync retry, with backoff"}}, now - 60)
    sim.tool(P, sid, cwd, "y1", "Bash", {"description": "Run the sync tests", "command": "deno test"}, now - 30)
    m("new", "session", "--title", "this session", inp=json.dumps({"blocks": []}))
    env = dict(ENV, CLAUDE_PROJECTS=root, QUEUE_EVERY="0.5", TURNS_EVERY="0.5")
    tp = sim.path(P, sid)
    m("feed", "add", "queue", "--stream", "--artifact", "session", "--owns", "q", "--", os.path.join(ROOT, "feeds/queue"), tp, env=env)
    m("feed", "add", "turns", "--stream", "--artifact", "session", "--owns", "tn", "--", os.path.join(ROOT, "feeds/turns"), tp, env=env)
    h = view("session", 26, "turns ·")
    h.send("\x1b[O")
    time.sleep(1.5)
    msg = "also keep the old collector's log format, the dashboard parses it"
    with Recorder(h, fps=12) as r:
        time.sleep(1.2)
        sim.put(P, sid, "", {"type": "queue-operation", "operation": "enqueue", "content": msg})  # he types mid-turn
        time.sleep(2.0)
        sim.result(P, sid, cwd, "y1")
        sim.put(P, sid, "", {"type": "queue-operation", "operation": "remove", "content": msg})
        sim.put(P, sid, cwd, {"type": "attachment", "attachment": {"type": "queued_command", "prompt": msg, "origin": {"kind": "human"}}})
        time.sleep(2.0)
        sim.tool(P, sid, cwd, "y2", "Edit", {"file_path": cwd + "/apps/web/collect.ts"})
        time.sleep(1.0)
        sim.result(P, sid, cwd, "y2")
        sim.end(P, sid, cwd)  # the turn that took it in ends: answered
        time.sleep(2.5)
    r.save("feed-queue.webp", "margin: the queue board and turn ledger (feeds over this session's transcript)")
    still(h, "feed-queue.png", "margin: the queue board and turn ledger")
    h.close()
    for n in ("queue", "turns"):
        m("feed", "rm", n)
    drain()


if __name__ == "__main__":
    which = sys.argv[2:] or ["agents", "jj", "crash", "transcripts"]
    for w in which:
        globals()[w]()
    print("out:", OUT)
