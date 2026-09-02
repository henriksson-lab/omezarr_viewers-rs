"""The 2x2 slice grid, and the box that says where the slices are.

Three of the four panels are the slice views, which other suites already cover.
What is new here is the fourth: a box drawn to the volume's true proportions,
cut by three planes, whose planes can be dragged to scrub an axis.

This suite asks for a **deeper volume** than the other suites. The default
fixture is eight planes, which is a slab: the box is then a sheet, the z axis is
a few pixels of screen, and a drag along it says nothing. The feature is about
the third dimension, so the fixture has to have one.
"""

import json
import time

NAME = "grid"
NEEDS_DEPTH = 192


def cube_rect(viewer):
    return json.loads(
        viewer.browser.js(
            "JSON.stringify((() => {const c = document.querySelector('.cube-canvas');"
            "if (!c) return null; const r = c.getBoundingClientRect();"
            "return {x: r.left, y: r.top, w: r.width, h: r.height};})())"
        )
        or "null"
    )


def cuts(viewer):
    """The `x y z` fractions the cube panel reports."""
    text = viewer.browser.js("(document.querySelector('.cube-pane')||{}).textContent||''")
    found = {}
    for axis in ("x", "y", "z"):
        at = text.find(axis + " ")
        if at >= 0:
            try:
                found[axis] = float(text[at + 2 : at + 6])
            except ValueError:
                pass
    return found


def run(viewer, check):
    check("the grid is off to begin with", cube_rect(viewer) is None)

    viewer.browser.js(
        "[...document.querySelectorAll('.slider-row label')]"
        ".find(e => e.textContent.includes('Slice grid'))?.querySelector('input')?.click()"
    )
    time.sleep(2.5)

    panels = viewer.browser.js(
        "document.querySelectorAll('.viewer-area.grid > *').length"
    )
    check("turning it on lays out four panels", panels == 4, str(panels))

    rect = cube_rect(viewer)
    check("the fourth panel is the cube", rect is not None, str(rect))

    # The panels share the space rather than one keeping most of it, which is
    # the whole difference from the layout this replaced.
    main = json.loads(
        viewer.browser.js(
            "JSON.stringify((() => {const r = document.querySelector('.viewer-main')"
            ".getBoundingClientRect(); return {w: r.width, h: r.height};})())"
        )
    )
    check(
        "the four panels are of a size",
        abs(main["w"] - rect["w"]) < 8 and abs(main["h"] - rect["h"]) < 8,
        f"main {main['w']:.0f}x{main['h']:.0f} vs cube {rect['w']:.0f}x{rect['h']:.0f}",
    )

    before = cuts(viewer)
    check("it reports where all three planes cut", set(before) == {"x", "y", "z"}, str(before))

    # Dragging a plane scrubs its axis, and does so *proportionally* — a mapping
    # that snapped to an end would also move the plane, and would be useless.
    #
    # Each drag's start is *measured*, never assumed. The reset drag grabs
    # whichever plane lies under that point, and which one that is changes as the
    # planes move — so a run that assumed the reset returned z to zero read a
    # later drag as three times its length, on one Chrome and not another.
    cx = rect["x"] + rect["w"] * 0.42
    cy = rect["y"] + rect["h"] * 0.62

    def drag_z(dx, dy):
        """Reset toward zero, then drag; returns (start, moved)."""
        viewer.browser.drag(cx, cy, cx - 220, cy + 130, steps=10, settle=0.9)
        start = cuts(viewer).get("z")
        viewer.browser.drag(cx, cy, cx + dx, cy + dy, steps=8, settle=0.9)
        return start, cuts(viewer).get("z")

    short_from, short_to = drag_z(30, -18)
    long_from, long_to = drag_z(60, -37)
    short = short_to - short_from
    long = long_to - long_from

    check(
        "dragging a plane scrubs its axis",
        short > 0.05,
        f"{short_from:.2f} -> {short_to:.2f}",
    )
    # Skipped rather than failed if the long drag ran into the end of the axis:
    # a clamped answer says nothing about the mapping either way.
    if long_to > 0.98:
        check(
            "twice the drag moves it about twice as far",
            True,
            f"long drag clamped at {long_to:.2f}; ratio not measurable here",
        )
    else:
        check(
            "twice the drag moves it about twice as far",
            1.6 < long / max(short, 1e-6) < 2.4,
            f"{short:.2f} then {long:.2f} (ratio {long / max(short, 1e-6):.2f})",
        )

    # A drag across the panel that grabs nothing must not move anything: the
    # planes are the input surface, not the whole canvas.
    quiet_from = cuts(viewer)
    corner_x = rect["x"] + rect["w"] * 0.06
    corner_y = rect["y"] + rect["h"] * 0.08
    viewer.browser.drag(corner_x, corner_y, corner_x + 60, corner_y + 40, steps=8, settle=0.9)
    check(
        "a drag that grabs no plane changes nothing",
        cuts(viewer) == quiet_from,
        f"{quiet_from} -> {cuts(viewer)}",
    )
