"""从设计稿生成符合 macOS 图标网格的源图。

设计稿通常是「满幅方形 + 图形悬空」，不能直接当 macOS 图标用：
四角是直角、没有留边、深色背景在浅色 Dock 里是一块黑方块。
这个脚本把图形抠出来，叠到规范底板上。

用法：

    python3 compose-icon.py 设计稿.png
    npx tauri icon assets/icon-1024.png

依赖 numpy 与 Pillow。只在 macOS 上需要跑，产物已入库，
普通构建不需要执行本脚本。
"""

import sys

import numpy as np
from PIL import Image, ImageDraw, ImageFilter

SIZE = 1024
INSET = 100      # macOS 图标网格：四周留 100
BOX = 824        # 图形区 824×824
C = SIZE / 2


def squircle_mask(box: int, n: float = 5.0, ss: int = 4) -> Image.Image:
    """超椭圆 mask。Apple 用的是连续曲率，不是普通圆角。"""
    big = box * ss
    img = Image.new("L", (big, big), 0)
    d = ImageDraw.Draw(img)
    r = big / 2
    pts = []
    steps = 720
    for i in range(steps):
        t = (i / steps) * np.pi * 2
        ct, st = np.cos(t), np.sin(t)
        p = 2 / n
        pts.append((r + r * np.sign(ct) * abs(ct) ** p,
                    r + r * np.sign(st) * abs(st) ** p))
    d.polygon(pts, fill=255)
    return img.resize((box, box), Image.LANCZOS)


def plate(box: int) -> Image.Image:
    """底板：对角深色渐变 + 顶部柔光 + 边缘内高光。

    内核两点经验：
    - 高光若用硬边椭圆，会在底板中间划出一条分界线，看着像两层，
      必须走高斯模糊
    - 没有边缘内高光，深色底板在浅色 Dock 里会发闷、像被禁用
    """
    yy, xx = np.mgrid[0:box, 0:box]
    t = (xx * 0.32 + yy * 0.68) / box
    top = np.array([46, 48, 70], dtype=np.float32)
    bottom = np.array([21, 21, 32], dtype=np.float32)
    arr = top[None, None, :] + (bottom - top)[None, None, :] * t[..., None]
    img = Image.fromarray(np.clip(arr, 0, 255).astype(np.uint8), "RGB")

    glow = Image.new("L", (box, box), 0)
    ImageDraw.Draw(glow).ellipse(
        [-box * 0.45, -box * 0.95, box * 1.45, box * 0.30], fill=52
    )
    glow = glow.filter(ImageFilter.GaussianBlur(box * 0.13))
    img = Image.composite(
        Image.blend(img, Image.new("RGB", (box, box), (150, 166, 255)), 0.5),
        img, glow,
    )

    mask = squircle_mask(box)
    eroded = mask.filter(ImageFilter.MinFilter(7))
    ring = Image.fromarray(
        np.clip(np.asarray(mask, np.int16) - np.asarray(eroded, np.int16), 0, 255)
        .astype(np.uint8)
    ).filter(ImageFilter.GaussianBlur(3))

    gy, _ = np.mgrid[0:box, 0:box]
    grad = np.clip(150 * np.maximum(0.0, 1.0 - gy / (box * 0.55)), 0, 255)
    ring = Image.fromarray((np.asarray(ring, np.float32) * grad / 255).astype(np.uint8))
    return Image.composite(Image.new("RGB", (box, box), (255, 255, 255)), img, ring)


def extract_foreground(im: Image.Image) -> Image.Image:
    """按与背景色的距离做 soft matte，并反混合去掉边缘暗边。"""
    a = np.asarray(im.convert("RGB")).astype(np.float32)
    corners = np.concatenate([
        a[:8, :8].reshape(-1, 3), a[:8, -8:].reshape(-1, 3),
        a[-8:, :8].reshape(-1, 3), a[-8:, -8:].reshape(-1, 3),
    ])
    bg = corners.mean(axis=0)

    dist = np.linalg.norm(a - bg, axis=2)
    alpha = np.clip((dist - 10.0) / 36.0, 0, 1)

    # 边缘像素是「前景 × a + 背景 × (1-a)」，直接留会形成暗边
    a3 = alpha[..., None]
    with np.errstate(divide="ignore", invalid="ignore"):
        fg = (a - bg * (1 - a3)) / np.maximum(a3, 1e-4)
    fg = np.clip(np.where(a3 > 0.02, fg, a), 0, 255)

    return Image.fromarray(np.dstack([fg, alpha * 255]).astype(np.uint8), "RGBA")


def main(src_path: str, out_path: str = "assets/icon-1024.png") -> None:
    fg = extract_foreground(Image.open(src_path).convert("RGB"))

    bbox = fg.getchannel("A").point(lambda v: 255 if v > 12 else 0).getbbox()
    fg = fg.crop(bbox)

    target = 522  # 图形在 824 底板上的占位
    scale = target / max(fg.size)
    fg = fg.resize((round(fg.size[0] * scale), round(fg.size[1] * scale)),
                   Image.LANCZOS)

    canvas = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
    base = plate(BOX)
    base.putalpha(squircle_mask(BOX))
    canvas.alpha_composite(base, (INSET, INSET))
    canvas.alpha_composite(
        fg, (round(C - fg.size[0] / 2), round(C - fg.size[1] / 2 - 6))
    )
    canvas.save(out_path)
    print("已写出", out_path)


if __name__ == "__main__":
    if len(sys.argv) < 2:
        print(__doc__)
        raise SystemExit(1)
    main(sys.argv[1], sys.argv[2] if len(sys.argv) > 2 else "assets/icon-1024.png")
