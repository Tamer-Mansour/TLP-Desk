"""Generate the TLP-Desk application icon and installer wizard images.

Original design (not derived from any other product): a rounded teal tile carrying two
overlapping screens -- a faint "remote" screen behind and a solid "local" monitor in front
with a pointer on it -- i.e. "your desk, on another screen". Small sizes (<= 24 px) use a
simplified single-monitor glyph so the shape stays legible.

Reproduce (needs Pillow):
    python -m venv .venv && .venv\\Scripts\\pip install pillow
    .venv\\Scripts\\python scripts\\make_icon.py

Outputs:
    crates/tlp-desk-app/assets/icon/TLP-Desk.ico        16..256 px, multi-resolution
    crates/tlp-desk-app/assets/icon/TLP-Desk-{256,512,1024}.png
    installer/assets/wizard-*.png, wizard-small-*.png  Inno Setup wizard images
"""

from __future__ import annotations

import os
import struct
import io

from PIL import Image, ImageDraw, ImageFilter

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
ICON_DIR = os.path.join(ROOT, "crates", "tlp-desk-app", "assets", "icon")
WIZ_DIR = os.path.join(ROOT, "installer", "assets")

# Palette: the app's teal accent (ui::theme accent 0x0E7C70 / dark 0x3DB5A6).
TILE_TOP = (0x1F, 0xAE, 0x9B)
TILE_BOTTOM = (0x0A, 0x5C, 0x53)
SCREEN_TOP = (0x13, 0x8C, 0x7E)
SCREEN_BOTTOM = (0x0B, 0x66, 0x5C)
WHITE = (255, 255, 255)

ICO_SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]
SS = 8  # supersampling factor


def vgradient(w: int, h: int, top, bottom) -> Image.Image:
    g = Image.new("RGBA", (1, h))
    for y in range(h):
        t = y / max(1, h - 1)
        g.putpixel((0, y), tuple(round(a + (b - a) * t) for a, b in zip(top, bottom)) + (255,))
    return g.resize((w, h))


def mask_rr(size, box, radius) -> Image.Image:
    m = Image.new("L", size, 0)
    ImageDraw.Draw(m).rounded_rectangle(box, radius=radius, fill=255)
    return m


def paste_fill(img: Image.Image, fill: Image.Image | tuple, mask: Image.Image) -> None:
    if isinstance(fill, tuple):
        fill = Image.new("RGBA", img.size, fill + (255,) if len(fill) == 3 else fill)
    img.paste(fill, (0, 0), mask)


def draw_monitor(img, s, x0, y0, x1, y1, bezel, radius, stand=True, pointer=True):
    """White monitor with gradient screen; coordinates in 1024-unit space scaled by s."""
    W, H = img.size
    k = s / 1024.0
    X0, Y0, X1, Y1 = (round(v * k) for v in (x0, y0, x1, y1))
    R = round(radius * k)
    B = max(1, round(bezel * k))
    if stand:
        cx = (X0 + X1) / 2
        neck_w = (X1 - X0) * 0.16
        base_w = (X1 - X0) * 0.48
        neck_h = (Y1 - Y0) * 0.17
        base_h = max(1, (Y1 - Y0) * 0.11)
        d = ImageDraw.Draw(img)
        d.rectangle([cx - neck_w / 2, Y1 - R, cx + neck_w / 2, Y1 + neck_h], fill=WHITE + (255,))
        d.rounded_rectangle(
            [cx - base_w / 2, Y1 + neck_h - 1, cx + base_w / 2, Y1 + neck_h + base_h],
            radius=base_h / 2,
            fill=WHITE + (255,),
        )
    paste_fill(img, WHITE, mask_rr((W, H), [X0, Y0, X1, Y1], R))
    inner = [X0 + B, Y0 + B, X1 - B, Y1 - B]
    scr = vgradient(W, H, SCREEN_TOP, SCREEN_BOTTOM)
    img.paste(scr, (0, 0), mask_rr((W, H), inner, max(0, R - B)))
    if pointer:
        # Arrow pointer, tip at upper-left of centre.
        iw, ih = inner[2] - inner[0], inner[3] - inner[1]
        u = ih / 100.0
        tx, ty = inner[0] + iw * 0.40, inner[1] + ih * 0.20
        pts = [
            (0, 0), (0, 62), (15, 48), (26, 72), (37, 67), (26, 44), (46, 44),
        ]
        poly = [(tx + px * u, ty + py * u) for px, py in pts]
        ImageDraw.Draw(img).polygon(poly, fill=WHITE + (255,))


def render_icon(size: int) -> Image.Image:
    s = size * SS
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    small = size <= 24
    k = s / 1024.0
    margin = 0 if size <= 32 else round(24 * k)
    radius = round(220 * k)
    tile_mask = mask_rr((s, s), [margin, margin, s - 1 - margin, s - 1 - margin], radius)
    tile = vgradient(s, s, TILE_TOP, TILE_BOTTOM)
    # Soft top sheen.
    sheen = Image.new("RGBA", (s, s), (255, 255, 255, 0))
    ImageDraw.Draw(sheen).ellipse([-s * 0.3, -s * 0.85, s * 1.3, s * 0.42], fill=(255, 255, 255, 20))
    sheen = sheen.filter(ImageFilter.GaussianBlur(radius=s * 0.03))
    tile = Image.alpha_composite(tile, sheen)
    img.paste(tile, (0, 0), tile_mask)

    if small:
        # Single bold monitor with stand.
        draw_monitor(img, s, 150, 190, 874, 690, bezel=92, radius=70, pointer=False)
    else:
        # Remote screen: behind, upper-left, translucent white outline + faint fill.
        layer = Image.new("RGBA", (s, s), (0, 0, 0, 0))
        bx0, by0, bx1, by1 = (round(v * k) for v in (176, 214, 650, 540))
        br = round(46 * k)
        bw = round(40 * k)
        ImageDraw.Draw(layer).rounded_rectangle([bx0, by0, bx1, by1], radius=br, fill=(255, 255, 255, 120))
        ImageDraw.Draw(layer).rounded_rectangle(
            [bx0 + bw, by0 + bw, bx1 - bw, by1 - bw], radius=max(0, br - bw), fill=(255, 255, 255, 50)
        )
        img.alpha_composite(layer)
        # Shadow under the front monitor so the two screens separate.
        sh = Image.new("RGBA", (s, s), (0, 0, 0, 0))
        ImageDraw.Draw(sh).rounded_rectangle(
            [round(316 * k), round(380 * k), round(866 * k), round(752 * k)],
            radius=round(52 * k), fill=(4, 40, 36, 110),
        )
        sh = sh.filter(ImageFilter.GaussianBlur(radius=max(1, 18 * k)))
        img.alpha_composite(sh)
        # Local monitor: front, lower-right.
        draw_monitor(img, s, 334, 372, 850, 722, bezel=60 if size <= 48 else 44, radius=50, pointer=True)

    # Clip everything to the tile.
    alpha = img.getchannel("A")
    alpha = Image.composite(alpha, Image.new("L", (s, s), 0), tile_mask)
    img.putalpha(alpha)
    return img.resize((size, size), Image.Resampling.LANCZOS)


def write_ico(path: str, images: list[Image.Image]) -> None:
    """ICO with PNG-compressed 256 px entry and BMP-free PNG entries for all sizes.

    Windows Vista+ accepts PNG payloads for every size; this keeps alpha exact."""
    entries = []
    for im in images:
        buf = io.BytesIO()
        im.save(buf, format="PNG", optimize=True)
        entries.append((im.width, im.height, buf.getvalue()))
    header = struct.pack("<HHH", 0, 1, len(entries))
    offset = 6 + 16 * len(entries)
    dirs = b""
    blobs = b""
    for w, h, data in entries:
        dirs += struct.pack("<BBBBHHII", w % 256, h % 256, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
        blobs += data
    with open(path, "wb") as f:
        f.write(header + dirs + blobs)


def wizard_image(w: int, h: int) -> Image.Image:
    """Tall left-hand wizard image: gradient panel with the icon near the top third."""
    img = vgradient(w, h, TILE_TOP, TILE_BOTTOM).convert("RGB")
    d = ImageDraw.Draw(img, "RGBA")
    # Large faint rings (abstract "LAN" ripples), purely decorative.
    cx, cy = w * 0.5, h * 0.36
    for i, rr in enumerate((0.55, 0.85, 1.15)):
        r = w * rr
        d.ellipse([cx - r, cy - r, cx + r, cy + r], outline=(255, 255, 255, 34 - i * 8), width=max(1, w // 80))
    isz = round(w * 0.62)
    icon = render_icon(256).resize((isz, isz), Image.Resampling.LANCZOS)
    img.paste(icon, (round(cx - isz / 2), round(cy - isz / 2)), icon)
    return img


def wizard_small(w: int, h: int) -> Image.Image:
    img = Image.new("RGBA", (w, h), (0, 0, 0, 0))  # transparent: fits light + dark wizard
    sz = min(w, h)
    icon = render_icon(256).resize((sz, sz), Image.Resampling.LANCZOS)
    img.paste(icon, ((w - sz) // 2, (h - sz) // 2), icon)
    return img


def main() -> None:
    os.makedirs(ICON_DIR, exist_ok=True)
    os.makedirs(WIZ_DIR, exist_ok=True)
    images = [render_icon(n) for n in ICO_SIZES]
    write_ico(os.path.join(ICON_DIR, "TLP-Desk.ico"), images)
    for n in (256, 512, 1024):
        render_icon(n).save(os.path.join(ICON_DIR, f"TLP-Desk-{n}.png"), optimize=True)
    # Inno Setup 6 recommended wizard image sizes (100% .. 250% DPI).
    for scale, (w, h) in zip((100, 125, 150, 175, 200, 225, 250),
                             ((164, 314), (192, 386), (246, 459), (273, 556), (328, 628), (355, 691), (410, 797))):
        wizard_image(w, h).save(os.path.join(WIZ_DIR, f"wizard-{scale}.png"))
    for scale, (w, h) in zip((100, 125, 150, 175, 200, 225, 250),
                             ((55, 55), (64, 68), (83, 80), (92, 97), (110, 106), (119, 123), (138, 140))):
        wizard_small(w, h).save(os.path.join(WIZ_DIR, f"wizard-small-{scale}.png"))
    print("wrote", ICON_DIR, "and", WIZ_DIR)


if __name__ == "__main__":
    main()
