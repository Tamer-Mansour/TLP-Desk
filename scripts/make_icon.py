"""Build the TLP-Desk application icon and the installer wizard images from the brand kit.

Sources (checked in, from the TLP brand kit; see docs/src/dev/design.md):
    crates/tlp-desk-app/assets/icon/TLP-Desk-1024.png   the app icon tile (ivory, reaching hand)
    installer/assets/brand-white.png                     the white TLP wordmark

Reproduce (needs Pillow):
    python -m venv .venv && .venv\\Scripts\\pip install pillow
    .venv\\Scripts\\python scripts\\make_icon.py

Outputs:
    crates/tlp-desk-app/assets/icon/TLP-Desk.ico        16..256 px, multi-resolution
    crates/tlp-desk-app/assets/icon/TLP-Desk-{256,512}.png
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

# Brand palette (TLP-colour-palette.md).
BURGUNDY = (0x74, 0x1D, 0x3D)
BURGUNDY_DEEP = (0x4E, 0x12, 0x29)
ROSE = (0xBE, 0x61, 0x7A)
WHITE = (255, 255, 255)

ICO_SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]
MASTER = os.path.join(ICON_DIR, "TLP-Desk-1024.png")
WORDMARK = os.path.join(WIZ_DIR, "brand-white.png")


def vgradient(w: int, h: int, top, bottom) -> Image.Image:
    g = Image.new("RGBA", (1, h))
    for y in range(h):
        t = y / max(1, h - 1)
        g.putpixel((0, y), tuple(round(a + (b - a) * t) for a, b in zip(top, bottom)) + (255,))
    return g.resize((w, h))


def render_icon(size: int) -> Image.Image:
    """The icon tile at `size` px, from the 1024 px master (Lanczos)."""
    return Image.open(MASTER).convert("RGBA").resize((size, size), Image.Resampling.LANCZOS)


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
    """Tall left-hand wizard image: a burgundy panel, the white TLP wordmark near the top and
    the Desk icon tile under it (the brand kit says: white wordmark on dark backgrounds, the
    product mark on its own)."""
    img = vgradient(w, h, BURGUNDY, BURGUNDY_DEEP).convert("RGB")
    d = ImageDraw.Draw(img, "RGBA")
    # A faint rose light in the upper half, purely decorative.
    glow = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    ImageDraw.Draw(glow).ellipse([-w * 0.4, -h * 0.25, w * 1.4, h * 0.55], fill=ROSE + (56,))
    glow = glow.filter(ImageFilter.GaussianBlur(radius=w * 0.25))
    img.paste(glow, (0, 0), glow)
    mark = Image.open(WORDMARK).convert("RGBA")
    mw = round(w * 0.58)
    mh = round(mark.height * mw / mark.width)
    mark = mark.resize((mw, mh), Image.Resampling.LANCZOS)
    img.paste(mark, (round((w - mw) / 2), round(h * 0.16)), mark)
    isz = round(w * 0.56)
    icon = render_icon(256).resize((isz, isz), Image.Resampling.LANCZOS)
    # Soft shadow so the ivory tile sits on the panel.
    sh = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    y0 = round(h * 0.16 + mh + h * 0.09)
    ImageDraw.Draw(sh).rounded_rectangle(
        [round((w - isz) / 2), y0 + isz * 0.06, round((w + isz) / 2), y0 + isz * 1.06],
        radius=round(isz * 0.22), fill=(30, 8, 16, 120),
    )
    sh = sh.filter(ImageFilter.GaussianBlur(radius=max(1, isz * 0.08)))
    img.paste(sh, (0, 0), sh)
    img.paste(icon, (round((w - isz) / 2), y0), icon)
    # Thin rose rule near the bottom.
    d.line([(w * 0.2, h * 0.9), (w * 0.8, h * 0.9)], fill=ROSE + (110,), width=max(1, w // 160))
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
    for n in (256, 512):
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
