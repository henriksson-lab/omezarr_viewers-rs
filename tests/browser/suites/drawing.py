"""Every drawing tool produces the geometry it claims, and it is drawn."""

NAME = "drawing"


def run(viewer, check):
    base = viewer.shot("base")

    # One of every shape, in world coordinates so the suite does not depend on
    # the window size or on how big the demo image happens to be.
    viewer.tool("point")
    viewer.click_world(120, 120)

    viewer.tool("box")
    viewer.drag_world(170, 120, 220, 170)

    viewer.tool("ellipse")
    viewer.drag_world(250, 120, 320, 165)

    viewer.tool("polygon")
    for x, y in [(120, 250), (185, 250), (185, 315), (145, 335)]:
        viewer.click_world(x, y)
    # Clicking the first vertex again closes the ring.
    viewer.click_world(120, 250)

    viewer.tool("freehand")
    viewer.drag_world(250, 250, 320, 320, steps=24)

    viewer.tool("polyline")
    viewer.click_world(120, 400)
    viewer.click_world(175, 420)
    viewer.double_click_world(220, 400)

    viewer.tool("line")
    viewer.drag_world(270, 400, 345, 440, steps=20)

    drawn = viewer.geometries()
    check(
        "every tool made its own geometry type",
        drawn
        == [
            "Point",
            "Polygon",
            "Polygon",
            "Polygon",
            "Polygon",
            "LineString",
            "LineString",
        ],
        str(drawn),
    )

    rows = viewer.annotations()
    check(
        "only the ellipse is flagged as one",
        [a["is_ellipse"] for a in rows] == [False, False, True, False, False, False, False],
        str([a["is_ellipse"] for a in rows]),
    )
    check(
        "the polygon kept the vertices that were clicked",
        len(rows[3]["geometry"]["coordinates"][0]) == 5,
        str(len(rows[3]["geometry"]["coordinates"][0])),
    )
    check(
        "the ellipse is polygonised finely",
        len(rows[2]["geometry"]["coordinates"][0]) == 65,
        str(len(rows[2]["geometry"]["coordinates"][0])),
    )

    shapes = viewer.shot("shapes")
    for what, (x, y) in {
        "point": (120, 120),
        "box edge": (170, 145),
        "ellipse edge": (250, 143),
        "polygon edge": (152, 250),
        "freehand": (285, 285),
        "polyline": (147, 410),
    }.items():
        ok, detail = viewer.changed_at(base, shapes, x, y)
        check(f"the {what} is drawn", ok, detail)

    # Fill, as QuPath's "Fill annotations" does.
    viewer.browser.js(
        "[...document.querySelectorAll('.channel-control input[type=checkbox]')]"
        ".find(e => e.parentElement.textContent.includes('Fill')).click()"
    )
    filled = viewer.shot("filled")
    ok, detail = viewer.changed_at(shapes, filled, 195, 145)
    check("filling paints the inside of a region", ok, detail)
    ok, detail = viewer.changed_at(shapes, filled, 147, 410)
    check("a line has no inside to fill", not ok, detail)

    # A near-zero drag is a misfire, not a zero-size region.
    before = viewer.rows()
    viewer.drag_world(400, 100, 400, 100, steps=2)
    check(
        "a click in a shape tool is not a zero-size shape",
        viewer.rows() == before,
        f"{before} -> {viewer.rows()}",
    )
