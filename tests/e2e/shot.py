"""shot: draw ANSI frames (from `margin render --ansi` or a capture) as Ghostty-faithful PNGs.

usage: shot.py out.png COLS ROWS < frame.ansi
"""
import os
import sys

import pyte

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "tools"))
from termshot import from_pyte  # noqa: E402


def canvas_from_ansi(data: bytes, cols: int, rows: int, title=None):
    sc = pyte.Screen(cols, rows)
    st = pyte.ByteStream(sc)
    from sgrfix import fix
    st.feed(fix(data).rstrip(b"\n").replace(b"\n", b"\r\n"))
    cv = from_pyte(sc)
    cv.title = title
    return cv


if __name__ == "__main__":
    out, cols, rows = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
    title = sys.argv[4] if len(sys.argv) > 4 else None
    canvas_from_ansi(sys.stdin.buffer.read(), cols, rows, title).png(out, scale=0.5)
