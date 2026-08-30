"""Editing every kind of shape: move, resize, vertices, and undo."""

from harness import bounds_of

NAME = "editing"


def run(viewer, check):
    # A rectangle and a polygon, which are edited differently: a rectangle by
    # its bounding corners, a polygon by its own vertices.
    viewer.tool("box")
    viewer.drag_world(120, 120, 220, 220)
    viewer.tool("polygon")
    for x, y in [(300, 300), (380, 300), (380, 380), (330, 400)]:
        viewer.click_world(x, y)
    viewer.click_world(300, 300)

    viewer.tool("pan")
    # Deselect, so "handles appear" has a before.
    viewer.browser.js(
        "document.querySelectorAll('.annot-row.selected .annot-kind')"
        ".forEach(e => e.click())"
    )
    plain = viewer.shot("plain")

    check("a shape can be selected", viewer.select_row(0) is True)
    handled = viewer.shot("handles")
    ok, detail = viewer.changed_at(plain, handled, 120, 120)
    check("handles appear on the selected shape", ok, detail)

    # --- Move by the body.
    before = bounds_of(viewer.annotations()[0])
    viewer.drag_world(170, 170, 210, 210)
    after = bounds_of(viewer.annotations()[0])
    check(
        "dragging the body moves the whole shape",
        all(abs((a + 40) - b) < 3 for a, b in zip(before, after)),
        f"{before} -> {after}",
    )
    viewer.undo()
    check(
        "undo puts it back",
        all(abs(a - b) < 1e-6 for a, b in zip(before, bounds_of(viewer.annotations()[0]))),
        str(bounds_of(viewer.annotations()[0])),
    )

    # --- Resize a rectangle by a bounding corner.
    check("still selected for the resize", viewer.select_row(0) is True)
    viewer.drag_world(220, 220, 270, 270)
    grown = bounds_of(viewer.annotations()[0])
    check(
        "a corner drag grows the rectangle",
        abs(grown[2] - (before[2] + 50)) < 3,
        f"{before} -> {grown}",
    )
    check(
        "and leaves the opposite corner alone",
        abs(grown[0] - before[0]) < 1 and abs(grown[1] - before[1]) < 1,
        str(grown),
    )
    viewer.undo()

    # --- A polygon is edited by its vertices.
    check("the polygon can be selected", viewer.select_row(1) is True)
    ring = viewer.annotations()[1]["geometry"]["coordinates"][0]
    viewer.drag_world(ring[1][0], ring[1][1], ring[1][0] + 40, ring[1][1] - 20)
    moved = viewer.annotations()[1]["geometry"]["coordinates"][0]
    differing = [i for i, (a, b) in enumerate(zip(ring, moved)) if a != b]
    check(
        "dragging a vertex moves exactly that vertex",
        differing == [1],
        f"{differing}",
    )
    check(
        "and it goes where the pointer went",
        abs(moved[1][0] - (ring[1][0] + 40)) < 3,
        str(moved[1]),
    )

    # --- Shift-click an edge to insert, a vertex to delete.
    ring = viewer.annotations()[1]["geometry"]["coordinates"][0]
    count = len(ring)
    midpoint = ((ring[0][0] + ring[1][0]) / 2, (ring[0][1] + ring[1][1]) / 2)
    viewer.shift_click_world(*midpoint)
    check(
        "shift-clicking an edge inserts a vertex",
        len(viewer.annotations()[1]["geometry"]["coordinates"][0]) == count + 1,
        f"{count} -> {len(viewer.annotations()[1]['geometry']['coordinates'][0])}",
    )
    inserted = viewer.annotations()[1]["geometry"]["coordinates"][0][1]
    check(
        "the new vertex sits where the click landed",
        abs(inserted[0] - midpoint[0]) < 3 and abs(inserted[1] - midpoint[1]) < 3,
        str(inserted),
    )
    viewer.shift_click_world(*inserted)
    check(
        "shift-clicking a vertex deletes it",
        len(viewer.annotations()[1]["geometry"]["coordinates"][0]) == count,
        str(len(viewer.annotations()[1]["geometry"]["coordinates"][0])),
    )

    # --- A point is moved, never resized: its corners are all one coordinate.
    viewer.tool("point")
    viewer.click_world(430, 430)
    viewer.tool("pan")
    check("the new point is selected without being clicked", viewer.select_row(-1) is True)
    viewer.drag_world(430, 430, 470, 460)
    point = viewer.annotations()[-1]
    check(
        "a point stays a point when dragged",
        point["geometry"]["type"] == "Point",
        point["geometry"]["type"],
    )
    check(
        "and it moved with the pointer",
        abs(point["geometry"]["coordinates"][0] - 470) < 3,
        str(point["geometry"]["coordinates"]),
    )

    # --- A locked shape refuses every one of those.
    viewer.browser.js(
        "(() => {"
        "  const el = [...document.querySelectorAll('.channel-control input[type=checkbox]')]"
        "    .find(e => e.parentElement.textContent.includes('Locked'));"
        "  el.click();"
        "})()"
    )
    import time

    time.sleep(1.3)
    check("a shape can be locked", viewer.annotations()[-1]["locked"] is True)
    frozen = viewer.annotations()[-1]["geometry"]["coordinates"]
    viewer.drag_world(470, 460, 500, 490)
    check(
        "a locked shape cannot be dragged",
        viewer.annotations()[-1]["geometry"]["coordinates"] == frozen,
        str(viewer.annotations()[-1]["geometry"]["coordinates"]),
    )
