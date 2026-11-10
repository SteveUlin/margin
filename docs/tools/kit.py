"""kit: margin's proposed component vocabulary, drawn on a termshot Canvas.

Each function mirrors a component in design.md's component tables, so the
mockups double as a visual spec. The look leans toward Charm's (rounded
borders, pill labels, gradient focus, generous padding, a quiet help line),
because sulin likes Bubble Tea best of what he's seen; the meanings are ours.

Every hue means one thing, and each meaning also has a glyph or word, so the
mockups still read in greyscale:
  purple  you: your decisions, your pins, the focused pane
  aqua    actionable: tokens you can double-click, links
  green   done, approved, healthy
  yellow  in progress, waiting, an assumption
  orange  changed since you looked (change bars, the since-you-looked mark)
  red     failed, must-fix, struck
  blue    Claude's content and read-warrant
  grey    meta: times, counts, hints
"""
import re
from termshot import Canvas, P, mix

# Nerd Font icons (JetBrainsMono Nerd Font Mono; codepoints checked against the font)
I = dict(
    plan="", done="", todo="", doing="", blocked="", check="",
    clock="", branch="", pr="", pin="", flag="", question="",
    bulb="", flask="", term="", robot="\U000f06a9", user="", eye="",
    bolt="", code="", ts="", book="", play="", hourglass="",
    warn="", inbox="", comments="", comment="", undo="",
    right="", down="", search="", image="", graph="", star="",
    gavel="", scale="", link="", gear="", pulse="", send="",
    db="", server="", moon="", leaf="", history="", refresh="",
    bug="", info="", file="", git="", fork="", chart="",
    half="\uf042", stop="", bell="", target="", cog2="", chip="", diff="",
)
SPIN = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"          # Bubble Tea's MiniDot
EIGHTHS = " ▏▎▍▌▋▊▉"
CAP_L, CAP_R = "", ""   # rounded pill caps
FADE = ["orange", mix("orange", "bg0", .35), mix("orange", "bg0", .65)]  # change-bar tones


def pill(cv: Canvas, x, y, text, bg="purple", fg="bg0", b=True, ground=None):
    """A rounded label: half-circle caps around a coloured run. Returns the x after it."""
    cv.put(x, y, CAP_L, bg, ground)
    x = cv.put(x + 1, y, text, fg, bg, b=b)
    cv.put(x, y, CAP_R, bg, ground)
    return x + 1


def tag(cv: Canvas, x, y, text, fg, ground="bg0"):
    """A soft pill: tinted ground, coloured text (counts, kinds, warrants)."""
    t = mix(fg, ground, .78)
    return pill(cv, x, y, text, bg=t, fg=fg, b=True, ground=ground)


def token(cv: Canvas, x, y, tok, fg="aqua", bg=None):
    """A selection token: double-click it and Zellij's copy_command hands it to margin select."""
    return cv.put(x, y, tok, fg, bg, u="dotted", uc=mix(fg, "bg0", .3))


def help_line(cv: Canvas, x, y, keys, ground=None):
    """Bubble Tea's help component: key, description, bullets between."""
    for n, (k, d) in enumerate(keys):
        if n:
            x = cv.put(x, y, " • ", "bg4", ground)
        x = cv.put(x, y, k, "grey2", ground, b=True)
        x = cv.put(x + 1, y, d, "grey0", ground)
    return x


def grad_bar(cv: Canvas, x, y, width, frac, a="green", b="aqua", empty="bg2", notch=None):
    """Bubble Tea's gradient progress bar: blended fill, eighth-block edge, quiet track."""
    full = frac * width
    for k in range(width):
        col = mix(a, b, k / max(1, width - 1))
        if k + 1 <= full:
            cv.put(x + k, y, "█", col)
        elif k < full:
            cv.put(x + k, y, EIGHTHS[int((full - k) * 8)] or " ", col, empty)
            cv.tint(x + k, y, 1, 1, empty)
        else:
            cv.put(x + k, y, " ", None, empty)
    if notch is not None:
        nx = x + min(width - 1, int(notch * width))
        cv.put(nx, y, "▕", "yellow", cv.cells[y][nx].bg)


def spark(cv: Canvas, x, y, vals, a="aqua", b="yellow"):
    """Sparkline with height and colour both carrying the value."""
    chars = "▁▂▃▄▅▆▇█"
    lo, hi = min(vals), max(vals)
    for k, v in enumerate(vals):
        t = (v - lo) / ((hi - lo) or 1)
        cv.put(x + k, y, chars[round(t * 7)], mix(a, b, t))
    return x + len(vals)


# ---------------------------------------------------------------------------
class Pane:
    """A margin pane: rounded border, pill title, padded body, help line at the foot.

    The left column inside the border is the gutter where change bars sit."""

    def __init__(self, cv: Canvas, x, y, w, h, title, icon="", meta="", focused=False, accent="purple",
                 pinned=False, help=None, ground="bg0"):
        self.cv, self.x, self.y, self.w, self.h = cv, x, y, w, h
        self.ground = ground
        cv.fill(x, y, w, h, ground)
        if focused:
            cv.box(x, y, w, h, grad=(accent, "aqua"))
        else:
            cv.box(x, y, w, h, fg="bg3")
        tx = x + 2
        tx = pill(cv, tx, y, f" {icon} {title} " if icon else f" {title} ",
                  bg=accent if focused else "bg3", fg="bg0" if focused else "fg", ground=ground)
        if pinned:
            cv.put(tx + 1, y, I["pin"], "purple", ground)
        if meta:
            cv.put(x + w - 3 - len(meta), y, f" {meta} ", "grey1", ground)
        self.gx = x + 1            # gutter column
        self.ix = x + 3            # content left edge (2 cols of padding)
        self.iw = w - 6            # content width
        self.row = y + 2
        self.bottom = y + h - 2
        if help:
            help_line(cv, self.ix, self.bottom, help, ground)
            self.bottom -= 1

    def line(self, indent=0):
        r = self.row
        self.row += 1
        return self.ix + indent, r

    def gap(self, n=1):
        self.row += n

    def right(self, y, text, fg="grey1", **kw):
        return self.cv.put(self.ix + self.iw - len(text), y, text, fg, **kw)

    def change(self, y, age=0):
        """A change bar in the gutter: age 0 is fresh, 1 and 2 fade, 3 is gone."""
        if age < 3:
            self.cv.put(self.gx, y, "▎", FADE[age])

    def focus(self, y, h=1):
        """The focused row: a raised ground across the content and a purple caret."""
        self.cv.tint(self.ix - 1, y, self.iw + 2, h, "bg1")
        self.cv.put(self.ix - 1, y, "▌", "purple", "bg1")


def section(p: Pane, title, count=None, open=True, focused=False, note="", icon="", note_fg="grey1"):
    """Collapsible: chevron, bold title, a count tag, a right-aligned note."""
    cv = p.cv
    x, y = p.line()
    cv.put(x, y, I["down"] if open else I["right"], "purple" if open else "grey1", b=True)
    if icon:
        x = cv.put(x + 2, y, icon, "grey2") - 1
    end = cv.put(x + 2, y, title, "fg", b=True)
    if count is not None:
        tag(cv, end + 1, y, f"{count}", "grey2", ground="bg1" if focused else p.ground)
    if note:
        p.right(y, note, note_fg)
    if focused:
        p.focus(y)
        cv.put(x, y, I["down"] if open else I["right"], "purple", "bg1", b=True)
    return y


TODO = {"done": (I["done"], "green"), "doing": (None, "yellow"), "todo": (I["todo"], "grey0"),
        "blocked": (I["blocked"], "red")}


def todo(p: Pane, state, text, tok="", aside="", aside_fg="grey1", indent=2, focused=False,
         changed=None, spin=0, by_you=False):
    cv = p.cv
    x, y = p.line(indent)
    glyph, col = TODO[state]
    if focused:
        p.focus(y)
    g = glyph or SPIN[spin % len(SPIN)]
    cv.put(x, y, g, col, b=True)
    end = cv.put(x + 2, y, text, "grey1" if state == "done" else "fg", b=(state == "doing"))
    if by_you:
        end = cv.put(end + 1, y, I["user"], "purple")
    if aside:
        end = cv.put(end + 2, y, aside, aside_fg)
    if tok:
        token(cv, p.ix + p.iw - len(tok), y, tok)
    if changed is not None:
        p.change(y, changed)
    return y


def progress(p: Pane, frac, done, total, width=None, indent=2, label="", extra=""):
    cv = p.cv
    x, y = p.line(indent)
    if label:
        x = cv.put(x, y, label, "grey1") + 1
    width = width or (p.iw - indent - 16 - len(label))
    grad_bar(cv, x, y, width, frac)
    end = cv.put(x + width + 2, y, f"{done}/{total}", "fg", b=True)
    if extra:
        cv.put(end + 2, y, extra, "grey1")
    return y


def timer(cv: Canvas, x, y, secs, icon=I["clock"], fg="grey2"):
    m, s = divmod(int(secs), 60)
    return cv.put(x, y, f"{icon} {m:02d}:{s:02d}", fg)


def since_mark(p: Pane, text):
    """The since-you-looked rule: everything below arrived while you were away."""
    cv = p.cv
    x, y = p.line()
    label = f" {I['eye']} {text} "
    cv.put(p.ix, y, "╌" * p.iw, mix("orange", p.ground, .45))
    cv.put(p.ix + 2, y, label, "orange", p.ground)
    return y


def kv(p: Pane, k, v, vfg="fg", indent=2, kw=12, **o):
    x, y = p.line(indent)
    p.cv.put(x, y, k, "grey1")
    p.cv.put(x + kw, y, v, vfg, **o)
    return y


def text(p: Pane, s, fg="fg", indent=2, **kw):
    x, y = p.line(indent)
    p.cv.put(x, y, s, fg, **kw)
    return y


def rich(p: Pane, parts, indent=2, y=None, x=None):
    """parts: (text, fg, {opts}) or ("@tok", token) or ("@pill", text, bg) or ("@tag", text, fg)."""
    if y is None:
        x, y = p.line(indent)
    cv = p.cv
    for part in parts:
        if part[0] == "@tok":
            x = token(cv, x, y, part[1])
        elif part[0] == "@pill":
            x = pill(cv, x, y, part[1], bg=part[2], ground=cv.cells[y][x].bg or p.ground)
        elif part[0] == "@tag":
            x = tag(cv, x, y, part[1], part[2], ground=cv.cells[y][x].bg or p.ground)
        elif part[0] == "@sp":
            x += part[1]
        else:
            t, fg, *o = part
            x = cv.put(x, y, t, fg, **(o[0] if o else {}))
    return y


WARRANT = {"ran": ("green", I["play"]), "read": ("blue", I["book"]), "inferred": ("yellow", I["bulb"])}


def warrant(cv: Canvas, x, y, kind, ground="bg0"):
    col, ic = WARRANT[kind]
    return tag(cv, x, y, f"{ic} {kind}", col, ground)


# ---------------------------------------------------------------------------
# syntax colours: Everforest's own mapping, for TypeScript-ish snippets
KW = {"const", "let", "return", "export", "function", "import", "from", "if", "else", "new", "for",
      "of", "await", "async", "type", "interface"}
TOK = re.compile(r"(//.*$)|(\"[^\"]*\"|'[^']*'|`[^`]*`)|(\b\d+(?:\.\d+)?\b)|([A-Za-z_$][\w$]*)|(=>|[=+\-*/<>!&|?:]+)|(\s+)|(.)")


def syntax(cv: Canvas, x, y, src, bg=None, limit=None):
    """Draw one line of code with Everforest syntax colours. Returns per-char x positions."""
    pos = []
    for m in TOK.finditer(src):
        s = m.group(0)
        if m.group(1): fg, it = "grey1", True
        elif m.group(2): fg, it = "aqua", False
        elif m.group(3): fg, it = "purple", False
        elif m.group(4):
            nxt = src[m.end():m.end() + 1]
            fg = "red" if s in KW else "green" if nxt == "(" else "yellow" if s[0].isupper() else "fg"
            it = False
        elif m.group(5): fg, it = "orange", False
        else: fg, it = "grey2", False
        for ch in s:
            if limit is not None and len(pos) >= limit:
                return pos
            pos.append(x)
            cv.put(x, y, ch, fg, bg, i=it)
            x += 1
    return pos


def diff_inline(p: Pane, rows, indent=0, width=None, focused=None):
    """Unified diff for a narrow pane: numbers, sign, tinted line, changed tokens stronger.

    rows: (kind, lineno, text, [changed substrings]); kind in " -+".
    The spans come from difftastic's JSON, so only the tokens that changed light up."""
    cv = p.cv
    width = width or p.iw - indent
    for n, (kind, no, src, marks) in enumerate(rows):
        x, y = p.line(indent)
        line_bg = {"-": "bg_red", "+": "bg_green", " ": None}[kind]
        hl = {"-": "hl_red", "+": "hl_green", " ": None}[kind]
        if line_bg:
            cv.tint(x, y, width, 1, line_bg)
        cv.put(x, y, f"{no:>3}" if no else "   ", {"-": "red", "+": "green", " ": "grey0"}[kind], line_bg)
        cv.put(x + 4, y, kind if kind != " " else " ", {"-": "red", "+": "green", " ": "grey0"}[kind], line_bg, b=True)
        body = src[: width - 7]
        syntax(cv, x + 6, y, body, line_bg)
        for mk in marks:
            s = body.find(mk)
            if s >= 0:
                cv.tint(x + 6 + s, y, len(mk), 1, hl)
        if focused == n:
            cv.put(x - 1, y, "▌", "purple", line_bg)
    return p.row


def diff_side(p: Pane, rows, indent=0):
    """Side by side for a wide pane. rows: (lno, ltext, lmarks, rno, rtext, rmarks)."""
    cv = p.cv
    inner = p.iw - indent
    half = (inner - 1) // 2
    for ln, lt, lm, rn, rt, rm in rows:
        x, y = p.line(indent)
        for no, t, marks, sx, kind in ((ln, lt, lm, x, "-"), (rn, rt, rm, x + half + 1, "+")):
            changed = bool(marks)
            lbg = ("bg_red" if kind == "-" else "bg_green") if changed else None
            hl = "hl_red" if kind == "-" else "hl_green"
            if lbg:
                cv.tint(sx, y, half, 1, lbg)
            cv.put(sx, y, f"{no:>3} " if no else "    ", ("red" if kind == "-" else "green") if changed else "grey0", lbg)
            body = (t or "")[: half - 5]
            syntax(cv, sx + 4, y, body, lbg)
            for mk in marks:
                s = body.find(mk)
                if s >= 0:
                    cv.tint(sx + 4 + s, y, len(mk), 1, hl)
        cv.put(x + half, y, "│", "bg3")
    return p.row


# ---------------------------------------------------------------------------
# artifact components built from the primitives

def card(p: Pane, h, focused=False, ground="bg1", ring="purple"):
    """A raised block. Focus draws a rounded ring around it; nothing else gets a border."""
    cv = p.cv
    x, y = p.ix - 1, p.row
    w = p.iw + 2
    if focused:
        cv.box(x, y, w, h, fg=ring)
        cv.fill(x + 1, y + 1, w - 2, h - 2, ground)
    else:
        cv.fill(x, y, w, h, ground)
    p.row += 1   # a row of padding (or the ring) above the content
    return x, y, w


def decision_card(p: Pane, qid, question, options, default, picked=None, applies="", focused=False,
                  flash=None, sent=False, why=""):
    """Question with a default: Claude goes on with the default; you flip it with a double-click.

    flash: index of the option whose token is selected (0) or flashing inverse (1)."""
    cv = p.cv
    rows = 4 + (1 if why else 0)
    h = rows + 2
    x0, y0, w = card(p, h, focused)
    ground = "bg1"
    x, y = p.line(1)
    cv.put(x, y, I["question"], "yellow", ground, b=True)
    end = cv.put(x + 2, y, qid, "yellow", ground, b=True)
    cv.put(end + 1, y, question, "fg", ground, b=True)
    if why:
        x, y = p.line(3)
        cv.put(x, y, why, "grey1", ground, i=True)
    x, y = p.line(3)
    for n, opt in enumerate(options):
        chosen = (picked or default) == opt
        mine = picked == opt
        col = "purple" if mine else "blue" if chosen else "grey1"
        cv.put(x, y, "" if chosen else "", col, ground, b=chosen)
        x = cv.put(x + 2, y, opt, "fg" if chosen else "grey2", ground, b=chosen) + 3
    x, y = p.line(3)
    for n, opt in enumerate(options):
        t = f"mg:pick/{qid}.{opt}"
        if flash is not None and flash[0] == n:
            stage = flash[1]
            if stage == 0:      # Zellij's selection highlight
                cv.put(x, y, t, "fg", "bg_visual")
                x += len(t)
            else:               # margin's acknowledgement: inverse for 150 ms
                cv.put(x, y, t, "bg0", "purple", b=True)
                x += len(t)
        else:
            x = token(cv, x, y, t, bg=ground)
        x += 2
    x, y = p.line(3)
    if sent:
        x = pill(cv, x, y, f" {I['send']} sent ", bg="purple", ground=ground)
        cv.put(x + 1, y, "Claude reads it next turn", "grey1", ground)
    elif picked:
        x = pill(cv, x, y, f" {I['user']} you picked {picked} ", bg="purple", ground=ground)
    else:
        cv.put(x, y, f"{I['hourglass']} default ", "grey1", ground)
        x = cv.put(x + 10, y, default, "blue", ground, b=True)
        cv.put(x + 1, y, f"applies in {applies}", "grey1", ground)
    p.row += 1
    return y0


def assumption(p: Pane, aid, textv, struck=False, reason="", focused=False, changed=None):
    cv = p.cv
    x, y = p.line(1)
    if focused:
        p.focus(y)
    g = p.cv.cells[y][x].bg
    if struck:
        cv.put(x, y, I["blocked"], "red", g, b=True)
        end = cv.put(x + 2, y, aid, "red", g, b=True)
        end = cv.put(end + 1, y, textv, "grey1", g, s=True)
        tag(cv, p.ix + p.iw - 11, y, f"{I['user']} struck", "purple", g or p.ground)
        if reason:
            x2, y2 = p.line(4)
            cv.put(x2, y2, f"you: “{reason}”", "purple", i=True)
    else:
        cv.put(x, y, I["bulb"], "yellow", g)
        end = cv.put(x + 2, y, aid, "yellow", g, b=True)
        cv.put(end + 1, y, textv, "fg", g)
        tk = f"mg:strike/{aid}"
        token(cv, p.ix + p.iw - len(tk), y, tk, bg=g)
    if changed is not None:
        p.change(y, changed)
    return y


def decided(p: Pane, did, textv, when, by="claude"):
    cv = p.cv
    x, y = p.line(1)
    cv.put(x, y, I["check"], "green", b=True)
    end = cv.put(x + 2, y, did, "green", b=True)
    cv.put(end + 1, y, textv, "fg")
    who = (I["user"], "purple") if by == "you" else (I["robot"], "blue")
    cv.put(p.ix + p.iw - len(when) - 2, y, who[0], who[1])
    p.right(y, when, "grey0")
    return y


def agent_row(p: Pane, state, name, kind, status, age, ctx=None, act=None, focused=False, spin=0):
    cv = p.cv
    x, y = p.line(0)
    if focused:
        p.focus(y)
    g = cv.cells[y][x].bg or p.ground
    icon, col = {"working": (SPIN[spin % 10], "yellow"), "waiting": (I["bell"], "purple"),
                 "running": (SPIN[(spin + 4) % 10], "green"), "done": (I["done"], "green"),
                 "failed": (I["blocked"], "red"), "idle": (I["todo"], "grey0")}[state]
    cv.put(x, y, icon, col, g, b=True)
    cv.put(x + 2, y, name, "fg", g, b=state in ("waiting", "failed"))
    kcol = {"peer": "blue", "sub": "aqua", "worker": "green", "unit": "orange"}[kind]
    tag(cv, x + 19, y, kind, kcol, g)
    cv.put(x + 28, y, status, col if state in ("waiting", "failed") else "grey2", g, b=state == "waiting")
    if act:
        spark(cv, x + 44, y, act)
    elif ctx is not None:
        pass
    cv.put(p.ix + p.iw - len(age), y, age, "grey0", g)
    return y


def line_gauge(cv: Canvas, x, y, width, frac, col="green", track="bg3", notch=None):
    """A thin meter on heavy box-drawing lines (━): reads as a rule, not a block."""
    full = frac * width
    for k in range(width):
        if k + 1 <= full:
            cv.put(x + k, y, "━", col)
        elif k < full and full - k >= .5:
            cv.put(x + k, y, "╸", col)
            cv.put(x + k, y, "╸", col)
        else:
            cv.put(x + k, y, "━", track)
    if notch is not None:
        cv.put(x + min(width - 1, int(notch * width)), y, "╋", "yellow")


def ctx_gauge(cv: Canvas, x, y, frac, width=10):
    """Context use: green, yellow past 60%, red past 80%; the notch marks compaction."""
    col = "green" if frac < .6 else "yellow" if frac < .8 else "red"
    line_gauge(cv, x, y, width, frac, col, notch=.8)
    return cv.put(x + width + 1, y, f"{int(frac * 100)}%", col, b=frac >= .8)


def finding(p: Pane, n, sev, title, where, focused=False, open=True):
    cv = p.cv
    x, y = p.line()
    if focused:
        p.focus(y)
    g = cv.cells[y][x].bg or p.ground
    cv.put(x, y, I["down"] if open else I["right"], "purple" if open else "grey1", g, b=True)
    col = {"must-fix": "red", "suggest": "yellow", "nit": "grey2"}[sev]
    end = tag(cv, x + 2, y, f"{n} {sev}", col, g)
    cv.put(end + 1, y, title, "fg", g, b=True)
    p.right(y, where, "grey0", bg=g)
    return y


def ledger(p: Pane, turn, desc, files, open=False, focused=False, changed=None):
    cv = p.cv
    x, y = p.line()
    if focused:
        p.focus(y)
    g = cv.cells[y][x].bg or p.ground
    cv.put(x, y, I["down"] if open else I["right"], "purple" if open else "grey1", g, b=True)
    end = tag(cv, x + 2, y, f"turn {turn}", "blue", g)
    cv.put(end + 1, y, desc, "fg", g, b=True)
    adds = sum(f[1] for f in files)
    dels = sum(f[2] for f in files)
    s = f"+{adds} -{dels}"
    xr = p.ix + p.iw - len(s)
    cv.put(xr, y, f"+{adds}", "green", g)
    cv.put(xr + len(f"+{adds}") + 1, y, f"-{dels}", "red", g)
    if changed is not None:
        p.change(y, changed)
    if open:
        for f, a, d in files:
            x2, y2 = p.line(4)
            cv.put(x2, y2, I["ts"] if f.endswith(".ts") else I["file"], "blue")
            cv.put(x2 + 2, y2, f, "fg")
            tot = max(1, a + d)
            w = min(12, a + d)
            na = round(w * a / tot)
            cv.put(p.ix + p.iw - 14, y2, "■" * na, "green")
            cv.put(p.ix + p.iw - 14 + na, y2, "■" * (w - na), "red")
    return y


def queue_item(p: Pane, when, msg, status, outcome, focused=False, changed=None):
    cv = p.cv
    x, y = p.line()
    if focused:
        p.focus(y, 2)
    g = cv.cells[y][x].bg or p.ground
    icon, col = {"done": (I["done"], "green"), "doing": (SPIN[3], "yellow"), "queued": (I["hourglass"], "grey1"),
                 "changed": (I["warn"], "orange"), "dropped": (I["blocked"], "grey0")}[status]
    cv.put(x, y, I["user"], "purple", g)
    cv.put(x + 2, y, when, "grey0", g)
    cv.put(x + 8, y, f"“{msg}”", "fg", g)
    x2, y2 = p.line(8)
    g2 = cv.cells[y2][x2].bg or p.ground
    cv.put(x2, y2, icon, col, g2, b=True)
    cv.put(x2 + 2, y2, outcome, col if status != "queued" else "grey1", g2)
    if changed is not None:
        p.change(y, changed)
        p.change(y2, changed)
    return y


def hypothesis(p: Pane, hid, textv, state, evidence, focused=False):
    cv = p.cv
    x, y = p.line()
    if focused:
        p.focus(y)
    g = cv.cells[y][x].bg or p.ground
    col = {"testing": "yellow", "holds": "green", "ruled out": "red", "untested": "grey1"}[state]
    cv.put(x, y, I["flask"], col, g)
    end = cv.put(x + 2, y, hid, col, g, b=True)
    cv.put(end + 1, y, textv, "grey1" if state == "ruled out" else "fg", g, s=state == "ruled out")
    tag(cv, p.ix + p.iw - len(state) - 2, y, state, col, g)
    if evidence:
        x2, y2 = p.line(4)
        cv.put(x2, y2, evidence, "grey1", i=True)
    return y


def table(p: Pane, cols, rows, sel=None, sort=None, indent=0):
    """A table: quiet header, a rule, aligned cells; the selected row raised.

    cols: (name, width, align) with align "<" or ">"; rows: lists of str or (str, fg)."""
    cv = p.cv
    x, y = p.line(indent)
    cx = x
    for n, (name, w, al) in enumerate(cols):
        lab = name + (" " + I["down"] if sort == n else "")
        cv.put(cx + (w - len(lab) if al == ">" else 0), y, lab, "purple" if sort == n else "grey1", b=True,
               u="single", uc="bg4")
        cx += w + 2
    for r, row in enumerate(rows):
        x, y = p.line(indent)
        if sel == r:
            p.focus(y)
        g = cv.cells[y][x].bg
        cx = x
        for (name, w, al), cell in zip(cols, row):
            t, fg = (cell, "fg") if isinstance(cell, str) else cell
            cv.put(cx + (w - len(t) if al == ">" else 0), y, t, fg, g)
            cx += w + 2
    return p.row


def tree(p: Pane, nodes, sel=None, indent=0):
    """A tree with guides. nodes: (depth, label, fg, open|None, note); open None = leaf."""
    cv = p.cv
    for n, (depth, label, fg, op, note) in enumerate(nodes):
        x, y = p.line(indent)
        if sel == n:
            p.focus(y)
        g = cv.cells[y][x].bg
        # guides: is there a later sibling at each depth?
        for d in range(depth):
            later = any(nd[0] == d + 1 for nd in nodes[n + 1:n + 1 + next(
                (k for k, nd in enumerate(nodes[n + 1:]) if nd[0] <= d), len(nodes))])
            if d == depth - 1:
                last = not any(nd[0] == depth for nd in nodes[n + 1:n + 1 + next(
                    (k for k, nd in enumerate(nodes[n + 1:]) if nd[0] < depth), len(nodes))])
                cv.put(x + d * 3, y, ("└─ " if last else "├─ "), "bg4", g)
            elif later:
                cv.put(x + d * 3, y, "│", "bg4", g)
        cx = x + depth * 3
        if op is not None:
            cv.put(cx, y, I["down"] if op else I["right"], "purple" if op else "grey1", g, b=True)
            cx += 2
        e = cv.put(cx, y, label, fg, g, b=op is not None)
        if note:
            cv.put(e + 2, y, note, "grey0", g)
    return p.row


def zellij_bar(cv: Canvas, y, tabs, active, session="main", mode="NORMAL"):
    """Zellij's compact bar, themed: session pill, tabs as pills with agent glyphs.

    A tab's glyph is the state of the agent in it (api working, margin waiting on you),
    so the tab bar doubles as a fleet overview."""
    cv.fill(0, y, cv.w, 1, "bg_dim")
    x = pill(cv, 1, y, f" {I['moon']} {session} ", bg="green", ground="bg_dim") + 1
    for n, (name, glyph, gcol) in enumerate(tabs):
        on = n == active
        lab = f" {n + 1} {name} "
        if on:
            cv.put(x, y, CAP_L, "bg3", "bg_dim")
            x = cv.put(x + 1, y, lab, "fg", "bg3", b=True)
            if glyph:
                x = cv.put(x, y, glyph + " ", gcol, "bg3", b=True)
            cv.put(x, y, CAP_R, "bg3", "bg_dim")
            x += 2
        else:
            x = cv.put(x + 1, y, lab, "grey1", "bg_dim")
            if glyph:
                x = cv.put(x, y, glyph, gcol, "bg_dim", b=True) + 1
            x += 1
    cv.put(cv.w - len(mode) - 4, y, CAP_L, "bg2", "bg_dim")
    cv.put(cv.w - len(mode) - 3, y, f" {mode} ", "grey2", "bg2", b=True)
    cv.put(cv.w - 1, y, CAP_R, "bg2", "bg_dim")
