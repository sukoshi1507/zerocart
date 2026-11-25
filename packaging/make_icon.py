#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Draws the ZeroCart icon (a pixel-art cartridge, same palette as the interface) into assets/images/.

    python3 packaging/make_icon.py
"""
import os
from PIL import Image

PALETTE = {
    ".": None,
    "D": (0x10, 0x10, 0x14, 255),  # outline
    "C": (0xEC, 0xE6, 0xD3, 255),  # cream shell
    "G": (0xA7, 0x9F, 0x8E, 255),  # shade
    "R": (0xC8, 0x20, 0x2F, 255),  # red label
    "Y": (0xF3, 0xC9, 0x3A, 255),  # gold
    "K": (0x6B, 0x2A, 0x30, 255),  # dark red
}
ART = [
    "....................",
    ".....DDDDDDDDDD.....",
    "....DCCCCCCCCCCD....",
    "...DCCCCCCCCCCCCD...",
    "...DCGGGGGGGGGGCD...",
    "...DCCCCCCCCCCCCD...",
    "...DCDDDDDDDDDDCD...",
    "...DCDRRRRRRRRDCD...",
    "...DCDRRRYYRRRDCD...",
    "...DCDRRYYYYRRDCD...",
    "...DCDRYYRRYYRDCD...",
    "...DCDRRRRRRRRDCD...",
    "...DCDDDDDDDDDDCD...",
    "...DCCCCCCCCCCCCD...",
    "...DCGGCCCCCCGGCD...",
    "...DDDDDDDDDDDDDD...",
    "....DYKYKYKYKYKD....",
    "....DYKYKYKYKYKD....",
    "....DDDDDDDDDDDD....",
    "....................",
]
assert all(len(r) == 20 for r in ART) and len(ART) == 20


def draw(cell: int, size: int) -> Image.Image:
    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    off = (size - 20 * cell) // 2
    for y, row in enumerate(ART):
        for x, ch in enumerate(row):
            if PALETTE[ch]:
                for dy in range(cell):
                    for dx in range(cell):
                        img.putpixel((off + x * cell + dx, off + y * cell + dy), PALETTE[ch])
    return img


out = os.path.join(os.path.dirname(__file__), "..", "assets", "images")
os.makedirs(out, exist_ok=True)
draw(4, 100).save(os.path.join(out, "zerocart.png"))
draw(4, 100).save(os.path.join(out, "zerocart_100.png"))
draw(4, 80).save(os.path.join(out, "zerocart_80.png"))
draw(13, 260).save(os.path.join(out, "zerocart_store.png"))
print("icons written to", os.path.abspath(out))
