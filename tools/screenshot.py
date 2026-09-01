#!/usr/bin/env python3
"""Regenerate the screenshot in `README.md`.

    make screenshot

A screenshot that drifts from the software is worse than none, so this is a
script rather than a picture somebody once took: it builds the synthetic demo
store, opens the layers, draws the annotations, and shoots the result. Rerun it
whenever the UI moves.

It reuses the browser suites' driver (`tests/browser/`) because that is already
the thing in this repo that knows how to start a server, drive Chrome and hit a
canvas at a world coordinate. The prerequisites are the same as
`make test-browser`: a release build, `dist/`, Chrome, and `websocket-client`.

Everything drawn here is deliberate. The scene has to show, in one frame, the
claim the README makes — several layer kinds over one set of coordinates — so it
opens an image with a label volume over it and annotates in classes, with the
classes colouring both the shapes and the label ids.
"""

import os
import shutil
import subprocess
import sys
import tempfile
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "tests", "browser"))

from cdp import Browser  # noqa: E402
from harness import Server, Viewer, binary, demo_store  # noqa: E402

#: Where the README expects it.
OUT = os.path.join(ROOT, "docs", "viewer.png")

#: Tall enough that the annotation panel is not sliced through a control. The
#: panel is the half of the picture that says what the thing *does*, so a shot
#: that cuts it mid-row advertises a broken layout rather than a feature.
SIZE = (1500, 1150)

#: The class box, addressed by its placeholder the way the suites do — there is
#: no class hook on it, and adding one just for a screenshot would be a change to
#: the app for the benefit of a picture.
CLASS_BOX = '.channel-control input[placeholder="for new shapes"]'


def compose(viewer):
    """Draw the scene. Kept in one place so a change is one diff."""
    # The demo image has a bright magenta background channel behind green
    # blobs. It is there to prove channels composite; on a still it swamps every
    # annotation colour, and the classes are the subject here.
    viewer.browser.js(
        "[...document.querySelectorAll('.channel-control')]"
        ".find(e => e.textContent.includes('background'))"
        "?.querySelector('input[type=checkbox]')?.click()"
    )
    time.sleep(0.5)
    viewer.press("New layer")

    # A class per kind of thing, so the key in the panel is not one grey row.
    # Drawn largest first: a later shape sits inside an earlier one and the
    # hierarchy shows that as nesting, which is worth having in the picture.
    viewer.set_field(CLASS_BOX, "vessel")
    viewer.tool("polygon")
    for x, y in [(120, 150), (250, 120), (300, 230), (200, 300), (110, 260)]:
        viewer.click_world(x, y)
    viewer.click_world(120, 150)

    viewer.set_field(CLASS_BOX, "cell")
    viewer.tool("point")
    for x, y in [(170, 200), (215, 235), (150, 245)]:
        viewer.click_world(x, y)

    viewer.set_field(CLASS_BOX, "lumen")
    viewer.tool("ellipse")
    viewer.drag_world(330, 330, 430, 400)

    viewer.set_field(CLASS_BOX, "debris")
    viewer.tool("box")
    viewer.drag_world(360, 130, 460, 210)

    viewer.tool("pan")
    # Filled, because an outline a pixel wide disappears at README scale and the
    # shapes are the subject. QuPath's default is unfilled for a good reason —
    # a fill hides the pixels the shape was drawn around — but that reason is
    # about working, not about a still.
    viewer.browser.js(
        "[...document.querySelectorAll('.channel-control input[type=checkbox]')]"
        ".find(e => e.parentElement.textContent.trim() === 'Fill')?.click()"
    )
    time.sleep(0.5)
    # Points are drawn at a screen size, and the default is tuned for working at
    # a zoom rather than for being legible in a figure.
    viewer.browser.js(
        """
        (() => {
          const row = [...document.querySelectorAll('.slider-row')]
            .find(e => e.textContent.trim().startsWith('Size'));
          const el = row && row.querySelector('input[type=range]');
          if (!el) return;
          const set = Object.getOwnPropertyDescriptor(
            window.HTMLInputElement.prototype, 'value').set;
          set.call(el, '20');
          el.dispatchEvent(new InputEvent('input', {bubbles: true}));
        })()
        """
    )
    time.sleep(0.5)
    # Colour by class, so the key means something and the picture shows that a
    # class is what carries the colour.
    viewer.browser.js(
        "[...document.querySelectorAll('.channel-control input[type=checkbox]')]"
        ".find(e => e.parentElement.textContent.includes('Colour by class'))?.click()"
    )
    time.sleep(0.8)
    # Nothing selected: a selection draws handles, which are interaction state
    # rather than something to advertise.
    viewer.browser.js(
        "document.querySelectorAll('.annot-row.selected .annot-kind')"
        ".forEach(e => e.click())"
    )
    time.sleep(0.6)
    # The save boxes default to the temp store this script made. A path under
    # /tmp in the README would be noise at best and misleading at worst.
    viewer.browser.js(
        """
        for (const sel of ['.label-save-target', '.label-region']) {
          const el = document.querySelector(sel);
          if (!el) continue;
          const set = Object.getOwnPropertyDescriptor(
            window.HTMLInputElement.prototype, 'value').set;
          set.call(el, '');
          el.dispatchEvent(new InputEvent('input', {bubbles: true}));
        }
        """
    )
    time.sleep(0.5)


def main():
    binary("server")
    binary("make_demo")
    if not os.path.exists(os.path.join(ROOT, "dist", "index.html")):
        sys.exit("dist/index.html is missing — run `make build` first")

    directory = tempfile.mkdtemp(prefix="omezarr-screenshot-")
    shots = tempfile.mkdtemp(prefix="omezarr-screenshot-shots-")
    server = viewer = None
    try:
        store = demo_store(directory)
        labels = os.path.join(directory, "labels.zarr")
        server = Server(store, layers=[f"{labels}:labels"])
        viewer = Viewer(server, Browser(size=SIZE), shots)
        compose(viewer)
        os.makedirs(os.path.dirname(OUT), exist_ok=True)
        viewer.browser.screenshot(OUT)
        print(f"wrote {os.path.relpath(OUT, ROOT)} ({os.path.getsize(OUT) // 1024} KB)")
    finally:
        if viewer is not None:
            viewer.close()
        if server is not None:
            server.close()
        shutil.rmtree(directory, ignore_errors=True)
        shutil.rmtree(shots, ignore_errors=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
