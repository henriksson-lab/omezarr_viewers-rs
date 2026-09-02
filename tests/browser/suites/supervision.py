"""A scribble covers pixels; a dense region says its emptiness is a finding.

Both fields are claims about *pixels*, and both are drawn, so both are
verifiable only here: the unit tests say `stroke_band` returns triangles of the
right extent, and nothing but a browser says those triangles reach the screen at
the width that was asked for, in the image's own coordinates rather than the
screen's.

Two things about the shape of this suite:

* The plain line is drawn first because the annotation panel does not exist
  until an annotation layer does, and the layer is created by the first shape.
  That is not a workaround — the width applies to the *next* shape, so setting
  it before there was a panel would assert about a control the suite never used.
* Widths are measured the way `picking` measures a radius: hide the annotation
  layer, shoot, show it, shoot, and walk out until the two stop differing. A
  plain before/after around a *draw* also catches the previous shape losing its
  selection highlight, which reads as a band four times its real width.
"""

import time

from PIL import Image

NAME = "supervision"

#: World px. Wide enough that the band cannot be confused with the centreline
#: stroked underneath it at any zoom this suite uses.
WIDTH = 24.0

#: Screen px to walk out from the path. Bounded well inside the 100 world px to
#: the next shape, so the measurement cannot wander onto it.
REACH = 100

#: The dense region, in world coordinates. Inside the 512-square demo image:
#: a corner outside it is a drag that ends off the canvas and draws nothing.
BOX = (360, 150, 470, 270)


def annotation_layer_checkbox():
    """The visibility box of the panel that owns the annotation table."""
    return """
        (() => {
          const panel = [...document.querySelectorAll('.channel-control')]
            .find(e => e.querySelector('.annot-table'));
          return panel && panel.querySelector('.channel-header input[type=checkbox]');
        })()
    """


def width_at(viewer, tag, x, y):
    """The drawn width across the path at `(x, y)`, layer off against on."""
    js = annotation_layer_checkbox()
    viewer.browser.js(f"{js}.click()")
    time.sleep(0.7)
    without = viewer.shot(f"{tag}-without")
    viewer.browser.js(f"{js}.click()")
    time.sleep(0.7)
    with_it = viewer.shot(f"{tag}-with")
    return viewer.drawn_width_at(without, with_it, x, y, reach=REACH)


def hatched_fraction(viewer, before, after, box):
    """What fraction of the pixels inside `box` (world) the change covers.

    A fraction rather than a probe: the hatch is ~10 lines across the region, so
    a five-pixel patch lands between them about as often as on them — which is
    a test that fails on where it happened to look. The fraction also says the
    thing worth saying, which is that a dense region is *hatched* and not
    filled: a solid wash would cover all of it, and would be indistinguishable
    from an ordinary region drawn with Fill on.
    """
    x0, y0 = viewer.to_screen(box[0], box[1])
    x1, y1 = viewer.to_screen(box[2], box[3])
    # Inset, so the region's own outline is not what is being counted.
    inset = 6
    a = Image.open(before).convert("RGB").crop(
        (int(x0) + inset, int(y0) + inset, int(x1) - inset, int(y1) - inset)
    )
    b = Image.open(after).convert("RGB").crop(
        (int(x0) + inset, int(y0) + inset, int(x1) - inset, int(y1) - inset)
    )
    changed = sum(
        1
        for p, q in zip(a.getdata(), b.getdata())
        if sum(abs(u - v) for u, v in zip(p, q)) > 40
    )
    return changed / float(a.size[0] * a.size[1])


def drag_showing_draft(viewer, tag, x0, y0, x1, y1, steps=20):
    """Drag, but stop with the button still down and shoot the draft.

    `Browser.drag` finishes the gesture, and a draft only exists while it is in
    progress. What is on screen at that moment is the whole point: the shape you
    are about to get should be the shape you can see, and until the band was
    drawn here it appeared only on mouse-up.
    """
    sx0, sy0 = viewer.to_screen(x0, y0)
    sx1, sy1 = viewer.to_screen(x1, y1)
    send = viewer.browser.send
    send("Input.dispatchMouseEvent", type="mousePressed", x=sx0, y=sy0,
         button="left", clickCount=1, buttons=1)
    for i in range(1, steps + 1):
        send("Input.dispatchMouseEvent", type="mouseMoved",
             x=sx0 + (sx1 - sx0) * i / steps, y=sy0 + (sy1 - sy0) * i / steps,
             button="left", buttons=1)
        time.sleep(0.02)
    time.sleep(0.6)
    shot = viewer.shot(tag)
    send("Input.dispatchMouseEvent", type="mouseReleased", x=sx1, y=sy1,
         button="left", clickCount=1, buttons=0)
    time.sleep(0.9)
    return shot


def hook(viewer, selector):
    return viewer.browser.js(
        f"(document.querySelector({selector!r}) || {{}}).textContent || ''"
    )


def run(viewer, check):
    # One world pixel, in screen pixels, at the camera the suite starts at.
    x0, _ = viewer.to_screen(0, 0)
    x1, _ = viewer.to_screen(100, 0)
    scale = (x1 - x0) / 100.0

    # -- a geometric line: no width, and drawn as none ------------------------

    # Vertical, because a width is measured across the path.
    viewer.tool("line")
    viewer.drag_world(300, 120, 300, 320, steps=20)
    viewer.tool("pan")
    hairline = width_at(viewer, "hairline", 300, 220)
    # A few pixels, not none: the line itself, plus the selection highlight it
    # still carries from being the shape most recently drawn.
    check("a line with no width is drawn as a line", 0 < hairline <= 12, f"{hairline}px")
    check(
        "and the panel says so in words rather than showing an empty box",
        "geometric line" in hook(viewer, ".annot-stroke-readout"),
        hook(viewer, ".annot-stroke-readout"),
    )
    check(
        "with no dense region drawn, the panel says what that means",
        "unexamined" in viewer.text(),
    )

    # -- a scribble -----------------------------------------------------------

    viewer.browser.js("document.querySelector('.annot-stroke-on').click()")
    viewer.select(".annot-stroke-width", str(WIDTH))
    check(
        "ticking it offers a width",
        "px wide" in hook(viewer, ".annot-stroke-readout"),
        hook(viewer, ".annot-stroke-readout"),
    )

    viewer.tool("line")
    # The baseline the draft is measured against: everything on screen except
    # the shape about to be drawn.
    settled = viewer.shot("settled")
    drafting = drag_showing_draft(viewer, "draft", 200, 120, 200, 320)
    draft_band = viewer.drawn_width_at(settled, drafting, 200, 220, reach=REACH)
    viewer.tool("pan")
    band = width_at(viewer, "band", 200, 220)
    want = WIDTH * scale
    check(
        "the band is drawn at the width that was asked for",
        0.6 * want <= band <= 1.6 * want,
        f"{band}px drawn, {want:.0f}px asked for (scale {scale:.2f})",
    )
    check(
        "which is wider than a line covering no pixels at all",
        band > 3 * hairline,
        f"{band}px against {hairline}px",
    )

    check(
        "the draft is banded while it is being drawn, not only once it lands",
        0.6 * want <= draft_band <= 1.6 * want,
        f"{draft_band}px mid-drag, {band}px once stored",
    )

    rows = viewer.annotations()
    check(
        "the widths reached the server, and only the scribble has one",
        [a["stroke_width"] for a in rows] == [None, WIDTH],
        str([a["stroke_width"] for a in rows]),
    )

    # -- a scribble is grabbed by its band, not by its centreline -------------

    # The scribble is the shape most recently drawn, and so the selected one.
    # Shift on an edge inserts a vertex there; the question is what counts as
    # the edge. Half the band is 12 world px, and the hand's own tolerance here
    # is about 4, so a click 9 px off the centreline is plainly inside the shape
    # on screen and outside a centreline tolerance.
    def vertices():
        return len(viewer.annotations()[1]["geometry"]["coordinates"])

    before = vertices()
    viewer.shift_click_world(209, 215)
    check(
        "a shift-click inside the band lands on the shape",
        vertices() == before + 1,
        f"{before} -> {vertices()} vertices",
    )

    # And the band is a bound, not an excuse for an unbounded target.
    held = vertices()
    viewer.shift_click_world(224, 245)
    check(
        "a shift-click outside the band still misses",
        vertices() == held,
        f"{held} -> {vertices()} vertices",
    )

    # -- a dense region -------------------------------------------------------

    viewer.tool("box")
    viewer.drag_world(*BOX)
    plain = viewer.shot("box")

    viewer.browser.js("document.querySelector('.annot-dense').click()")
    dense = viewer.shot("dense")
    covered = hatched_fraction(viewer, plain, dense, BOX)
    check(
        "marking a region dense marks its inside",
        covered > 0.02,
        f"{covered:.1%} of the region changed",
    )
    check(
        "and hatches it rather than filling it, which would say something else",
        covered < 0.5,
        f"{covered:.1%} of the region changed",
    )

    flags = [a["dense_region"] for a in viewer.annotations()]
    check(
        "the flag reached the server, and only for that shape",
        flags == [False, False, True],
        str(flags),
    )
    check(
        "a box drawn while a width is set does not become a scribble",
        [a["stroke_width"] for a in viewer.annotations()] == [None, WIDTH, None],
        str([a["stroke_width"] for a in viewer.annotations()]),
    )
    check(
        "the panel counts it",
        "1 dense region" in hook(viewer, ".annot-dense-count"),
        hook(viewer, ".annot-dense-count"),
    )
    check(
        "and counts the scribble separately, because they are different claims",
        "1 scribble" in hook(viewer, ".annot-scribble-count"),
        hook(viewer, ".annot-scribble-count"),
    )

    # -- the width is a size in the image, not on the screen ------------------

    # Zoomed about a point on the scribble, so that point does not move and the
    # measurement is taken across the same place on the same path.
    viewer.zoom(-600, at=(200, 220))
    wider = width_at(viewer, "zoomed", 200, 220)
    check(
        "the band grows with the image, because its width is in image pixels",
        wider > band * 1.2,
        f"{band}px -> {wider}px",
    )
