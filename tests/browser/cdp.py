"""A minimal Chrome DevTools Protocol driver.

There are no wasm tests: the frontend is verified by driving a real browser,
acting, screenshotting, and asserting on pixels. This is the driver that does
the driving; `harness.py` is what the suites actually use.

Deliberately small. A browser-automation library would bring a dependency, a
version to keep current, and its own opinions about waiting; what these tests
need is navigate, click, drag, evaluate, screenshot.
"""

import base64
import json
import os
import shutil
import subprocess
import tempfile
import time
import urllib.request

import websocket

#: WebGL2 fails in headless Chrome without a software rasteriser, and SwiftShader
#: has to be asked for twice — once to select it and once to permit it.
WEBGL_FLAGS = ["--use-angle=swiftshader", "--enable-unsafe-swiftshader"]

#: CDP modifier bits. Only Shift is used, as the vertex-editing modifier.
SHIFT = 8


class Browser:
    """One headless Chrome, on a port nobody else is using."""

    def __init__(self, size=(1500, 1400), port=None):
        # A port of our own, derived from the pid: other sessions on the same
        # machine run their own headless Chrome, and attaching to somebody
        # else's page target is a confusing way to fail — it presents as a
        # window size nobody asked for.
        self.port = port or 9400 + (os.getpid() % 500)
        self.profile = tempfile.mkdtemp(prefix="omezarr-cdp-")
        self.proc = subprocess.Popen(
            [
                chrome_binary(),
                "--headless=new",
                f"--remote-debugging-port={self.port}",
                f"--user-data-dir={self.profile}",
                f"--window-size={size[0]},{size[1]}",
                *WEBGL_FLAGS,
                "--no-sandbox",
                "--disable-gpu-sandbox",
                "--hide-scrollbars",
                "about:blank",
            ],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        self.id = 0
        self.ws = self._attach()

    def _attach(self, timeout=30.0):
        deadline = time.time() + timeout
        while time.time() < deadline:
            try:
                targets = json.load(
                    urllib.request.urlopen(f"http://127.0.0.1:{self.port}/json")
                )
                pages = [t for t in targets if t["type"] == "page"]
                if pages:
                    return websocket.create_connection(
                        pages[0]["webSocketDebuggerUrl"], timeout=60
                    )
            except Exception:
                pass
            time.sleep(0.25)
        raise RuntimeError(f"chrome did not come up on port {self.port}")

    def send(self, method, **params):
        self.id += 1
        self.ws.send(json.dumps({"id": self.id, "method": method, "params": params}))
        while True:
            message = json.loads(self.ws.recv())
            if message.get("id") == self.id:
                if "error" in message:
                    raise RuntimeError(f"{method}: {message['error']}")
                return message.get("result", {})

    # -- navigation and evaluation ------------------------------------------

    def goto(self, url, settle=3.0):
        self.send("Page.enable")
        self.send("Page.navigate", url=url)
        time.sleep(settle)

    def js(self, expression):
        """Evaluate an expression and return its value."""
        result = self.send(
            "Runtime.evaluate",
            expression=expression,
            returnByValue=True,
            awaitPromise=True,
        )
        return result.get("result", {}).get("value")

    # -- input ---------------------------------------------------------------

    def click(self, x, y, modifiers=0, settle=0.35):
        for kind, buttons in (("mousePressed", 1), ("mouseReleased", 0)):
            self.send(
                "Input.dispatchMouseEvent",
                type=kind,
                x=x,
                y=y,
                button="left",
                clickCount=1,
                buttons=buttons,
                modifiers=modifiers,
            )
            time.sleep(0.04)
        time.sleep(settle)

    def double_click(self, x, y, settle=0.8):
        """A double-click, preceded by the single click the browser sends first.

        The viewer's click-by-click tools add a vertex on `mouseup` and finish on
        `dblclick`, so a test that skips the single click is testing a sequence
        no browser produces.
        """
        self.click(x, y, settle=0.05)
        for kind, buttons in (("mousePressed", 1), ("mouseReleased", 0)):
            self.send(
                "Input.dispatchMouseEvent",
                type=kind,
                x=x,
                y=y,
                button="left",
                clickCount=2,
                buttons=buttons,
            )
        time.sleep(settle)

    def drag(self, x0, y0, x1, y1, steps=8, settle=0.8):
        self.send(
            "Input.dispatchMouseEvent",
            type="mousePressed",
            x=x0,
            y=y0,
            button="left",
            clickCount=1,
            buttons=1,
        )
        for i in range(1, steps + 1):
            self.send(
                "Input.dispatchMouseEvent",
                type="mouseMoved",
                x=x0 + (x1 - x0) * i / steps,
                y=y0 + (y1 - y0) * i / steps,
                button="left",
                buttons=1,
            )
            time.sleep(0.02)
        self.send(
            "Input.dispatchMouseEvent",
            type="mouseReleased",
            x=x1,
            y=y1,
            button="left",
            clickCount=1,
            buttons=0,
        )
        time.sleep(settle)

    # -- output --------------------------------------------------------------

    def screenshot(self, path):
        data = self.send("Page.captureScreenshot", format="png")["data"]
        with open(path, "wb") as handle:
            handle.write(base64.b64decode(data))
        return path

    def close(self):
        try:
            self.ws.close()
        except Exception:
            pass
        self.proc.terminate()
        try:
            self.proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.proc.kill()
        shutil.rmtree(self.profile, ignore_errors=True)


def chrome_binary():
    """The first Chrome or Chromium on this machine."""
    for name in ("google-chrome", "chromium", "chromium-browser", "chrome"):
        found = shutil.which(name)
        if found:
            return found
    raise RuntimeError(
        "no Chrome or Chromium found; the browser tests need one "
        "(see tests/browser/README.md)"
    )
