"""Tile textures are bounded, and giving them up actually gives them up.

VRAM is the resource with a hard limit and no way to ask about it: WebGL2
offers no query for how much there is or how much is left. So the only honest
check is behavioural — that the viewer's own accounting rises when it loads
tiles and falls when it stops needing them.

That second half is the one worth a test. Dropping a `TileTexture` releases the
JS handle and leaves the GPU memory for the garbage collector, which cannot see
VRAM pressure and has no reason to run when the JS heap has not grown. The
eviction has to say so explicitly, and nothing but a test would notice if it
stopped.
"""

import re
import time

NAME = "caching"
#: Big enough to have several levels and far more tiles than fit on screen. The
#: default fixture fits in eight tiles at every zoom, so nothing is ever
#: evicted and every assertion here would pass without testing anything.
NEEDS_SHAPE = (4, 4096, 4096)


def held(viewer):
    """`(tiles, megabytes)` from the status line."""
    text = viewer.text()
    match = re.search(r"Tiles: (\d+) cached \((\d+) MB\)", text)
    return (int(match.group(1)), int(match.group(2))) if match else (None, None)


def run(viewer, check):
    time.sleep(1.5)
    tiles, mb = held(viewer)
    check("the viewer reports what it is holding", tiles is not None, f"{tiles} {mb}")

    # Zoom in: a finer level loads, so more tiles and more bytes.
    for _ in range(4):
        viewer.zoom(-240)
    time.sleep(2.5)
    zoomed, zoomed_mb = held(viewer)
    check(
        "zooming in loads tiles and the bytes follow",
        zoomed is not None and zoomed_mb is not None and zoomed_mb > mb,
        f"{tiles} tiles/{mb} MB -> {zoomed} tiles/{zoomed_mb} MB",
    )

    # Zoom back out: the finer level is dead weight once the camera leaves it,
    # and the store must actually let go rather than accumulate.
    for _ in range(6):
        viewer.zoom(240)
    time.sleep(3.0)
    out, out_mb = held(viewer)
    check(
        "zooming back out gives the tiles up again",
        out is not None and out < zoomed,
        f"{zoomed} tiles/{zoomed_mb} MB -> {out} tiles/{out_mb} MB",
    )
    check(
        "and the bytes it reports go with them",
        out_mb is not None and zoomed_mb is not None and out_mb < zoomed_mb,
        f"{zoomed_mb} MB -> {out_mb} MB",
    )
