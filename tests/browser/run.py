#!/usr/bin/env python3
"""Run the browser suites.

    python3 tests/browser/run.py [suite ...]

Each suite gets a **fresh server and a fresh store**: the session holds
annotations in memory, so a suite that inherited another's would be asserting
against state it did not create. That is not caution — it is the bug that made
two suites pass individually and fail together.
"""

import argparse
import os
import shutil
import subprocess
import sys
import tempfile
import traceback

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(HERE, "suites"))

from cdp import ORIGIN_CHECK_FROM, Browser, chrome_version  # noqa: E402
from harness import Checks, Server, Viewer, binary, demo_store  # noqa: E402

SUITES = ["drawing", "editing", "classes", "hierarchy", "formats", "tables"]


def feature_table(store):
    """A label image and a feature table describing it.

    Written here rather than by the server, because the point of the suite is to
    read a table this viewer did not write — a file as ngio would leave one.
    """
    group = os.path.join(store, "tables", "features")
    os.makedirs(group, exist_ok=True)
    tables = os.path.join(store, "tables")
    _write(os.path.join(tables, ".zgroup"), '{"zarr_format":2}')
    _write(os.path.join(tables, ".zattrs"), '{"tables": ["features"]}')
    _write(os.path.join(group, ".zgroup"), '{"zarr_format":2}')
    _write(
        os.path.join(group, ".zattrs"),
        '{"type":"feature_table","table_version":"1","backend":"csv",'
        '"region":{"path":"../labels/nuclei"},"instance_key":"label",'
        '"index_key":"label","index_type":"int"}',
    )
    rows = ["label,area,intensity_mean,cell_type"]
    for i in range(1, 37):
        rows.append(
            f"{i},{100 + i * 13.5:.1f},{20 + (i * 7) % 60:.2f},"
            f"{'tumour' if i % 3 else 'stroma'}"
        )
    _write(os.path.join(group, "table.csv"), "\n".join(rows) + "\n")


def _write(path, text):
    with open(path, "w") as handle:
        handle.write(text)


def run_suite(module, shots, keep):
    """One suite, against its own server and store."""
    directory = tempfile.mkdtemp(prefix=f"omezarr-browser-{module.NAME}-")
    checks = Checks(module.NAME)
    server = viewer = None
    try:
        store = demo_store(directory)
        layers = []
        if getattr(module, "NEEDS_FEATURE_TABLE", False):
            feature_table(store)
            # The demo's labels, renamed to what the table's `region` names, so
            # the join has something to match.
            nuclei = os.path.join(directory, "nuclei.zarr")
            shutil.copytree(os.path.join(directory, "labels.zarr"), nuclei)
            layers = [f"{nuclei}:labels", f"{store}/tables/features:annotations"]

        server = Server(store, layers=layers)
        viewer = Viewer(server, Browser(), shots)
        viewer.store = store
        print(f"\n{module.NAME}:")
        module.run(viewer, checks)
    except Exception:
        checks.failed.append(f"{module.NAME} raised")
        print(f"  FAIL  {module.NAME} raised:")
        traceback.print_exc()
    finally:
        if viewer is not None:
            viewer.close()
        if server is not None:
            server.close()
        if keep:
            print(f"  (store kept at {directory})")
        else:
            shutil.rmtree(directory, ignore_errors=True)
    return checks


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("suites", nargs="*", default=None, help="which suites to run")
    parser.add_argument(
        "--shots",
        default=os.environ.get("BROWSER_SHOTS", tempfile.mkdtemp(prefix="omezarr-shots-")),
        help="where to write screenshots",
    )
    parser.add_argument("--keep", action="store_true", help="keep each suite's store")
    args = parser.parse_args()

    # Fail early and clearly rather than deep inside a suite.
    binary("server")
    binary("make_demo")
    dist = os.path.join(os.path.dirname(os.path.dirname(HERE)), "dist", "index.html")
    if not os.path.exists(dist):
        sys.exit("dist/index.html is missing — run `make build` (the server serves it)")

    os.makedirs(args.shots, exist_ok=True)

    # One browser before any suite. A Chrome that will not start is an
    # environment problem, not a test failure, and discovering it six times over
    # buries the one line that says why in six identical stack traces.
    try:
        Browser().close()
    except RuntimeError as error:
        sys.exit(f"the browser suites need a working Chrome:\n{error}")

    # Which browser proved it. Printed because the answer has already differed
    # between a developer's machine and CI in a way that mattered: Chrome only
    # began refusing a DevTools WebSocket over its `Origin` in 111, so a pass on
    # an older build cannot say anything about that path.
    version, major = chrome_version()
    print(f"browser: {version}")
    if major is not None and major < ORIGIN_CHECK_FROM:
        print(
            f"  note: this is older than Chrome {ORIGIN_CHECK_FROM}. CI runs a "
            "newer one, so a pass here is not conclusive for anything the "
            "DevTools protocol changed since."
        )

    wanted = args.suites or SUITES
    results = []
    for name in wanted:
        if name not in SUITES:
            sys.exit(f"no suite named {name!r}; have {', '.join(SUITES)}")
        results.append(run_suite(__import__(name), args.shots, args.keep))

    passed = sum(r.passed for r in results)
    failed = [f"{r.name}: {what}" for r in results for what in r.failed]
    print(f"\n{passed} passed, {len(failed)} failed")
    for line in failed:
        print(f"  {line}")
    print(f"screenshots in {args.shots}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
