"""ptyhost: a fake terminal client for headless Zellij tests.

Runs a command (usually a Zellij client) in a pty, answers the queries a real terminal answers the
way Ghostty does (kitty graphics, DA1, pixel sizes, DECRQM 2026, colours), records every byte it
receives, and replays the stream through pyte so tests can read the screen and termshot can draw it.

The test machine has no screen, so this stands in for Ghostty: research.md §2 showed Zellij drops
images entirely unless the host answers the kitty probe, so the answers matter.
"""
from __future__ import annotations

import os
import pty
import re
import select
import signal
import struct
import fcntl
import termios
import threading
import time

import pyte

from sgrfix import fix

CELL_W, CELL_H = 10, 20

APC = re.compile(rb"\x1b_G([^\x1b]*)\x1b\\")


class Host:
    def __init__(self, argv, cols=160, rows=48, env=None, record=None):
        self.cols, self.rows = cols, rows
        self.screen = pyte.Screen(cols, rows)
        self.stream = pyte.ByteStream(self.screen)
        self.lock = threading.Lock()
        self.raw = bytearray()
        self.kitty_seen = []      # kitty graphics commands the host received (proof images got through)
        self.osc52 = []
        self._carry = b""
        self._kpending = None
        self.kimages = {}
        self.placements = {}
        self.kitty_errors = []
        self.record = open(record, "wb") if record else None
        self.frames = []          # (t, display lines) when capturing
        pid, fd = pty.fork()
        if pid == 0:
            e = dict(os.environ)
            e.update(env or {})
            e["TERM"] = "xterm-256color"
            e["COLORTERM"] = "truecolor"
            os.execvpe(argv[0], argv, e)
        self.pid, self.fd = pid, fd
        self._resize(cols, rows)
        self.alive = True
        self.t = threading.Thread(target=self._read, daemon=True)
        self.t.start()

    def _resize(self, cols, rows):
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, cols * CELL_W, rows * CELL_H))

    def resize(self, cols, rows):
        with self.lock:
            self.cols, self.rows = cols, rows
            self.screen.resize(rows, cols)
        self._resize(cols, rows)
        os.kill(self.pid, signal.SIGWINCH)

    def _answer(self, data: bytes):
        out = b""
        for m in APC.finditer(data):
            body = m.group(1)
            self.kitty_seen.append(body[:120])
            if b"a=q" in body:
                i = re.search(rb"i=(\d+)", body)
                out += b"\x1b_Gi=%s;OK\x1b\\" % (i.group(1) if i else b"1")
        if b"\x1b[c" in data or b"\x1b[0c" in data:
            out += b"\x1b[?62;22;52c"
        if b"\x1b[14t" in data:
            out += b"\x1b[4;%d;%dt" % (self.rows * CELL_H, self.cols * CELL_W)
        if b"\x1b[16t" in data:
            out += b"\x1b[6;%d;%dt" % (CELL_H, CELL_W)
        for m in re.finditer(rb"\x1b\[\?(\d+)\$p", data):
            mode = int(m.group(1))
            out += b"\x1b[?%d;%d$y" % (mode, 2 if mode in (2026, 2004, 1004, 1006) else 0)
        if b"\x1b[?u" in data:
            out += b"\x1b[?0u"
        if b"\x1b]11;?" in data:
            out += b"\x1b]11;rgb:2d2d/3535/3b3b\x1b\\"
        if b"\x1b]10;?" in data:
            out += b"\x1b]10;rgb:d3d3/c6c6/aaaa\x1b\\"
        if b"\x1b[6n" in data:
            with self.lock:
                out += b"\x1b[%d;%dR" % (self.screen.cursor.y + 1, self.screen.cursor.x + 1)
        for m in re.finditer(rb"\x1b\]52;c;([A-Za-z0-9+/=]*)(?:\x07|\x1b\\)", data):
            self.osc52.append(m.group(1))
        if out:
            os.write(self.fd, out)

    def _read(self):
        while self.alive:
            try:
                r, _, _ = select.select([self.fd], [], [], 0.2)
            except OSError:
                break
            if not r:
                continue
            try:
                data = os.read(self.fd, 65536)
            except OSError:
                break
            if not data:
                break
            with self.lock:
                self.raw += data
                if self.record:
                    self.record.write(data)
                # hold back a trailing, unfinished escape sequence so sgrfix sees it whole
                buf = self._carry + data
                cut = buf.rfind(b"\x1b")
                if cut >= 0 and not re.match(rb"\x1b(\[[0-9;:?<>=$]*[@-~]|[^\[_P\]]|_[^\x1b]*\x1b\\|[P\]][^\x07\x1b]*(\x07|\x1b\\))", buf[cut:]):
                    self._carry, buf = buf[cut:], buf[:cut]
                else:
                    self._carry = b""
                pos = 0
                for mm in APC.finditer(buf):
                    self._feed(buf[pos:mm.start()])
                    self._kitty(mm.group(1))
                    pos = mm.end()
                self._feed(buf[pos:])
            self._answer(data)
        self.alive = False

    def _feed(self, b):
        try:
            self.stream.feed(fix(b))
        except Exception:
            pass

    def _kitty(self, body: bytes):
        """Act on a kitty graphics command the way Ghostty would: keep transmitted images, and
        remember where each is placed (the cursor position at the time), so screenshots can draw
        the real pixels. Runs with the lock held, in stream order."""
        head, _, payload = body.partition(b";")
        kv = dict(p.split(b"=", 1) for p in head.split(b",") if b"=" in p)
        a = kv.get(b"a", b"t")
        if b"a" not in kv and self._kpending is not None:  # continuation chunk
            self._kpending["data"] += payload
            if kv.get(b"m", b"0") == b"0":
                self._kfinish()
            return
        iid = int(kv.get(b"i", b"0"))
        if a in (b"t", b"T"):
            self._kpending = {"id": iid, "kv": kv, "data": payload, "place": a == b"T",
                              "at": (self.screen.cursor.x, self.screen.cursor.y)}
            if kv.get(b"m", b"0") == b"0":
                self._kfinish()
        elif a == b"p":
            self._kplace(iid, kv, (self.screen.cursor.x, self.screen.cursor.y))
        elif a == b"d":
            if kv.get(b"d", b"a") in (b"a", b"A"):
                self.placements.clear()
            else:
                self.placements.pop(iid, None)

    def _kfinish(self):
        import base64
        import io
        from PIL import Image
        k = self._kpending
        self._kpending = None
        kv = k["kv"]
        try:
            raw = base64.b64decode(k["data"])
            f = kv.get(b"f", b"32")
            w, h = int(kv.get(b"s", b"0")), int(kv.get(b"v", b"0"))
            if f == b"100":
                img = Image.open(io.BytesIO(raw)).convert("RGBA")
            elif f == b"24":
                img = Image.frombytes("RGB", (w, h), raw).convert("RGBA")
            else:
                img = Image.frombytes("RGBA", (w, h), raw)
            self.kimages[k["id"]] = img
            if k["place"]:
                self._kplace(k["id"], kv, k["at"])
        except Exception as e:
            self.kitty_errors.append(str(e))

    def _kplace(self, iid, kv, at):
        img = self.kimages.get(iid)
        if img is None:
            return
        pw, ph = int(kv.get(b"w", b"0")) or img.width, int(kv.get(b"h", b"0")) or img.height
        cols = int(kv.get(b"c", b"0")) or -(-pw // CELL_W)
        rows = int(kv.get(b"r", b"0")) or -(-ph // CELL_H)
        self.placements[iid] = (at[0], at[1], cols, rows, img.crop((0, 0, pw, ph)))

    def send(self, data: bytes | str):
        if isinstance(data, str):
            data = data.encode()
        os.write(self.fd, data)

    def text(self) -> str:
        with self.lock:
            return "\n".join(self.screen.display)

    def find(self, needle: str):
        """(x, y) of the first occurrence of `needle` on screen, 0-based, or None."""
        with self.lock:
            for y, line in enumerate(self.screen.display):
                x = line.find(needle)
                if x >= 0:
                    return x, y
        return None

    def wait_for(self, needle: str, timeout=10.0) -> bool:
        end = time.time() + timeout
        while time.time() < end:
            if needle in self.text():
                return True
            time.sleep(0.05)
        return False

    def double_click(self, x: int, y: int):
        """Two SGR left clicks at a cell (1-based on the wire), as Ghostty sends a double-click."""
        p = b"\x1b[<0;%d;%dM" % (x + 1, y + 1)
        r = b"\x1b[<0;%d;%dm" % (x + 1, y + 1)
        self.send(p + r)
        time.sleep(0.05)
        self.send(p + r)

    def canvas(self):
        import sys
        sys.path.insert(0, os.path.join(os.path.dirname(__file__), "tools"))
        from termshot import from_pyte
        with self.lock:
            cv = from_pyte(self.screen)
            for x, y, c, r, img in self.placements.values():
                cv.image(x, y, c, r, img)
            return cv

    def close(self):
        self.alive = False
        try:
            os.kill(self.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            os.waitpid(self.pid, 0)
        except ChildProcessError:
            pass
        if self.record:
            self.record.close()
