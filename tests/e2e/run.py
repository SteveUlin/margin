"""End-to-end: margin in a real, throwaway, headless Zellij session with a fake Ghostty client.

Checks, against Zellij 0.45.1:
  1. `margin open` from Claude's pane puts a pinned float in Claude's tab while sulin sits on
     another tab: the active tab doesn't move, and the margin pane doesn't take focus.
  2. Back on the tab, typed text still reaches the chat pane (no focus steal).
  3. The kitty probe answered by the fake client makes Zellij re-emit margin's image to the host.
  4. sulin folds a section in the pane; an AI update lands; the fold survives and the update shows.
  5. Double-clicking `mg:pick/Q1=mcp` on screen runs Zellij's copy_command (`margin select`), which
     writes state.json and events.jsonl; double-clicking a plain word goes to the clipboard (OSC 52).
  6. The prompt hook hands the pick to Claude; the card resolves into a decided row.
Screens are saved as PNGs (termshot) in the output dir.

usage: python3 tests/e2e/run.py [OUTDIR]     (run inside `nix develop`)
Never touches sulin's sessions or config: its own session name, --config, --config-dir, MARGIN_HOME.
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

MARGIN = os.path.join(ROOT, "target/debug/margin")
OUT = os.path.abspath(sys.argv[1]) if len(sys.argv) > 1 else os.path.join(ROOT, "target/e2e")
os.makedirs(OUT, exist_ok=True)
T = tempfile.mkdtemp(prefix="margin-e2e-")
S = f"margin-e2e-{os.getpid()}"
ENV = dict(os.environ)
ENV.update({
    "MARGIN_HOME": os.path.join(T, "home"),
    "MARGIN_PROJECT": "e2e",
    "MARGIN_CONFIG": os.path.join(T, "margin.json"),
    "PATH": os.path.dirname(MARGIN) + ":" + os.environ["PATH"],
})
ENV.pop("ZELLIJ", None)
ENV.pop("ZELLIJ_SESSION_NAME", None)
ENV.pop("ZELLIJ_PANE_ID", None)
ENV.pop("WAYLAND_DISPLAY", None)  # force the OSC 52 clipboard path

results = []


def check(name, ok, detail=""):
    results.append((name, bool(ok), detail))
    print(("PASS " if ok else "FAIL ") + name + (f"  ({detail})" if detail else ""), flush=True)
    return ok


def sh(*args, env=None, inp=None):
    r = subprocess.run(args, capture_output=True, text=True, env=env or ENV, input=inp)
    return r


def z(*args):
    return sh("zellij", "-s", S, "action", *args)


def zj(*args):
    r = z(*args)
    try:
        return json.loads(r.stdout)
    except Exception:
        return None


def m(*args, env=None, inp=None):
    return sh(MARGIN, *args, env=env, inp=inp)


def shot(h, name, title="Zellij · margin e2e"):
    cv = h.canvas()
    cv.title = title
    path = os.path.join(OUT, name)
    cv.png(path, scale=0.5)
    return path


def wait(pred, timeout=8.0, step=0.1):
    end = time.time() + timeout
    while time.time() < end:
        v = pred()
        if v:
            return v
        time.sleep(step)
    return None


def main():
    with open(os.path.join(T, "config.kdl"), "w") as f:
        f.write("show_startup_tips false\nshow_release_notes false\npane_frames true\n")
    with open(os.path.join(T, "layout.kdl"), "w") as f:
        f.write('layout {\n  tab name="work" {\n    pane\n  }\n  tab name="other" {\n    pane\n  }\n}\n')
    with open(ENV["MARGIN_CONFIG"], "w") as f:
        json.dump({"clipboard": "osc52", "opener": "true"}, f)
    # the same wiring `margin session` uses: copy_command set for this session only
    argv = ["zellij", "--config", os.path.join(T, "config.kdl"), "--config-dir", T, "-n", os.path.join(T, "layout.kdl"),
            "-s", S, "options", "--copy-command", f"{MARGIN} select", "--copy-on-select", "true", "--mouse-click-through", "true"]
    h = Host(argv, cols=150, rows=46, env=ENV, record=os.path.join(OUT, "client.stream"))
    try:
        run(h)
    finally:
        h.close()
        sh("zellij", "kill-session", S)
        sh("zellij", "delete-session", S)
        shutil.rmtree(T, ignore_errors=True)
    bad = [r for r in results if not r[1]]
    print(f"\n{len(results) - len(bad)}/{len(results)} checks passed; screens in {OUT}")
    sys.exit(1 if bad else 0)


def run(h):
    tabs = wait(lambda: zj("list-tabs", "-j"), 10)
    if not check("zellij session up", tabs):
        print(h.text())
        return
    work = next(t for t in tabs if t["name"] == "work")["tab_id"]
    # tabs can list before their panes do: wait for the chat pane
    chat = wait(lambda: next((p for p in zj("list-panes", "-a", "-j") or [] if not p["is_plugin"] and p["tab_id"] == work), None), 8)
    r = m("demo")
    check("demo artifact created", r.returncode == 0, r.stderr.strip())

    # 1. open from "Claude's pane" while sulin is on another tab
    z("go-to-tab-name", "other")
    time.sleep(0.4)
    claude_env = dict(ENV, ZELLIJ_SESSION_NAME=S, ZELLIJ_PANE_ID=str(chat["id"]))
    r = m("open", "plan", "--float", env=claude_env)
    check("margin open --float", r.returncode == 0 and "opened" in r.stdout, (r.stdout + r.stderr).strip())
    time.sleep(1.0)
    tabs = zj("list-tabs", "-j")
    active = next(t for t in tabs if t["active"])["name"]
    check("active tab unchanged (still 'other')", active == "other", active)
    panes = zj("list-panes", "-a", "-j")
    mp = next((p for p in panes if p["title"] == "margin:plan"), None)
    check("margin pane in Claude's tab, pinned float", mp and mp["tab_id"] == work and mp["is_floating"], str(mp and (mp["tab_id"], mp["is_floating"])))
    chat_now = next(p for p in panes if p["id"] == chat["id"] and not p["is_plugin"])
    check("chat pane keeps focus in its tab", chat_now["is_focused"] and not mp["is_focused"])
    r = m("open", "plan", "--float", env=claude_env)
    check("second open reuses the pane", "reused" in r.stdout, r.stdout.strip())

    # 2. back on the tab: the float shows, keystrokes reach the chat
    z("go-to-tab-name", "work")
    check("pane renders the plan", h.wait_for("Steps", 6))
    time.sleep(0.6)
    h.send("echo typed-into-chat\r")
    time.sleep(0.6)
    dump = z("dump-screen", "-p", f"terminal_{chat['id']}").stdout
    check("typing lands in the chat pane", "typed-into-chat" in dump)
    shot(h, "01-open-float.png")

    # the working surface: a split (one-gesture double-click with mouse_click_through)
    m("close", "plan", env=claude_env)
    time.sleep(0.4)
    r = m("open", "plan", env=claude_env)
    check("margin open (default split)", "opened" in r.stdout, (r.stdout + r.stderr).strip())
    time.sleep(1.2)
    panes = zj("list-panes", "-a", "-j")
    mp = next((p for p in panes if p["title"] == "margin:plan"), None)
    chat_now = next(p for p in panes if p["id"] == chat["id"] and not p["is_plugin"])
    check("split opened unfocused", mp and not mp["is_floating"] and chat_now["is_focused"] and not mp["is_focused"])

    # 4. fold a section from inside the pane, then an AI update arrives
    z("move-focus", "right")
    time.sleep(0.5)
    h.send("\r")  # focus starts on the first section (Steps): fold it
    time.sleep(0.6)
    vdir = os.path.join(ENV["MARGIN_HOME"], "e2e", "plan", "view")
    def folded():
        for fn in os.listdir(vdir) if os.path.isdir(vdir) else []:
            v = json.load(open(os.path.join(vdir, fn)))
            if v.get("folds", {}).get("steps"):
                return fn
        return None
    vf = wait(folded, 4)
    check("fold recorded in view.json", vf, vf or "")
    z("move-focus", "left")
    time.sleep(0.3)
    r = m("add", "plan", "steps", "todo", "t9", "Added by Claude while folded")
    r2 = m("set", "plan", "c1", "result", "18/20")
    check("AI update applied", r.returncode == 0 and r2.returncode == 0, (r.stderr + r2.stderr).strip())
    time.sleep(1.0)
    mdump = z("dump-screen", "-p", f"terminal_{mp['id']}").stdout
    check("update shows (18/20)", "18/20" in mdump)
    check("fold survives the update (steps hidden)", "Reproduce the flaky test" not in mdump and "Added by Claude" not in mdump)
    check("view.json still folded after update", folded())
    shot(h, "02-folded-after-update.png")

    # 3. kitty images: unfold the explainer so the image is on screen, check Zellij re-emits it
    n_before = len(h.kitty_seen)
    m("put", "pic", "--title", "graph", inp=json.dumps({"blocks": [{"type": "image", "id": "g", "rows": 10,
        "path": os.path.join(ROOT, "assets/demo/graph.png"), "alt": "matcher pipeline"}]}))
    m("open", "pic", "--float", env=claude_env)
    time.sleep(2.0)
    got = [k for k in h.kitty_seen[n_before:] if b"a=q" not in k]
    check("kitty image reaches the host through Zellij", got, str(got[:2])[:160])
    shot(h, "02b-image-float.png")
    m("close", "pic", env=claude_env)
    time.sleep(0.6)

    # 5. double-click a token on screen
    pos = wait(lambda: h.find("mg:pick/Q1=mcp"), 5)
    if not pos:
        m("set", "plan", "checks", "folded", "true")
        pos = wait(lambda: h.find("mg:pick/Q1=mcp"), 4)
    check("token visible on screen", pos, str(pos))
    if pos:
        x, y = pos
        h.double_click(x + 8, y)
        st = os.path.join(ENV["MARGIN_HOME"], "e2e", "plan", "state.json")
        ok = wait(lambda: os.path.exists(st) and json.load(open(st))["decisions"].get("Q1", {}).get("value") == "mcp", 5)
        check("double-click → state.json Q1 = mcp", ok)
        evs = [json.loads(l) for l in open(os.path.join(ENV["MARGIN_HOME"], "e2e", "events.jsonl"))]
        pick = [e for e in evs if e["kind"] == "pick" and e["id"] == "Q1"]
        check("double-click → events.jsonl pick via select", pick and pick[-1]["via"] == "select", str(pick[-1:]))
        time.sleep(0.15)
        shot(h, "03-picked-flash.png")
        time.sleep(0.8)
        shot(h, "04-picked.png")
    pw = h.find("CI runs tests")
    if pw:
        h.osc52.clear()
        z("move-focus", "right"); time.sleep(0.2)
        h.double_click(pw[0] + 3 + 3, pw[1])  # the word "runs"
        got = wait(lambda: h.osc52, 4)
        import base64
        check("plain word → clipboard via OSC 52", got and base64.b64decode(got[-1]) in (b"runs", b"tests"), str(got[-1:]))

    # 6. the prompt hook hands the pick to Claude, and the card resolves
    r = m("hook", "prompt", inp=json.dumps({"session_id": "e2e-session", "cwd": ROOT}))
    check("prompt hook reports the pick", "picked mcp" in r.stdout, r.stdout.strip()[:200])
    r = m("hook", "prompt", inp=json.dumps({"session_id": "e2e-session", "cwd": ROOT}))
    check("hook is silent once delivered", r.stdout.strip() == "", r.stdout.strip()[:80])
    time.sleep(1.2)
    mdump = z("dump-screen", "-p", f"terminal_{mp['id']}").stdout
    check("card resolved into a decided row", "→ mcp" in mdump)
    shot(h, "05-resolved.png")
    r = m("close", "plan", env=claude_env)
    check("margin close", "closed 1" in r.stdout, r.stdout.strip())


if __name__ == "__main__":
    main()
