"""Make margin's SGR output digestible for pyte, which stands in for Ghostty in tests.

pyte doesn't parse colon sub-parameters (4:3 curly / 4:4 dotted underline, 58:2::r:g:b underline
colour), and applies only the first extended colour in one sequence (`38;2;…;48;2;…`). Ghostty
and Zellij handle both, so margin emits them; this splits and simplifies them for pyte only."""
import re

SGR = re.compile(rb"\x1b\[([0-9;:]*)m")


def _fix(m):
    ps = m.group(1).split(b";") if m.group(1) else [b"0"]
    groups, i = [], 0
    while i < len(ps):
        p = ps[i]
        if p in (b"38", b"48") and i + 1 < len(ps) and ps[i + 1] == b"2":
            groups.append(b";".join(ps[i:i + 5]))
            i += 5
            continue
        if p in (b"38", b"48") and i + 1 < len(ps) and ps[i + 1] == b"5":
            groups.append(b";".join(ps[i:i + 3]))
            i += 3
            continue
        i += 1
        if p.startswith(b"58:") or p == b"59":
            continue
        if p.startswith(b"4:"):
            p = b"24" if p == b"4:0" else b"4"
        groups.append(p)
    return b"".join(b"\x1b[" + g + b"m" for g in groups)


def fix(data: bytes) -> bytes:
    return SGR.sub(_fix, data)
