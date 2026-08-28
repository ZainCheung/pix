#!/usr/bin/env python3
"""Render the Pix product explainer diagram (dark hand-drawn style).

Layout is art-directed for a non-technical audience:
  1. Title: 把电脑上的 Pi 带在身边
  2. Main path: phone (Pix App) <-> your computer (Pix Host -> Pi -> projects)
     with two route labels and a small relay accessory card
  3. Hero strip: your computer is the real workspace
  4. First-time onboarding strip + trust checklist
Outputs .png, .gif, and an editable .excalidraw source.
"""
import argparse
import json
import math
import random
import sys
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw, ImageFilter, ImageFont

W = 1210
H = 1138
FRAMES = 41
FPS = 20
SCALE = 2
UPDATED = 1782475200000

THEME = {
    "bg": "#000000",
    "white": "#f4f0ee",
    "muted": "#cfc7c5",
    "faint": "#9a938f",
    "frame": "#5c6265",
    "green": "#22c86f",
    "green_fill": "#04220f",
    "blue": "#1d8be8",
    "blue_fill": "#081626",
    "blue_soft": "#5f8fc9",
    "purple": "#bd54d3",
    "purple_fill": "#120814",
    "cyan": "#7ee3d6",
    "amber": "#f4b64e",
    "highlight": "#124238",
    "pink": "#ff7ab6",
}

FONT_DIR = Path.home() / ".agents/skills/lanshu-animated-architecture-diagram/assets/fonts"
KAI_BOLD_CANDIDATES = [
    FONT_DIR / "TsangerJinKai02-W05.ttf",
    "/System/Library/Fonts/STHeiti Medium.ttc",
    "/System/Library/Fonts/Hiragino Sans GB.ttc",
    "/Library/Fonts/Arial Unicode.ttf",
]
KAI_REG_CANDIDATES = [
    FONT_DIR / "TsangerJinKai02-W04.ttf",
    "/System/Library/Fonts/STHeiti Light.ttc",
    "/System/Library/Fonts/Hiragino Sans GB.ttc",
    "/Library/Fonts/Arial Unicode.ttf",
]
HAND_CANDIDATES = [
    "/System/Library/Fonts/Supplemental/Chalkduster.ttf",
    "/System/Library/Fonts/MarkerFelt.ttc",
]


def hex_rgba(value, alpha=255):
    value = value.lstrip("#")
    return tuple(int(value[i : i + 2], 16) for i in (0, 2, 4)) + (alpha,)


def c(v):
    return int(round(v * SCALE))


def scaled_box(x, y, w, h):
    return (c(x), c(y), c(x + w), c(y + h))


def load_font(size, hand=False, bold=False):
    candidates = HAND_CANDIDATES if hand else (KAI_BOLD_CANDIDATES if bold else KAI_REG_CANDIDATES)
    for path in candidates:
        try:
            return ImageFont.truetype(str(path), c(size))
        except OSError:
            continue
    return ImageFont.load_default()


def text_size(draw, text, font, spacing=3):
    if not text:
        return 0, 0
    box = draw.multiline_textbbox((0, 0), text, font=font, spacing=c(spacing))
    return box[2] - box[0], box[3] - box[1]


def draw_text(ex, draw, text, x, y, w, h, size, color=None, align="center", bold=False, min_size=9, spacing=3):
    """Draw one text block (manual \n allowed), shrinking until it fits.

    Bold is a real heavier font (W05), never a stroke outline — stroking the
    thin Kai face at small sizes turns glyphs into unreadable blobs.
    """
    color = color or THEME["white"]
    max_w, max_h = c(w), c(h)
    font = None
    for candidate in range(int(size), int(min_size) - 1, -1):
        font = load_font(candidate, bold=bold)
        tw, th = text_size(draw, text, font, spacing=spacing)
        if tw <= max_w and th <= max_h:
            size = candidate
            break
    ex.text(text, x, y, w, h, size, color, align=align)
    tw, th = text_size(draw, text, font, spacing=spacing)
    tx = c(x)
    if align == "center":
        tx = c(x) + (c(w) - tw) / 2
    elif align == "right":
        tx = c(x + w) - tw
    ty = c(y) + (c(h) - th) / 2
    draw.multiline_text((tx, ty), text, font=font, fill=hex_rgba(color), spacing=c(spacing), align=align)


class Excal:
    def __init__(self, width, height):
        self.width = width
        self.height = height
        self.elements = []
        self.count = 0
        self.rng = random.Random(2069769416930414980)

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

    def rect(self, x, y, w, h, stroke, fill="transparent", width=2, style="solid"):
        return self.base("rect", "rectangle", x, y, w, h, stroke, fill, width, style, {"type": 3})

    def ellipse(self, x, y, w, h, stroke, fill="transparent", width=2, style="solid"):
        return self.base("ellipse", "ellipse", x, y, w, h, stroke, fill, width, style, None)

    def text(self, text, x, y, w, h, size, color, align="left"):
        element = self.base("text", "text", x, y, w, h, color, "transparent", 1, "solid", None)
        element.update({
            "text": text,
            "fontSize": int(round(size)),
            "fontFamily": 5,
            "textAlign": align,
            "verticalAlign": "top",
            "baseline": int(round(size * 1.25)),
            "containerId": None,
            "originalText": text,
            "lineHeight": 1.25,
        })
        return element

    def line(self, points, stroke, width=2, style="solid", arrow=False):
        kind = "arrow" if arrow else "line"
        min_x = min(x for x, _ in points)
        min_y = min(y for _, y in points)
        max_x = max(x for x, _ in points)
        max_y = max(y for _, y in points)
        element = self.base(kind, kind, min_x, min_y, max_x - min_x, max_y - min_y, stroke, "transparent", width, style, {"type": 2})
        element["points"] = [[round(x - min_x, 2), round(y - min_y, 2)] for x, y in points]
        element["startBinding"] = None
        element["endBinding"] = None
        return element

    def write(self, path):
        data = {
            "type": "excalidraw",
            "version": 2,
            "source": "https://excalidraw.com",
            "elements": self.elements,
            "appState": {"viewBackgroundColor": THEME["bg"], "gridSize": 20, "currentItemFontFamily": 5},
            "files": {},
        }
        path.write_text(json.dumps(data, ensure_ascii=False, indent=2), encoding="utf-8")


def draw_rect(ex, draw, x, y, w, h, stroke, fill=None, width=2, radius=10):
    ex.rect(x, y, w, h, stroke, fill or "transparent", width)
    draw.rounded_rectangle(scaled_box(x, y, w, h), radius=c(radius), outline=hex_rgba(stroke), fill=hex_rgba(fill) if fill else None, width=max(1, c(width)))


def draw_ellipse(ex, draw, x, y, w, h, stroke, fill=None, width=2):
    ex.ellipse(x, y, w, h, stroke, fill or "transparent", width)
    draw.ellipse(scaled_box(x, y, w, h), outline=hex_rgba(stroke), fill=hex_rgba(fill) if fill else None, width=max(1, c(width)))


def draw_line(ex, draw, points, stroke, width=2, arrow=False):
    ex.line(points, stroke, width, "solid", arrow)
    scaled = [(c(x), c(y)) for x, y in points]
    draw.line(scaled, fill=hex_rgba(stroke), width=max(1, c(width)), joint="curve")
    if arrow and len(points) >= 2:
        arrow_head(draw, points[-2], points[-1], stroke, width)


def arrow_head(draw, a, b, stroke, width=2):
    angle = math.atan2(b[1] - a[1], b[0] - a[0])
    length = 14 + width
    spread = 0.52
    p1 = (b[0] - length * math.cos(angle - spread), b[1] - length * math.sin(angle - spread))
    p2 = (b[0] - length * math.cos(angle + spread), b[1] - length * math.sin(angle + spread))
    draw.line([(c(p1[0]), c(p1[1])), (c(b[0]), c(b[1])), (c(p2[0]), c(p2[1]))], fill=hex_rgba(stroke), width=max(1, c(width)))


def icon(ex, draw, kind, x, y, color=None, scale=1.0):
    color = color or THEME["cyan"]
    s = scale
    if kind == "iphone":
        draw_rect(ex, draw, x + 18 * s, y, 26 * s, 48 * s, THEME["white"], color, 2, 6)
        draw_rect(ex, draw, x + 23 * s, y + 4 * s, 16 * s, 4 * s, THEME["bg"], THEME["bg"], 1, 2)
        draw_rect(ex, draw, x + 24 * s, y + 43 * s, 14 * s, 2 * s, THEME["white"], THEME["white"], 1, 1)
    elif kind == "mac":
        draw_rect(ex, draw, x + 4 * s, y + 1 * s, 58 * s, 34 * s, THEME["white"], color, 2, 4)
        draw_rect(ex, draw, x + 28 * s, y + 2 * s, 10 * s, 3 * s, THEME["bg"], THEME["bg"], 1, 1)
        draw_rect(ex, draw, x, y + 35 * s, 66 * s, 6 * s, THEME["white"], color, 2, 3)
        draw_rect(ex, draw, x + 29 * s, y + 35 * s, 8 * s, 2 * s, THEME["bg"], THEME["bg"], 1, 1)
    elif kind == "linux":
        draw_rect(ex, draw, x + 2 * s, y + 1 * s, 52 * s, 32 * s, THEME["white"], color, 2, 4)
        draw_line(ex, draw, [(x + 12 * s, y + 10 * s), (x + 17 * s, y + 15 * s), (x + 12 * s, y + 20 * s)], THEME["white"], 2)
        draw_line(ex, draw, [(x + 21 * s, y + 19 * s), (x + 32 * s, y + 19 * s)], THEME["white"], 2)
        draw_line(ex, draw, [(x + 28 * s, y + 33 * s), (x + 28 * s, y + 39 * s)], THEME["white"], 3)
        draw_rect(ex, draw, x + 14 * s, y + 39 * s, 28 * s, 4 * s, THEME["white"], color, 2, 2)
    elif kind == "chat":
        draw_rect(ex, draw, x + 2 * s, y + 4 * s, 52 * s, 30 * s, THEME["white"], color, 2, 8)
        draw_line(ex, draw, [(x + 14 * s, y + 34 * s), (x + 10 * s, y + 46 * s), (x + 26 * s, y + 34 * s)], THEME["white"], 2)
        for dx in (14, 25, 36):
            draw_ellipse(ex, draw, x + dx * s, y + 16 * s, 6 * s, 6 * s, THEME["white"], THEME["white"], 1)
    elif kind == "sparkle":
        pts = [(x + 24 * s, y + 4 * s), (x + 30 * s, y + 20 * s), (x + 46 * s, y + 26 * s), (x + 30 * s, y + 32 * s), (x + 24 * s, y + 48 * s), (x + 18 * s, y + 32 * s), (x + 2 * s, y + 26 * s), (x + 18 * s, y + 20 * s)]
        draw.polygon([(c(px), c(py)) for px, py in pts], fill=hex_rgba(color), outline=hex_rgba(THEME["white"]))
        draw_line(ex, draw, pts + [pts[0]], THEME["white"], 2)
        pts2 = [(x + 46 * s, y + 1 * s), (x + 49 * s, y + 8 * s), (x + 55 * s, y + 10 * s), (x + 49 * s, y + 13 * s), (x + 46 * s, y + 20 * s), (x + 43 * s, y + 13 * s), (x + 37 * s, y + 10 * s), (x + 43 * s, y + 8 * s)]
        draw.polygon([(c(px), c(py)) for px, py in pts2], fill=hex_rgba(THEME["white"]))
    elif kind == "cloud":
        pts = [(x + 10 * s, y + 42 * s), (x + 3 * s, y + 33 * s), (x + 8 * s, y + 21 * s), (x + 22 * s, y + 13 * s), (x + 38 * s, y + 12 * s), (x + 52 * s, y + 19 * s), (x + 57 * s, y + 32 * s), (x + 49 * s, y + 42 * s)]
        draw.polygon([(c(px), c(py)) for px, py in pts], fill=hex_rgba(color, 180), outline=hex_rgba(THEME["white"]))
        draw_line(ex, draw, pts + [pts[0]], THEME["white"], 2)
    elif kind == "folder":
        draw_line(ex, draw, [(x, y + 9 * s), (x, y + 35 * s), (x + 48 * s, y + 35 * s), (x + 48 * s, y + 7 * s), (x + 26 * s, y + 7 * s), (x + 21 * s, y), (x + 2 * s, y), (x + 2 * s, y + 9 * s)], THEME["white"], 2)
        draw_rect(ex, draw, x + 5 * s, y + 15 * s, 38 * s, 15 * s, color, color, 1, 3)
    elif kind == "shield":
        pts = [(x + 38 * s, y + 7 * s), (x + 63 * s, y + 17 * s), (x + 58 * s, y + 47 * s), (x + 38 * s, y + 65 * s), (x + 18 * s, y + 47 * s), (x + 13 * s, y + 17 * s)]
        draw.polygon([(c(px), c(py)) for px, py in pts], fill=hex_rgba(color, 180), outline=hex_rgba(THEME["white"]))
        draw_line(ex, draw, pts + [pts[0]], THEME["white"], 3)
        draw_line(ex, draw, [(x + 27 * s, y + 37 * s), (x + 36 * s, y + 48 * s), (x + 51 * s, y + 27 * s)], THEME["white"], 4)
    elif kind == "scan":
        draw_ellipse(ex, draw, x + 14, y + 11, 38, 38, THEME["white"], None, 4)
        draw_line(ex, draw, [(x + 47, y + 45), (x + 64, y + 62)], THEME["white"], 5)


def brand(ex, draw, signature):
    dots = [
        (0, 0, THEME["cyan"]), (10, 8, THEME["white"]), (0, 16, THEME["purple"]), (10, 24, THEME["white"]),
        (20, 0, THEME["white"]), (30, 8, THEME["pink"]), (20, 16, THEME["white"]), (30, 24, THEME["green"]),
    ]
    for dx, dy, color in dots:
        draw_ellipse(ex, draw, 950 + dx, 128 + dy, 5, 5, color, color, 1)
    draw_text(ex, draw, signature, 994, 120, 140, 38, 23, THEME["white"], "left", bold=True)


def draw_title(ex, draw):
    draw_line(ex, draw, [(29, 31), (29, 82)], THEME["purple"], 11)
    draw_text(ex, draw, "把电脑上的", 45, 16, 262, 66, 44, THEME["white"], "left", bold=True)
    draw_rect(ex, draw, 314, 22, 436, 72, THEME["highlight"], THEME["highlight"], 2, 16)
    draw_text(ex, draw, "Pi 带在身边", 334, 32, 396, 52, 40, THEME["green"], "center", bold=True)
    draw_text(ex, draw, "在手机上继续用电脑里的 Pi · 项目、会话、凭据都留在电脑上", 110, 98, 640, 22, 15, THEME["muted"], "left")


def feature_row(ex, draw, x, y, w, icon_kind, icon_color, title, body, stroke, fill):
    draw_rect(ex, draw, x, y, w, 74, stroke, fill, 2, 10)
    icon(ex, draw, icon_kind, x + 16, y + 13, icon_color, 0.78)
    draw_text(ex, draw, title, x + 74, y + 12, w - 90, 28, 18, THEME["white"], "left", bold=True)
    draw_text(ex, draw, body, x + 74, y + 42, w - 90, 22, 13, THEME["muted"], "left")


def draw_phone_panel(ex, draw):
    draw_rect(ex, draw, 55, 165, 330, 375, THEME["green"], "#02160a", 2, 14)
    icon(ex, draw, "iphone", 88, 182, THEME["cyan"], 1.05)
    draw_text(ex, draw, "你的手机 · 平板", 152, 190, 160, 34, 25, THEME["white"], "left", bold=True)
    draw_text(ex, draw, "Pix App", 152, 226, 160, 22, 14, THEME["green"], "left")

    rows = [
        ("chat", THEME["cyan"], "发消息 · 传图片", "想到就发,不用守在电脑前"),
        ("sparkle", THEME["pink"], "看 AI 实时回复", "干活进度随时刷新"),
        ("folder", THEME["amber"], "接着上次继续聊", "历史会话都在"),
    ]
    for y, (kind, color, title, body) in zip([258, 348, 438], rows):
        feature_row(ex, draw, 75, y, 290, kind, color, title, body, "#2e7d5a", THEME["green_fill"])


def draw_computer_panel(ex, draw):
    draw_rect(ex, draw, 825, 165, 330, 375, THEME["blue"], THEME["blue_fill"], 2, 14)
    icon(ex, draw, "mac", 858, 184, THEME["amber"], 0.95)
    draw_text(ex, draw, "你的电脑", 936, 188, 130, 34, 25, THEME["white"], "left", bold=True)
    draw_text(ex, draw, "Mac · Linux", 936, 224, 130, 22, 14, THEME["blue_soft"], "left")

    rows = [
        ("shield", THEME["cyan"], "Pix Host", "门卫,只放行你批准的设备"),
        ("sparkle", THEME["amber"], "Pi 助手", "真正干活的 AI"),
        ("folder", THEME["pink"], "你的项目", "文件都在这里"),
    ]
    for i, (y, (kind, color, title, body)) in enumerate(zip([258, 348, 438], rows)):
        feature_row(ex, draw, 845, y, 290, kind, color, title, body, "#39618f", THEME["blue_fill"])
        if i < 2:
            draw_line(ex, draw, [(990, y + 76), (990, y + 90)], THEME["blue_soft"], 2, arrow=True)
    draw_text(ex, draw, "Mac 常驻菜单栏 · 点开就管", 845, 514, 290, 20, 12, THEME["faint"], "center")


def draw_connector(ex, draw):
    draw_rect(ex, draw, 455, 225, 300, 42, THEME["green"], THEME["green_fill"], 2, 21)
    draw_text(ex, draw, "同一网络 · 直接连接", 455, 231, 300, 30, 18, THEME["green"], "center", bold=True)
    draw_line(ex, draw, [(415, 300), (795, 300)], THEME["green"], 3, arrow=True)
    draw_line(ex, draw, [(795, 336), (415, 336)], THEME["cyan"], 3, arrow=True)
    draw_rect(ex, draw, 455, 378, 300, 42, THEME["amber"], "#241a04", 2, 21)
    draw_text(ex, draw, "不同网络 · 加密接力", 455, 384, 300, 30, 18, THEME["amber"], "center", bold=True)

    draw_rect(ex, draw, 465, 436, 280, 120, THEME["purple"], THEME["purple_fill"], 2, 12)
    icon(ex, draw, "cloud", 484, 446, THEME["cyan"], 0.8)
    draw_text(ex, draw, "Pix 接力站", 556, 446, 176, 28, 18, THEME["white"], "left", bold=True)
    draw_text(ex, draw, "只转发加密数据\n看不到你的内容", 556, 478, 176, 42, 12, THEME["muted"], "left")
    draw_text(ex, draw, "高级用户可自行部署", 556, 522, 176, 18, 11, THEME["faint"], "left")


def draw_hero(ex, draw):
    draw_rect(ex, draw, 140, 580, 930, 94, THEME["green"], THEME["green_fill"], 3, 18)
    icon(ex, draw, "folder", 172, 602, THEME["green"], 0.95)
    draw_text(ex, draw, "你的电脑才是真正的工作区", 235, 592, 640, 42, 24, THEME["white"], "left", bold=True)
    draw_text(ex, draw, "项目文件 · Pi 会话 · 开发环境 · 凭据 —— 不会搬上云端", 235, 638, 760, 26, 14, THEME["muted"], "left")


def draw_onboarding(ex, draw):
    draw_rect(ex, draw, 55, 700, 1100, 150, THEME["amber"], "#0e0b04", 2, 14)
    draw_text(ex, draw, "第一次 · 只需一次", 85, 714, 200, 28, 17, THEME["amber"], "left", bold=True)
    steps = [("电脑安装 Pix", THEME["cyan"]), ("手机扫码配对", THEME["green"]), ("电脑上点确认", THEME["pink"])]
    xs = [300, 574, 848]
    for i, ((label, color), x) in enumerate(zip(steps, xs)):
        draw_rect(ex, draw, x, 706, 238, 46, color, "#101018", 2, 10)
        draw_ellipse(ex, draw, x + 14, 718, 22, 22, color, color, 1)
        draw_text(ex, draw, str(i + 1), x + 14, 720, 22, 18, 13, THEME["bg"], "center", bold=True)
        draw_text(ex, draw, label, x + 44, 714, 186, 30, 16, THEME["white"], "left", bold=True)
        if i < 2:
            draw_line(ex, draw, [(x + 246, 729), (x + 268, 729)], THEME["amber"], 2, arrow=True)
    draw_text(ex, draw, "以后 · 无需扫码", 85, 782, 200, 28, 17, THEME["green"], "left", bold=True)
    draw_text(ex, draw, "打开 Pix,就能接着上次继续", 300, 778, 700, 34, 17, THEME["white"], "left", bold=True)


def draw_checklist(ex, draw):
    draw_text(ex, draw, "为什么放心", 70, 876, 160, 24, 15, THEME["muted"], "left", bold=True)
    chips = [
        ("只有配对的设备", "陌生设备一律拒绝"),
        ("只有授权的文件夹", "没开放的看不到"),
        ("云端看不到内容", "接力只转发密文"),
        ("断线 Pi 也不停", "回来接着看结果"),
        ("无需 Pix 账户", "配好即用"),
    ]
    for x, (title, body) in zip([55, 277, 499, 721, 943], chips):
        draw_rect(ex, draw, x, 908, 212, 84, THEME["green"], "#04200f", 2, 12)
        icon(ex, draw, "shield", x + 12, y := 918, THEME["green"], 0.62)
        draw_text(ex, draw, title, x + 58, 924, 148, 26, 15, THEME["white"], "left", bold=True)
        draw_text(ex, draw, body, x + 58, 954, 148, 22, 12, THEME["muted"], "left")
    draw_text(ex, draw, "Pix — 让 Pi 跟着你走,东西都留在你电脑上", 205, 1036, 800, 30, 15, THEME["green"], "center")


def render_static():
    ex = Excal(W, H)
    img = Image.new("RGBA", (W * SCALE, H * SCALE), hex_rgba(THEME["bg"]))
    draw = ImageDraw.Draw(img)

    draw_title(ex, draw)
    draw_rect(ex, draw, 18, 117, 1174, 994, THEME["frame"], None, 2, 29)
    brand(ex, draw, "@Pix")

    draw_phone_panel(ex, draw)
    draw_computer_panel(ex, draw)
    draw_connector(ex, draw)
    draw_hero(ex, draw)
    draw_onboarding(ex, draw)
    draw_checklist(ex, draw)

    for x, y, color in [(395, 148, THEME["cyan"]), (72, 556, THEME["green"]), (1136, 556, THEME["purple"])]:
        draw_line(ex, draw, [(x - 8, y), (x + 8, y)], color, 2)
        draw_line(ex, draw, [(x, y - 8), (x, y + 8)], color, 2)

    return ex, img.resize((W, H), Image.Resampling.LANCZOS).convert("RGB")


def premium_finish(base):
    width, height = base.size
    img = base.convert("RGBA")
    glow = Image.new("RGBA", (width, height), (0, 0, 0, 0))
    g = ImageDraw.Draw(glow)
    for rect, color, line_width in [
        ((18, 117, 1192, 1111), THEME["frame"], 3),
        ((55, 165, 385, 540), THEME["green"], 3),
        ((825, 165, 1155, 540), THEME["blue"], 3),
        ((465, 436, 745, 556), THEME["purple"], 2),
        ((140, 580, 1070, 674), THEME["green"], 3),
        ((55, 700, 1155, 850), THEME["amber"], 2),
        ((314, 22, 750, 94), THEME["green"], 2),
    ]:
        g.rounded_rectangle(rect, radius=18, outline=hex_rgba(color, 70), width=line_width)
    img.alpha_composite(glow.filter(ImageFilter.GaussianBlur(4)))

    grain = Image.new("RGBA", (width, height), (0, 0, 0, 0))
    gd = ImageDraw.Draw(grain)
    rng = random.Random(2069769416930414980)
    for _ in range(2600):
        x = rng.randrange(width)
        y = rng.randrange(height)
        tone = rng.randrange(120, 220)
        gd.point((x, y), fill=(tone, tone, tone, rng.randrange(4, 14)))
    img.alpha_composite(grain)

    mask_small = Image.new("L", (180, 170), 0)
    pixels = []
    cx, cy = 90, 78
    max_dist = math.dist((0, 0), (cx, cy))
    for y in range(170):
        for x in range(180):
            dist = math.dist((x, y), (cx, cy)) / max_dist
            pixels.append(int(max(0, min(115, (dist - 0.38) * 150))))
    mask_small.putdata(pixels)
    mask = mask_small.resize((width, height), Image.Resampling.BICUBIC)
    vignette = Image.new("RGBA", (width, height), (0, 0, 0, 0))
    vignette.putalpha(mask)
    img.alpha_composite(vignette)
    return img.convert("RGB")


def path_len(points):
    return sum(math.dist(a, b) for a, b in zip(points, points[1:]))


def point_at_distance(points, distance):
    left = distance
    for a, b in zip(points, points[1:]):
        seg = math.dist(a, b)
        if seg == 0:
            continue
        if left <= seg:
            t = left / seg
            return (a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t)
        left -= seg
    return points[-1]


def point_at_fraction(points, t):
    return point_at_distance(points, (t % 1.0) * path_len(points))


def draw_glow_dot(draw, x, y, color, strength=1.0):
    for radius, alpha in [(15, 42), (10, 70), (5, 210)]:
        a = int(alpha * strength)
        draw.ellipse((x - radius, y - radius, x + radius, y + radius), fill=hex_rgba(color, a))
    draw.ellipse((x - 2, y - 2, x + 2, y + 2), fill=hex_rgba(THEME["white"], 245))


def pulse_rect(draw, rect, color, phase, radius=10):
    x1, y1, x2, y2 = rect
    alpha = int(70 + 70 * (0.5 + 0.5 * math.sin(phase)))
    for grow, width in [(0, 2), (4, 2), (8, 1)]:
        draw.rounded_rectangle((x1 - grow, y1 - grow, x2 + grow, y2 + grow), radius=radius + grow, outline=hex_rgba(color, max(25, alpha - grow * 8)), width=width)


def animate_frame(base, idx, total):
    frame = base.convert("RGBA")
    overlay = Image.new("RGBA", frame.size, (0, 0, 0, 0))
    draw = ImageDraw.Draw(overlay)
    progress = idx / total
    paths = [
        ([(415, 300), (795, 300)], THEME["green"], 0.00),
        ([(795, 336), (415, 336)], THEME["cyan"], 0.30),
        ([(546, 729), (568, 729)], THEME["amber"], 0.12),
        ([(820, 729), (842, 729)], THEME["amber"], 0.22),
        ([(990, 338), (990, 350)], THEME["blue_soft"], 0.40),
        ([(990, 428), (990, 440)], THEME["blue_soft"], 0.50),
    ]
    for points, color, offset in paths:
        for trail, strength in [(0, 1.0), (-0.035, 0.72), (-0.07, 0.44)]:
            x, y = point_at_fraction(points, progress + offset + trail)
            draw_glow_dot(draw, x, y, color, strength)
    pulse_targets = [
        ((55, 165, 385, 540), THEME["green"]),
        ((825, 165, 1155, 540), THEME["blue"]),
        ((465, 436, 745, 556), THEME["purple"]),
        ((140, 580, 1070, 674), THEME["green"]),
        ((55, 700, 1155, 850), THEME["amber"]),
    ]
    active = (idx // 8) % len(pulse_targets)
    for pos, (rect, color) in enumerate(pulse_targets):
        if pos == active:
            pulse_rect(draw, rect, color, progress * math.tau * 2, 14)
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


def write_outputs(outdir, basename):
    outdir.mkdir(parents=True, exist_ok=True)
    ex, static = render_static()
    final = premium_finish(static)
    png_path = outdir / f"{basename}.png"
    gif_path = outdir / f"{basename}.gif"
    excalidraw_path = outdir / f"{basename}.excalidraw"
    final.save(png_path, "PNG")
    frames = [animate_frame(final, i, FRAMES) for i in range(FRAMES)]
    frames[0].save(gif_path, save_all=True, append_images=frames[1:], duration=int(1000 / FPS), loop=0, optimize=False)
    ex.write(excalidraw_path)
    return {"png": str(png_path), "gif": str(gif_path), "excalidraw": str(excalidraw_path), "elements": len(ex.elements)}


def check_outputs(result):
    checks = []
    with Image.open(result["gif"]) as gif:
        checks.append({"name": "gif_size", "ok": gif.size == (W, H), "actual": gif.size})
        checks.append({"name": "gif_frames", "ok": gif.n_frames == FRAMES, "actual": gif.n_frames})
        duration = gif.info.get("duration")
        checks.append({"name": "gif_fps", "ok": duration == int(1000 / FPS), "actual": round(1000 / duration, 2) if duration else None})
    report = frame_diff_report(result["gif"])
    checks.append({"name": "gif_has_motion", "ok": any(d["changed_pixels"] > 0 for d in report["diffs"])})
    excalidraw = json.loads(Path(result["excalidraw"]).read_text(encoding="utf-8"))
    elements = excalidraw.get("elements", [])
    ids = [e.get("id") for e in elements]
    texts = [e for e in elements if e.get("type") == "text"]
    checks.append({"name": "excalidraw_unique_ids", "ok": len(ids) == len(set(ids))})
    checks.append({"name": "excalidraw_text_font_family", "ok": all(e.get("fontFamily") == 5 for e in texts)})
    checks.append({"name": "excalidraw_files_empty", "ok": excalidraw.get("files") == {}})
    with Image.open(result["png"]) as png:
        checks.append({"name": "png_size", "ok": png.size == (W, H), "actual": png.size})
    return {"ok": all(c["ok"] for c in checks), "checks": checks}


def main():
    parser = argparse.ArgumentParser(description="Render the Pix product explainer diagram.")
    parser.add_argument("--outdir", type=Path, default=Path(__file__).resolve().parent)
    parser.add_argument("--basename", default="pix-architecture")
    args = parser.parse_args()
    result = write_outputs(args.outdir, args.basename)
    result["verification"] = frame_diff_report(result["gif"])
    result["checks"] = check_outputs(result)
    print(json.dumps(result, ensure_ascii=False, indent=2))
    if not result["checks"]["ok"]:
        sys.exit(1)


if __name__ == "__main__":
    main()
