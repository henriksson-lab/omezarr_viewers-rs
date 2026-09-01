"""A pick's circle is a size in the image, not a size on the screen.

The whole judgement a particle picker makes is *does this circle enclose the
thing*, so a radius that looked right at one zoom and wrong at another would be
worse than no circle at all. That is the claim, it is a claim about pixels, and
so it is measured off screenshots rather than read back out of the state.

The measurement is a with/without difference at one camera position: hide the
annotation layer, shoot, show it, shoot, and walk outwards until the two stop
differing. Comparing two *zooms* directly would not work — everything moves.
"""

import time

NAME = "picking"


def annotation_layer_checkbox(viewer):
    """The visibility box of the panel that owns the annotation table."""
    return """
        (() => {
          const panel = [...document.querySelectorAll('.channel-control')]
            .find(e => e.querySelector('.annot-table'));
          return panel && panel.querySelector('.channel-header input[type=checkbox]');
        })()
    """


def width_here(viewer, tag):
    """The drawn width of the pick at (200, 200), with the layer off and on."""
    js = annotation_layer_checkbox(viewer)
    viewer.browser.js(f"{js}.click()")
    time.sleep(0.7)
    without = viewer.shot(f"{tag}-without")
    viewer.browser.js(f"{js}.click()")
    time.sleep(0.7)
    with_it = viewer.shot(f"{tag}-with")
    return viewer.drawn_width_at(without, with_it, 200, 200)


def run(viewer, check):
    viewer.tool("point")
    viewer.click_world(200, 200)
    viewer.tool("pan")
    check("a pick was placed", viewer.rows() == 1, str(viewer.rows()))

    # Screen-space marker: the existing behaviour, and the control.
    marker = width_here(viewer, "marker")
    check("the marker is drawn", marker > 2, f"{marker}px")

    viewer.browser.js("document.querySelector('.annot-world-radius').click()")
    time.sleep(0.8)
    near = width_here(viewer, "near")
    check("a true-size circle is drawn", near > 2, f"{near}px")

    # Zoom in about the pick, then measure the same way. A world radius grows
    # with the image; a screen radius would not move at all.
    viewer.zoom(-600, at=(200, 200))
    far = width_here(viewer, "far")
    check(
        "the circle grows with the image when zoomed",
        far > near * 1.3,
        f"{near}px -> {far}px",
    )

    # And the control: the marker must *not* grow, or the test above proves
    # nothing about world-space in particular.
    viewer.browser.js("document.querySelector('.annot-world-radius').click()")
    time.sleep(0.8)
    marker_zoomed = width_here(viewer, "marker-zoomed")
    check(
        "a screen-space marker keeps its size at any zoom",
        abs(marker_zoomed - marker) <= max(4, marker * 0.35),
        f"{marker}px -> {marker_zoomed}px",
    )
