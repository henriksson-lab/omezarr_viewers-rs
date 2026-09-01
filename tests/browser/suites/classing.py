"""Classing the objects in a label image, by clicking them.

This is annotation for training an *object* classifier: the instances already
exist, so the work is a class per label id and the label image is never
modified. The claim here is the loop a curator actually performs — set a class,
click an object, see it take the class — plus the distinction the whole design
rests on: an id nobody has looked at is not the same as an id looked at and
found to be nothing in particular.
"""

import time

NAME = "classing"
NEEDS_LABELS = True

# A place the demo's labels have an object, borrowed from the tables suite.
ON_A_LABEL = (43, 43)


def count_text(viewer):
    return viewer.browser.js(
        "(document.querySelector('.label-class-count') || {}).textContent || ''"
    )


def run(viewer, check):
    check(
        "a label layer is open and nothing sits above it",
        [k for _, _, k in viewer.layers()] == ["image", "labels"],
        str(viewer.layers()),
    )

    viewer.browser.js("document.querySelector('.label-classing').click()")
    time.sleep(0.6)
    viewer.set_field(".label-class", "tumour")

    before = viewer.shot("unclassed")
    viewer.click_world(*ON_A_LABEL)
    time.sleep(0.8)
    check(
        "clicking an object gives it the class in force",
        "tumour" in count_text(viewer),
        count_text(viewer),
    )

    # The two states that must not collapse. An empty class box is a decision,
    # not an absence of one.
    viewer.set_field(".label-class", "")
    viewer.click_world(*ON_A_LABEL)
    time.sleep(0.8)
    text = count_text(viewer)
    check(
        "an object classed as nothing says so, rather than reading as unlooked-at",
        "nothing in particular" in text,
        text,
    )

    # And clearing is a third thing again: back to never looked at.
    viewer.browser.js("document.querySelector('.label-unassign').click()")
    time.sleep(0.8)
    text = count_text(viewer)
    check(
        "clearing an object returns it to unlooked-at",
        "0 id" in text or "nothing in particular" not in text,
        text,
    )

    # Colour by class is the feedback loop: it is how a curator sees what is
    # left to do without reading a list.
    viewer.set_field(".label-class", "tumour")
    viewer.click_world(*ON_A_LABEL)
    time.sleep(0.8)
    plain = viewer.shot("hash-coloured")
    viewer.browser.js("document.querySelector('.label-color-by-class').click()")
    time.sleep(1.0)
    by_class = viewer.shot("class-coloured")
    ok, detail = viewer.changed_at(plain, by_class, *ON_A_LABEL, radius=6)
    check("colouring ids by class repaints the classed object", ok, detail)
