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
import socket
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


def free_port():
    """A port nobody is listening on.

    Asked of the kernel rather than derived from the pid: a hash of the pid
    collides, and a Chrome that cannot bind its debugging port exits silently,
    which presented as "chrome did not come up" with nothing to go on.
    """
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


class Browser:
    """One headless Chrome, on a port nobody else is using."""

    def __init__(self, size=(1500, 1400), port=None, attempts=3):
        # Set first so `close()` is safe even if the launch below never gets
        # off the ground.
        self.proc = None
        self.profile = None
        self.log = None
        self.ws = None
        self.id = 0
        # Resolved once, outside the retry loop: a browser that is not
        # installed will not become installed on the second attempt, and three
        # copies of that message buries it.
        self.binary = chrome_binary()

        failures = []
        for attempt in range(1, attempts + 1):
            try:
                self._launch(size, port)
                self.ws = self._attach()
                return
            except RuntimeError as error:
                failures.append(f"attempt {attempt}: {error}")
                self._stop()
        raise RuntimeError(
            "chrome would not start.\n  " + "\n  ".join(failures)
        )

    def _launch(self, size, port):
        """Start one Chrome, with its output kept rather than discarded."""
        # A port of our own: other sessions on the same machine run their own
        # headless Chrome, and attaching to somebody else's page target is a
        # confusing way to fail — it presents as a window size nobody asked for.
        self.port = port or free_port()
        self.profile = tempfile.mkdtemp(prefix="omezarr-cdp-")
        # Chrome's diagnostics go to a file rather than to DEVNULL. When it
        # refuses to start, its own stderr is the only thing that says why, and
        # throwing that away turned every startup failure into a bare
        # "chrome did not come up" — which is what it did on CI.
        self.log = tempfile.NamedTemporaryFile(
            prefix="omezarr-chrome-", suffix=".log", delete=False
        )
        self.command = [
            self.binary,
            "--headless=new",
            f"--remote-debugging-port={self.port}",
            f"--user-data-dir={self.profile}",
            f"--window-size={size[0]},{size[1]}",
            *WEBGL_FLAGS,
            "--no-sandbox",
            "--disable-setuid-sandbox",
            "--disable-gpu-sandbox",
            # /dev/shm is small on a CI runner, and Chrome's default is to put
            # its shared memory there; when it does not fit, Chrome dies during
            # startup rather than reporting anything useful.
            "--disable-dev-shm-usage",
            "--hide-scrollbars",
            # Chrome 111+ refuses a WebSocket to the DevTools port unless the
            # connection's Origin is allowed. Scoped to the only origin that can
            # reach this port rather than `*`, which would let any page on the
            # machine drive the browser.
            f"--remote-allow-origins=http://127.0.0.1:{self.port}",
            "about:blank",
        ]
        self.proc = subprocess.Popen(
            self.command, stdout=self.log, stderr=subprocess.STDOUT
        )

    def _attach(self, timeout=30.0):
        deadline = time.time() + timeout
        while time.time() < deadline:
            # A Chrome that has already exited will never open the port, so say
            # so now instead of waiting out the timeout.
            code = self.proc.poll()
            if code is not None:
                raise RuntimeError(
                    f"chrome exited with status {code} before opening port "
                    f"{self.port}{self._diagnosis()}"
                )
            try:
                targets = json.load(
                    urllib.request.urlopen(f"http://127.0.0.1:{self.port}/json")
                )
                pages = [t for t in targets if t["type"] == "page"]
                if pages:
                    # `suppress_origin` because this is not a page: websocket
                    # -client otherwise sends an `Origin` derived from the URL,
                    # and Chrome 111+ rejects the handshake as cross-origin.
                    # That is the actual cause; the launch flag above is the
                    # documented remedy kept as a second line of defence.
                    return websocket.create_connection(
                        pages[0]["webSocketDebuggerUrl"],
                        timeout=60,
                        suppress_origin=True,
                    )
            except Exception:
                pass
            time.sleep(0.25)
        raise RuntimeError(
            f"chrome did not open port {self.port} within {timeout:.0f}s, and is "
            f"still running{self._diagnosis()}"
        )

    def _diagnosis(self):
        """The binary, and whatever Chrome managed to say before giving up."""
        detail = f"\n    binary: {getattr(self, 'binary', '?')}"
        try:
            self.log.flush()
            with open(self.log.name) as handle:
                said = handle.read().strip()
        except Exception:
            said = ""
        if said:
            tail = "\n      ".join(said.splitlines()[-12:])
            detail += f"\n    chrome said:\n      {tail}"
        else:
            detail += "\n    chrome said nothing"
        return detail

    def _stop(self):
        """Tear down whatever the last attempt managed to create."""
        if self.proc is not None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.proc.kill()
            self.proc = None
        if self.log is not None:
            try:
                self.log.close()
                os.unlink(self.log.name)
            except Exception:
                pass
            self.log = None
        if self.profile is not None:
            shutil.rmtree(self.profile, ignore_errors=True)
            self.profile = None

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
            if self.ws is not None:
                self.ws.close()
        except Exception:
            pass
        self._stop()


#: The Chrome that first refused a DevTools WebSocket whose `Origin` it had not
#: been told to allow. Below this the suites cannot exercise that path at all,
#: so a local pass says less than it looks like it does.
ORIGIN_CHECK_FROM = 111


def chrome_version(binary=None):
    """`(text, major)` for the browser that would be used, or `(text, None)`."""
    try:
        text = subprocess.run(
            [binary or chrome_binary(), "--version"],
            capture_output=True, text=True, timeout=20,
        ).stdout.strip()
    except Exception as error:
        return (f"unknown ({error})", None)
    digits = "".join(c if c.isdigit() else " " for c in text).split()
    return (text, int(digits[0]) if digits else None)


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
