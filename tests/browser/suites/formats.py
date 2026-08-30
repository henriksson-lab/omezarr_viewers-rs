"""Saving and reopening: GeoJSON keeps the shape, an ROI table keeps the box."""

import json
import os

NAME = "formats"


def run(viewer, check):
    store = viewer.store

    # A hole is the shape an ROI table cannot hold and GeoJSON can, so it is the
    # one worth carrying through both.
    viewer.tool("polygon")
    for x, y in [(100, 100), (300, 100), (300, 300), (100, 300)]:
        viewer.click_world(x, y)
    viewer.click_world(100, 100)
    viewer.tool("point")
    viewer.click_world(400, 150)

    drawn = viewer.annotations()
    check("two shapes drawn", len(drawn) == 2, str(len(drawn)))

    # --- GeoJSON: the native form.
    target = os.path.join(store, "annotations", "saved")
    viewer.set_field('.channel-control input[placeholder^="/path/to/image.zarr"]', target)
    viewer.press("Save")
    check("the save reported GeoJSON", "as GeoJSON" in viewer.text(), viewer.text()[:0])
    check(
        "saving cleared the unsaved marker",
        viewer.browser.js("!!document.querySelector('.dirty-dot')") is False,
    )

    path = os.path.join(target, "annotations.geojson")
    check("annotations.geojson exists", os.path.isfile(path))
    document = json.load(open(path))
    check("it is a FeatureCollection", document["type"] == "FeatureCollection")
    kinds = [f["geometry"]["type"] for f in document["features"]]
    check("every shape is in the file", kinds == ["Polygon", "Point"], str(kinds))
    check(
        "the default plane is omitted, as QuPath omits it",
        all("plane" not in f["geometry"] for f in document["features"]),
    )

    # --- An ROI table: boxes only, and it says what it flattened.
    table = os.path.join(store, "tables", "boxes")
    viewer.set_field('.channel-control input[placeholder^="/path/to/image.zarr"]', table)
    viewer.press("Save")
    check(
        "an ROI table save reports what it flattened",
        "bounding boxes" in viewer.text() or "um/px" in viewer.text(),
        viewer.text()[:0],
    )
    check(
        "table.csv exists",
        os.path.isfile(os.path.join(table, "table.csv")),
    )

    # --- Reopening the GeoJSON puts the shapes back where they were.
    before = viewer.layer_ids("annotations")
    opened = viewer.browser.js(
        "(() => {"
        "  const rows = [...document.querySelectorAll('.add-layer .slider-row')];"
        f"  const row = rows.find(r => r.textContent.includes({'saved'!r}));"
        "  if (!row) return false;"
        "  const b = row.querySelector('button'); if (!b) return false;"
        "  b.click(); return true;"
        "})()"
    )
    check("the saved set is offered to reopen", opened is True)
    import time

    time.sleep(2.5)
    fresh = [id for id in viewer.layer_ids("annotations") if id not in before]
    check("it opened as a new layer", len(fresh) == 1, str(viewer.layers()))
    if fresh:
        reopened = viewer.annotations(fresh[0])
        check("the reopened set has both shapes", len(reopened) == 2, str(len(reopened)))
        same = [a["geometry"] for a in reopened] == [a["geometry"] for a in drawn]
        check("with the same geometry", same, "identical" if same else "differs")
