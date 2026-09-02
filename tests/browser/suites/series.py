"""A container's series are alternatives, and only the first is drawn.

`bioformats2raw` — and `img2omezarr`, which writes the same layout for
everything it produces — stores several images in one container. Opening it
gives a layer per series, and they must not all be drawn at once: stacked image
layers composite *additively* here, so two visible series sum two unrelated
pictures into one that means nothing.

This is a claim about pixels, so it is measured in pixels. Before the fix, two
identical series rendered at 1.75x the brightness of one.
"""

import time

from PIL import Image

NAME = "series"
NEEDS_CONTAINER = True


def centre_brightness(path, size=300):
    image = Image.open(path).convert("L")
    width, height = image.size
    box = image.crop(
        (
            width // 2 - size // 2,
            height // 2 - size // 2,
            width // 2 + size // 2,
            height // 2 + size // 2,
        )
    )
    return sum(box.getdata()) / float(size * size)


def run(viewer, check):
    layers = viewer.layers()
    check(
        "both series opened, one layer each",
        len(layers) == 2,
        str(layers),
    )
    check(
        "and they are named apart, or the panel shows two identical rows",
        len({name for _, name, _ in layers}) == 2,
        str([name for _, name, _ in layers]),
    )

    # A layer's visibility box is the one labelled with the layer's name.
    # Addressed that way rather than by class because `.channel-header` belongs
    # to each *channel* row as well, and a selector that matches both silently
    # measures the wrong control — which is what the first version of this suite
    # did, reporting four boxes for two layers.
    names = [name for _, name, _ in layers]
    LAYER_BOXES = (
        "[...document.querySelectorAll('input[type=checkbox]')].filter(e => "
        f"{names!r}.includes((e.parentElement.textContent||'').trim()))"
    ).replace("'", '"')
    boxes = viewer.browser.js(f"{LAYER_BOXES}.map(e => e.checked)")
    check(
        "both layers have a visibility box, and only those two were found",
        len(boxes) == 2,
        str(boxes),
    )
    check(
        "the second series arrives unticked",
        boxes == [True, False],
        str(boxes),
    )

    # The measurement that matters: what is on the canvas.
    one = centre_brightness(viewer.shot("first-only"))

    # Turn the second on, and the picture must change — otherwise this suite
    # would pass just as well against a viewer that never drew it at all.
    viewer.browser.js(f"{LAYER_BOXES}[1].click()")
    # A layer that has never been shown has no tiles yet, and the request for
    # them is a round trip. Shooting straight after the click measures the
    # picture without it, which reads exactly like the layer not drawing.
    time.sleep(3.0)
    both = centre_brightness(viewer.shot("both"))
    check(
        "showing the second one does change the picture",
        both > one * 1.2,
        f"{one:.0f} -> {both:.0f}",
    )
    check(
        "so the default of drawing only the first is doing real work",
        one < both,
        f"{one:.0f} against {both:.0f} with both on",
    )
