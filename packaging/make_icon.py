#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Renders the ZeroCart icon (ZeroStack family logo, assets/logo/zerocart.svg) with rsvg-convert.

usage: make_icon.py [<output_dir>] [<store_icon_dir>]
  output_dir      receives zerocart.png, zerocart_100.png (100 px) and zerocart_80.png (default: assets/images)
  store_icon_dir  receives the 260 px store icon (default: assets/images (zerocart_store.png))
"""
import os
import subprocess
import sys

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
SVG = os.path.join(ROOT, "assets", "logo", "zerocart.svg")
out = os.path.abspath(sys.argv[1]) if len(sys.argv) > 1 else os.path.join(ROOT, "assets", "images")
store = os.path.abspath(sys.argv[2]) if len(sys.argv) > 2 else os.path.join(ROOT, "assets/images")


def render(size, path):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    subprocess.run(["rsvg-convert", "-w", str(size), "-h", str(size), SVG, "-o", path], check=True)


render(100, os.path.join(out, "zerocart.png"))
render(100, os.path.join(out, "zerocart_100.png"))
render(80, os.path.join(out, "zerocart_80.png"))
render(260, os.path.join(store, "zerocart_store.png"))
print("icons written to", out, "and", store)
