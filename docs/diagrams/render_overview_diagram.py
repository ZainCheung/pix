#!/usr/bin/env python3
"""Render the Pix overview diagram (dark, neon, hand-drawn style).

Visual language mirrors docs/diagrams/oldversion.png: pure-black canvas,
wobbly neon borders (green/blue/purple/amber/mint/pink) over dark tinted
fills, a hand-written title block with a green "Pix" pill, a muted outer
container, scattered "+" decorations and an "@Pix" corner tag.

Content is unchanged from the light one-pager: left "phone/tablet" card,
right "computer" card, two connecting arrows in the middle (solid green
"same network", dashed amber "encrypted relay"), and a first-time pairing
bar at the bottom. Only the two middle arrows are animated (flowing glow
dot on the solid arrow, marching dashes + glow dot on the dashed arrow).
Outputs .png, .gif and a matching .excalidraw file, in both languages:
--lang zh (default, pix-overview.*) and --lang en (pix-overview-en.*).
"""
import argparse
import json
import math
import random
import sys
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw, ImageFont

W = 1210
H = 1000
FRAMES = 40
FPS = 20
SCALE = 2

# Palette lifted from oldversion.excalidraw
PAGE = "#000000"
INK = "#f4f0ee"
SUB = "#cfc7c5"
FAINT = "#8a97a1"
MUTED = "#5c6265"
GREEN = "#22c86f"
GREEN_TINT = "#052515"
MINT = "#7ee3d6"
BLUE = "#1d8be8"
BLUE_TINT = "#081626"
PURPLE = "#bd54d3"
PURPLE_TINT = "#17091d"
AMBER = "#f4b64e"
AMBER_TINT = "#241703"
PINK = "#ff7ab6"
PILL_FILL = "#124238"
CARD_SHADOW = "#050507"

CN_FONT = "/System/Library/Fonts/Hiragino Sans GB.ttc"
LATIN_FONT = "/System/Library/Fonts/Supplemental/ChalkboardSE.ttc"

SOLID_ARROW = (470, 340, 744, 340)
DASH_ARROW = (744, 470, 470, 470)
DASH_PATTERN = (10, 8)

SKETCH_SEED = 2069769416930414980
UPDATED = 1782475200000


def c(v):
    return int(round(v * SCALE))


def hex_rgba(value, alpha=255):
    value = value.lstrip("#")
    return tuple(int(value[i : i + 2], 16) for i in (0, 2, 4)) + (alpha,)


def to_hex(color):
    if color is None:
        return "transparent"
    if isinstance(color, str):
        return color
    return "#{:02x}{:02x}{:02x}".format(*color[:3])


def cn(size, bold=False):
    return ImageFont.truetype(CN_FONT, c(size), index=2 if bold else 0)


def latin(size, bold=False):
    return ImageFont.truetype(LATIN_FONT, c(size), index=2 if bold else 1)


def _wobble_pts(pts, rng, amp):
    """Add a deterministic hand-drawn wobble to a point list (1x coords)."""
    out = []
    for i, (x, y) in enumerate(pts):
        if i == 0 or i == len(pts) - 1:
            out.append((x, y))
            continue
        out.append((x + rng.uniform(-amp, amp), y + rng.uniform(-amp, amp)))
    return out


def _subdivide(pts, step=26):
    out = []
    for (x0, y0), (x1, y1) in zip(pts, pts[1:]):
        seg = math.hypot(x1 - x0, y1 - y0)
        n = max(1, int(seg / step))
        for k in range(n):
            t = k / n
            out.append((x0 + (x1 - x0) * t, y0 + (y1 - y0) * t))
    out.append(pts[-1])
    return out


def _rrect_path(x, y, w, h, r):
    r = min(r, w / 2, h / 2)
    pts = [(x + r, y)]
    for cx, cy, a0 in [(x + w - r, y + r, 270), (x + w - r, y + h - r, 0), (x + r, y + h - r, 90), (x + r, y + r, 180)]:
        for a in range(a0, a0 + 91, 13):
            rad = math.radians(a)
            pts.append((cx + r * math.cos(rad), cy + r * math.sin(rad)))
    return pts


def _ellipse_path(cx, cy, rx, ry):
    pts = []
    for a in range(0, 360, 9):
        rad = math.radians(a)
        pts.append((cx + rx * math.cos(rad), cy + ry * math.sin(rad)))
    pts.append(pts[0])
    return pts


class Excal:
    """Collects Excalidraw elements (1x coordinates)."""

    def __init__(self, width, height):
        self.width = width
        self.height = height
        self.elements = []
        self.count = 0
        self.rng = random.Random(2069769416930414980 ^ 0x5EED)

    def base(self, prefix, kind, x, y, w, h, stroke, fill="transparent", stroke_width=2, stroke_style="solid", roundness=None):
        self.count += 1
        element = {
            "id": f"{prefix}-{self.count:04d}",
            "type": kind,
            "x": round(x, 2),
            "y": round(y, 2),
            "width": round(w, 2),
            "height": round(h, 2),
            "angle": 0,
            "strokeColor": stroke,
            "backgroundColor": fill or "transparent",
            "fillStyle": "solid",
            "strokeWidth": stroke_width,
            "strokeStyle": stroke_style,
            "roughness": 1,
            "opacity": 100,
            "groupIds": [],
            "frameId": None,
            "index": f"a{self.count:04d}",
            "roundness": roundness,
            "seed": self.rng.randint(1, 2147483646),
            "version": 1,
            "versionNonce": self.rng.randint(1, 2147483646),
            "isDeleted": False,
            "boundElements": None,
            "updated": UPDATED,
            "link": None,
            "locked": False,
        }
        self.elements.append(element)
        return element

    def rect(self, x, y, w, h, stroke, fill="transparent", width=2, roundness=None):
        return self.base("rect", "rectangle", x, y, w, h, stroke, fill, width, "solid", roundness)

    def ellipse(self, x, y, w, h, stroke, fill="transparent", width=2):
        return self.base("ellipse", "ellipse", x, y, w, h, stroke, fill, width)

    def line(self, points, stroke, width=2, style="solid", closed=False):
        min_x = min(x for x, _ in points)
        min_y = min(y for _, y in points)
        max_x = max(x for x, _ in points)
        max_y = max(y for _, y in points)
        element = self.base("line", "line", min_x, min_y, max_x - min_x, max_y - min_y, stroke, "transparent", width, style, None)
        pts = [[round(x - min_x, 2), round(y - min_y, 2)] for x, y in points]
        if closed:
            pts.append(list(pts[0]))
        element["points"] = pts
        element["startBinding"] = None
        element["endBinding"] = None
        return element

    def text(self, s, x, y, size, color, font_family=1, width=0.0, height=0.0, baseline=0.0):
        element = self.base("text", "text", x, y, width, height, color, "transparent", 1)
        element.update({
            "text": s,
            "fontSize": int(round(size)),
            "fontFamily": font_family,
            "textAlign": "left",
            "verticalAlign": "top",
            "baseline": int(round(baseline)),
            "containerId": None,
            "originalText": s,
            "lineHeight": 1.25,
        })
        return element

    def write(self, path):
        data = {
            "type": "excalidraw",
            "version": 2,
            "source": "https://excalidraw.com",
            "elements": self.elements,
            "appState": {"viewBackgroundColor": PAGE, "gridSize": None, "currentItemFontFamily": 1},
            "files": {},
        }
        path.write_text(json.dumps(data, ensure_ascii=False, indent=2), encoding="utf-8")


class Sketch:
    """Hand-drawn renderer: wobbly shapes to PIL, ideal shapes to Excalidraw.

    Wobble comes from a seeded RNG so the PNG, GIF frames and the recorded
    excalidraw geometry are all deterministic.
    """

    def __init__(self, pil_draw, ex, seed=SKETCH_SEED):
        self.d = pil_draw
        self.ex = ex
        self.rng = random.Random(seed)

    def _amp(self, w, h):
        return max(0.7, min(2.2, min(w, h) * 0.012))

    def line(self, pts, color, width=2, wobble=True):
        pts1x = [(x, y) for x, y in pts]
        drawn = _subdivide(pts1x) if wobble else pts1x
        if wobble:
            amp = self._amp(max(x for x, _ in pts1x) - min(x for x, _ in pts1x) or 1, max(y for _, y in pts1x) - min(y for _, y in pts1x) or 1)
            drawn = _wobble_pts(drawn, self.rng, amp)
        self.d.line([(c(x), c(y)) for x, y in drawn], fill=hex_rgba(color), width=max(1, c(width)), joint="curve")
        if self.ex:
            self.ex.line(pts1x, to_hex(color), width)

    def rrect(self, x, y, w, h, radius, fill=None, outline=None, width=2, excal_width=None):
        if self.ex:
            self.ex.rect(x, y, w, h, to_hex(outline), to_hex(fill), excal_width or max(1, round(width)), {"type": 3})
        path = _wobble_pts(_subdivide(_rrect_path(x, y, w, h, radius)), self.rng, self._amp(w, h))
        sp = [(c(px), c(py)) for px, py in path]
        if fill:
            self.d.polygon(sp, fill=hex_rgba(fill))
        if outline:
            self.d.line(sp, fill=hex_rgba(outline), width=max(1, c(width)), joint="curve")

    def circle(self, cx, cy, r, fill=None, outline=None, width=2, excal_width=None):
        self.ellipse(cx, cy, r, r, fill, outline, width, excal_width)

    def ellipse(self, cx, cy, rx, ry, fill=None, outline=None, width=2, excal_width=None):
        if self.ex:
            self.ex.ellipse(cx - rx, cy - ry, 2 * rx, 2 * ry, to_hex(outline), to_hex(fill), excal_width or max(1, round(width)))
        path = _wobble_pts(_ellipse_path(cx, cy, rx, ry), self.rng, self._amp(2 * rx, 2 * ry))
        sp = [(c(px), c(py)) for px, py in path]
        if fill:
            self.d.polygon(sp, fill=hex_rgba(fill))
        if outline:
            self.d.line(sp, fill=hex_rgba(outline), width=max(1, c(width)), joint="curve")

    def arc(self, cx, cy, rx, ry, start, end, color, width=2):
        if end < start:
            end += 360
        pts = []
        for a in range(int(start), int(end) + 1, 12):
            rad = math.radians(a)
            pts.append((cx + rx * math.cos(rad), cy + ry * math.sin(rad)))
        self.line(pts, color, width)

    def text(self, s, x, y, size, color=INK, bold=False, align="left", latin_font=False, h=0):
        font = latin(size, bold) if latin_font else cn(size, bold)
        tw = self.d.textbbox((0, 0), s, font=font)[2]
        tx = c(x)
        if align == "right":
            tx = c(x) - tw
        ascent, descent = font.getmetrics()
        th = ascent + descent
        base_h = h if h else size * 1.4
        ty = c(y) + (c(base_h) - th) / 2
        self.d.text((tx, ty), s, font=font, fill=hex_rgba(color))
        if self.ex:
            # Record real glyph metrics: importers lay text out from the
            # stored width/height and do not re-measure like excalidraw.com.
            self.ex.text(s, tx / SCALE, ty / SCALE, font.size / SCALE, to_hex(color),
                         font_family=1, width=tw / SCALE, height=th / SCALE, baseline=ascent / SCALE)

    def text_w(self, s, size, bold=False, latin_font=False):
        font = latin(size, bold) if latin_font else cn(size, bold)
        return self.d.textbbox((0, 0), s, font=font)[2] / SCALE

    def center_text(self, s, cx, cy, size, color=INK, bold=False, latin_font=False):
        """Draw text centered on (cx, cy)."""
        font = latin(size, bold) if latin_font else cn(size, bold)
        tw = self.d.textbbox((0, 0), s, font=font)[2]
        ascent, descent = font.getmetrics()
        th = ascent + descent
        tx, ty = c(cx) - tw / 2, c(cy) - th / 2
        self.d.text((tx, ty), s, font=font, fill=hex_rgba(color))
        if self.ex:
            self.ex.text(s, tx / SCALE, ty / SCALE, font.size / SCALE, to_hex(color),
                         font_family=1, width=tw / SCALE, height=th / SCALE, baseline=ascent / SCALE)

    def plus(self, cx, cy, r, color, width=2):
        self.line([(cx - r, cy), (cx + r, cy)], color, width)
        self.line([(cx, cy - r), (cx, cy + r)], color, width)


# ---------------------------------------------------------------- glyphs

def phone_glyph(sk, cx, cy, s, color):
    sk.rrect(cx - 13 * s, cy - 20 * s, 26 * s, 40 * s, 6 * s, outline=color, width=2)
    sk.line([(cx - 4 * s, cy + 13 * s), (cx + 4 * s, cy + 13 * s)], color, 2)


def terminal_glyph(sk, cx, cy, color=MINT):
    sk.line([(cx - 13, cy - 9), (cx - 4, cy), (cx - 13, cy + 9)], color, 3)
    sk.line([(cx + 2, cy + 9), (cx + 13, cy + 9)], color, 3)


def image_glyph(sk, cx, cy, color=PINK):
    sk.rrect(cx - 18, cy - 13, 36, 26, 5, outline=color, width=2)
    sk.circle(cx - 8, cy - 4, 3.5, fill=color)
    sk.line([(cx - 12, cy + 8), (cx - 4, cy - 1), (cx + 2, cy + 5), (cx + 8, cy - 3), (cx + 13, cy + 8)], color, 2)


def wand_glyph(sk, cx, cy, color=PURPLE):
    sk.line([(cx - 11, cy + 11), (cx + 6, cy - 6)], color, 3)
    sk.line([(cx + 3, cy - 11), (cx + 10, cy - 11)], color, 2)
    sk.line([(cx + 6.5, cy - 15), (cx + 6.5, cy - 7)], color, 2)
    sk.circle(cx + 13, cy - 2, 2.2, fill=color)


def laptop(sk, x, y, color=INK):
    sk.rrect(x, y, 92, 60, 7, outline=color, width=2.5)
    sk.rrect(x - 14, y + 63, 120, 9, 4, fill=color, outline=color, width=1)


def apple_glyph(sk, cx, cy, color=AMBER, carve=PURPLE_TINT):
    sk.circle(cx - 1, cy + 3, 10, fill=color)
    sk.circle(cx + 8.5, cy + 2, 4.2, fill=carve)
    sk.line([(cx - 1, cy - 5), (cx + 0.5, cy - 11)], color, 2.5)
    sk.ellipse(cx + 5.5, cy - 11, 4.5, 2.5, color, color, 1.2)


def windows_glyph(sk, cx, cy, color=BLUE):
    pane, gap = 10, 3
    total = pane * 2 + gap
    x0, y0 = cx - total / 2, cy - total / 2
    for dx in (0, pane + gap):
        for dy in (0, pane + gap):
            sk.rrect(x0 + dx, y0 + dy, pane, pane, 2, fill=color, outline=color, width=1)


def shield_check(sk, cx, cy, color=GREEN, scale=1.0):
    s = scale
    pts = [(cx, cy - 11 * s), (cx + 9 * s, cy - 7 * s), (cx + 8 * s, cy + 4 * s), (cx, cy + 11 * s), (cx - 8 * s, cy + 4 * s), (cx - 9 * s, cy - 7 * s)]
    sk.line(pts + [pts[0]], color, 2)
    sk.line([(cx - 4 * s, cy - 1 * s), (cx - 1 * s, cy + 3.5 * s), (cx + 5 * s, cy - 4 * s)], color, 2.5)


def lock_glyph(sk, cx, cy, color=AMBER, s=1.0):
    sk.rrect(cx - 9 * s, cy - 1 * s, 18 * s, 14 * s, 3 * s, fill=color, outline=color, width=1)
    sk.arc(cx, cy - 1 * s, 6.5 * s, 7 * s, 180, 360, color, 2.5)


def download_glyph(sk, cx, cy, color=INK):
    sk.rrect(cx - 19, cy + 2, 38, 13, 3, outline=color, width=2)
    sk.line([(cx, cy - 14), (cx, cy - 1)], color, 2.5)
    sk.line([(cx - 5, cy - 6), (cx, cy - 1), (cx + 5, cy - 6)], color, 2.5)


def scan_glyph(sk, cx, cy, color=INK):
    r = 12
    for sx, sy in [(-1, -1), (1, -1), (-1, 1), (1, 1)]:
        px, py = cx + sx * r, cy + sy * r
        sk.line([(px - sx * 7, py), (px, py), (px, py - sy * 7)], color, 2.5)
    sk.rrect(cx - 4.5, cy - 4.5, 9, 9, 2, fill=MINT, outline=MINT, width=1)


def check_circle_glyph(sk, cx, cy, color=INK):
    sk.circle(cx, cy, 15, outline=color, width=2.5)
    sk.line([(cx - 6, cy + 1), (cx - 1.5, cy + 6), (cx + 7, cy - 5)], MINT, 3)


def chevron(sk, cx, cy, color=MUTED):
    sk.line([(cx - 3.5, cy - 7), (cx + 3.5, cy), (cx - 3.5, cy + 7)], color, 2)


STRINGS = {
    "zh": {
        "subtitle": "把你电脑上的 Pi 助手装进口袋",
        "title_plus_x": 370,
        "left_label_size": 23,
        "phone_title": "手机 / 平板",
        "phone_tag": "随手用",
        "left_rows": [
            ("文本指令", terminal_glyph, MINT),
            ("图片附件", image_glyph, PINK),
            ("Skills", wand_glyph, PURPLE),
        ],
        "computer_title": "你的电脑",
        "computer_tag": "(Mac 或 Linux)",
        "direct": "同网直连",
        "relay": "异网加密接力",
        "pairing_title": "首次配对",
        "pairing_tag": "三步就好",
        "steps": [
            (376, "1", download_glyph, "电脑安装 Pix", 200),
            (668, "2", scan_glyph, "手机扫码", 120),
            (902, "3", check_circle_glyph, "电脑确认", 120),
        ],
    },
    "en": {
        "subtitle": "Your Pi coding agent, in your pocket",
        "title_plus_x": 425,
        "left_label_size": 21,
        "phone_title": "Phone / Tablet",
        "phone_tag": "On the go",
        "left_rows": [
            ("Text prompts", terminal_glyph, MINT),
            ("Image attachments", image_glyph, PINK),
            ("Skills", wand_glyph, PURPLE),
        ],
        "computer_title": "Your computer",
        "computer_tag": "(Mac or Linux)",
        "direct": "Direct connect",
        "relay": "Encrypted relay",
        "pairing_title": "Pairing",
        "pairing_tag": "3 quick steps",
        "steps": [
            (330, "1", download_glyph, "Install Pix", 130),
            (610, "2", scan_glyph, "Scan QR code", 135),
            (900, "3", check_circle_glyph, "Approve", 100),
        ],
    },
}


def arrow_head(sk, tip_x, tip_y, direction, color, width=4):
    size = 10 + width
    sk.line([(tip_x - direction * size, tip_y - size * 0.72), (tip_x, tip_y), (tip_x - direction * size, tip_y + size * 0.72)], color, width)


def pix_dots(sk, cx, cy):
    dots = [(-12, -4, GREEN), (-2, -9, PURPLE), (8, -3, MINT), (-6, 6, AMBER), (5, 7, PINK)]
    for dx, dy, color in dots:
        sk.circle(cx + dx, cy + dy, 3.2, fill=color)


# ---------------------------------------------------------------- layout

def draw_solid_arrow(sk):
    x1, y1, x2, _ = SOLID_ARROW
    sk.line([(x1, y1), (x2, y1)], GREEN, 3.5)
    arrow_head(sk, x2, y1, +1, GREEN, 3.5)


def dash_y_offset(idx):
    rng = random.Random(SKETCH_SEED ^ (0xD05A0000 + idx))
    return rng.uniform(-2.0, 2.0)


def dash_segments(phase=0.0, sketch=None, pil=None, color=AMBER):
    """Marching dashes along the dashed arrow; deterministic per dash index."""
    x2, y, x1, _ = DASH_ARROW  # drawn right -> left
    total = x2 - x1
    dash, gap = DASH_PATTERN
    dist = -phase
    idx = 0
    while dist < total:
        start = max(0.0, dist)
        end = min(float(total), dist + dash)
        if end > start:
            oy = dash_y_offset(idx)
            if sketch is not None:
                sketch.line([(x2 - start, y + oy), (x2 - end, y + oy)], color, 3, wobble=False)
            else:
                # Animation overlays are already final-size: 1x coords only.
                pil.line([(x2 - start, y + oy), (x2 - end, y + oy)], fill=hex_rgba(color), width=3)
        dist += dash + gap
        idx += 1


def render_base(with_dashes, lang="zh"):
    t = STRINGS[lang]
    img = Image.new("RGBA", (W * SCALE, H * SCALE), hex_rgba(PAGE))
    ex = Excal(W, H)
    sk = Sketch(ImageDraw.Draw(img), ex)

    # Title block
    sk.rrect(40, 28, 10, 50, 5, fill=PURPLE)
    sk.text("The overview of", 66, 28, 38, INK, bold=True, latin_font=True, h=50)
    sk.rrect(474, 26, 168, 54, 16, fill=PILL_FILL, outline=GREEN, width=2.5)
    sk.center_text("Pix", 558, 53, 32, GREEN, bold=True, latin_font=True)
    sk.text(t["subtitle"], 70, 90, 16, SUB, h=22)
    pix_dots(sk, 1052, 44)
    sk.text("@Pix", 1072, 30, 22, INK, bold=True, latin_font=True, h=28)

    # Outer container
    sk.rrect(24, 124, 1162, 848, 22, outline=MUTED, width=1.5, excal_width=1)

    # Decorative "+" marks
    sk.plus(t["title_plus_x"], 104, 11, GREEN, 2.5)
    sk.plus(846, 106, 11, GREEN, 2.5)
    sk.plus(1092, 236, 11, PURPLE, 2.5)
    sk.plus(556, 682, 11, GREEN, 2.5)
    sk.plus(656, 682, 11, PURPLE, 2.5)

    # Left card: phone / tablet
    sk.rrect(44, 150, 392, 600, 20, fill=GREEN_TINT, outline=GREEN, width=3)
    sk.text(t["phone_title"], 70, 168, 24, INK, bold=True, h=32)
    sk.text(t["phone_tag"], 348, 176, 13, GREEN, h=18)
    for i, (label, glyph, color) in enumerate(t["left_rows"]):
        y = 226 + i * 172
        sk.rrect(64, y, 352, 152, 16, fill="#04200f", outline=GREEN, width=2)
        sk.rrect(88, y + 44, 64, 64, 13, outline=color, width=2)
        glyph(sk, 120, y + 76, color)
        sk.text(label, 176, y + 60, t["left_label_size"], INK, bold=True, h=32)

    # Right card: computers
    sk.rrect(774, 150, 392, 600, 20, fill=PURPLE_TINT, outline=PURPLE, width=3)
    sk.text(t["computer_title"], 800, 168, 24, INK, bold=True, h=32)
    sk.text(t["computer_tag"], 1044, 176, 14, FAINT, h=18)
    os_rows = [
        (apple_glyph, "Mac", "host"),
        (terminal_glyph, "Linux", "host"),
        (windows_glyph, "Windows", "soon"),
    ]
    for i, (glyph, name, kind) in enumerate(os_rows):
        y = 226 + i * 172
        laptop(sk, 812, y + 40)
        glyph(sk, 858, y + 70)
        name_color = INK if kind == "host" else FAINT
        sk.text(name, 948, y + 42, 26, name_color, bold=True, h=34)
        if kind == "host":
            shield_check(sk, 962, y + 104)
            sk.text("Pix Host", 982, y + 92, 17, GREEN, bold=True, h=24)
        else:
            sk.rrect(948, y + 88, 158, 38, 12, fill="#080711", outline=MUTED, width=1.5)
            sk.center_text("Coming Soon", 1027, y + 107, 15, FAINT)
        if i < 2:
            sk.line([(800, y + 158), (1140, y + 158)], MUTED, 1.2)

    # Middle: pills and arrows
    sk.rrect(488, 236, 234, 54, 16, fill=GREEN_TINT, outline=GREEN, width=2.5)
    sk.center_text(t["direct"], 605, 263, 22, MINT, bold=True)
    draw_solid_arrow(sk)
    arrow_head(sk, DASH_ARROW[2], DASH_ARROW[1], -1, AMBER, 3)
    if with_dashes:
        dash_segments(sketch=sk)
    sk.rrect(468, 528, 274, 54, 16, fill=AMBER_TINT, outline=AMBER, width=2.5)
    lock_glyph(sk, 502, 555)
    sk.text(t["relay"], 528, 542, 21, AMBER, bold=True, h=30)

    # Bottom bar: first-time pairing steps
    sk.rrect(44, 788, 1122, 168, 18, fill=BLUE_TINT, outline=BLUE, width=3)
    sk.text(t["pairing_title"], 80, 838, 24, INK, bold=True, h=32)
    sk.text(t["pairing_tag"], 80, 878, 13, MINT, h=18)
    sk.line([(272, 826), (272, 918)], MUTED, 1.2)
    cy = 872
    for i, (x, num, glyph, label, label_w) in enumerate(t["steps"]):
        sk.circle(x + 18, cy, 19, fill=MINT)
        sk.center_text(num, x + 18, cy, 20, "#04200f", bold=True, latin_font=True)
        glyph(sk, x + 84, cy)
        sk.text(label, x + 118, cy - 16, 21, INK, h=32)
        if i < 2:
            chevron(sk, x + 118 + label_w + 24, cy)

    return ex, img.resize((W, H), Image.Resampling.LANCZOS).convert("RGB")


def glow_dot(draw, x, y, strength=1.0, halo=GREEN, core=MINT):
    for radius, alpha in [(16, 40), (10, 75), (5, 170)]:
        a = int(alpha * strength)
        draw.ellipse((x - radius, y - radius, x + radius, y + radius), fill=hex_rgba(halo, a))
    draw.ellipse((x - 2.5, y - 2.5, x + 2.5, y + 2.5), fill=hex_rgba(core))


def draw_dash_segments_1x(draw, phase):
    """Animation frames are composited on the final-size image, so the
    c() supersampling helpers must not be used here."""
    dash_segments(phase=phase, pil=draw)


def animate_frame(base, idx):
    frame = base.convert("RGBA")
    overlay = Image.new("RGBA", frame.size, (0, 0, 0, 0))
    draw = ImageDraw.Draw(overlay)
    t = (idx % 20) / 20
    x1, y1, x2, _ = SOLID_ARROW
    span = x2 - x1
    for trail, strength in [(0.0, 1.0), (-0.05, 0.55), (-0.10, 0.28)]:
        tt = (t + trail) % 1.0
        glow_dot(draw, x1 + tt * span, y1, strength)
    draw_dash_segments_1x(draw, phase=t * (DASH_PATTERN[0] + DASH_PATTERN[1]))
    for trail, strength in [(0.0, 1.0), (-0.05, 0.55), (-0.10, 0.28)]:
        tt = (t + trail) % 1.0
        glow_dot(draw, x2 - tt * span, DASH_ARROW[1], strength, halo=AMBER, core=AMBER)
    frame.alpha_composite(overlay)
    return frame.convert("RGB")


def frame_diff_report(gif_path):
    with Image.open(gif_path) as im:
        picks = [0, im.n_frames // 4, im.n_frames // 2, 3 * im.n_frames // 4, im.n_frames - 1]
        frames = []
        for idx in picks:
            im.seek(idx)
            frames.append(im.convert("RGB"))
        frame_count = im.n_frames
    diffs = []
    for left, right, a, b in zip(frames, frames[1:], picks, picks[1:]):
        diff = ImageChops.difference(left, right)
        bbox = diff.getbbox()
        changed = 0
        if bbox:
            cropped = diff.crop(bbox)
            data = cropped.get_flattened_data() if hasattr(cropped, "get_flattened_data") else cropped.getdata()
            changed = sum(1 for px in data if px != (0, 0, 0))
        diffs.append({"from": a, "to": b, "changed_pixels": changed})
    return {"frames": frame_count, "diffs": diffs}


def main():
    parser = argparse.ArgumentParser(description="Render the Pix overview diagram (dark hand-drawn style).")
    parser.add_argument("--outdir", type=Path, default=Path(__file__).resolve().parent)
    parser.add_argument("--lang", choices=sorted(STRINGS), default="zh", help="Text language of the diagram.")
    parser.add_argument("--basename", default=None, help="Defaults to pix-overview (zh) / pix-overview-en (en).")
    args = parser.parse_args()
    if args.basename is None:
        args.basename = "pix-overview" if args.lang == "zh" else "pix-overview-en"
    args.outdir.mkdir(parents=True, exist_ok=True)

    ex, static = render_base(with_dashes=True, lang=args.lang)
    png_path = args.outdir / f"{args.basename}.png"
    static.save(png_path, "PNG")

    excalidraw_path = args.outdir / f"{args.basename}.excalidraw"
    ex.write(excalidraw_path)

    _, base = render_base(with_dashes=False, lang=args.lang)
    frames = [animate_frame(base, i) for i in range(FRAMES)]
    # Shared palette + no dither: static pixels stay byte-identical across
    # frames, so only the two middle arrows actually change in the GIF.
    palette = frames[0].quantize(colors=256, dither=Image.Dither.NONE)
    quantized = [f.quantize(palette=palette, dither=Image.Dither.NONE) for f in frames]
    gif_path = args.outdir / f"{args.basename}.gif"
    quantized[0].save(gif_path, save_all=True, append_images=quantized[1:], duration=int(1000 / FPS), loop=0, optimize=False)

    report = frame_diff_report(gif_path)
    excalidraw = json.loads(excalidraw_path.read_text(encoding="utf-8"))
    elements = excalidraw.get("elements", [])
    ids = [e.get("id") for e in elements]
    texts = [e for e in elements if e.get("type") == "text"]
    rough = [e for e in elements if e.get("type") != "text"]
    checks = {
        "png_size_ok": static.size == (W, H),
        "gif_frames_ok": report["frames"] == FRAMES,
        "gif_motion_ok": any(d["changed_pixels"] > 0 for d in report["diffs"]),
        "excalidraw_ids_ok": len(ids) == len(set(ids)),
        "excalidraw_font_ok": all(e.get("fontFamily") == 1 for e in texts),
        "excalidraw_text_dims_ok": all(e.get("width", 0) > 0 and e.get("height", 0) > 0 for e in texts),
        "excalidraw_rough_ok": all(e.get("roughness") == 1 for e in rough),
        "excalidraw_files_empty": excalidraw.get("files") == {},
    }
    result = {"png": str(png_path), "gif": str(gif_path), "excalidraw": str(excalidraw_path), "elements": len(elements), "verification": report, "checks": checks, "ok": all(checks.values())}
    print(json.dumps(result, ensure_ascii=False, indent=2))
    if not result["ok"]:
        sys.exit(1)


if __name__ == "__main__":
    main()
