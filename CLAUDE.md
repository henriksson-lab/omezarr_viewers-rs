# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build Commands

The project uses a Makefile. The WASM frontend requires `trunk` (`cargo install trunk`) and the `wasm32-unknown-unknown` target (`rustup target add wasm32-unknown-unknown`). The desktop crate additionally needs `webkit2gtk-4.1` and `libsoup-3.0` development packages on Linux.

```bash
make demo               # write a synthetic image + labels + object tables to /tmp/omezarr-demo
make run PROJECT=<dir>  # open a run directory (or project .json) at http://127.0.0.1:8078
make serve STORE=<uri>  # open one store; S3 flags apply (BUCKET/ENDPOINT/…)
make build              # frontend (trunk) then server, release
make desktop            # the Tauri binary (frontend is compiled into it)
make test               # cargo test -p server, plus clippy on both crates
make dev-app            # watch-rebuild frontend only
make dev-server         # watch-rebuild server only
```

Server-only build: `cargo build` (workspace default-members is just `server`).
Frontend-only build: `cd app && trunk build`.

Tests live in `server/` — unit tests beside the code and integration tests in `server/tests/`, against synthetic fixtures whose every value is known by arithmetic (`server/src/synthetic.rs`). There are no frontend tests; the frontend is verified by driving a real browser (see "Verifying the frontend").

## Architecture

Cargo workspace with four crates:

- **Root crate** (`omezarr-viewer-common`): serde types for the API contract — `SessionInfo`, `LayerInfo`, `LayerKind`, `ObjectSchema`, plus the OME-NGFF metadata structs.
- **`server/`**: actix-web server. Readers, the session model, project scanning, the `.npy` → OME-Zarr converter, the annotation writer, and the synthetic data generator.
- **`app/`**: Yew 0.21 + WebGL2 frontend, compiled by Trunk into `dist/`.
- **`desktop/`**: Tauri v2 shell that runs the server in-process and embeds the frontend.

`PLAN.md` is the tracked design document: what was decided, what is built, and
what is deliberately left undone. `QUALITY.md` tracks the engineering debt
against it — untested paths, oversized files, panic sites — with an acceptance
criterion per task. `info_roi.md` is the research behind the
annotation work: what OME-Zarr actually specifies for annotations (one thing,
and it is pixels), what the ecosystem conventions are, and what `zarrs` can
write.

### The session model

A session is an ordered list of **layers**, bottom to top. Each layer names a `SourceSpec` (`file://`, `http(s)://`, `s3://[profile@]bucket/key`) and a kind:

- **Image** — a multiscale zarr store, or a flat `.npy` volume (one level). Both are `Volume` (`server/src/volume.rs`), an enum over `ZarrStore` and `NpyVolume`, so every endpoint serves both.
- **Labels** — an integer id volume; auto-detected from OME-NGFF `image-label` metadata or forced with `role=labels`.
- **Objects** — a row per detection (`server/src/objects/`), read from CSV, `blockflow` table blobs, or `.npy` point arrays.
- **Annotations** — geometry drawn in the viewer (`server/src/annotations/`), in QuPath's model: points, lines, polygons with holes, rectangles and ellipses. The only layer kind that is *written*.

Layer kind is decided by the *source*, not by its name: a `.npy` is classified from its header (`npy_volume::classify`) because `clearmap-ng` writes masks and cell tables under the same extension.

### Coordinates

Everything is drawn in one **world**: the reference (first image) layer's full-resolution x/y. Each layer maps into it by a per-level scale, and object layers carry an `ObjectSpace` (scale/offset) because a detector that ran on a downsampled volume writes coordinates nothing in the file explains.

### Annotations follow QuPath, because OME-Zarr has nothing

OME-Zarr specifies exactly one annotation form and it is pixel data — `labels` /
`image-label`. There is no vector geometry in the 0.5 release or the 0.6rc0
draft. OME-XML's ROI model has shapes but **cannot express a polygon with a
hole**: its only composition operator is `Union`, there is no difference.

So the native form is **QuPath's GeoJSON dialect** — RFC 7946 geometry, plus
QuPath's two foreign members (`plane`, `isEllipse`) and its `properties`
(`objectType`, `name`, `color`, `classification`, `isLocked`, `measurements`,
`metadata`, nested `childObjects`). `info_annotation_formats.md` has the schemas,
the compatibility matrix and the sources; `info_roi.md` covers the OME-Zarr side.

Three consequences worth knowing before touching this code:

* **The coordinate systems already agree.** QuPath uses full-resolution pixels,
  origin top-left, y down; so does this viewer's world. GeoJSON is written
  unconverted. Only the ROI table needs a scale, and it records the one it used.
* **Ellipses need a flag; rectangles do not.** A polygonised ellipse cannot be
  recovered from its vertices, so `isEllipse` is load-bearing. A rectangle is
  recognised by inspecting the ring — which is what QuPath does too.
* **Everything the reader parses is preserved**, including what nothing here
  displays — UUID, measurements, metadata. A round trip must not flatten
  somebody else's work. Two members were missing and are now handled:
  `nucleusGeometry` (a cell's second ROI, drawn as an inner outline) and
  `isMissing`. What is *dropped* on purpose: colour alpha, and the non-finite
  measurements QuPath writes as the strings `"NaN"`/`"Infinity"`.
* **`objectType` is a processing role, not a semantic kind.** The kind is the
  `classification`. Both are settable — the class per shape, the type per layer
  for new shapes and per shape for the selected one — because a detection is
  treated as bulk data by QuPath and an annotation is not.
* **`name` and `classification` are different things.** A name identifies one
  object; a class says what kind it is, carries the colour, and drives the
  filter. QuPath has both, so does this.
* **`isLocked` is enforced.** A locked shape offers no drag handles.
* **The hierarchy is spatial.** A shape drawn inside another becomes its child
  by the smallest-covering-shape rule, as in QuPath. Deleting a parent *lifts*
  its children rather than deleting them.

Two on-disk forms, for two audiences:

- `<store>.zarr/annotations/<name>/annotations.geojson` — the native form. The
  group attributes **declare the coordinate space**, because GeoJSON's own
  convention is WGS84 and every bioimaging user of it silently means pixels;
  RFC 7946 removed `crs`, so there is nowhere in the file to say so.
- `<store>.zarr/tables/<name>` — the ngio/Fractal ROI table, **axis-aligned
  boxes only**. Kept for that ecosystem; a save reports how many shapes it
  flattened rather than doing it quietly.

Which one a save or an open uses is decided by the **shape of the path**, not a
flag: `annotations/<name>` or a `.geojson` file is GeoJSON, `tables/<name>` is a
table. Both written with `zarrs` — `GroupBuilder` (v3) or `GroupMetadataV2` for
the group metadata, `WritableStorageTraits::set` for the payload — following the
**host store's** zarr version and merging the parent index rather than replacing
it, because a store may hold sets this viewer knows nothing about.

### Disk and S3

Every layer kind reads from `file://`, `http(s)://` and `s3://` alike, through
`SourceRegistry` — a local path uses `zarrs::filesystem` synchronously, anything
else an `AsyncOpendalStore`, which is why most of this code has two paths rather
than one. What differs by source:

| | local | `s3://`, `http(s)://` |
|---|---|---|
| Images, labels, objects | yes | yes |
| GeoJSON annotations, read and write | yes | yes |
| ROI table, CSV/JSON/Parquet | yes | yes |
| ROI table, all four backends | yes | yes |
| A bare `.geojson` file | yes | no — name a set instead |

**Writes to a remote store need `--allow-remote-writes`.** Credentials given to a
viewer so it can *read* a bucket must not silently become write access to it;
that is the operator's call, not the viewer's.

AnnData needed a second reader for exactly that reason — its rows are zarr
*arrays* rather than one object, and zarrs' sync and async storage traits are
different types that no generic unifies. The *assembly* is shared
(`assemble_anndata`); only the fetching is written twice.

### Tables that are not geometry

Two ngio table types carry no coordinates and so are layers of their own
(`LayerKind::Table`), not annotations:

* a **feature table** is per-object measurements keyed to a label image — one
  row per label id. Where a row *is*, is wherever its id sits in the label image
  its `region` names. So the way to see one is to **paint that label image**,
  colouring each id by a column; the table beside it is for reading the numbers
  the picture cannot show.
* a **condition table** is experiment-level metadata, with no position even in
  principle.

A table that *declares* itself a `roi_table` and has no coordinates is a broken
file and is refused by name; a feature table having none is the spec working.

Positions, when a table has them, come from one of two conventions: ngio's
`*_micrometer` columns (divided by the scale they were written with), or
scverse's **`obsm["spatial"]`** — an `(n_obs, 2|3)` array, the default key for
scanpy and squidpy, already in the image's own pixels and so taken unscaled.

See PLAN.md phases 8-11.

### Things that are the way they are for a reason

Each of these was a bug or a wrong picture before it was a rule:

- **Premultiplied alpha.** The canvas is `premultipliedAlpha`; every shader multiplies its colour by its own alpha and the blend is `ONE, ONE_MINUS_SRC_ALPHA`. A colour channel above its alpha is not a valid premultiplied pixel and the compositor drops it — a green label over a green image composited to no green at all.
- **Labels never travel as f32.** An id above 2²⁴ does not survive the round trip, and a filtered id is an id that does not exist. Label tiles are `encoding=raw` into an `R32UI` texture read with `texelFetch`.
- **A projection is always f32**, whatever `encoding` asked for, and label layers are never projected: the maximum of a set of ids is not an id.
- **Stacked image layers add.** Only the bottom-most visible image layer replaces; the rest composite additively, so a mask over a stain lights it up instead of hiding it.
- **Auto-contrast happens once.** A layer with no OMERO window takes its contrast from the first tile that arrives and never again, so an adjustment is not undone by the next tile.
- **Decimation is deterministic and reported.** `/api/objects` strides over the canonical order and returns `X-Total`, so the client can say "showing N of M" rather than showing a subset as if it were everything.
- **Annotation buffers upload from both sides of the startup race.** The session can arrive before the canvas exists or after it, and whichever happens second does the upload (`App::upload_annotations`). Doing it twice costs a buffer rebuild; doing it neither time is what a reopened ROI table looked like — an empty screen.
- **A drawing tool takes the drag on `mousedown`, not later.** The camera and the rubber band cannot both own a drag, and `mousemove` has no way to tell them apart after the fact; a box drawn in a frame that slid out from under it lands somewhere else entirely.
- **A near-zero drag in a shape tool is dropped.** It was a misfire, not a zero-size region, and storing it litters the layer with rows nothing can see. A *point* is the exception: a click is exactly what it is.
- **A ring's seam is asked about before it moves.** Once one end of a closed ring has shifted, the ring no longer looks closed, and the check that keeps its two ends together fails — so `move_vertex` decides it is a ring first, then moves.
- **A scale of exactly 1 is a no-op, not a computation.** `ox + (p - ox)` is not always `p`, so a resize drag that ends where it began would otherwise shift every vertex by an ULP.
- **Shift is the vertex modifier, so a shift-click that misses does nothing.** Panning instead sends the picture sliding away from somebody who was aiming at a handle and was three pixels out.
- **A freehand trace is simplified as it is stored.** A mouse-move fires far more often than a hand moves a pixel, and the vertex editor cannot offer handles nobody can tell apart.
- **A session reload takes an annotation layer's rows from the server, not from the client.** `adopt_session` keeps a layer's UI state so a redraw does not reset the panel — but an annotation layer's *rows* arrive with the session, so carrying the old state wholesale wrote the stale rows back over the fresh ones and made every undo-by-reload silently do nothing. `AnnotUiState::keep_view_of` keeps the colours and filters and nothing else.
- **A point is grabbed by its body, never by a corner.** All four of its corners are the same coordinate, so a corner drag resized a zero-size box into a zero-size box — which looks exactly like a broken drag.
- **A store knows where its own image is, and every array path goes through it.** For a `bioformats2raw` container that is `/<series>`, and resolving it for the metadata but not for the reads gives a store whose pyramid is described perfectly and whose pixels cannot be fetched at all. Found by `make test-network`, which opens real public stores (OME's catalogue, the Open SciVis set) — that suite is `#[ignore]`d because it reaches the internet, so anything it finds is pinned by a local test as well.
- **A container's series arrive with only the first visible.** They are alternative scenes, not things to overlay: stacked image layers composite additively, so two visible series of one container sum two unrelated pictures — measured at 1.75x the brightness for two identical ones. They do not share a coordinate space either, the world being the first image's. `LayerInfo::visible` carries this, defaulting to true so an older project file still means "show it".
- **A store root is not always an image.** `bioformats2raw` — and `img2omezarr`, which writes this for everything it produces — puts no pixels at the root at all: just `bioformats2raw.layout`, an `OME/` group listing the series, and one numbered subgroup per image. The reader resolves that before parsing multiscales. Both the layout key and the series list come in the same two shapes `multiscales` does (root for 0.4, under `ome` for 0.5), so each lookup checks both. A container expands into **one layer per series** in `Session::add`, not inside `ZarrStore` — which is what makes a layer's spec the *series*, so annotations are written beside the image they describe rather than at a root with no pixels. Several series are named `store.zarr[0]`, `[1]`; one keeps the store's plain name. A direct `ZarrStore` open cannot expand, so it still refuses a multi-series container by name: one scene of a three-scene slide is a wrong picture that looks like a right one.
- **A fixture this repo writes cannot test the reader.** Every image fixture comes from `synthetic.rs`, so the metadata structs had only ever been shown the shape our own writer produces; the first real files they met (`tests/data/ngff/`, three producers) turned up a multiscale-level `coordinateTransformations` being silently dropped, which gives `world_scale()` the wrong voxel size and so writes the ROI table's `*_micrometer` columns at the wrong physical scale. Fixtures for a *reader* have to come from outside, and `SOURCES.md` records where each one did.
- **A series name read from a file is a claim too.** The container expansion terminates only because `SourceSpec::child` descends: `Session::add` re-detects a container in whatever it opens, so `""`, `"."` or `".."` make it re-detect the same store forever. That was a measured stack overflow, which *aborts* rather than unwinding — a malformed store taking the server down. A series must be exactly one `Normal` path component, and a bad index is refused rather than filtered, because dropping an entry shows a subset of a slide as though it were all of it.
- **A count read from a file is a claim, not a size.** Every length in a `blockflow` table blob is a `u64` the file chooses, and the blob is the only thing that can contradict it — so it is checked before anything allocates or multiplies with it. `Vec::with_capacity` on a stranger's `u64` is a panic where an error was available — the client gets a dropped connection and the log says `capacity overflow` instead of which file was bad. Panics here unwind and the server stays up, so this is about diagnosis and resilience rather than safety. Found by fuzzing, on its first run.
- **Class zero is not a class.** A rasteriser zeroes its volume before anything draws into it, so zero has to mean *no shape covers this voxel*; a class rendering as zero produces exactly the volume a shape that never arrived produces. Fragment class ids are therefore one-based, and `blockflow`'s rasterise op refuses class zero by name rather than drawing it. The viewer and the op were each internally consistent about this and disagreed with each other, which is why the pipeline is now pinned by one **golden blob committed to both repositories** — `fragments.bftable`. It is copied rather than shared: tying two repositories to a directory layout neither controls is the worse coupling.
- **A stroke width is a size in the image, never on the screen.** The band is world-space triangles, not a wide line: `lineWidth` above 1 is not portable in WebGL2 and is screen-space where it works at all, and a scribble whose apparent width changed with the zoom would be showing an assertion nobody made. The centreline is stroked underneath so a sub-pixel band cannot vanish entirely.
- **A scribble is grabbed by its band, not by its centreline.** The grab tolerance is a fixed number of *screen* pixels expressed in world ones, so it shrinks as the view zooms in; on a wide band only its middle answered a click, and a shift-click that plainly landed on the shape did nothing. `grab_reach` takes the larger of the two — the hand's tolerance stays the floor, so a narrow band is never *harder* to hit than a bare line, and the body test uses it too because the bounds are the vertices and the band stands half its width outside them.
- **The draft is drawn with the width it will be stored with.** Otherwise the band appears on mouse-up and the shape you are about to get is not the shape you can see. The canvas cannot work the width out — there is no geometry until the drag ends — so the app resolves it; the tool→open-path shortcut that needs is pinned to `geometry_of` by a test over every `Tool`, because two answers to "is this an open path?" is exactly how a shape gets drawn with a band it is not stored with.
- **A dense region is hatched, not filled.** It claims something an ordinary region does not — inside it, a pixel nothing covers is *background* rather than unexamined — so it must not look like an ordinary region that happens to have Fill on.
- **A plane cache keys on the transpose as well as the slice.** The texture is uploaded after the transpose, so a plane fetched for the other orientation is a wrong picture rather than a slow one. Pane size is deliberately *not* in the key: it chooses the level, and the level is.
- **Undo is a stack of inverses, not of snapshots.** Every annotation edit is one API call, so its undo is one too; snapshots would grow with the size of the set rather than with the number of edits.

### CI

`.github/workflows/ci.yml` runs tests, clippy (`-D warnings`) and a desktop
build on Linux, macOS and Windows; `release.yml` builds the bundles on a `v*`
tag (draft release) or on demand (artifacts). Both build the frontend first,
because the desktop crate embeds it.

Windows is not decoration: path-separator assumptions (`SourceSpec::short_name`,
project layer names) are the kind of thing only it catches.

`make test` runs the same set CI does, and **touches each crate root first**:
clippy caches by crate fingerprint, so an unchanged crate is not re-linted and a
run can pass on stale results. It also lints every crate by name — linting
`server` says nothing about `omezarr-viewer-common`, which is only its
dependency.

### Verifying the frontend

There are no wasm tests. The way frontend work has been checked here is a real browser driven over CDP: headless Chrome with `--use-angle=swiftshader --enable-unsafe-swiftshader` (WebGL2 fails without it), navigate, act, screenshot, and assert on *pixels* — a blob's known colour, a crosshair's percentage, an inspector line. The desktop build was checked the same way under `Xvfb` on WebKitGTK.
