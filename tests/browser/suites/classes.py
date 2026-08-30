"""Classes: colouring by them, filtering to one, and bulk deletion."""

import json

NAME = "classes"


def run(viewer, check):
    viewer.tool("box")
    viewer.set_field('.channel-control input[placeholder="for new shapes"]', "cell")
    viewer.drag_world(120, 120, 170, 170)
    viewer.set_field('.channel-control input[placeholder="for new shapes"]', "vessel")
    viewer.drag_world(300, 300, 350, 350)

    classes = viewer.browser.js(
        "JSON.stringify([...document.querySelectorAll('.annot-row input')].map(e => e.value))"
    )
    check(
        "each shape took the class in force when it was drawn",
        json.loads(classes) == ["cell", "vessel"],
        classes,
    )

    one_colour = viewer.shot("one-colour")
    viewer.browser.js(
        "[...document.querySelectorAll('.channel-control input[type=checkbox]')]"
        ".find(e => e.parentElement.textContent.includes('Colour by class')).click()"
    )
    by_class = viewer.shot("by-class")
    ok_a, d_a = viewer.changed_at(one_colour, by_class, 145, 120)
    ok_b, d_b = viewer.changed_at(one_colour, by_class, 325, 300)
    check("colouring by class repaints both", ok_a and ok_b, f"{d_a}; {d_b}")

    swatches = viewer.browser.js(
        "JSON.stringify([...document.querySelectorAll('.class-swatch')]"
        ".map(e => e.style.background))"
    )
    check("a class key is shown", len(json.loads(swatches)) == 2, swatches)
    check(
        "the two classes get different colours",
        len(set(json.loads(swatches))) == 2,
        swatches,
    )

    # --- Filtering hides one from the canvas but not from the list.
    viewer.select(".annot-filter", "cell")
    filtered = viewer.shot("filtered")
    gone, detail = viewer.changed_at(by_class, filtered, 325, 300)
    check("the filtered-out class stops being drawn", gone, detail)
    kept, detail = viewer.changed_at(by_class, filtered, 145, 120)
    check("the kept class is untouched", not kept, detail)
    check("the list still shows both", viewer.rows() == 2, str(viewer.rows()))
    viewer.select(".annot-filter", "__all__")

    check(
        "the layer is marked unsaved",
        viewer.browser.js("!!document.querySelector('.dirty-dot')") is True,
    )

    # --- Delete shown: armed, done, then undone.
    viewer.press("Delete shown")
    check(
        "the first click only arms it",
        viewer.rows() == 2
        and viewer.browser.js("!!document.querySelector('button.danger')") is True,
        str(viewer.rows()),
    )
    viewer.browser.js("document.querySelector('button.danger').click()")
    import time

    time.sleep(1.4)
    check("the second click deletes them", viewer.rows() == 0, str(viewer.rows()))
    cleared = viewer.shot("cleared")
    gone, detail = viewer.changed_at(by_class, cleared, 145, 120)
    check("and the canvas is clear of them", gone, detail)

    viewer.undo()
    time.sleep(1.5)
    check("undo brings the whole set back", viewer.rows() == 2, str(viewer.rows()))
    back = viewer.annotations()
    check(
        "with their classes",
        [a["label"] for a in back] == ["cell", "vessel"],
        str([a["label"] for a in back]),
    )
