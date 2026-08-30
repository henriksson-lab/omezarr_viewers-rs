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

## When Chrome will not start

`run.py` launches one browser before any suite and gives up immediately if that
fails, because a Chrome that will not start is an environment problem rather
than a test failure — discovering it once per suite buried the one useful line
under six identical stack traces.

The error carries Chrome's own stderr, its exit status and the binary that was
run:

    the browser suites need a working Chrome:
    chrome would not start.
      attempt 1: chrome exited with status 133 before opening port 59221
        binary: /usr/bin/google-chrome
        chrome said:
          Failed to move to new namespace

That detail exists because the first CI run of this gate failed with nothing but
`chrome did not come up on port 9549`: the driver was sending Chrome's output to
`DEVNULL`, so the only thing that knew what was wrong was thrown away. It is
kept now. Two flags earn their place for the same reason —
`--disable-dev-shm-usage`, because a CI runner's `/dev/shm` is small and Chrome
dies during startup when its shared memory will not fit, and
`--disable-setuid-sandbox` beside the existing `--no-sandbox`.

The *second* CI failure is why the driver passes `suppress_origin=True` and
`--remote-allow-origins`. **Chrome 111 began refusing a DevTools WebSocket whose
`Origin` it had not been told to allow**, and `websocket-client` sends one
derived from the URL. A non-browser client has no business sending `Origin` at
all, so suppressing it is the fix; the launch flag is Chrome's own documented
remedy, kept as a second line of defence and scoped to the single origin that
can reach the port rather than `*`.

## A local pass is weaker evidence than it looks

That bug reached CI three times **because this machine could not reproduce it**:
Chrome 104 here, a current build on the runner, and the check that failed did
not exist before 111. `run.py` now prints the browser it used, and says so when
it is older than 111. When a browser failure appears only in CI, compare the
version in the runner log's "Check for a browser" step against the one printed
locally before assuming the code is at fault.
