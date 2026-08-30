"""The spatial hierarchy: nesting, showing it, and maintaining it."""

import json

NAME = "hierarchy"


def run(viewer, check):
    # A big region, a smaller one inside it, and a point inside that.
    viewer.tool("box")
    viewer.drag_world(80, 80, 380, 380)
    viewer.drag_world(120, 120, 250, 250)
    viewer.tool("point")
    viewer.click_world(170, 170)

    rows = viewer.annotations()
    ids = [a["id"] for a in rows]
    parents = [a["parent"] for a in rows]
    check(
        "a shape drawn inside another becomes its child",
        parents == [None, ids[0], ids[1]],
        f"ids {ids} parents {parents}",
    )

    indents = viewer.browser.js(
        "JSON.stringify([...document.querySelectorAll('.annot-kind')]"
        ".map(e => parseInt(e.style.paddingLeft) || 0))"
    )
    check(
        "the list is indented by nesting depth",
        json.loads(indents) == [2, 12, 22],
        indents,
    )
    counts = viewer.browser.js(
        "JSON.stringify([...document.querySelectorAll('.annot-children')]"
        ".map(e => e.textContent.trim()))"
    )
    check("a parent says how many are inside it", json.loads(counts) == ["1", "1", ""], counts)
    viewer.shot("tree")

    # --- Name and type on one shape.
    viewer.tool("pan")
    viewer.select_row(-1)
    viewer.set_field(
        ".channel-control input[placeholder=\"this shape's own name\"]",
        "Cell 7",
        event="change",
    )
    check("a name can be set on one shape", viewer.annotations()[2]["name"] == "Cell 7",
          str(viewer.annotations()[2]["name"]))
    viewer.select(".annot-selected-type", "detection")
    check(
        "an object type can be set on one shape",
        viewer.annotations()[2]["object_type"] == "detection",
        str(viewer.annotations()[2]["object_type"]),
    )

    # --- Detach and re-nest.
    viewer.press("Detach")
    check("detach lifts it to the top level", viewer.annotations()[2]["parent"] is None)
    viewer.press("Re-nest")
    check(
        "re-nest puts it back where it sits",
        viewer.annotations()[2]["parent"] == ids[1],
        str(viewer.annotations()[2]["parent"]),
    )

    # --- Deleting a parent lifts its children rather than taking them along.
    count = len(viewer.annotations())
    viewer.browser.js(
        "document.querySelectorAll('.annot-row')[1].querySelector('.layer-remove').click()"
    )
    import time

    time.sleep(1.5)
    after = viewer.annotations()
    check("deleting a parent keeps what was inside it", len(after) == count - 1, str(len(after)))
    check("and lifts it one level", after[-1]["parent"] == ids[0], str(after[-1]["parent"]))
