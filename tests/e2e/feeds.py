"""End-to-end: feeds and the host split, in a throwaway headless Zellij session.

Checks:
  1. A registered stream feed updates an open pane live, with no AI op: the doc's revisions are
     all `by: feed:tick` after Claude's first put, and the pane's text changes on its own.
  2. Ownership: Claude's write inside the feed's prefix is refused with a clear error; Claude's write
     elsewhere in the same artifact still lands.
  3. Hooks stay fast under load: with a busy feed writing 20 times a second, `margin hook prompt`
     and `margin hook post-tool` are timed over 40 calls each (each call a new session, so each one
     reads and describes every unread event: the slow path), and they still hand over the pick.
  4. Crash and restart: a feed that exits after 2 s shows "crashed · restart in", restarts, and
     `margin feed ls` counts the restarts.
  5. `margin feed rm` stops the feed: its process is gone, the pane stops changing and says
     "removed", and the daemon exits once no feed is left.
  6. Plain host: `margin open` prints the `margin view` command; the view starts with mouse on, a
     click on a token applies it, and `margin close` ends the view through SIGTERM.

usage: python3 tests/e2e/feeds.py [OUTDIR]      (inside `nix develop`, after `cargo build`)
Never touches sulin's sessions or config: its own session name, --config, MARGIN_HOME.
"""
import json
import os
import shutil
import statistics
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "../.."))
sys.path.insert(0, HERE)
from ptyhost import Host  # noqa: E402

MARGIN = os.environ.get("MARGIN_BIN", os.path.join(ROOT, "target/debug/margin"))
OUT = os.path.abspath(sys.argv[1]) if len(sys.argv) > 1 else os.path.join(ROOT, "target/e2e-feeds")
os.makedirs(OUT, exist_ok=True)
T = tempfile.mkdtemp(prefix="margin-feeds-")
S = f"margin-feeds-{os.getpid()}"
HOME = os.path.join(T, "home")
ENV = dict(os.environ)
ENV.update({
    "MARGIN_HOME": HOME,
    "MARGIN_PROJECT": "e2e",
    "MARGIN_CONFIG": os.path.join(T, "margin.json"),
    "PATH": os.path.dirname(MARGIN) + ":" + os.environ["PATH"],
})
for k in ("ZELLIJ", "ZELLIJ_SESSION_NAME", "ZELLIJ_PANE_ID", "WAYLAND_DISPLAY", "MARGIN_HOST"):
    ENV.pop(k, None)
PROJ = os.path.join(HOME, "e2e")
results = []
timings = {}


def check(name, ok, detail=""):
    results.append((name, bool(ok), detail))
    print(("PASS " if ok else "FAIL ") + name + (f"  ({detail})" if detail else ""), flush=True)
    return ok


def sh(*args, env=None, inp=None, cwd=None):
    return subprocess.run(args, capture_output=True, text=True, env=env or ENV, input=inp, cwd=cwd or T)


def m(*args, env=None, inp=None):
    return sh(MARGIN, *args, env=env, inp=inp)


def z(*args):
    return sh("zellij", "-s", S, "action", *args)


def zj(*args):
    try:
        return json.loads(z(*args).stdout)
    except Exception:
        return None


def wait(pred, timeout=8.0, step=0.1):
    end = time.time() + timeout
    while time.time() < end:
        v = pred()
        if v:
            return v
        time.sleep(step)
    return None


def script(name, body):
    p = os.path.join(T, name)
    with open(p, "w") as f:
        f.write("#!/usr/bin/env bash\n" + body)
    os.chmod(p, 0o755)
    return p


def doc():
    return json.load(open(os.path.join(PROJ, "live", "doc.json")))


def pids_of(needle):
    r = subprocess.run(["pgrep", "-f", needle], capture_output=True, text=True)
    return [p for p in r.stdout.split() if p.strip()]


def main():
    with open(os.path.join(T, "config.kdl"), "w") as f:
        f.write("show_startup_tips false\nshow_release_notes false\npane_frames true\n")
    with open(os.path.join(T, "layout.kdl"), "w") as f:
        f.write('layout {\n  tab name="work" {\n    pane\n  }\n}\n')
    with open(ENV["MARGIN_CONFIG"], "w") as f:
        json.dump({"clipboard": "none", "opener": "true", "motion": "off"}, f)
    argv = ["zellij", "--config", os.path.join(T, "config.kdl"), "--config-dir", T, "-n", os.path.join(T, "layout.kdl"),
            "-s", S, "options", "--copy-command", f"{MARGIN} select", "--mouse-click-through", "true"]
    h = Host(argv, cols=150, rows=40, env=ENV)
    try:
        run(h)
        plain()
    finally:
        h.close()
        for f in json.load(open(os.path.join(PROJ, "feeds.json"))).get("feeds", []) if os.path.exists(os.path.join(PROJ, "feeds.json")) else []:
            m("feed", "rm", f["name"])
        time.sleep(0.5)
        sh("zellij", "kill-session", S)
        sh("zellij", "delete-session", S)
        shutil.rmtree(T, ignore_errors=True)
    bad = [r for r in results if not r[1]]
    print(f"\n{len(results) - len(bad)}/{len(results)} checks passed")
    if timings:
        print("hook timings (ms): " + json.dumps(timings))
    sys.exit(1 if bad else 0)


def run(h):
    tabs = wait(lambda: zj("list-tabs", "-j"), 10)
    if not check("zellij session up", tabs):
        return
    work = tabs[0]["tab_id"]
    chat = next(p for p in zj("list-panes", "-a", "-j") if not p["is_plugin"] and p["tab_id"] == work)
    claude_env = dict(ENV, ZELLIJ_SESSION_NAME=S, ZELLIJ_PANE_ID=str(chat["id"]))

    # Claude's part of the artifact: a question sulin will answer, and a note
    m("put", "live", "--title", "feeds e2e", inp=json.dumps({"blocks": [
        {"type": "text", "id": "intro", "text": "Claude wrote this line; feeds own the rest."},
        {"type": "question", "id": "Q1", "text": "Keep going?", "options": ["yes", "no"], "default": "yes"},
    ]}))
    r = m("open", "live", env=claude_env)
    check("open through ZellijHost", "opened" in r.stdout, (r.stdout + r.stderr).strip())
    mp = wait(lambda: next((p for p in zj("list-panes", "-a", "-j") or [] if p["title"] == "margin:live"), None), 5)

    def pane():
        return z("dump-screen", "-p", f"terminal_{mp['id']}").stdout

    # 1. a stream feed updates the pane with no AI op
    tick = script("tick.sh", 'i=0\nwhile :; do i=$((i+1)); printf \'{"type":"row","id":"tick-1","glyph":"done","text":"count %s"}\\n\' "$i"; sleep 0.4; done\n')
    r = m("feed", "add", "tick", "--stream", "--artifact", "live", "--", tick)
    check("feed add starts the daemon", r.returncode == 0 and "daemon started" in r.stdout, (r.stdout + r.stderr).strip())
    seen = set()
    def counts():
        for line in pane().splitlines():
            if "count " in line:
                seen.add(line.split("count ")[1].split()[0])
        return len(seen) >= 3
    check("pane updates live from the feed (3+ distinct counts)", wait(counts, 8), str(sorted(seen, key=int)[:6]))
    d = doc()
    hist = [json.loads(l) for l in open(os.path.join(PROJ, "live", "history.jsonl"))]
    bys = {x.get("by", "") for x in hist[1:]} | {d["by"]}
    check("every revision after Claude's put is by feed:tick", bys == {"feed:tick"}, str(bys))
    check("feed health line in the pane", "feed tick · live" in pane())

    # 2. ownership
    r = m("set", "live", "tick-1", "text", "mine now")
    check("Claude's write in the feed's prefix is refused", r.returncode != 0 and "belongs to feed `tick`" in r.stderr, r.stderr.strip()[:120])
    r = m("set", "live", "intro", "text", "Claude can still write its own lines.")
    check("Claude's write outside the prefix lands", r.returncode == 0, r.stderr.strip())

    # 3. hooks under a busy feed: 20 writes a second
    busy = script("busy.sh", 'i=0\nwhile :; do i=$((i+1)); printf \'{"type":"row","id":"busy-1","text":"n %s"}\\n\' "$i"; done\n')
    m("feed", "add", "busy", "--stream", "--artifact", "live", "--min-gap", "0.05", "--", busy)
    m("select", "mg:pick/live.Q1=no", env=claude_env)
    wait(lambda: doc()["rev"] > d["rev"] + 20, 5)
    for kind in ("prompt", "post-tool"):
        ts = []
        outs = []
        for i in range(40):
            t0 = time.perf_counter()
            r = m("hook", kind, inp=json.dumps({"session_id": f"t-{kind}-{i}", "cwd": T}))
            ts.append((time.perf_counter() - t0) * 1000)
            outs.append(r.stdout)
        ts.sort()
        timings[kind] = {"median": round(statistics.median(ts), 1), "p95": round(ts[int(len(ts) * 0.95) - 1], 1), "max": round(ts[-1], 1)}
        check(f"hook {kind} under a busy feed: max {ts[-1]:.0f} ms < 100 ms", ts[-1] < 100, json.dumps(timings[kind]))
        check(f"hook {kind} still hands over the pick", all("picked no" in o for o in outs), outs[0][:120])
    revs0 = doc()["rev"]
    time.sleep(1.0)
    check("busy feed really was busy (10+ writes in 1 s)", doc()["rev"] - revs0 >= 10, f"{doc()['rev'] - revs0} revs")
    m("feed", "rm", "busy")

    # 4. crash and restart
    crash = script("crash.sh", 'printf \'{"type":"row","id":"cr-1","glyph":"spin","text":"up since %s"}\\n\' "$(date +%T)"\nsleep 2\necho "simulated crash" >&2\nexit 3\n')
    m("feed", "add", "cr", "--stream", "--artifact", "live", "--", crash)
    check("crashed feed shows a restart countdown", wait(lambda: "restart in" in pane(), 8))
    time.sleep(0.3)
    z("dump-screen", "-p", f"terminal_{mp['id']}")
    ls = wait(lambda: (lambda o: o if "restarts 1" in o or "restarts 2" in o else None)(m("feed", "ls").stdout), 8)
    check("feed ls counts restarts", ls, (ls or "").strip()[:200])
    log = m("feed", "logs", "cr").stdout
    check("feed log has stderr and the exit", "simulated crash" in log and "exit 3" in log, log.strip()[-160:])
    m("feed", "rm", "cr")

    # 5. rm stops the feed
    r = m("feed", "rm", "tick")
    check("feed rm", r.returncode == 0, r.stderr.strip())
    check("feed process gone", wait(lambda: not pids_of(tick), 4), str(pids_of(tick)))
    check("pane says removed", wait(lambda: "feed tick · removed" in pane(), 4))
    def count_line():
        return [l.split("count ")[1].split()[0] for l in pane().splitlines() if "count " in l]
    a = count_line()
    time.sleep(1.5)
    # the health line's "wrote 2s" age keeps ticking in the renderer; the feed's data must not
    check("feed data stops changing after rm", a and count_line() == a, f"{a} -> {count_line()}")
    check("daemon exits with no feeds left", wait(lambda: "not running" in m("feed", "ls").stdout, 6), m("feed", "ls").stdout.strip()[:80])
    m("close", "live", env=claude_env)


def plain():
    env = dict(ENV, MARGIN_HOST="plain")
    r = m("open", "live", env=env)
    check("plain host: open prints the view command", "margin view live" in r.stdout, r.stdout.strip())
    m("put", "pq", "--title", "plain", env=env, inp=json.dumps({"blocks": [
        {"type": "question", "id": "Q2", "text": "Plain host works?", "options": ["yes", "no"], "default": "no"}]}))
    h = Host([MARGIN, "view", "pq"], cols=80, rows=20, env=env)
    try:
        check("plain view renders", h.wait_for("Plain host works", 6))
        pos = wait(lambda: h.find("mg:pick/Q2=yes"), 4)
        if check("token on screen", pos, str(pos)):
            h.send(b"\x1b[<0;%d;%dM\x1b[<0;%d;%dm" % (pos[0] + 4, pos[1] + 1, pos[0] + 4, pos[1] + 1))
            st = os.path.join(PROJ, "pq", "state.json")
            ok = wait(lambda: os.path.exists(st) and json.load(open(st))["decisions"].get("Q2", {}).get("value") == "yes", 4)
            check("plain host: a click on a token applies it (mouse on by default)", ok)
        r = m("panes", "pq", env=env)
        check("plain host: panes finds the view process", r.stdout.startswith("pid "), r.stdout.strip())
        r = m("close", "pq", env=env)
        check("plain host: close ends the view", "closed 1" in r.stdout, r.stdout.strip())
        gone = wait(lambda: os.waitpid(h.pid, os.WNOHANG)[0] != 0, 4)
        check("view exited on SIGTERM", gone)
        h.alive = False
    finally:
        try:
            h.close()
        except Exception:
            pass


if __name__ == "__main__":
    main()
