# omezarr-viewer-rs

**Experimental / WIP**

A viewer for what an imaging pipeline actually produces: the image, the label
volumes, the masks, and the tables of detected objects — over the same
coordinates, from disk or from object storage, in a browser or as a desktop app.

Built for the output of [`clearmap-ng`](https://github.com/henriksson-lab) and
[`blockflow`](https://github.com/henriksson-lab/blockflow) (cellpose, stardist
and YOLO detections among them), but it imposes no format of its own: it reads
what those pipelines already write. `PLAN.md` records the design and what is
built.

## What it shows

| Layer kind | Read from |
|---|---|
| **Image** | OME-Zarr multiscale stores; a flat `.npy` volume is an image with one level |
| **Labels** | integer id volumes (cellpose/stardist instances, atlas regions), drawn from the array's own dtype so ids above 2²⁴ stay distinct |
| **Objects** | one row per detection: YOLO CSV, `blockflow` table blobs, `.npy` point arrays (plain or structured) |

Layers stack in one world: a half-resolution label volume lands on the image it
describes, and a detector's coordinates can be scaled into place per layer.

## Features

- Multi-layer session: add and close layers at runtime, or open a whole run
  directory in one go
- Per-channel colour, contrast and opacity; stacked image layers composite
  additively, and a layer that declares no display window takes its contrast
  from the first tile it loads
- Label rendering with hashed or `image-label` colours, outline mode,
  isolate-selection, and click-to-identify
- Object overlays with screen-sized points, colour-by-column, per-column filters
  that apply without a round trip, z-slab fading, and click-to-inspect
- Orthogonal XZ/YZ panes with a crosshair linked across all three views
- Server-side maximum and mean projection through a z slab
- Atlas region names for label ids, and a per-region object tally
- Pyramid level chosen per layer from the zoom; tiled loading with coarse-level
  fallback; a bounded tile cache on the server
- Local filesystem, HTTP and S3 (named profiles) as peers

## Prerequisites

- Rust toolchain (stable)
- `trunk` for the WASM frontend: `cargo install trunk`
- WASM target: `rustup target add wasm32-unknown-unknown`
- For the desktop build on Linux: `webkit2gtk-4.1` and `libsoup-3.0` development
  packages

## Quick start

```bash
make demo                        # writes a synthetic image + labels + cells
make run PROJECT=/tmp/omezarr-demo
```

Then open http://127.0.0.1:8078.

Open a real run directory the same way — every zarr store, `.npy` volume and
object table under it becomes a layer:

```bash
make run PROJECT=/path/to/clearmap-run
```

Or a single store:

```bash
make serve STORE=/path/to/dataset.zarr
make serve STORE=s3://bucket/prefix/dataset.zarr
```

## Desktop

```bash
make desktop          # a runnable binary
./target/release/omezarr-viewer-desktop --project /path/to/run
```

The desktop app starts the same server in-process on a port the OS picks, serves
the frontend from inside the binary, and adds native file dialogs — which go
back through the same HTTP API the browser uses. `make desktop-bundle` produces
installers and needs `cargo install tauri-cli`.

## Converting `.npy` for object storage

A `.npy` has no chunk grid and no pyramid: fine on local disk, poor over S3.

```bash
cargo run --release --bin omezarr_convert -- mask.npy mask.zarr
```

Levels are mean-reduced for intensity data and nearest-sampled for labels,
because averaging two ids invents a third.

## Build commands

```bash
make build            # frontend + server, release
make run PROJECT=…    # open a run directory or project file
make serve STORE=…    # open one store (S3 flags apply)
make desktop          # desktop binary
make demo             # synthetic dataset
make test             # server tests + clippy on both crates
make dev-app          # watch-rebuild the frontend
make dev-server       # watch-rebuild the server
```

## Continuous integration

| Workflow | When | What |
|---|---|---|
| `ci.yml` | push to `main`, PRs, manual | server tests, `clippy -D warnings` on server and frontend, `cargo fmt --check`, and a desktop build — on Linux, macOS and Windows |
| `release.yml` | a `v*` tag, or manual | desktop bundles for all three platforms; a tag attaches them to a **draft** release, a manual run just keeps them as artifacts |

Both build the WASM frontend first: the desktop binary compiles it in, so a
missing `dist/` is a build failure rather than a run-time surprise.

`desktop/gen/schemas/` is not committed — Tauri regenerates it on every build
and writes a different set per platform.

## Architecture

A Cargo workspace of four crates:

| Crate | Path | What it is |
|-------|------|------------|
| `omezarr-viewer-common` | `src/` | the API contract: session, layers, schemas |
| `server` | `server/` | actix-web API, readers, project scanning, converter |
| `app` | `app/` | Yew + WebGL2 frontend |
| `omezarr-viewer-desktop` | `desktop/` | Tauri shell around both |

### API

| Endpoint | Answers |
|---|---|
| `GET /api/session` | every open layer, in draw order |
| `GET /api/tile` | a tile; `encoding=f32\|raw`, `zproj=max\|mean&depth=` |
| `GET /api/slice` | a whole plane across `axis=z\|y\|x` |
| `GET /api/value` | one voxel — the id under a click, with its region name |
| `GET /api/objects` | rows in a region, packed binary, with `X-Total` |
| `GET /api/objects/at` | the row nearest a point, exact values |
| `GET /api/regions` | objects per region, joined through a label volume |
| `GET`/`POST /api/project` | the session as a project file, and back |
| `POST`/`DELETE /api/layers` | add a layer (or scan a run folder), close one |
| `GET /api/stats` | cache occupancy |

`GET /api/info` and `POST /api/open` remain for the single-dataset S3 flow the
viewer started with.
