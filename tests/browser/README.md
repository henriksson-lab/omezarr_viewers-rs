# Browser tests

There are no wasm tests. The frontend is verified the way it has always been
verified here: by driving a real browser over the DevTools Protocol, acting, and
asserting on **pixels** and on the **server's own rows**.

    make build          # the server serves dist/, so the frontend must exist
    make test-browser

## What it needs

* **Chrome or Chromium** on `PATH`. WebGL2 fails in headless Chrome without a
  software rasteriser, so the driver passes `--use-angle=swiftshader
  --enable-unsafe-swiftshader`.
* **Python 3** with `websocket-client` and `Pillow`:

      pip install websocket-client pillow

Everything else — the dataset, the server, the ports — the harness makes.

## How it is put together

| | |
|---|---|
| `cdp.py` | the DevTools driver: navigate, click, drag, evaluate, screenshot |
| `harness.py` | a server, a browser, coordinates, and a tally of assertions |
| `suites/` | one module per area, each exposing `run(viewer, check)` |
| `run.py` | starts a fresh server and store per suite, and reports |

Run one suite while working on it:

    python3 tests/browser/run.py editing --keep --shots /tmp/shots

## Three rules these tests follow

Each is here because breaking it cost real debugging time.

**Coordinates are world coordinates.** `viewer.click_world(120, 120)` converts
through a scale read from the page at run time. Suites used to compute
`world * 2.3828 + 90` by hand, which silently encodes one window size and one
dataset: change either and every coordinate is wrong while every assertion still
*looks* right.

**Controls are addressed by role**, never by counting elements — `.annot-filter`,
`.annot-selected-type`, or a button's own text. Adding one `<select>` to a panel
once broke five assertions that were indexing into the list of them.

**Pixels are asserted as a difference** against a screenshot taken moments
before, at a point the test chose. Annotations are coloured by a hash of their
class name and label images by a measurement ramp, so there is no fixed colour
to threshold against; "did this change" is the answerable question.

## When one fails

Screenshots are kept — the path is printed at the end, or pass `--shots`. A
failure line carries what was measured, so a wrong *expectation* and a wrong
*rendering* can be told apart without re-running.

Each suite gets a fresh server and store. Sharing one is how two suites came to
pass individually and fail together: the session holds annotations in memory.
