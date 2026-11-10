"""termshot: paint terminal cells and render them the way Ghostty would.

Used for margin's design mockups and to turn captured Zellij client streams
(rendered through pyte) into pictures, since the machine they run on has no screen.

Rendering mirrors a GPU terminal, so the pictures don't promise what a cell
grid can't do:
- text uses JetBrainsMono Nerd Font Mono (MARGIN_FONT_DIR, set by the flake),
  so Nerd Font icons draw single-width, as in Ghostty with that font;
- box drawing, block elements and powerline caps are drawn as geometry, not
  font glyphs (Ghostty and kitty do the same), so borders, bars and pill caps
  tile with no seams at any line height;
- images sit in a rectangle of cells, as a kitty graphics placement does;
- underline styles (single, curly, dotted) and underline colour are the
  SGR 4:x and 58 extensions Ghostty supports;
- a gradient is per-cell truecolor, the way Lip Gloss blends a title.

Palette: Everforest dark (medium), the family of sulin's terminal theme, with
its full accent range; kit.py gives each hue one meaning.
"""
from __future__ import annotations
import math
import os
from PIL import Image, ImageDraw, ImageFont

P = dict(
    bg_dim="#232a2e", bg0="#2d353b", bg1="#343f44", bg2="#3d484d", bg3="#475258",
    bg4="#4f585e", bg5="#56635f",
    bg_red="#514045", bg_green="#425047", bg_yellow="#4d4c43", bg_blue="#3a515d",
    bg_purple="#4c3f4c", bg_visual="#543a48", bg_aqua="#3c4f48", bg_orange="#54473e",
    # stronger tints for changed tokens inside a diff line
    hl_red="#7a4a52", hl_green="#5a7350",
    fg="#d3c6aa", grey0="#7a8478", grey1="#859289", grey2="#9da9a0",
    red="#e67e80", orange="#e69875", yellow="#dbbc7f", green="#a7c080",
    aqua="#83c092", blue="#7fbbb3", purple="#d699b6",
)

FONT_DIR = os.environ.get("MARGIN_FONT_DIR", "")
FS = 28          # font px at scale 1 (a 2x render of 14 px text)
CW = 16.8        # cell advance of JetBrains Mono at FS
CH = 36          # cell height (line height 1.29)
_fonts = {}


def font(b=False, i=False, size=FS):
    k = (b, i, size)
    if k not in _fonts:
        style = {(0, 0): "Regular", (1, 0): "Bold", (0, 1): "Italic", (1, 1): "BoldItalic"}[(int(b), int(i))]
        path = os.path.join(FONT_DIR, f"JetBrainsMonoNerdFontMono-{style}.ttf")
        if not os.path.exists(path):
            raise SystemExit("termshot: no Nerd Font; run inside `nix develop` so MARGIN_FONT_DIR is set")
        _fonts[k] = ImageFont.truetype(path, size)
    return _fonts[k]


def rgb(c):
    c = P.get(c, c).lstrip("#")
    return tuple(int(c[i:i + 2], 16) for i in (0, 2, 4))


def mix(a, b, t):
    """t=0 → a, t=1 → b (hex or palette name in, hex out)."""
    a, b = rgb(a), rgb(b)
    return "#%02x%02x%02x" % tuple(round(x + (y - x) * t) for x, y in zip(a, b))


class Cell:
    __slots__ = ("ch", "fg", "bg", "b", "i", "u", "uc", "s", "dim")

    def __init__(self, ch=" ", fg=None, bg=None):
        self.ch, self.fg, self.bg = ch, fg, bg
        self.b = self.i = self.s = self.dim = False
        self.u = None   # None | "single" | "curly" | "dotted"
        self.uc = None  # underline colour

    def key(self):
        return (self.fg, self.bg, self.b, self.i, self.u, self.uc, self.s, self.dim)


class Canvas:
    def __init__(self, w, h, bg="bg0", fg="fg", title=None):
        self.w, self.h = w, h
        self.bg, self.fg = P.get(bg, bg), P.get(fg, fg)
        self.title = title  # Ghostty window title; None = no window chrome
        self.cells = [[Cell() for _ in range(w)] for _ in range(h)]
        self.images = []  # (x, y, w, h, PIL image or path)

    def put(self, x, y, text, fg=None, bg=None, b=False, i=False, u=None, uc=None, s=False, dim=False):
        if u is True:
            u = "single"
        if not (0 <= y < self.h):
            return x + len(text)
        for ch in text:
            if 0 <= x < self.w:
                c = self.cells[y][x]
                c.ch = ch
                if fg: c.fg = P.get(fg, fg)
                if bg: c.bg = P.get(bg, bg)
                c.b, c.i, c.u, c.s, c.dim = b, i, u, s, dim
                c.uc = P.get(uc, uc) if uc else None
            x += 1
        return x

    def grad(self, x, y, text, a, b, bg=None, bold=True):
        """Text whose colour blends a → b across its cells (Lip Gloss style)."""
        n = max(1, len(text) - 1)
        for k, ch in enumerate(text):
            self.put(x + k, y, ch, mix(a, b, k / n), bg, b=bold)
        return x + len(text)

    def fill(self, x, y, w, h, bg=None, ch=" ", fg=None):
        for yy in range(max(y, 0), min(y + h, self.h)):
            for xx in range(max(x, 0), min(x + w, self.w)):
                c = self.cells[yy][xx]
                c.ch = ch
                c.bg = P.get(bg, bg) if bg else None
                c.fg = P.get(fg, fg) if fg else None
                c.b = c.i = c.s = c.dim = False
                c.u = c.uc = None

    def tint(self, x, y, w, h, bg):
        """Set the ground without touching the text."""
        for yy in range(max(y, 0), min(y + h, self.h)):
            for xx in range(max(x, 0), min(x + w, self.w)):
                self.cells[yy][xx].bg = P.get(bg, bg)

    def box(self, x, y, w, h, title="", fg="bg3", title_fg="fg", bg=None, style="round", right="",
            right_fg=None, grad=None):
        """A panel border with a title set into the top edge (lazygit, Lip Gloss)."""
        tl, tr, bl, br, hz, vt = {"round": "╭╮╰╯─│", "heavy": "┏┓┗┛━┃", "light": "┌┐└┘─│"}[style]
        if bg:
            self.fill(x, y, w, h, bg)
        if grad:  # border colour blends along its length, like a Lip Gloss border gradient
            a, b = grad
            for k in range(w):
                c = mix(a, b, k / max(1, w - 1))
                self.put(x + k, y, tl if k == 0 else tr if k == w - 1 else hz, c)
                self.put(x + k, y + h - 1, bl if k == 0 else br if k == w - 1 else hz, c)
            for yy in range(y + 1, y + h - 1):
                self.put(x, yy, vt, a)
                self.put(x + w - 1, yy, vt, b)
        else:
            self.put(x, y, tl + hz * (w - 2) + tr, fg)
            for yy in range(y + 1, y + h - 1):
                self.put(x, yy, vt, fg)
                self.put(x + w - 1, yy, vt, fg)
            self.put(x, y + h - 1, bl + hz * (w - 2) + br, fg)
        if title:
            self.put(x + 2, y, f" {title} ", title_fg, b=True)
        if right:
            self.put(x + w - 3 - len(right), y, f" {right} ", right_fg or fg)

    def image(self, x, y, w, h, img):
        self.images.append((x, y, w, h, img))

    def copy(self):
        import copy
        return copy.deepcopy(self)

    # ---- export -------------------------------------------------------
    def render(self, scale=1.0) -> Image.Image:
        return _render(self, scale)

    def png(self, path, scale=1.0):
        self.render(scale).save(path, optimize=True)

    def ansi(self):
        def hx(c):
            return ";".join(str(v) for v in rgb(c))
        lines = []
        for row in self.cells:
            s, last = [], None
            for c in row:
                k = c.key()
                if k != last:
                    sgr = ["0"]
                    if c.fg: sgr.append("38;2;" + hx(c.fg))
                    if c.bg: sgr.append("48;2;" + hx(c.bg))
                    if c.b: sgr.append("1")
                    if c.i: sgr.append("3")
                    if c.u: sgr.append({"single": "4", "curly": "4:3", "dotted": "4:4"}[c.u])
                    if c.uc: sgr.append("58;2;" + hx(c.uc))
                    if c.s: sgr.append("9")
                    if c.dim: sgr.append("2")
                    s.append("\x1b[" + ";".join(sgr) + "m")
                    last = k
                s.append(c.ch)
            lines.append("".join(s) + "\x1b[0m")
        return "\n".join(lines)


def animate(frames, path, scale=1.0):
    """frames: [(Canvas, ms)] → an animated WebP (lossless, loops forever)."""
    imgs = [f.render(scale) for f, _ in frames]
    imgs[0].save(path, save_all=True, append_images=imgs[1:], duration=[ms for _, ms in frames],
                 loop=0, lossless=True, quality=100, method=4)


# ---------------------------------------------------------------------------
# geometry glyphs: drawn, not typeset

# box drawing: (up, right, down, left) arm weights, 1 light, 2 heavy
BOX = {
    "─": (0, 1, 0, 1), "│": (1, 0, 1, 0), "┌": (0, 1, 1, 0), "┐": (0, 0, 1, 1),
    "└": (1, 1, 0, 0), "┘": (1, 0, 0, 1), "├": (1, 1, 1, 0), "┤": (1, 0, 1, 1),
    "┬": (0, 1, 1, 1), "┴": (1, 1, 0, 1), "┼": (1, 1, 1, 1),
    "━": (0, 2, 0, 2), "┃": (2, 0, 2, 0), "┏": (0, 2, 2, 0), "┓": (0, 0, 2, 2),
    "┗": (2, 2, 0, 0), "┛": (2, 0, 0, 2), "╴": (0, 0, 0, 1), "╶": (0, 1, 0, 0),
    "╵": (1, 0, 0, 0), "╷": (0, 0, 1, 0), "╺": (0, 2, 0, 0), "╸": (0, 0, 0, 2), "╋": (2, 2, 2, 2),
}
ROUND = {"╭": "dr", "╮": "dl", "╰": "ur", "╯": "ul"}
DASH = {"╌": "h", "┄": "h", "╎": "v", "┆": "v"}
LOWER = {"▁": 1, "▂": 2, "▃": 3, "▄": 4, "▅": 5, "▆": 6, "▇": 7, "█": 8}
LEFT = {"▏": 1, "▎": 2, "▍": 3, "▌": 4, "▋": 5, "▊": 6, "▉": 7}
SHADE = {"░": .25, "▒": .5, "▓": .75}
POWERLINE = {"", "", "", "", "", "", "", ""}
GEOM = set(BOX) | set(ROUND) | set(DASH) | set(LOWER) | set(LEFT) | set(SHADE) | POWERLINE | {"▀", "▐", "▔", "▕"}


def _aa(w, h, draw_fn, ss=4):
    """Draw a shape on a supersampled mask and return it downsampled (antialiased)."""
    m = Image.new("L", (max(1, w * ss), max(1, h * ss)), 0)
    draw_fn(ImageDraw.Draw(m), w * ss, h * ss, ss)
    return m.resize((max(1, w), max(1, h)), Image.LANCZOS)


def _geom(img, dr, ch, x0, y0, x1, y1, fg, bg, s):
    w, h = x1 - x0, y1 - y0
    lw = max(1, round(2.2 * s))       # light line
    hw = max(2, round(4.4 * s))       # heavy line
    cx, cy = x0 + w // 2, y0 + h // 2
    if ch in BOX:
        up, rt, dn, lf = BOX[ch]
        for on, side in ((up, "u"), (dn, "d"), (lf, "l"), (rt, "r")):
            if not on:
                continue
            t = hw if on == 2 else lw
            a = cx - t // 2
            b = cy - t // 2
            if side == "u": r = (a, y0, a + t - 1, b + t - 1)
            elif side == "d": r = (a, b, a + t - 1, y1 - 1)
            elif side == "l": r = (x0, b, a + t - 1, b + t - 1)
            else: r = (a, b, x1 - 1, b + t - 1)
            dr.rectangle(r, fill=fg)
        return
    if ch in DASH:
        if DASH[ch] == "h":
            seg = w / 4
            for k in range(2):
                a = x0 + round(seg * (2 * k + .5))
                dr.rectangle((a, cy - lw // 2, a + round(seg) - 1, cy - lw // 2 + lw - 1), fill=fg)
        else:
            seg = h / 4
            for k in range(2):
                a = y0 + round(seg * (2 * k + .5))
                dr.rectangle((cx - lw // 2, a, cx - lw // 2 + lw - 1, a + round(seg) - 1), fill=fg)
        return
    if ch in ROUND:
        kind = ROUND[ch]
        # local line centre, matching the straight pieces exactly
        lx = (cx - lw // 2 - x0) + lw / 2
        ly = (cy - lw // 2 - y0) + lw / 2

        ss = 4
        W, H, t = w * ss, h * ss, lw * ss
        mx, my = lx * ss, ly * ss
        r = min(mx, my, W - mx, H - my) * .9
        ax = mx + (r if "r" in kind else -r)   # arc centre: one radius toward the arms
        ay = my + (r if "d" in kind else -r)
        ring = Image.new("L", (W, H), 0)
        d = ImageDraw.Draw(ring)
        d.ellipse((ax - r - t / 2, ay - r - t / 2, ax + r + t / 2, ay + r + t / 2), fill=255)
        d.ellipse((ax - r + t / 2, ay - r + t / 2, ax + r - t / 2, ay + r - t / 2), fill=0)
        m = Image.new("L", (W, H), 0)
        keep = Image.new("L", (W, H), 0)   # the quarter between the cell centre and the arms
        qx = (mx - t, ax) if "r" in kind else (ax, mx + t)
        qy = (my - t, ay) if "d" in kind else (ay, my + t)
        ImageDraw.Draw(keep).rectangle((qx[0], qy[0], qx[1], qy[1]), fill=255)
        m.paste(ring, (0, 0), keep)
        d = ImageDraw.Draw(m)
        if "r" in kind: d.rectangle((ax, my - t / 2, W, my + t / 2 - 1), fill=255)
        if "l" in kind: d.rectangle((0, my - t / 2, ax, my + t / 2 - 1), fill=255)
        if "d" in kind: d.rectangle((mx - t / 2, ay, mx + t / 2 - 1, H), fill=255)
        if "u" in kind: d.rectangle((mx - t / 2, 0, mx + t / 2 - 1, ay), fill=255)
        img.paste(fg, (x0, y0, x1, y1), m.resize((w, h), Image.LANCZOS))
        return
    if ch in LOWER:
        hh = round(h * LOWER[ch] / 8)
        dr.rectangle((x0, y1 - hh, x1 - 1, y1 - 1), fill=fg)
        return
    if ch in LEFT:
        ww = max(1, round(w * LEFT[ch] / 8))
        dr.rectangle((x0, y0, x0 + ww - 1, y1 - 1), fill=fg)
        return
    if ch == "▀":
        dr.rectangle((x0, y0, x1 - 1, y0 + h // 2 - 1), fill=fg); return
    if ch == "▐":
        dr.rectangle((x0 + w // 2, y0, x1 - 1, y1 - 1), fill=fg); return
    if ch == "▔":
        dr.rectangle((x0, y0, x1 - 1, y0 + max(1, h // 8) - 1), fill=fg); return
    if ch == "▕":
        dr.rectangle((x1 - max(1, w // 8), y0, x1 - 1, y1 - 1), fill=fg); return
    if ch in SHADE:
        dr.rectangle((x0, y0, x1 - 1, y1 - 1), fill=mix(bg, fg, SHADE[ch])); return
    if ch in POWERLINE:
        def f(d, W, H, ss):
            t = lw * ss
            if ch == "": d.polygon([(0, 0), (W, H / 2), (0, H)], fill=255)
            elif ch == "": d.polygon([(W, 0), (0, H / 2), (W, H)], fill=255)
            elif ch == "": d.ellipse((-W, 0, W, H - 1), fill=255)
            elif ch == "": d.ellipse((0, 0, 2 * W, H - 1), fill=255)
            elif ch == "": d.line([(0, 0), (W, H / 2), (0, H)], fill=255, width=t)
            elif ch == "": d.line([(W, 0), (0, H / 2), (W, H)], fill=255, width=t)
            elif ch == "": d.arc((-W, 0, W - t / 2, H - 1), -90, 90, fill=255, width=t)
            elif ch == "": d.arc((t / 2, 0, 2 * W, H - 1), 90, 270, fill=255, width=t)
        img.paste(fg, (x0, y0, x1, y1), _aa(w, h, f))
        return


_missing, _notdef = set(), None


def _check_glyph(ch):
    """Warn once per character the font lacks: Ghostty would fall back to another
    font, but a mockup should only promise glyphs the chosen font has."""
    global _notdef
    if ch in _missing or ord(ch) < 0x2000:
        return
    f = font()
    sig = lambda c: (f.getmask(c).size, bytes(f.getmask(c)))
    if _notdef is None:
        _notdef = sig("\U0010fffd")
    if sig(ch) == _notdef:
        _missing.add(ch)
        print(f"termshot: no glyph for {ch!r} U+{ord(ch):04X} in the font", file=__import__("sys").stderr)


def _render(cv: Canvas, scale):
    cw, ch_ = CW * scale, CH * scale
    fsz = round(FS * scale)

    def fnt(b, i):
        return font(b, i, fsz)

    asc, desc = fnt(False, False).getmetrics()
    base = round((ch_ + asc - desc) / 2)
    gw, gh = round(cv.w * cw), round(cv.h * ch_)

    chrome = cv.title is not None
    pad = round(16 * scale) if chrome else 0
    bar = round(44 * scale) if chrome else 0
    radius = round(20 * scale)
    W, H = gw + 2 * pad, gh + 2 * pad + bar
    img = Image.new("RGB", (W, H), rgb(cv.bg))
    dr = ImageDraw.Draw(img)
    ox, oy = pad, pad + bar

    def X(col):
        return ox + round(col * cw)

    def Y(row):
        return oy + round(row * ch_)

    for yy, row in enumerate(cv.cells):
        x0 = 0
        while x0 < cv.w:
            bg = row[x0].bg
            x1 = x0
            while x1 < cv.w and row[x1].bg == bg:
                x1 += 1
            if bg:
                dr.rectangle((X(x0), Y(yy), X(x1) - 1, Y(yy + 1) - 1), fill=bg)
            x0 = x1
    for x, y, w, h, im in cv.images:
        if isinstance(im, str):
            im = Image.open(im)
        im = im.convert("RGBA")
        bw, bh = X(x + w) - X(x), Y(y + h) - Y(y)
        r = min(bw / im.width, bh / im.height)
        im = im.resize((max(1, round(im.width * r)), max(1, round(im.height * r))), Image.LANCZOS)
        img.paste(im, (X(x) + (bw - im.width) // 2, Y(y) + (bh - im.height) // 2), im)
    for yy, row in enumerate(cv.cells):
        for xx, c in enumerate(row):
            fg = c.fg or cv.fg
            bg = c.bg or cv.bg
            if c.dim:
                fg = mix(fg, bg, .45)
            x0, y0, x1, y1 = X(xx), Y(yy), X(xx + 1), Y(yy + 1)
            if c.ch in GEOM:
                _geom(img, dr, c.ch, x0, y0, x1, y1, fg, bg, scale)
            elif c.ch != " ":
                _check_glyph(c.ch)
                dr.text((x0, y0 + base), c.ch, fill=fg, font=fnt(c.b, c.i), anchor="ls")
            if c.u:
                uc = c.uc or fg
                uy = y0 + base + round(5 * scale)
                t = max(1, round(2 * scale))
                if c.u == "single":
                    dr.rectangle((x0, uy, x1 - 1, uy + t - 1), fill=uc)
                elif c.u == "dotted":
                    step = round(6 * scale)
                    for px in range(x0 + 1, x1 - 1, step):
                        dr.rectangle((px, uy, px + t, uy + t), fill=uc)
                elif c.u == "curly":
                    pts = [(px, uy + 2 * scale * math.sin((px - ox) / (3.0 * scale))) for px in range(x0, x1 + 1)]
                    dr.line(pts, fill=uc, width=t)
            if c.s:
                sy = y0 + round(ch_ / 2) + round(1 * scale)
                dr.rectangle((x0, sy, x1 - 1, sy + max(1, round(2 * scale)) - 1), fill=fg)
    if not chrome:
        return img
    # Ghostty's GTK header bar: new-tab, centred title, menu, close
    top = bar + pad // 2
    dr.rectangle((0, 0, W, top), fill=rgb("bg_dim"))
    tf = font(True, False, round(FS * .72 * scale))
    icf = font(False, False, round(FS * .72 * scale))
    my = top / 2
    dr.text((W / 2, my), cv.title, fill=rgb("grey2"), font=tf, anchor="mm")
    dr.text((pad + round(8 * scale), my), "", fill=rgb("grey0"), font=icf, anchor="lm")
    dr.text((W - pad - round(52 * scale), my), "", fill=rgb("grey0"), font=icf, anchor="lm")
    dr.text((W - pad - round(14 * scale), my), "", fill=rgb("grey0"), font=icf, anchor="lm")
    out = Image.new("RGBA", (W, H), (0, 0, 0, 0))
    mask = _aa(W, H, lambda d, w, h, ss: d.rounded_rectangle((0, 0, w - 1, h - 1), radius * ss, fill=255), ss=2)
    out.paste(img, (0, 0), mask)
    edge = Image.new("RGBA", (W, H), (0, 0, 0, 0))
    ImageDraw.Draw(edge).rounded_rectangle((0, 0, W - 1, H - 1), radius, outline=rgb("bg3") + (255,),
                                           width=max(1, round(2 * scale)))
    return Image.alpha_composite(out, edge)


# ---------------------------------------------------------------------------
PYTE_NAMED = {"default": None, "black": "bg1", "red": "red", "green": "green", "brown": "yellow",
              "yellow": "yellow", "blue": "blue", "magenta": "purple", "cyan": "aqua", "white": "fg",
              "brightblack": "grey1", "brightred": "red", "brightgreen": "green", "brightyellow": "yellow",
              "brightblue": "blue", "brightmagenta": "purple", "brightcyan": "aqua", "brightwhite": "fg"}


def from_pyte(screen) -> Canvas:
    cv = Canvas(screen.columns, screen.lines)
    for y in range(screen.lines):
        line = screen.buffer[y]
        for x in range(screen.columns):
            ch = line[x]
            c = cv.cells[y][x]
            c.ch = ch.data or " "
            for attr, val in (("fg", ch.fg), ("bg", ch.bg)):
                if val in PYTE_NAMED:
                    v = PYTE_NAMED[val]
                    setattr(c, attr, P[v] if v else None)
                elif val and len(val) == 6:
                    setattr(c, attr, "#" + val)
            if ch.reverse:
                c.fg, c.bg = (c.bg or P["bg0"]), (c.fg or P["fg"])
            c.b, c.i = ch.bold, ch.italics
            c.u = "single" if ch.underscore else None
    return cv
