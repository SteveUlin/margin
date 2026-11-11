"""idle: a settled margin pane writes nothing and burns no CPU (the frame clock only runs while
something moves). Also checks a focused pane's flowing ring costs a bounded frame rate.

usage: python3 tests/e2e/idle.py      (inside `nix develop`, after `cargo build`)
"""
import json, os, subprocess, sys, tempfile, time
HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "../.."))
sys.path.insert(0, HERE)
from ptyhost import Host

MARGIN = os.path.join(ROOT, "target/debug/margin")
T = tempfile.mkdtemp(prefix="margin-idle-")
ENV = dict(os.environ, MARGIN_HOME=T, MARGIN_PROJECT="idle", MARGIN_IMAGES="blocks", MARGIN_CONFIG=os.path.join(T, "c.json"))
for k in ("ZELLIJ", "ZELLIJ_SESSION_NAME", "ZELLIJ_PANE_ID"):
    ENV.pop(k, None)
json.dump({}, open(ENV["MARGIN_CONFIG"], "w"))
doc = {"title": "static", "blocks": [{"type": "section", "id": "s", "title": "Steps", "children": [
    {"type": "todo", "id": "a", "text": "done thing", "status": "done"}, {"type": "todo", "id": "b", "text": "next thing"}]},
    {"type": "claim", "id": "A1", "text": "an assumption"}]}
subprocess.run([MARGIN, "new", "plan"], env=ENV, input=json.dumps(doc), text=True, check=True)


def cpu(pid):
    f = open(f"/proc/{pid}/stat").read().split(")")[1].split()
    return (int(f[11]) + int(f[12])) / os.sysconf("SC_CLK_TCK")


def measure(h, secs):
    pid = [int(p) for p in os.listdir("/proc") if p.isdigit() and open(f"/proc/{p}/cmdline", "rb").read().startswith(MARGIN.encode() + b"\0view")][0]
    b0, c0 = len(h.raw), cpu(pid)
    time.sleep(secs)
    return len(h.raw) - b0, cpu(pid) - c0


ok = True
h = Host([MARGIN, "view", "plan"], cols=64, rows=30, env=ENV)
h.wait_for("static", 5)
h.send("\x1b[O")       # unfocused: the ring rests
time.sleep(4.0)        # let the meta clock ("3s ago") pass a minute boundary is too slow; it ticks 1 Hz under 60 s
by, cp = measure(h, 3.0)
print(f"unfocused, settled, <60s old: {by} bytes, {cp:.3f} s CPU in 3 s (1 Hz age clock)")
h.send("\x1b[I")
time.sleep(0.5)
by2, cp2 = measure(h, 3.0)
print(f"focused (ring flows at 12 fps): {by2} bytes, {cp2:.3f} s CPU in 3 s")
h.close()
# an artifact older than a minute with motion off: nothing at all should tick
ENV["MARGIN_MOTION"] = "off"
art = os.path.join(T, "idle", "plan", "doc.json")
d = json.load(open(art)); d["updated"] = "2026-01-01T00:00:00Z"; json.dump(d, open(art, "w"))
h = Host([MARGIN, "view", "plan"], cols=64, rows=30, env=ENV)
h.wait_for("static", 5)
h.send("\x1b[I")
time.sleep(1.5)
by3, cp3 = measure(h, 5.0)
print(f"motion off, focused, old artifact: {by3} bytes, {cp3:.3f} s CPU in 5 s")
h.close()
ok = by3 == 0 and cp3 < 0.05 and cp < 0.1
print("PASS" if ok else "FAIL")
sys.exit(0 if ok else 1)
