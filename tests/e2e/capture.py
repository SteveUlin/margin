"""capture: record the running margin UI (`margin view` in a pty) as animated WebPs and PNG stills.

These are real renders: the bytes margin wrote to its terminal, replayed through pyte and drawn
cell by cell by termshot (JetBrainsMono Nerd Font, Ghostty's geometry for box and block glyphs).
Images use the halfblock fallback here because pyte can't show kitty graphics.

usage: python3 tests/e2e/capture.py OUTDIR      (inside `nix develop`, after `cargo build`)
"""
import json
import os
import subprocess
import sys
import tempfile
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "../.."))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(ROOT, "docs/tools"))
from ptyhost import Host  # noqa: E402
from termshot import animate  # noqa: E402

MARGIN = os.path.join(ROOT, "target/debug/margin")
OUT = os.path.abspath(sys.argv[1]) if len(sys.argv) > 1 else os.path.join(ROOT, "docs/img/real")
os.makedirs(OUT, exist_ok=True)
T = tempfile.mkdtemp(prefix="margin-cap-")
ENV = dict(os.environ, MARGIN_HOME=os.path.join(T, "home"), MARGIN_PROJECT="demo", MARGIN_IMAGES="blocks",
           MARGIN_CONFIG=os.path.join(T, "cfg.json"))
for k in ("ZELLIJ", "ZELLIJ_SESSION_NAME", "ZELLIJ_PANE_ID"):
    ENV.pop(k, None)
json.dump({"clipboard": "none", "opener": "true"}, open(ENV["MARGIN_CONFIG"], "w"))
COLS, ROWS = 64, 46
SCALE = 0.5


def m(*a, inp=None):
    return subprocess.run([MARGIN, *a], env=ENV, capture_output=True, text=True, input=inp)


class Recorder:
    """Samples the screen at `fps` while running; identical frames merge into one longer frame."""

    def __init__(self, h, fps=15):
        self.h, self.dt = h, 1.0 / fps
        self.frames = []  # [canvas, ms]
        self.stop = False
        self.last = None

    def run(self):
        while not self.stop:
            t0 = time.time()
            with self.h.lock:
                key = (tuple(self.h.screen.display), str([[(c.fg, c.bg) for c in row.values()] for row in list(self.h.screen.buffer.values())[:ROWS]]))
            if key == self.last and self.frames:
                self.frames[-1][1] += int(self.dt * 1000)
            else:
                self.frames.append([self.h.canvas(), int(self.dt * 1000)])
                self.last = key
            time.sleep(max(0.0, self.dt - (time.time() - t0)))

    def __enter__(self):
        self.t = threading.Thread(target=self.run, daemon=True)
        self.t.start()
        return self

    def __exit__(self, *a):
        self.stop = True
        self.t.join()

    def save(self, name, title):
        for f, _ in self.frames:
            f.title = title
        animate([(f, ms) for f, ms in self.frames], os.path.join(OUT, name), scale=SCALE)
        print(f"{name}: {len(self.frames)} frames", flush=True)


def still(h, name, title):
    cv = h.canvas()
    cv.title = title
    cv.png(os.path.join(OUT, name), scale=SCALE)


def viewer():
    h = Host([MARGIN, "view", "plan"], cols=COLS, rows=ROWS, env=ENV)
    h.wait_for("Steps", 5)  # a section of `margin demo`, whatever its title
    time.sleep(0.5)
    return h


def main():
    m("demo")
    # fold the long parts so the turn fits one phone-width pane
    for sec in ("changed", "decided"):
        m("set", "plan", sec, "folded", "true")

    # 1. a turn with no model calls: checks climb, a step completes and progress springs, a step
    #    slides in, a reorder glides, a default applies and its card resolves; bars fade once seen
    h = viewer()
    h.send("\x1b[I")  # focus in: the ring flows
    still(h, "plan.png", "margin: plan")
    with Recorder(h, fps=15) as r:
        live = subprocess.Popen([MARGIN, "demo", "live", "--speed", "1.3"], env=ENV, stderr=subprocess.DEVNULL)
        live.wait()
        time.sleep(3.0)
    r.save("live.webp", "margin: a turn, live")
    still(h, "plan-after.png", "margin: plan after the turn")
    h.close()

    # 2. the user picks: the token flashes, the card says sent; the hook hands it to Claude and the card
    #    springs shut into a decided row. Then he strikes an assumption; Claude acknowledges it.
    m("demo")
    for sec in ("changed", "decided", "checks"):
        m("set", "plan", sec, "folded", "true")
    h = viewer()
    h.send("\x1b[O")  # unfocused: the ring rests, the card pulse is the only motion
    time.sleep(0.3)
    with Recorder(h, fps=20) as r:
        time.sleep(1.5)
        m("select", "mg:pick/Q1=mcp")
        time.sleep(2.2)
        m("hook", "prompt", inp=json.dumps({"session_id": "capture", "cwd": ROOT}))
        time.sleep(2.0)
        m("select", "mg:strike/A2=the-laptop-runs-it")
        time.sleep(1.8)
        m("set", "plan", "A2", "ack", "will probe where Zellij runs before relying on it")
        time.sleep(3.0)
    r.save("pick.webp", "margin: pick, resolve, strike")
    still(h, "decisions.png", "margin: decisions")
    h.close()

    # 3. focus: the gradient ring flows around the focused panel; j/k move the caret; ⏎ folds and
    #    unfolds with an eased height; a new question arrives and breathes
    m("demo")
    for sec in ("changed", "decided", "checks", "inbox"):
        m("set", "plan", sec, "folded", "true")
    h = viewer()
    h.send("\x1b[I")
    with Recorder(h, fps=20) as r:
        time.sleep(1.2)
        for k in ["j", "j", "j", "j", "j", "j"]:
            h.send(k)
            time.sleep(0.25)
        h.send("\r")  # Checks? whichever section is focused folds/unfolds
        time.sleep(1.0)
        h.send("\r")
        time.sleep(1.0)
        m("ask", "plan", "Q3", "Ship the demo to the guide?", "now", "later", "--default", "now", "--parent", "assume",
          "--applies", "after review")
        time.sleep(4.0)
    r.save("focus.webp", "margin: focus, fold, a new question")
    h.close()

    # 4. motion off: the same pane, nothing moves, nothing wakes
    ENV["MARGIN_MOTION"] = "off"
    m("demo")
    h = viewer()
    still(h, "motion-off.png", "margin: motion off")
    h.close()
    print("out:", OUT)


if __name__ == "__main__":
    main()
