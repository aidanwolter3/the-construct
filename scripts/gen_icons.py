#!/usr/bin/env python3
"""App icon generator: "the construct" (a bright white infinite space with a perspective grid floor and one cube).

One geometry definition (scene()) is emitted three ways, so every output matches:
  * SVG          assets/icon/construct.svg (full-bleed tile), assets/icon/construct-macos.svg (macOS margin + shadow)
  * Android      VectorDrawables (adaptive background / foreground / monochrome) in crates/construct-render/android/res
  * PNG          rasterised with Pillow (4x supersampled), for the Linux PNG, legacy mipmaps,
                 macOS .icns (via iconutil) and Windows .ico

Detail levels: "full" (large PNGs, SVG master), "launcher" (Android adaptive layers: heavier lines, bigger cube, since
launchers show them at ~48 dp inside a circle), "small" (48 px PNGs: fewer, heavier lines) and "tiny" (PNGs <= 32 px:
a few heavy grid lines, a darker sky/floor and frame so the tile reads on white, a dark cube outline).
Run via scripts/gen_icons.sh (needs python3 + Pillow + numpy; iconutil on macOS for the .icns).
"""
import math
import os
import shutil
import subprocess
import sys
import tempfile

import numpy as np
from PIL import Image, ImageDraw, ImageFilter

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
ICON_DIR = os.path.join(ROOT, "assets", "icon")
ANDROID_RES = os.path.join(ROOT, "crates", "construct-render", "android", "res")

# ---------------------------------------------------------------------------------------------- geometry
# Art space: the unit square (y down). Ops may extend beyond it; every target clips to its own frame.

SKY_TOP, SKY_HORIZON = "#E9EDF1", "#FFFFFF"
FLOOR_HORIZON, FLOOR_NEAR = "#FFFFFF", "#DCE1E7"
GRID = "#98A1AC"

# Per detail level: grid spacing (world), stroke width (art units), farthest depth line, haze depth, cube size,
# grid colour (smaller renderings get darker lines), cube edge width factor. Optional overrides (tiny only): sky / floor
# gradient ends, cube edge and face colours, tile frame colour.
DETAIL = {
    "full":     dict(cell=0.45, line_w=0.014, z_far=6.0, haze_z=1.25, cube=1.12, grid=GRID, edge_k=1.15),
    "launcher": dict(cell=0.52, line_w=0.022, z_far=5.0, haze_z=1.35, cube=1.32, grid="#8A94A0", edge_k=1.1),
    "small":    dict(cell=0.62, line_w=0.034, z_far=3.8, haze_z=1.5, cube=1.3, grid="#85909C", edge_k=1.0),
    "tiny":     dict(cell=0.8, line_w=0.05, z_far=2.4, haze_z=2.2, cube=1.3, grid="#77828F", edge_k=1.3,
                     sky_top="#D9DFE6", floor_near="#D3D9E0", edge="#4A5563", face_left="#D2D8E0",
                     face_right="#E8ECF1", frame="#8E98A4"),
}
FRAME = "#C3C9D1"


def detail_for(size):
    return "tiny" if size <= 32 else "small" if size <= 48 else "full"
HAZE = "#FFFFFF"
EDGE = "#768190"
FACE_TOP, FACE_LEFT, FACE_RIGHT = "#FFFFFF", "#E3E7EC", "#F2F4F7"
SHADOW = "#7D8794"

HORIZON = 0.40     # horizon height in art space
FOCAL = 0.62       # projection scale
CAM_H = 1.25       # camera height above the floor (world units)


def project(x, y, z):
    """World (x right, y up, z forward; camera at (0, CAM_H, 0)) -> art space."""
    return (0.5 + FOCAL * x / z, HORIZON + FOCAL * (CAM_H - y) / z)


def scene(detail="full"):
    """Returns the icon as a list of layers {name: [ops]}; ops are plain tuples understood by every emitter."""
    d = DETAIL[detail]
    cell, line_w, z_far = d["cell"], d["line_w"], d["z_far"]

    bg, fg = [], []
    # Sky and floor gradients (extend past the unit square so adaptive-icon viewports are covered).
    bg.append(("grad", (-1, -1, 2, HORIZON), [(0.0, d.get("sky_top", SKY_TOP), 1), (1.0, SKY_HORIZON, 1)]))
    bg.append(("grad", (-1, HORIZON, 2, 2), [(0.0, FLOOR_HORIZON, 1), (1.0, d.get("floor_near", FLOOR_NEAR), 1)]))

    # Depth (horizontal) lines at regular world spacing -> denser toward the horizon.
    lines = []
    z = cell
    while z <= z_far + 1e-6:
        y = project(0, 0, z)[1]
        if y < 2:
            lines.append(((-1, y), (2, y)))
        z += cell
    # Lateral lines converge on the vanishing point; they start at the farthest depth line (no blob at the VP).
    # Offset by half a cell so no line runs through the centre (the cube sits there).
    for k in range(-14, 14):
        x = (k + 0.5) * cell
        p_far = project(x, 0, z_far)
        p_near = project(x, 0, 0.12)
        lines.append((p_far, p_near))
    bg.append(("lines", lines, d["grid"], line_w))

    # Haze: fades the grid into the horizon so the space reads as infinite.
    haze_end = project(0, 0, d["haze_z"])[1]
    bg.append(("grad", (-1, HORIZON - 0.004, 2, haze_end), [(0.0, HAZE, 1), (0.25, HAZE, 1), (0.45, HAZE, 0.7), (1.0, HAZE, 0)]))

    # One cube, rotated 45 degrees, standing on the grid: a hint of "construction". Drawn axonometrically
    # (elevation matching the grid's view angle at its depth) so it does not look distorted by the wide grid camera.
    s = d["cube"]
    elev = math.radians(26)
    zc = CAM_H / math.tan(elev)
    a = FOCAL / zc
    h = s / math.sqrt(2)
    bx, by = project(0, 0, zc)
    def cube_pt(x, y, z):  # world offsets around the cube's base centre
        return (bx + a * x, by + a * (z * math.sin(elev) - y * math.cos(elev)))
    base = {"f": (0, h), "l": (-h, 0), "r": (h, 0), "b": (0, -h)}  # (x, toward-viewer)
    P = {k + "0": cube_pt(x, 0, z) for k, (x, z) in base.items()}
    P.update({k + "1": cube_pt(x, s, z) for k, (x, z) in base.items()})
    # Contact shadow: the footprint pushed back and spread a little.
    sh = [cube_pt(x * 1.2 + 0.08, 0, z * 1.2 - 0.1) for (x, z) in base.values()]
    fg.append(("poly", [sh[0], sh[1], sh[3], sh[2]], SHADOW, 0.20))
    fg.append(("poly", [P["f0"], P["l0"], P["l1"], P["f1"]], d.get("face_left", FACE_LEFT), 1))
    fg.append(("poly", [P["f0"], P["r0"], P["r1"], P["f1"]], d.get("face_right", FACE_RIGHT), 1))
    fg.append(("poly", [P["f1"], P["l1"], P["b1"], P["r1"]], FACE_TOP, 1))
    edges = [("f0", "l0"), ("f0", "r0"), ("f0", "f1"), ("l0", "l1"), ("r0", "r1"),
             ("f1", "l1"), ("f1", "r1"), ("l1", "b1"), ("r1", "b1")]
    fg.append(("lines", [(P[a], P[b]) for a, b in edges], d.get("edge", EDGE), line_w * d["edge_k"]))
    return {"background": bg, "foreground": fg}


def all_ops(sc):
    return sc["background"] + sc["foreground"]


def convex_hull(pts):
    pts = sorted(set(pts))
    cross = lambda o, a, b: (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])
    lower, upper = [], []
    for p in pts:
        while len(lower) >= 2 and cross(lower[-2], lower[-1], p) <= 0:
            lower.pop()
        lower.append(p)
    for p in reversed(pts):
        while len(upper) >= 2 and cross(upper[-2], upper[-1], p) <= 0:
            upper.pop()
        upper.append(p)
    return lower[:-1] + upper[:-1]


def outside_parts(a, b, hull):
    """Parts of segment a-b outside the convex polygon `hull` (Cyrus-Beck)."""
    (x0, y0), (x1, y1) = a, b
    dx, dy = x1 - x0, y1 - y0
    t0, t1 = 0.0, 1.0
    n = len(hull)
    # Orientation-independent: inside means same side as the centroid for every edge.
    cx = sum(p[0] for p in hull) / n
    cy = sum(p[1] for p in hull) / n
    for i in range(n):
        (px, py), (qx, qy) = hull[i], hull[(i + 1) % n]
        nx, ny = qy - py, -(qx - px)
        if nx * (cx - px) + ny * (cy - py) < 0:
            nx, ny = -nx, -ny
        num = nx * (x0 - px) + ny * (y0 - py)   # >= 0 inside
        den = nx * dx + ny * dy
        if abs(den) < 1e-12:
            if num < 0:
                return [(a, b)]
            continue
        t = -num / den
        if den > 0:
            t0 = max(t0, t)
        else:
            t1 = min(t1, t)
        if t0 >= t1:
            return [(a, b)]
    pt = lambda t: (x0 + dx * t, y0 + dy * t)
    out = []
    if t0 > 1e-6:
        out.append((a, pt(t0)))
    if t1 < 1 - 1e-6:
        out.append((pt(t1), b))
    return out


def mono_ops(detail="full"):
    """Monochrome (themed icon) layer, one colour, line art only: the nearer grid (a single-colour layer has no haze)
    with the cube's silhouette cut out of it, plus the cube's edges."""
    sc = scene(detail)
    cube_pts = [p for op in sc["foreground"] if op[0] == "poly" and op[2] != SHADOW for p in op[1]]
    hull = convex_hull(cube_pts)
    cut = project(0, 0, 2.2)[1]
    out = []
    for op in sc["background"]:
        if op[0] != "lines":
            continue
        kept = []
        for (a, b) in op[1]:
            if a[1] == b[1]:
                if a[1] < cut:
                    continue
            else:
                (x0, y0), (x1, y1) = a, b
                t = (cut - y0) / (y1 - y0)
                a = (x0 + (x1 - x0) * t, cut)
            kept.extend(outside_parts(a, b, hull))
        out.append(("lines", kept, "#000000", op[3] * 1.2))
    for op in sc["foreground"]:
        if op[0] == "lines":
            out.append(("lines", op[1], "#000000", op[3] * 1.2))
    return out


# ---------------------------------------------------------------------------------------------- helpers

def rgb(hex_):
    hex_ = hex_.lstrip("#")
    return tuple(int(hex_[i:i + 2], 16) for i in (0, 2, 4))


def fmt(v):
    s = f"{v:.3f}".rstrip("0").rstrip(".")
    return "0" if s in ("-0", "") else s


# ---------------------------------------------------------------------------------------------- SVG

def svg(ops, size, art_rect, clip=None, shadow=False):
    """art_rect = (x, y, w): where the unit square lands in the SVG; clip = (x, y, w, radius) rounded rect."""
    ax, ay, aw = art_rect
    T = lambda p: (ax + p[0] * aw, ay + p[1] * aw)
    defs, body, gid = [], [], 0
    for op in ops:
        kind = op[0]
        if kind == "grad":
            (x0, y0, x1, y1), stops = op[1], op[2]
            gid += 1
            p0, p1 = T((x0, y0)), T((x1, y1))
            st = "".join(f'<stop offset="{fmt(o)}" stop-color="{c}" stop-opacity="{fmt(a)}"/>' for o, c, a in stops)
            defs.append(f'<linearGradient id="g{gid}" gradientUnits="userSpaceOnUse" x1="0" y1="{fmt(p0[1])}" '
                        f'x2="0" y2="{fmt(p1[1])}">{st}</linearGradient>')
            body.append(f'<rect x="{fmt(p0[0])}" y="{fmt(p0[1])}" width="{fmt(p1[0] - p0[0])}" '
                        f'height="{fmt(p1[1] - p0[1])}" fill="url(#g{gid})"/>')
        elif kind == "lines":
            d = " ".join(f"M{fmt(T(a)[0])} {fmt(T(a)[1])}L{fmt(T(b)[0])} {fmt(T(b)[1])}" for a, b in op[1])
            body.append(f'<path d="{d}" stroke="{op[2]}" stroke-width="{fmt(op[3] * aw)}" '
                        f'stroke-linecap="round" fill="none"/>')
        elif kind == "poly":
            pts = " ".join(f"{fmt(T(p)[0])},{fmt(T(p)[1])}" for p in op[1])
            body.append(f'<polygon points="{pts}" fill="{op[2]}" fill-opacity="{fmt(op[3])}" stroke-linejoin="round"/>')
    content = "\n    ".join(body)
    if clip:
        cx, cy, cw, cr = clip
        defs.append(f'<clipPath id="clip"><rect x="{fmt(cx)}" y="{fmt(cy)}" width="{fmt(cw)}" height="{fmt(cw)}" '
                    f'rx="{fmt(cr)}"/></clipPath>')
        frame = (f'<rect x="{fmt(cx)}" y="{fmt(cy)}" width="{fmt(cw)}" height="{fmt(cw)}" rx="{fmt(cr)}" '
                 f'fill="none" stroke="#C3C9D1" stroke-width="{fmt(size / 512)}"/>')
        if shadow:
            defs.append('<filter id="shadow" x="-20%" y="-20%" width="140%" height="140%">'
                        f'<feDropShadow dx="0" dy="{fmt(size * 0.012)}" stdDeviation="{fmt(size * 0.014)}" '
                        'flood-color="#000000" flood-opacity="0.28"/></filter>')
            body_pre = (f'<rect x="{fmt(cx)}" y="{fmt(cy)}" width="{fmt(cw)}" height="{fmt(cw)}" rx="{fmt(cr)}" '
                        f'fill="#FFFFFF" filter="url(#shadow)"/>\n  ')
        else:
            body_pre = ""
        content = f'{body_pre}<g clip-path="url(#clip)">\n    {content}\n  </g>\n  {frame}'
    return (f'<?xml version="1.0" encoding="UTF-8"?>\n'
            f'<!-- Generated by scripts/gen_icons.py; do not edit by hand. -->\n'
            f'<svg xmlns="http://www.w3.org/2000/svg" width="{size}" height="{size}" viewBox="0 0 {size} {size}">\n'
            f'  <defs>{"".join(defs)}</defs>\n  {content}\n</svg>\n')


# ---------------------------------------------------------------------------------------------- VectorDrawable

def vector_drawable(ops):
    """Adaptive icon layer: 108x108 viewport, the unit square mapped to the 72 dp visible area (18..90)."""
    T = lambda p: (18 + p[0] * 72, 18 + p[1] * 72)
    out = []
    for op in ops:
        kind = op[0]
        if kind == "grad":
            (x0, y0, x1, y1), stops = op[1], op[2]
            a, b = T((max(x0, -0.25), max(y0, -0.25))), T((min(x1, 1.25), min(y1, 1.25)))
            items = "".join(f'\n                <item android:offset="{fmt(o)}" android:color="#{int(round(al * 255)):02X}{c[1:]}"/>'
                            for o, c, al in stops)
            out.append(
                f'    <path android:pathData="M{fmt(a[0])},{fmt(a[1])}H{fmt(b[0])}V{fmt(b[1])}H{fmt(a[0])}Z">\n'
                f'        <aapt:attr name="android:fillColor">\n'
                f'            <gradient android:type="linear" android:startX="0" android:startY="{fmt(a[1])}" '
                f'android:endX="0" android:endY="{fmt(b[1])}">{items}\n            </gradient>\n'
                f'        </aapt:attr>\n    </path>')
        elif kind == "lines":
            d = "".join(f"M{fmt(T(a)[0])},{fmt(T(a)[1])}L{fmt(T(b)[0])},{fmt(T(b)[1])}" for a, b in op[1])
            out.append(f'    <path android:pathData="{d}" android:strokeColor="{op[2]}" '
                       f'android:strokeWidth="{fmt(op[3] * 72)}" android:strokeLineCap="round" '
                       f'android:strokeLineJoin="round"/>')
        elif kind == "poly":
            d = "M" + "L".join(f"{fmt(T(p)[0])},{fmt(T(p)[1])}" for p in op[1]) + "Z"
            out.append(f'    <path android:pathData="{d}" android:fillColor="{op[2]}" android:fillAlpha="{fmt(op[3])}"/>')
    return ('<?xml version="1.0" encoding="utf-8"?>\n'
            '<!-- Generated by scripts/gen_icons.py ("the construct" app icon); do not edit by hand. -->\n'
            '<vector xmlns:android="http://schemas.android.com/apk/res/android"\n'
            '    xmlns:aapt="http://schemas.android.com/aapt"\n'
            '    android:width="108dp" android:height="108dp"\n'
            '    android:viewportWidth="108" android:viewportHeight="108">\n'
            + "\n".join(out) + "\n</vector>\n")


# ---------------------------------------------------------------------------------------------- raster

SS = 4  # supersampling


def raster_ops(ops, size, art_rect):
    """Renders ops onto a transparent RGBA image of size x size; art_rect = (x, y, w) in output pixels."""
    n = size * SS
    ax, ay, aw = (v * SS for v in art_rect)
    T = lambda p: (ax + p[0] * aw, ay + p[1] * aw)
    img = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    for op in ops:
        kind = op[0]
        layer = Image.new("RGBA", (n, n), (0, 0, 0, 0))
        if kind == "grad":
            (x0, y0, x1, y1), stops = op[1], op[2]
            px0, py0 = T((x0, y0))
            px1, py1 = T((x1, y1))
            ys = np.arange(n) + 0.5
            t = np.clip((ys - py0) / max(py1 - py0, 1e-9), 0, 1)
            offs = [s[0] for s in stops]
            chans = [np.interp(t, offs, [rgb(s[1])[i] for s in stops]) for i in range(3)]
            alpha = np.interp(t, offs, [s[2] * 255 for s in stops])
            inside = (ys >= py0) & (ys < py1)
            alpha = np.where(inside, alpha, 0)
            col = np.stack(chans + [alpha], axis=1)  # n x 4
            arr = np.repeat(col[:, None, :], n, axis=1)
            xs = np.arange(n) + 0.5
            arr[:, (xs < px0) | (xs >= px1), 3] = 0
            layer = Image.fromarray(arr.astype(np.uint8), "RGBA")
        elif kind == "lines":
            d = ImageDraw.Draw(layer)
            w = max(1, int(round(op[3] * aw)))
            c = rgb(op[2]) + (255,)
            for a, b in op[1]:
                pa, pb = T(a), T(b)
                d.line([pa, pb], fill=c, width=w)
                for p in (pa, pb):
                    d.ellipse([p[0] - w / 2, p[1] - w / 2, p[0] + w / 2, p[1] + w / 2], fill=c)
        elif kind == "poly":
            d = ImageDraw.Draw(layer)
            d.polygon([T(p) for p in op[1]], fill=rgb(op[2]) + (int(round(op[3] * 255)),))
        img = Image.alpha_composite(img, layer)
    return img


def rounded_mask(size, rect, radius):
    n = size * SS
    m = Image.new("L", (n, n), 0)
    x, y, w = (v * SS for v in rect)
    ImageDraw.Draw(m).rounded_rectangle([x, y, x + w - 1, y + w - 1], radius=radius * SS, fill=255)
    return m


def tile_png(size, shape="squircle"):
    """Full-bleed tile (PNG set, legacy Android mipmaps, desktop window icon)."""
    detail = detail_for(size)
    frame_col = rgb(DETAIL[detail].get("frame", FRAME)) + (255,)
    img = raster_ops(all_ops(scene(detail)), size, (0, 0, size))
    n = size * SS
    if shape == "circle":
        mask = Image.new("L", (n, n), 0)
        ImageDraw.Draw(mask).ellipse([0, 0, n - 1, n - 1], fill=255)
    else:
        mask = rounded_mask(size, (0, 0, size), size * 0.2237)
    out = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    out.paste(img, (0, 0), mask)
    # Hairline frame so a white icon still separates from a white background.
    frame = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    fw = max(SS, int(round(n / 256)))
    if shape == "circle":
        ImageDraw.Draw(frame).ellipse([0, 0, n - 1, n - 1], outline=frame_col, width=fw)
    else:
        ImageDraw.Draw(frame).rounded_rectangle([0, 0, n - 1, n - 1], radius=size * 0.2237 * SS,
                                                outline=frame_col, width=fw)
    out = Image.alpha_composite(out, frame)
    return out.resize((size, size), Image.LANCZOS)


def macos_png(size):
    """macOS (Big Sur grid): 824/1024 body, ~185/1024 corner radius, soft drop shadow."""
    detail = detail_for(size)
    k = size / 1024
    bx, bw, br = 100 * k, 824 * k, 185 * k
    n = size * SS
    art = raster_ops(all_ops(scene(detail)), size, (bx, bx, bw))
    mask = rounded_mask(size, (bx, bx, bw), br)
    out = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    shadow = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    ImageDraw.Draw(shadow).rounded_rectangle([bx * SS, (bx + 12 * k) * SS, (bx + bw) * SS, (bx + bw + 12 * k) * SS],
                                             radius=br * SS, fill=(0, 0, 0, 70))
    shadow = shadow.filter(ImageFilter.GaussianBlur(14 * k * SS))
    out = Image.alpha_composite(out, shadow)
    body = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    body.paste(art, (0, 0), mask)
    out = Image.alpha_composite(out, body)
    frame = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    ImageDraw.Draw(frame).rounded_rectangle([bx * SS, bx * SS, (bx + bw) * SS - 1, (bx + bw) * SS - 1],
                                            radius=br * SS, outline=rgb(DETAIL[detail].get("frame", FRAME)) + (255,),
                                            width=max(SS, int(round(2 * k * SS))))
    out = Image.alpha_composite(out, frame)
    return out.resize((size, size), Image.LANCZOS)


# ---------------------------------------------------------------------------------------------- main

def write(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.write(text)


def main():
    os.makedirs(os.path.join(ICON_DIR, "png"), exist_ok=True)

    # SVG masters.
    write(os.path.join(ICON_DIR, "construct.svg"),
          svg(all_ops(scene()), 1024, (0, 0, 1024), clip=(0, 0, 1024, 229)))
    write(os.path.join(ICON_DIR, "construct-small.svg"),
          svg(all_ops(scene("small")), 1024, (0, 0, 1024), clip=(0, 0, 1024, 229)))
    write(os.path.join(ICON_DIR, "construct-macos.svg"),
          svg(all_ops(scene()), 1024, (100, 100, 824), clip=(100, 100, 824, 185), shadow=True))

    # Linux desktop icon (the only PNG size anything uses).
    tile_png(512).save(os.path.join(ICON_DIR, "png", "construct-512.png"))
    # .ico: every entry rendered at its own size (so 16/32 get the "tiny" detail level), not downscaled from 256.
    ico_sizes = (16, 32, 48, 64, 128, 256)
    tile_png(256).save(os.path.join(ICON_DIR, "construct.ico"), sizes=[(s, s) for s in ico_sizes],
                       append_images=[tile_png(s) for s in ico_sizes[:-1]])

    # Android adaptive icon layers (VectorDrawable) + legacy PNG mipmaps.
    sc = scene("launcher")
    write(os.path.join(ANDROID_RES, "drawable", "ic_launcher_background.xml"), vector_drawable(sc["background"]))
    write(os.path.join(ANDROID_RES, "drawable", "ic_launcher_foreground.xml"), vector_drawable(sc["foreground"]))
    write(os.path.join(ANDROID_RES, "drawable", "ic_launcher_monochrome.xml"), vector_drawable(mono_ops("launcher")))
    adaptive = ('<?xml version="1.0" encoding="utf-8"?>\n'
                '<!-- "the construct" app icon; layers generated by scripts/gen_icons.py. -->\n'
                '<adaptive-icon xmlns:android="http://schemas.android.com/apk/res/android">\n'
                '    <background android:drawable="@drawable/ic_launcher_background" />\n'
                '    <foreground android:drawable="@drawable/ic_launcher_foreground" />\n'
                '    <monochrome android:drawable="@drawable/ic_launcher_monochrome" />\n'
                '</adaptive-icon>\n')
    for name in ("ic_launcher", "ic_launcher_round"):
        write(os.path.join(ANDROID_RES, "mipmap-anydpi-v26", f"{name}.xml"), adaptive)
    for dens, s in (("mdpi", 48), ("hdpi", 72), ("xhdpi", 96), ("xxhdpi", 144), ("xxxhdpi", 192)):
        d = os.path.join(ANDROID_RES, f"mipmap-{dens}")
        os.makedirs(d, exist_ok=True)
        tile_png(s).save(os.path.join(d, "ic_launcher.png"))
        tile_png(s, "circle").save(os.path.join(d, "ic_launcher_round.png"))

    # QA preview of the themed (monochrome) layer, not shipped.
    if os.environ.get("ICON_PREVIEW_DIR"):
        pv = os.environ["ICON_PREVIEW_DIR"]
        os.makedirs(pv, exist_ok=True)
        raster_ops([("grad", (-1, -1, 2, 2), [(0, "#FFFFFF", 1), (1, "#FFFFFF", 1)])] + mono_ops(), 432,
                   (72, 72, 288)).resize((432, 432), Image.LANCZOS).save(os.path.join(pv, "monochrome_preview.png"))
        raster_ops(all_ops(sc), 432, (72, 72, 288)).resize((432, 432), Image.LANCZOS).save(
            os.path.join(pv, "adaptive_full_preview.png"))

    # Desktop: the macOS .icns (the app bundle's icon, scripts/macos_app.sh).
    tmp = tempfile.mkdtemp()
    iconset = os.path.join(tmp, "construct.iconset")
    os.makedirs(iconset)
    for s in (16, 32, 128, 256, 512):
        macos_png(s).save(os.path.join(iconset, f"icon_{s}x{s}.png"))
        macos_png(s * 2).save(os.path.join(iconset, f"icon_{s}x{s}@2x.png"))
    macos_png(1024).save(os.path.join(ICON_DIR, "construct-macos-1024.png"))
    if shutil.which("iconutil"):
        subprocess.run(["iconutil", "-c", "icns", iconset, "-o", os.path.join(ICON_DIR, "construct.icns")], check=True)
    else:
        print("iconutil not found (not macOS?): construct.icns left unchanged", file=sys.stderr)
    shutil.rmtree(tmp)
    print("icons written to", ICON_DIR, ANDROID_RES)


if __name__ == "__main__":
    main()
