"""What the browser suites drive: a server, a browser, and a way to assert.

Three things here earn their place, each because getting them wrong cost real
debugging time:

* **`world_to_screen` is derived at run time** from the canvas rectangle and the
  world size the session reports. Every suite used to recompute
  `world * 2.3828 + 90` by hand, which silently encodes one window size and one
  dataset — change either and every coordinate is wrong while every assertion
  still *looks* right.
* **Controls are addressed by role**, never by counting elements. Adding one
  `<select>` to a panel once broke five assertions that were indexing into the
  list of them.
* **Pixels are asserted as a *difference*** against a screenshot taken moments
  before. Marks are coloured by a hash of their class name, so there is no fixed
  colour to threshold against; "did this region change" is the answerable
  question.
"""

import json
import os
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

from PIL import Image

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from cdp import SHIFT, Browser, free_port  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

#: Index of each tool in the canvas tool bar, in the order it renders.
TOOLS = {
    "pan": 0,
    "point": 1,
    "box": 2,
    "ellipse": 3,
    "polygon": 4,
    "freehand": 5,
    "polyline": 6,
    "line": 7,
}


def binary(name):
    """A release binary, else a debug one, else an explanation."""
    for profile in ("release", "debug"):
        path = os.path.join(ROOT, "target", profile, name)
        if os.path.exists(path):
            return path
    raise RuntimeError(f"target/{{release,debug}}/{name} not built — run `make build`")


def demo_store(directory, depth=None):
    """Write the synthetic dataset the suites annotate.

    Generated rather than checked in: every value in it is known by arithmetic,
    which is the same reason the Rust fixtures are synthetic.

    `depth` asks for more z planes than the default eight. The default is a
    *slab*, which is the right cheap fixture for the xy work but tells a suite
    about the z axis almost nothing: an orthogonal pane becomes eight rows
    stretched over half a screen, and a box drawn to true proportions is a sheet.
    """
    command = [binary("make_demo"), directory]
    if depth is not None:
        command += ["--z", str(depth)]
    subprocess.run(command, check=True, capture_output=True)
    return os.path.join(directory, "image.zarr")


class Server:
    """One viewer server, on a port nobody else is using."""

    def __init__(self, store, layers=(), extra=()):
        self.port = free_port()
        self.url = f"http://127.0.0.1:{self.port}"
        command = [binary("server"), "--store", store, "--bind", f"127.0.0.1:{self.port}"]
        for layer in layers:
            command += ["--layer", layer]
        command += list(extra)
        self.proc = subprocess.Popen(
            command, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE
        )
        self._wait()

    def _wait(self, timeout=30.0):
        deadline = time.time() + timeout
        while time.time() < deadline:
            if self.proc.poll() is not None:
                message = self.proc.stderr.read().decode(errors="replace")
                raise RuntimeError(f"the server exited: {message.strip()[:400]}")
            try:
                urllib.request.urlopen(f"{self.url}/api/session", timeout=1)
                return
            except Exception:
                time.sleep(0.2)
        raise RuntimeError("the server did not come up")

    def get(self, path):
        with urllib.request.urlopen(f"{self.url}{path}") as response:
            return json.load(response)

    def close(self):
        self.proc.terminate()
        try:
            self.proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.proc.kill()


class Viewer:
    """A browser pointed at a server, with the viewer's own vocabulary."""

    def __init__(self, server, browser, shots):
        self.server = server
        self.browser = browser
        self.shots = shots
        self._shot = 0
        self.browser.goto(server.url + "/")
        self._measure()

    # -- coordinates ---------------------------------------------------------

    def _measure(self):
        """Learn the canvas rectangle and the world it draws, from the page.

        The vertex shader fits the world into the canvas preserving aspect, so
        the scale is `zoom * min(w/world_w, h/world_h)` and the image is centred.
        Reading it rather than assuming it is what lets a suite run at any
        window size and against any dataset.
        """
        rect = json.loads(
            self.browser.js(
                "JSON.stringify((() => {"
                "  const c = document.querySelector('.viewer-canvas');"
                "  const r = c.getBoundingClientRect();"
                "  return {x: r.left, y: r.top, w: r.width, h: r.height};"
                "})())"
            )
        )
        text = self.browser.js("document.body.innerText") or ""
        world = None
        for line in text.splitlines():
            # The status line says "World: 512 × 512 px".
            if line.strip().startswith("World:"):
                parts = [p for p in line.replace("×", " ").split() if p.isdigit()]
                if len(parts) >= 2:
                    world = (float(parts[0]), float(parts[1]))
                break
        if world is None:
            raise RuntimeError("the page does not report a world size")
        self.rect, self.world = rect, world
        self.fit = min(rect["w"] / world[0], rect["h"] / world[1])

    def to_screen(self, x, y):
        """World pixels to viewport pixels, at the camera's home position."""
        return (
            self.rect["x"] + self.rect["w"] / 2 + (x - self.world[0] / 2) * self.fit,
            self.rect["y"] + self.rect["h"] / 2 + (y - self.world[1] / 2) * self.fit,
        )

    def to_world(self, x, y):
        return (
            (x - self.rect["x"] - self.rect["w"] / 2) / self.fit + self.world[0] / 2,
            (y - self.rect["y"] - self.rect["h"] / 2) / self.fit + self.world[1] / 2,
        )

    # -- acting --------------------------------------------------------------

    def tool(self, name):
        self.browser.js(
            f"document.querySelectorAll('.tool-button')[{TOOLS[name]}].click()"
        )
        time.sleep(0.4)

    def undo(self):
        self.browser.js(
            "[...document.querySelectorAll('.tool-button')].at(-1).click()"
        )
        time.sleep(1.6)

    def click_world(self, x, y, **kw):
        self.browser.click(*self.to_screen(x, y), **kw)

    def double_click_world(self, x, y, **kw):
        self.browser.double_click(*self.to_screen(x, y), **kw)

    def drag_world(self, x0, y0, x1, y1, **kw):
        a, b = self.to_screen(x0, y0), self.to_screen(x1, y1)
        self.browser.drag(a[0], a[1], b[0], b[1], **kw)

    def shift_click_world(self, x, y):
        self.browser.click(*self.to_screen(x, y), modifiers=SHIFT, settle=1.3)

    def zoom(self, delta_y, at=None):
        """Scroll to zoom, about the world point `at` (default: the centre).

        Re-measures afterwards, because every screen coordinate the suite
        computes depends on the camera and the whole point of zooming is to
        change it.
        """
        world = at or (self.world[0] / 2, self.world[1] / 2)
        self.browser.wheel(*self.to_screen(*world), delta_y)
        self._measure_camera()

    def _measure_camera(self):
        """Read the camera back, so `to_screen` still tells the truth."""
        state = self.browser.js(
            "JSON.stringify((() => {"
            "  const c = document.querySelector('.viewer-canvas');"
            "  const r = c.getBoundingClientRect();"
            "  return {x: r.left, y: r.top, w: r.width, h: r.height};"
            "})())"
        )
        self.rect = json.loads(state)
        self.fit = min(self.rect["w"] / self.world[0], self.rect["h"] / self.world[1])

    def drawn_width_at(self, before, after, x, y, reach=260):
        """How wide, in screen pixels, the change around `(x, y)` extends.

        Walks out along the centre row from the point until the pixels stop
        differing from `before`. A radius is only meaningful as a measured
        extent — asserting that *something* changed would pass for a dot.
        """
        cx, cy = self.to_screen(x, y)
        a, b = Image.open(before).convert("RGB"), Image.open(after).convert("RGB")
        width, height = a.size

        def differs(px, py):
            px, py = min(max(int(px), 0), width - 1), min(max(int(py), 0), height - 1)
            return sum(abs(p - q) for p, q in zip(a.getpixel((px, py)), b.getpixel((px, py)))) > 40

        span = 0
        for step in range(1, reach):
            if differs(cx + step, cy) or differs(cx - step, cy):
                span = step
        return span * 2

    def press(self, label):
        """Click the button whose text is `label`."""
        ok = self.browser.js(
            "(() => {"
            "  const b = [...document.querySelectorAll('button')]"
            f"    .find(e => e.textContent.trim() === {label!r});"
            "  if (!b) return false; b.click(); return true;"
            "})()"
        )
        if not ok:
            raise RuntimeError(f"no button labelled {label!r}")
        time.sleep(1.4)

    def set_field(self, selector, value, event="input"):
        """Set a Yew-controlled input the way a keystroke would.

        The native value setter plus a *typed* event: Yew's `oninput` takes an
        `InputEvent`, and a plain `Event` named "input" is silently not
        delivered — which looks exactly like the control not working.
        """
        kind = "InputEvent" if event == "input" else "Event"
        result = self.browser.js(
            f"""
            (() => {{
              const el = document.querySelector({selector!r});
              if (!el) return 'missing';
              const proto = el.tagName === 'SELECT'
                ? window.HTMLSelectElement.prototype
                : window.HTMLInputElement.prototype;
              const set = Object.getOwnPropertyDescriptor(proto, 'value').set;
              set.call(el, {value!r});
              el.dispatchEvent(new {kind}({event!r}, {{bubbles: true}}));
              return 'ok';
            }})()
            """
        )
        if result != "ok":
            raise RuntimeError(f"no control matching {selector!r}")
        time.sleep(0.6)

    def select(self, selector, value):
        self.set_field(selector, value, event="change")

    # -- the annotation list -------------------------------------------------

    def rows(self):
        return self.browser.js("document.querySelectorAll('.annot-row').length")

    def select_row(self, index):
        """Make sure row `index` ends up selected.

        Clicking the glyph *toggles*, and a freshly drawn shape is already
        selected — so clicking unconditionally is how a test deselects the thing
        it is about to drag, and then pans the camera instead.
        """
        self.browser.js(
            f"""
            (() => {{
              const list = [...document.querySelectorAll('.annot-row')];
              const row = {index} < 0 ? list.at({index}) : list[{index}];
              if (row && !row.classList.contains('selected')) {{
                row.querySelector('.annot-kind').click();
              }}
            }})()
            """
        )
        time.sleep(0.7)
        return self.browser.js(
            f"""
            (() => {{
              const list = [...document.querySelectorAll('.annot-row')];
              const row = {index} < 0 ? list.at({index}) : list[{index}];
              return !!row && row.classList.contains('selected');
            }})()
            """
        )

    def layers(self):
        """Every open layer as `(id, name, kind)`."""
        return [
            (l["id"], l["name"], l["kind"]["kind"])
            for l in self.server.get("/api/session")["layers"]
        ]

    def layer_ids(self, kind):
        """The ids of every layer of one kind, in draw order.

        Asked for rather than assumed: a layer id is assigned by the session and
        depends on what else has been opened, so a test that hardcodes `L1` is
        one that breaks the first time a suite opens something before it.
        """
        return [id for id, _, k in self.layers() if k == kind]

    def annotations(self, layer=None):
        layer = layer or self.layer_ids("annotations")[0]
        return self.server.get(f"/api/annotations/{layer}")

    def geometries(self, layer=None):
        return [a["geometry"]["type"] for a in self.annotations(layer)]

    def text(self):
        return self.browser.js("document.body.innerText") or ""

    # -- pixels --------------------------------------------------------------

    def shot(self, tag):
        self._shot += 1
        path = os.path.join(self.shots, f"{self._shot:02d}-{tag}.png")
        return self.browser.screenshot(path)

    def changed_at(self, before, after, x, y, radius=5, threshold=40):
        """Did the neighbourhood of world point `(x, y)` change between shots?

        A difference rather than a colour test: annotations are coloured by a
        hash of their class name and label images by a measurement ramp, so
        there is no fixed colour to compare against.
        """
        sx, sy = self.to_screen(x, y)
        a, b = _patch(before, sx, sy, radius), _patch(after, sx, sy, radius)
        worst = max(
            sum(abs(p - q) for p, q in zip(pa, pb)) for pa, pb in zip(a, b)
        )
        return worst > threshold, f"delta {worst}"

    def close(self):
        self.browser.close()


def _patch(path, x, y, radius):
    image = Image.open(path).convert("RGB")
    width, height = image.size
    return [
        image.getpixel(
            (
                min(max(int(x) + dx, 0), width - 1),
                min(max(int(y) + dy, 0), height - 1),
            )
        )
        for dy in range(-radius, radius + 1)
        for dx in range(-radius, radius + 1)
    ]


def bounds_of(annotation):
    """An annotation's bounding box, from its GeoJSON geometry."""
    flat = []

    def walk(value):
        if isinstance(value, list) and value and isinstance(value[0], (int, float)):
            flat.append(value)
        elif isinstance(value, list):
            for item in value:
                walk(item)

    walk(annotation["geometry"]["coordinates"])
    xs = [p[0] for p in flat]
    ys = [p[1] for p in flat]
    return min(xs), min(ys), max(xs), max(ys)


class Checks:
    """A tally of assertions, so a suite reports rather than stopping dead."""

    def __init__(self, name):
        self.name = name
        self.passed = 0
        self.failed = []

    def __call__(self, what, ok, detail=""):
        if ok:
            self.passed += 1
            print(f"  PASS  {what}" + (f"   [{detail}]" if detail else ""))
        else:
            self.failed.append(what)
            print(f"  FAIL  {what}" + (f"   [{detail}]" if detail else ""))
        return ok

    def near(self, what, got, want, tolerance, detail=""):
        return self(
            what,
            abs(got - want) <= tolerance,
            detail or f"{got:.2f}, wanted {want:.2f} ± {tolerance}",
        )
