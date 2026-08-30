"""A feature table: shown as a table, and painting the labels it describes."""

import json

NAME = "tables"
#: This suite needs a label image and a feature table describing it, which the
#: runner builds and passes as extra layers.
NEEDS_FEATURE_TABLE = True


def run(viewer, check):
    check(
        "the table opens as a table layer",
        viewer.browser.js("!!document.querySelector('.data-table')") is True,
    )
    check("it says what kind of table it is", "feature table" in viewer.text())
    check("it says which label image it describes", "../labels/nuclei" in viewer.text())

    headers = viewer.browser.js(
        "JSON.stringify([...document.querySelectorAll('.data-table th')].map(e => e.textContent))"
    )
    check(
        "the columns are in the file's order",
        json.loads(headers) == ["label", "area", "intensity_mean", "cell_type"],
        headers,
    )
    shown = viewer.browser.js("document.querySelectorAll('.data-table tbody tr').length")
    check("the rows are shown", shown == 36, str(shown))
    first = viewer.browser.js(
        "JSON.stringify([...document.querySelectorAll('.data-table tbody tr')[0]"
        ".querySelectorAll('td')].map(e => e.textContent))"
    )
    check("the values are the file's", json.loads(first)[:2] == ["1", "113.5"], first)

    before = viewer.shot("labels")
    viewer.select(".channel-control select", "area")
    import time

    time.sleep(1.6)
    painted = viewer.shot("painted")
    check("it says which layer it is painting", "painting" in viewer.text())

    # The demo blobs are a grid and `area` rises with the label id, so two far
    # apart must end up different colours.
    ok_a, d_a = viewer.changed_at(before, painted, 43, 43)
    ok_b, d_b = viewer.changed_at(before, painted, 469, 469)
    check("colouring by a column repaints the labels", ok_a and ok_b, f"{d_a}; {d_b}")

    lo = _brightest(viewer, painted, 43, 43)
    hi = _brightest(viewer, painted, 469, 469)
    check("a low value and a high value get different colours", lo != hi, f"{lo} vs {hi}")

    viewer.select(".channel-control select", "")
    time.sleep(1.6)
    off = viewer.shot("off")
    ok, detail = viewer.changed_at(painted, off, 43, 43)
    check("switching the colouring off changes it back", ok, detail)


def _brightest(viewer, path, x, y):
    from harness import _patch

    sx, sy = viewer.to_screen(x, y)
    return max(_patch(path, sx, sy, 6), key=sum)
