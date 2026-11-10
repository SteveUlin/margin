"""End-to-end: `margin propose` in a throwaway headless Zellij session.

Checks: Claude's proposal shows under its component (what, why, accept/decline tokens); a refused
proposal (an option that doesn't exist) changes nothing; sulin's accept (the double-click route,
`margin select`) applies the decision as his and closes the proposal; a decline leaves his state
alone; both reach Claude through the prompt hook; a newer proposal on the same component shows again;
a click on the accept button in a plain-host view (mouse on) applies it.

usage: python3 tests/e2e/propose.py      (inside `nix develop`, after `cargo build`)
Never touches sulin's sessions or config: its own session name, --config, MARGIN_HOME.
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "../.."))
sys.path.insert(0, HERE)
from ptyhost import Host  # noqa: E402

MARGIN = os.environ.get("MARGIN_BIN", os.path.join(ROOT, "target/debug/margin"))
T = tempfile.mkdtemp(prefix="margin-propose-")
S = f"margin-prop-{os.getpid()}"
ENV = dict(os.environ)
ENV.update({"MARGIN_HOME": os.path.join(T, "home"), "MARGIN_PROJECT": "e2e", "MARGIN_CONFIG": os.path.join(T, "margin.json"),
            "PATH": os.path.dirname(MARGIN) + ":" + os.environ["PATH"]})
for k in ("ZELLIJ", "ZELLIJ_SESSION_NAME", "ZELLIJ_PANE_ID", "WAYLAND_DISPLAY", "MARGIN_HOST", "CLAUDE_CODE_SESSION_ID"):
    ENV.pop(k, None)
results = []


def check(name, ok, detail=""):
    results.append((name, bool(ok), detail))
    print(("PASS " if ok else "FAIL ") + name + (f"  ({detail})" if detail else ""), flush=True)
    return ok


def sh(*args, env=None, inp=None):
    return subprocess.run(args, capture_output=True, text=True, env=env or ENV, input=inp, cwd=T)


def m(*args, env=None, inp=None):
    return sh(MARGIN, *args, env=env, inp=inp)


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


def run():
    tabs = wait(lambda: zj("list-tabs", "-j"), 10)
    if not check("zellij session up", tabs):
        return
    chat = wait(lambda: next((p for p in zj("list-panes", "-a", "-j") or [] if not p["is_plugin"]), None), 8)
    cenv = dict(ENV, ZELLIJ_SESSION_NAME=S, ZELLIJ_PANE_ID=str(chat["id"]))
    m("put", "plan", "--title", "propose e2e", inp=json.dumps({"blocks": [
        {"type": "question", "id": "Q1", "text": "Ship as CLI or MCP?", "options": ["cli", "mcp"], "default": "cli"},
        {"type": "todo", "id": "t1", "text": "Write the changelog"}]}))
    m("open", "plan", env=cenv)
    mp = wait(lambda: next((p for p in zj("list-panes", "-a", "-j") or [] if p["title"] == "margin:plan"), None), 5)

    def pane():
        return z("dump-screen", "-p", f"terminal_{mp['id']}").stdout

    r = m("propose", "plan", "Q1", "pick", "nope")
    check("a proposal that can't apply is refused", r.returncode != 0 and "can't propose" in r.stderr, r.stderr.strip())
    r = m("propose", "plan", "Q1", "pick", "mcp", "--why", "the bug agent calls it")
    check("propose writes a revision", r.returncode == 0 and "rev" in r.stdout, (r.stdout + r.stderr).strip())
    m("propose", "plan", "t1", "tick", "skipped", "--why", "nothing user-facing changed")
    check("pane shows both proposals with accept/decline", wait(lambda: pane().count("proposes") == 2 and pane().count(" accept ") == 2 and "the bug agent calls it" in pane(), 6), pane())
    r = m("select", "mg:accept/plan.Q1", env=cenv)
    check("accept applies", r.returncode == 0, (r.stdout + r.stderr).strip())
    r = m("select", "mg:decline/plan.t1", env=cenv)
    check("decline applies", r.returncode == 0, (r.stdout + r.stderr).strip())
    check("proposals leave the pane once answered", wait(lambda: "proposes" not in pane(), 6), pane()[:600])
    show = m("show", "plan").stdout
    check("accepted: the pick is sulin's; declined: the todo is untouched", "[sulin picked mcp]" in show and "todo t1 [todo]" in show and "[accepted by sulin]" in show and "[declined by sulin]" in show, show)
    hook = m("hook", "prompt", inp=json.dumps({"session_id": "p1", "cwd": T})).stdout
    check("the hook hands Claude both answers", "accepted your proposal to pick mcp" in hook and "declined your proposal to tick skipped" in hook and "sulin picked mcp" in hook, hook)
    time.sleep(1.1)  # a newer `at`
    m("propose", "plan", "t1", "tick", "done", "--why", "it shipped")
    check("a newer proposal on the same component shows again", wait(lambda: "it shipped" in pane(), 6))
    n = len(m("events").stdout.splitlines())
    m("select", "mg:accept/plan.Q1", env=cenv)
    check("accepting an answered proposal records nothing", len(m("events").stdout.splitlines()) == n, m("events").stdout[-300:])
    with open(os.path.join(T, "..", f"propose-pane-{os.getpid()}.txt"), "w") as f:
        f.write(pane())
    m("close", "plan", env=cenv)

    # the mouse route: in a plain-host view (mouse on), a click on the accept button applies it
    h = Host([MARGIN, "view", "plan"], cols=80, rows=20, env=dict(ENV, MARGIN_HOST="plain"))
    try:
        pos = wait(lambda: h.find(" accept "), 6)
        if check("plain view shows the accept button", pos, h.text()[:400]):
            h.send(b"\x1b[<0;%d;%dM\x1b[<0;%d;%dm" % (pos[0] + 3, pos[1] + 1, pos[0] + 3, pos[1] + 1))
            st = os.path.join(ENV["MARGIN_HOME"], "e2e", "plan", "state.json")
            ok = wait(lambda: json.load(open(st))["decisions"].get("t1", {}).get("value") == "done", 4)
            check("a click on accept ticks t1 as sulin's", ok, json.dumps(json.load(open(st))["decisions"].get("t1")))
            check("and the proposal leaves the view", wait(lambda: "proposes" not in h.text(), 4), h.text()[:400])
    finally:
        h.close()


def main():
    with open(os.path.join(T, "config.kdl"), "w") as f:
        f.write("show_startup_tips false\nshow_release_notes false\npane_frames true\n")
    with open(os.path.join(T, "layout.kdl"), "w") as f:
        f.write('layout {\n  tab name="work" {\n    pane\n  }\n}\n')
    with open(ENV["MARGIN_CONFIG"], "w") as f:
        json.dump({"clipboard": "none", "opener": "true", "motion": "off"}, f)
    argv = ["zellij", "--config", os.path.join(T, "config.kdl"), "--config-dir", T, "-n", os.path.join(T, "layout.kdl"), "-s", S]
    h = Host(argv, cols=150, rows=40, env=ENV)
    try:
        run()
    finally:
        h.close()
        sh("zellij", "kill-session", S)
        sh("zellij", "delete-session", S)
        shutil.rmtree(T, ignore_errors=True)
    bad = [r for r in results if not r[1]]
    print(f"\n{len(results) - len(bad)}/{len(results)} checks passed")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
