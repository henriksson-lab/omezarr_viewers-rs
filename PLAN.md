# Plan: a general viewer for `clearmap-ng` / `blockflow` output

Status: **phases 0-6 and 8-11 complete**, phase 7 deferred on purpose. Written
and landed 2026-08-28; annotation phases 8-11 added 2026-08-29/30.

This file is the tracked plan. Each phase has concrete file-level tasks and an
acceptance criterion; tick them off in place (`- [x]`) as they land.

---

## 1. Goal

Today this repo renders one OME-Zarr image: multi-channel, pyramidal, 2D slice,
pan/zoom (`app/src/webgl/renderer.rs`, `server/src/zarr_reader.rs`). ClearMap
ships a specialised viewer for its own output; we want a *general* one that can
show, over the same image, the things a run actually produces:

* instance/label volumes (cellpose, stardist),
* object tables (one row per detected cell: position + size + intensity + class),
* point sets (ClearMap cell detection, before and after registration),
* mask and density volumes (binarised vessels, skeletons, voxelized densities),
* eventually atlas regions and vessel graphs.

…from **disk first, S3 as a peer**, and shippable as a **Tauri desktop app** in
addition to the current server + browser deployment.

## 2. Decisions taken (2026-08-28)

| Question | Decision |
|---|---|
| 3D | **Slices + orthogonal views + MIP.** No ray-marched volume rendering. XY slice with Z scrubbing stays the primary view; XZ/YZ panes and a max-projection over a Z range are added. |
| Desktop | **Tauri embeds the existing actix server** in-process on `127.0.0.1:0`; the webview loads that URL. One code path for web and desktop; Tauri adds only native file dialogs and local-path handling. |
| Annotation formats | **The viewer reads what the pipelines already write.** No format is imposed on `clearmap-ng` or `blockflow`; the server grows readers. A sidecar *spec* may follow later, once real use has shown the shape (§9). **Settled in phase 8** (2026-08-29): what the viewer *writes* is an ngio/Fractal ROI table, because OME-Zarr specifies no vector annotation at all and that is the convention the ecosystem reads. |

## 3. What has to be read — an inventory of what the pipelines write today

Established by reading `/home/mahogny/github/claude/clearmap-ng` and
`/home/mahogny/github/claude/blockflow`.

| Producer | Artifact | On-disk form today | Viewer treats it as |
|---|---|---|---|
| `clearmap-ng` `Workspace` | `<root>/binary/ch0/filled.npy`, `skeleton/ch0/skeleton.npy`, … plus `<root>/manifest.json` (probe, chain, per-stage shape/dtype/nonzero, `unread`) | **`.npy`**, C-order, at the width the mask is held in (`src/workspace.rs`) | single-level **mask / volume layer** |
| `clearmap-ng` `voxelize` | density volume from a point set | `.npy` (same path) | **volume layer**, colormapped |
| `clearmap-ng` `cells` / `Value::Detected`, `Placed`, `Columns` | point set + per-object columns (`size`, intensity method column, …) | **nothing on disk yet** — `Workspace` refuses every non-`Mask` value by name, deliberately | **object layer** — needs a bridge, see §9 |
| `blockflow` `yolo::run` | detections | **CSV**: `id,x,y,confidence,class` (+ a `summary` CSV `image,spot_count`) | **object layer** |
| `blockflow` `model_segment` (cellpose / stardist) | one row per instance | `blockflow::table::Table` blob — schema `id, count, sum_0..2, sum_cN/min_cN/max_cN` per measured image; centroid derived via `model_segment::centroid` | **object layer** (+ a label volume where written) |
| `blockflow` `sidecar` | block-keyed opaque blobs, `(stream, phase, block)` keys | plain objects on fs or S3 | source of the table blobs above |
| `blockflow` `export` | order log, `schema: "clearmap-rs.block_ops.order_log"` v1 | JSON | out of scope (a scheduler view, not a data view) |
| `clearmap-ng` `graph` / `VesselGraph` | vasculature graph | **no canonical byte form** (stated in `src/workflow.rs`) | deferred, §8 phase 7 |

Two consequences worth stating up front:

* **`.npy` is a first-class input**, not a curiosity. It is the only volume form
  `clearmap-ng` writes. It has no chunking, so it is fine on local disk
  (mmap + strided read) and bad over S3 — hence the `npy → OME-Zarr` converter
  in phase 4.
* **The object table is where the interesting annotation lives**, and the two
  producers disagree on form (CSV vs. a `blockflow` table blob). The reader
  layer absorbs that; the viewer sees one row model.

## 4. The blockflow table blob, decoded

Documented here because the reader in phase 2 implements it and the format is
defined only in `blockflow/src/table.rs`:

* A blob is a `Vec<u64>` packed little-endian: `[MAGIC, VERSION, n_columns, n_rows]`,
  then per column `[type_code, name_len_bytes, name words (8 bytes/word, zero-padded)]`,
  then the rows.
* A row is `3 + n_columns` words, **row-major, position first** (`POSITION_WORDS = 3`).
* A `U64` column is stored as its own bits; an `F64` column as `to_bits()`.
* `blockflow::table::encoded_schema(bytes)` reads the schema without a table —
  which is what lets the viewer say "this stream has columns x,y,z,size" rather
  than guess.

Reading it needs ~60 lines and no dependency on `blockflow`. **Do not depend on
`blockflow` from this repo** — it drags burn/candle/CUDA. Re-implement the
decoder against the format above and pin it with a fixture blob checked into
`server/tests/data/`.

## 5. Target architecture

### 5.1 From "one store" to "a session of layers"

`AppState` currently holds `RwLock<Option<Arc<ZarrStore>>>` plus one optional S3
config (`server/src/api.rs`). That becomes:

```
Session
 ├ layers: Vec<Layer>            // ordered, bottom to top
 └ sources: SourceRegistry       // id -> resolved backend
```

```rust
enum LayerKind {
    Image  { dataset: DatasetInfo },              // multiscale, or a one-level .npy volume
    Labels { dataset: DatasetInfo,                // integer ids, nearest sampling
             colors: Option<Vec<LabelColor>>,     // OME-NGFF `image-label` colors if present
             properties: Option<String> },        // join to a table by id
    Objects{ schema: ObjectSchema, count: u64 },  // points + columns
}
```

*(As built: a flat `.npy` volume turned out to be an `Image` with exactly one
level rather than a kind of its own — there is no third kind of pixel layer,
only a source with no pyramid.)*

A `Layer` carries `id`, `name`, `kind`, a `SourceSpec`, and render state.
`GET /api/session` replaces `GET /api/info` (keep `/api/info` as an alias for the
single-image case so nothing breaks mid-refactor).

### 5.2 Source abstraction (disk and S3 as peers)

Keep `zarrs` doing the work; unify how a source is *named* and *resolved*:

```rust
enum SourceSpec {
    File(PathBuf),                       // file:///... or a bare path
    Http(Url),
    S3 { bucket: String, key: String, profile: S3ProfileId },
}
```

* `file:` keeps `zarrs::filesystem::FilesystemStore` (sync, no tokio hop).
* `http:`/`s3:` keep `AsyncOpendalStore`. S3 credentials move out of CLI flags
  into a named-profile map so several layers can live in different buckets
  (current flags become the `default` profile; `Makefile` keeps working).
* A `.npy` source is not a zarr store at all — it gets its own `NpyVolume`
  backend (mmap on `file:`, whole-object GET + cache on `s3:`).

### 5.3 Tiles: stop forcing f32

`read_tile` normalises every dtype to f32 (`bytes_to_f32`). That is right for
intensity and **wrong for labels**: ids above 2^24 do not survive an f32 round
trip, and f32 filtering blends neighbouring ids into ids that do not exist.

* `GET /api/tile` gains `&encoding=f32|raw`. `raw` returns the array's own dtype
  with `X-Dtype` set (`uint32`, `uint16`, …).
* The frontend uploads label tiles as **`R32UI` + `usampler2D`, `NEAREST`**, and
  intensity tiles as `R32F` as it does now.

### 5.4 Rendering pipeline (frontend)

`Renderer` today is one program drawing a quad per tile with up to 6 R32F
channel textures (`app/src/webgl/shaders.rs`). It becomes three programs sharing
the same camera uniforms (`u_pan`, `u_zoom`, `u_canvas_size`, tile placement):

1. **intensity** — today's shader, unchanged.
2. **labels** — `usampler2D`, id → colour by a hash (`id * 2654435761u`, golden
   ratio HSV), plus: `outline` mode (compare the 4 neighbours, draw only where
   the id differs), `selected id` highlight, and `hidden ids` via a small
   uniform array or a 1D LUT texture.
3. **objects** — instanced point sprites (`drawArraysInstanced`, per-instance
   `vec3 pos` + `float value` + `uint id`), circle/cross/box glyphs in the
   fragment shader, size in screen or data units, alpha faded by
   `|z - z_current| / slab_half`.

Compositing order is the layer order; each layer has opacity and a blend mode
(`add` for intensity as now, `over` for labels/objects).

### 5.5 Objects: server-side index, client-side filtering

A whole-brain run is millions of cells; neither the wire nor the GPU wants all
of them per frame.

* On layer open, the server reads the source once into a compact in-memory
  columnar form: `Vec<[f32;3]>` positions + one `Vec<f64>` (or `Vec<u64>`) per
  column, plus a **uniform grid index** over XY×Z-slab (the same shape
  `blockflow::table`'s gridded index uses; we are not reusing the code, only the
  idea).
* `GET /api/objects?layer=&x=&y=&w=&h=&z0=&z1=&max=` returns a **binary** packed
  buffer: header (count, column names/types, offsets) + positions + requested
  columns. `max` triggers deterministic decimation (stride over the canonical
  order, so the same query returns the same subset) and the response reports
  `X-Truncated: <total>` so the UI can say "showing 50k of 1.2M".
* Filtering (`size > 40 && confidence > 0.6`), colour-by-column, and
  class-visibility toggles run **client-side** over the loaded subset — instant
  feedback, no round trip. The predicate is also sent with the query so
  decimation happens after filtering, not before.
* `GET /api/objects/at?layer=&x=&y=&z=&r=` → the nearest row, full columns, for
  click-to-inspect.

### 5.6 Caching

Add a bounded LRU tile cache in the server keyed by
`(layer, level, t, c, z, y, x, h, w, encoding)`. It is not an optimisation for
the current XY path; it is what makes **ortho views and MIP affordable**, since
both re-read the same chunks repeatedly. Size configurable (`--cache-mb`,
default 512).

## 6. Ortho views and MIP (the 3D that we are doing)

* `GET /api/slice?layer=&axis=x|y|z&index=&level=&...` returns a plane through
  the volume. `zarrs` reads arbitrary rectangles already; the cost is that an
  XZ plane touches every chunk row, hence §5.6 and a default of "ortho panes
  render one pyramid level coarser than the main view".
* `GET /api/tile&zproj=max|mean&z0=&z1=` — projection computed **server-side**
  over a Z slab; far cheaper than shipping the slab. Slab thickness is a UI
  control shared with the object-overlay fade in §5.4.
* Layout: main XY pane + optional XZ (below) and YZ (right) panes, crosshair
  linked across all three, click in any pane moves the other two.

## 7. Desktop (Tauri)

New crate `desktop/` (tauri v2), not in the workspace's `default-members`:

* `main()` builds the same `Session`, starts actix on `127.0.0.1:0`, reads back
  the bound port, and points the webview at `http://127.0.0.1:<port>`.
* The frontend is embedded with `rust-embed` and served by actix from memory, so
  the bundled app has no `dist/` next to it. The web build keeps `actix_files`;
  one `#[cfg]` switch on the static handler.
* Tauri commands, deliberately few: `pick_folder`, `pick_file`, `recent_projects`.
  Each resolves to a path and then calls the *same* `/api/layers` HTTP endpoint —
  no second API surface.
* Bundle targets: AppImage + deb (Linux), dmg (macOS), msi (Windows).
* **Risk checked first, and cleared:** WebGL2 in WebKitGTK. The built app was
  run under `Xvfb` on WebKitGTK 2.50.4 and rendered the full stack, so the
  browser-on-Linux fallback is not needed.

## 8. Phases

### Phase 0 — groundwork (no user-visible change) — **done**
- [x] `src/lib.rs`: `SessionInfo`, `LayerInfo`, `LayerKind`, `LabelColor`,
      `ObjectSchema`, `ObjectColumn`; `DatasetInfo` stays the image-layer payload.
- [x] `server/src/session.rs`: `Session`, `Layer`, `LayerRole`, label
      auto-detection from `image-label` metadata.
- [x] `server/src/source.rs`: `SourceSpec` (file/http/s3 + named profiles),
      `SourceRegistry`, `S3Profile`.
- [x] `server/src/api.rs`: `GET /api/session`, `GET /api/stats`,
      `POST /api/layers`, `DELETE /api/layers/{id}`; `/api/tile` gains `layer=`
      and `encoding=`; `/api/info` kept as the default image layer.
- [x] `server/src/cache.rs`: byte-bounded LRU `TileCache`, `X-Cache` header.
- [x] `zarr_reader`: `TileRequest`/`TileEncoding`, per-level array handle cache,
      `int8`/`int32`/`uint64` added to the dtype table.
- [x] `server/src/lib.rs`: the server is a library with a thin `main.rs`, so
      tests drive the same code the binary runs.
- [x] `server/tests/`: synthetic OME-Zarr fixture generator + 7 integration
      tests (every dtype, raw encoding, wide ids, level decimation, session
      resolution, refusals) and 8 unit tests.
- [x] `--layer source[:role]` and `--cache-mb` on the CLI.
- **Accept:** met. `cargo test -p server` green (15 tests), `cargo clippy -p
  server --all-targets` clean, the frontend compiles unchanged against the
  extended common crate and still talks to `/api/info` + `/api/tile`.

### Phase 1 — label layers — **done**
- [x] `raw` tile encoding end to end (server → `X-Dtype` → JS `Uint32Array`),
      with `int8/16/32`, `uint8/16/32/64` widened client-side and a `uint64`
      overflow reported rather than wrapped.
- [x] Second GL program (`LABEL_FRAGMENT_SHADER`) reading an `R32UI` texture
      through a `usampler2D` with `texelFetch`, plus `upload_label_tile`.
- [x] Label UI: hash colouring, `image-label` colour table as a GPU LUT,
      outline mode, opacity, isolate-selection, click-to-select.
- [x] Click-to-select reads the id from the array through `GET /api/value`
      rather than holding label tiles in client memory.
- [x] The frontend became **multi-layer** throughout — `app/src/layers.rs`,
      per-layer level choice, a shared *world* coordinate space so a
      half-resolution label volume overlays the image it describes, per-layer
      tile keys and eviction, add/remove layer from the panel.
- [x] `server/src/synthetic.rs` + `make_demo` bin: a synthetic image + label
      pair with known blob positions, radii and ids, for developing and
      verifying without an acquisition on hand.
- **Accept:** met, verified in a real browser (headless Chrome + CDP) against
  the synthetic pair: labels overlay the image at half resolution and stay
  aligned, LUT ids and hashed ids both render, outline mode draws boundaries
  only, and clicking blob 4 reports `id 4 (uint32) at (299, 43)` — its
  generated centre.
- **Bug found and fixed on the way:** the canvas is `premultipliedAlpha`, and
  the label pass wrote un-premultiplied colour. Pixels whose colour exceeded
  their alpha are not valid premultiplied pixels, and the compositor dropped a
  whole channel — a green label over a green image composited to *no green*.
  Now every shader premultiplies and the blend is `ONE, ONE_MINUS_SRC_ALPHA`
  (`app/src/webgl/context.rs`).

### Phase 2 — object layers — **done**
- [x] `server/src/objects/mod.rs` — `ObjectStore`: columnar values, a uniform
      grid index over `(y, x)`, region + z-slab queries, deterministic
      decimation that reports the true total, and a per-layer `ObjectSpace`
      (scale/offset) so a detector that ran on a downsampled volume lands in the
      right place.
- [x] `objects/csv.rs` — position columns found by name, numeric columns kept
      with their type (`u64` stays exact), non-numeric dropped with a log line.
- [x] `objects/table.rs` — the `blockflow` blob decoder of §4, written against
      the format rather than the crate; a foreign magic, a bumped version and a
      truncated blob are each refused by name.
- [x] `objects/npy.rs` — plain `(N, k)` arrays and **structured** arrays
      (ClearMap's cell-table shape), including big-endian fields; Fortran order
      refused by name.
- [x] `GET /api/objects` (packed binary: header, positions, row ids, columns,
      with `X-Total`/`X-Returned`/`X-Truncated`) and `GET /api/objects/at`
      (exact values, `u64` still a `u64`).
- [x] Frontend: a `POINTS` program with screen-sized discs or rings, z-slab
      fade, colour-by-column through a ramp, per-column filters applied
      **client-side** over the loaded rows, click-to-inspect, and a counts line
      that says what was left out.
- [x] `make_demo` also writes `cells.csv`, `cells.npy` and `cells.blob` — the
      same blobs in all three formats, which is what makes reader disagreement
      visible rather than theoretical.
- **Accept:** met. `server/tests/objects.rs` checks all three readers land the
  same blob in the same place with the same size; in the browser, 144 detections
  render on their blobs, colour-by-size works, a size filter cuts 144 to 40
  without a round trip, and clicking a point reports
  `(1, 43, 128) · id 2 · intensity 0.5500 · size 9203` — blob 2's generated
  values.

### Phase 3 — ortho views and MIP — **done**
- [x] `GET /api/slice?axis=z|y|x&index=` reads a whole plane; `zproj=max|mean`
      and `depth=` on `/api/tile` project through z **server-side** (a 32-plane
      max is 32 tiles over the wire and one tile after the reduction).
- [x] A projection is always `float32` on the wire, whatever `encoding` asked
      for: the maximum of a set of label ids is not a label id, and a label
      layer is never projected.
- [x] A slab that runs past the top of the stack projects the planes that are
      there rather than failing — the slab is a viewing choice, the volume's end
      is not a mistake.
- [x] `app/src/ortho_pane.rs`: XZ (below) and YZ (right) panes, each owning its
      own GL context, reading **one plane per channel** at a level that fits the
      pane. Planes are read whole rather than tiled — a `(z, x)` plane crosses
      every chunk row, so tiling multiplies requests instead of dividing work —
      and stretched to fill, because what the pane answers is *where in z*.
- [x] A crosshair links all three views: clicking any pane moves the other two,
      and each layer is cut at the same world position in its own pixels.
- [x] `Z project` (slice / max / mean) and `Depth` controls in the panel.
- **Accept:** met, verified in the browser: clicking world (128, 128) in the
  main view puts the blob columns through z in the XZ pane and the blob rows in
  the YZ pane, the crosshair reads 25%/25% of a 512-pixel world, and an 8-plane
  max projection merges every z into the main view while the label layer stays
  on its own slice.

### Phase 4 — open a run, not a file — **done**
- [x] `server/src/npy_volume.rs`: `.npy` volumes, memory-mapped on disk and read
      whole from object storage, with tiles, planes, projections, edge padding
      and big-endian arrays. A flat `.npy` presents as a **one-level image
      layer** — there is no third kind of pixel layer, only a source with no
      pyramid.
- [x] `server/src/volume.rs`: one `Volume` handle over zarr and `.npy`, so
      every endpoint serves both without knowing which it has.
- [x] **A `.npy` is classified by its header, not its name.** `clearmap-ng`
      writes masks *and* cell tables as `.npy`; a structured dtype or an
      `(N, k<=4)` shape is a table, anything else is a volume — decided from a
      4 KB read, a range request over S3.
- [x] `server/src/project.rs`: scan a run directory (zarr stores, `.npy`
      volumes, object tables, three levels deep, `.zarr` treated as a store
      rather than walked), ordered images → volumes → objects. A layer that
      fails to open is skipped with a warning rather than sinking the run.
- [x] `--project <dir|file>`, `GET/POST /api/project`, a `run folder` role on
      `POST /api/layers`, and a **Save view** button that hands the browser the
      project JSON.
- [x] `omezarr-convert`: `.npy` → chunked, pyramidal OME-Zarr. Levels are
      mean-reduced for intensity and **nearest-sampled for labels**, because
      averaging two ids invents a third; the dtype picks the default.
- [x] Two rendering fixes the run view forced, both real: stacked image layers
      now **add** instead of replacing (a mask over a stain lights it up rather
      than hiding it), and a layer that declares no display window takes its
      contrast from the first tile that arrives — once — so a 0/1 mask is not a
      black rectangle at `0..255`.
- **Accept:** met. `omezarr-viewer --project <run>` opens a directory holding
  `image.zarr`, `binary/ch0/filled.npy`, `cells.csv` and `cells.npy` as four
  correctly-typed layers with no hand configuration, and the converted store
  returns tile-for-tile identical pixels to the `.npy` it came from.

### Phase 5 — Tauri desktop — **done**
- [x] **The WebGL2-in-WebKitGTK risk is settled, with a picture.** The desktop
      binary was run under `Xvfb` on WebKitGTK 2.50.4 and rendered the whole
      stack — image, converted mask store, `.npy` mask, and object points — so
      the Linux fallback the plan reserved is not needed.
- [x] `desktop/` crate: Tauri v2 starts `actix-web` in-process on
      `127.0.0.1:0`, reads back the port the OS gave it, and points the webview
      at that URL. The frontend is compiled in with `rust-embed`, so a bundled
      app has no `dist/` beside it.
- [x] Native `pick_folder` / `pick_file` dialogs. **They add no second API**: a
      picked path goes back through the same `POST /api/layers` the browser
      uses, and the buttons appear only when the Tauri IPC is present.
- [x] `--project` / `--store` on the desktop binary, `make desktop` for a
      runnable binary and `make desktop-bundle` for installers (the latter needs
      `cargo install tauri-cli`).
- **Accept:** met. `omezarr-viewer-desktop --project <run>` opened the five-layer
  run directory in its own window with no server started by hand and no
  browser.

### Phase 6 — atlas regions — **done, except the tree browser**
- [x] `server/src/ontology.rs`: a JSONL atlas table, parsed **permissively** —
      `id`, `st_level` and `parent_structure_id` are accepted as integers,
      floats (`8.0`) or strings, which is what the shipped
      `ABA_annotation_last.jsonl` actually contains and what the reference
      loader chokes on. An unreadable line is skipped and counted, not fatal.
- [x] `--ontology <file>`; `/api/value` reports the region `name` and `acronym`
      for the id under the cursor, and the panel shows it.
- [x] `GET /api/regions?labels=&objects=` joins an object layer to a label
      volume: **one label plane read per occupied z**, not one voxel per object,
      so a million cells cost a few hundred reads. Counts are named where the
      ontology knows the id, most populous first, ties broken by id.
- [x] A `Regions` panel appears when a label layer and an object layer are both
      open, with the tally as a table.
- [ ] **Not built:** a hierarchical region *tree* browser (collapse counts up
      the parent chain). `Ontology` already reads `parent`, so this is a UI
      piece rather than a data one.

### Phase 7 — deferred, deliberately
- **Vessel graphs.** Blocked upstream and not by this repo: `clearmap-ng`'s
  `VesselGraph` has no canonical byte form, and its own header says a
  serialisation decision belongs beside the stage that owns the value. When one
  exists, it is a reader beside `objects/` and a line program beside `webgl/`.
- **Ray-marched volume rendering.** §2's decision was slices, and phases 3-6
  did not turn up a case where the slab and the orthogonal panes were not
  enough. Left undone on purpose rather than left over.

### Phase 8 — annotation: drawing points and boxes — **done**

Added 2026-08-29. The first phase in which the viewer *writes* anything.

**What OME-Zarr actually offers** was established first and is written up in
`info_roi.md`: the 0.5 release and the 0.6rc0 editor's draft between them
specify exactly one annotation form, and it is pixel data (`labels` /
`image-label`). There is no vector geometry in either. The convention the
ecosystem settled on for regions is the **ngio/Fractal ROI table** — a
`tables/<name>` group beside `labels/`, whose rows are *axis-aligned bounding
boxes and nothing else*.

That shape is the whole design, and it is not a simplification chosen here:

- **A point is a box with zero extent.** The ROI table has no point type. One
  struct (`Annotation`) covers both, in memory, on the wire and on disk, so
  nothing downstream has to translate between two models that the file format
  collapses into one anyway.
- **No polygons.** They have no home in OME-Zarr. Rasterising into a `labels`
  image or writing GeoJSON as our own sidecar are the two options, and both wait
  for a real need — a file only this viewer reads is worse than no file.
- **World pixels in, `*_micrometer` out.** Annotations are held in the world the
  camera works in, because that is the space a click arrives in. The conversion
  happens once, at save, through the reference image's own
  `coordinateTransformations` scale — and the factor used is written into the
  table's attributes, so reading undoes exactly what writing did rather than
  guessing. Where a store declares no scale the factor is 1 and a "micrometre"
  is a pixel, stated as such. The 0.6rc0 draft endorses ROIs in array
  coordinates provided the choice is explicit, which is what that attribute is.

Landed:

- [x] `Annotation` and `pick_annotation` in the common crate — shared so the
      client, which holds every row, picks locally without a round trip and gets
      the same answer the server would.
- [x] `server/src/annotations/` — the in-memory set (ids never reused, backwards
      drags normalised) and `roi_table.rs`, the reader/writer.
- [x] Writing through `zarrs`: `GroupBuilder` (v3) or `GroupMetadataV2` for the
      group metadata, `WritableStorageTraits::set` for the CSV payload. The
      table follows the **host store's** zarr version, and the parent `tables`
      list is merged rather than replaced.
- [x] `LayerKind::Annotations`, carried inline in `/api/session` — a hand-drawn
      set is small and every edit needs the whole list.
- [x] `POST|GET|PUT|DELETE /api/annotations…`, plus `…/save` and
      `/api/annotations/tables`.
- [x] A line program (`GL_LINES`, per-vertex z range) for box outlines; the
      existing point program, in ring mode, for points.
- [x] A tool bar over the canvas — pan / point / box — with a live rubber band.
- [x] `--layer <store>.zarr/tables/<name>:annotations`, and `Project::scan`
      offering the tables a scanned store already holds.

Acceptance: a box drawn over an image, saved, and reopened in a fresh session
lands on the same pixels. Checked both ways — an integration test asserting
coordinates through the file, and a real browser asserting on rendered pixels.

**Left undone on purpose:** writing to `s3://` and `http(s)://`. `zarrs` will
write through opendal, but a viewer that can silently write into a bucket it was
given read access to is not a trade to make without a deliberate credential
path for it.

### Phase 9 — annotation, part two — **done**

Opened and landed 2026-08-29. Tiers 1, 2 and 4 of the follow-up list; tier 3
(painting a `labels/<name>` image) is deliberately **not** in this phase — it is
where a rasterised polygon would land, and the polygon question is being settled
separately.

#### 9a. Editing what was drawn

Phase 8 can place a mark and delete it. It cannot fix one, which means the only
repair for a misplaced box is delete-and-redraw, and a mis-click is unrecoverable.

- [x] **Geometry editing.** Hit-test the selected annotation's corners and edges
      in `viewer_canvas.rs`; drag one to resize, drag the interior to move,
      `PUT /api/annotations/{layer}/{id}` on release. The server side already
      exists and already normalises a backwards drag.
- [x] **Undo.** A client-side stack of inverse operations in `app.rs`. Every
      edit is exactly one API call, so the inverse of each is one too.
- [x] **Dirty flag.** `AnnotUiState` knows where it saves but not whether it has
      drifted from there. Set on every mutation, clear on save, guard `unload`.
- [x] **Z and T extent.** `extent[0]` is always 0 — a box spans exactly the
      slice it was drawn on — and an annotation carries no `t` at all, so one
      drawn on frame 3 is indistinguishable from one on frame 0. Both belong in
      the panel rather than in a third drag handle.

Acceptance: a box is drawn, moved, resized, undone back to where it started, and
the file on disk after saving says so.

#### 9b. Reading tables this viewer did not write

`roi_table::read` refuses `anndata_v1` and `parquet` **by name**. That is honest,
but ngio's *default* backend is AnnData, so a table written by anything but this
viewer cannot be opened.

- [x] **AnnData backend.** A zarr group: numeric columns in `X`, categorical and
      integer columns in `obs`, the index as `obs/_index`, with
      `encoding-type`/`encoding-version` attributes. Readable with `zarrs`;
      the wrinkle is string columns, which differ between zarr v2 (object dtype
      plus a codec) and v3 (`DataType::String`).
- [x] **Parquet backend.** A `.parquet` file inside the table group. Weigh the
      dependency: `parquet` without `arrow` is more code and far fewer crates.
- [x] **Remote writes.** `zarrs_opendal` implements the async writable traits,
      so `roi_table::write` becoming store-generic is mechanical. The real
      question is consent: credentials given to a viewer for *reading* must not
      silently become write access, so this is gated behind an explicit flag.

Acceptance: a table written by the AnnData and Parquet backends opens with the
same rows this viewer would have written; a save to `s3://` is refused without
the flag and works with it.

#### 9c. Making a set legible

- [x] **Colour by class.** A layer is one colour, so a multi-class set is
      unreadable. One draw call per class rather than a per-vertex colour
      attribute: the class count is what a person typed, and the shared point
      program must keep working for object layers.
- [x] **Label properties.** `LayerKind::Labels.properties` is declared as "id of
      an object layer whose rows describe these ids" and is hardcoded `None`
      (`session.rs:127`) — dead since it was written. The in-spec answer is
      `image-label.properties`: arbitrary per-id key/values, already half-parsed
      beside `colors`. Surface it when a label id is picked.
- [x] **Bulk operations.** Filter the list and the canvas by class; delete every
      annotation in one action, which undo makes safe to offer.
- [x] **A test for the project skip.** `/api/project` leaves out annotation
      layers that have never been saved, because their source is nowhere. That
      went in without a test.

Acceptance: a two-class set is drawn in two colours, filtered to one, and a
picked label id shows what the store says about it.

#### What the work actually turned up

Four things that were bugs before they were rules, all of them found by driving
a browser rather than by reading the code:

* **A session reload must not carry the old rows back over the new ones.**
  `adopt_session` kept a layer's whole UI state so a redraw would not reset the
  panel — which for an annotation layer meant the rows the client already had
  were written back over the rows the server had just sent. Every undo that goes
  through a reload silently did nothing. Now the *view* settings are kept and
  the rows come from the server (`AnnotUiState::keep_view_of`).
* **A point has no corners.** Its four are the same coordinate, so a grab
  resolved to a corner drag, which resized a zero-size box into a zero-size box —
  visibly nothing happening. A degenerate box is always grabbed by the body.
* **A newly drawn mark is already selected**, so its handles are live the moment
  the tool is switched back to pan. This is deliberate, and it is what made the
  first version of the browser test deselect the thing it was about to drag.
* **The `<select>` sentinels have to be printable.** "every class" and "the class
  whose name is empty" are different answers and both need a value that is not a
  class name; the first attempt used a NUL, which is untypable and unreadable in
  a DOM dump.

**Still open, deliberately:** reading an AnnData table over `s3://`. Its rows are
zarr arrays rather than one object, so the async path would be a second copy of
the decoder — worth writing when a remote AnnData table turns up, and not
before. The byte-payload backends (CSV, JSON, Parquet) work remotely today.

### Phase 10 — QuPath GeoJSON: every shape, drawn and edited — **done**

Landed 2026-08-29/30. `info_annotation_formats.md` is the analysis behind it,
written first and against QuPath's source and the OME-XML 2016-06 schema rather
than against documentation.

**The decision.** OME-Zarr specifies no vector annotation; OME-XML's ROI model
**cannot express a polygon with a hole** — its only composition operator is
`Union`, there is no difference — and "tissue minus lumen" is routine. So the
native form is **QuPath's GeoJSON dialect**: RFC 7946 underneath, already JSON,
already what the tool we mean to replace reads and writes, and a strict superset
of OME-XML's vector shapes apart from `Ellipse`, `Rectangle` and the raster
`Mask`. QuPath has already solved the first two (a foreign member and shape
inspection); the third belongs in `labels/`, which is in the spec and which this
viewer already reads.

The clincher is coordinates: QuPath uses full-resolution pixels, origin
top-left, y down — **exactly this viewer's world**. Nothing is converted, unlike
the ROI table's `*_micrometer` columns, which are unrecoverable unless the scale
used is recorded alongside.

Landed:

- [x] `Geometry` in the common crate — RFC 7946's set, serialising as GeoJSON
      verbatim, so the wire form *is* the file form. Holes are interior rings.
- [x] `Annotation` carrying QuPath's per-object properties: `objectType`,
      `name`, `color`, `classification` (derived classes joined with `": "`),
      `isLocked`, `measurements`, `metadata`, the hierarchy, and the UUID —
      preserved even where the viewer does nothing with them, so a round trip
      through here does not flatten somebody else's file.
- [x] `annotations/geojson.rs`: parse and write the dialect, including the two
      foreign members (`plane`, `isEllipse`) and nested `childObjects`.
- [x] An `annotations/` group beside `labels/` and `tables/`, whose attributes
      **declare the coordinate space** — GeoJSON's own convention is WGS84 and
      every bioimaging user of it silently means pixels; RFC 7946 removed `crs`,
      so there is nowhere inside the file to say so.
- [x] Bare `.geojson` files open and save too, which is what QuPath exports.
- [x] Eight tools: point, rectangle, ellipse, polygon (click-by-click, closing
      on the first vertex or a double-click), freehand region, polyline,
      freehand line — plus pan.
- [x] Editing **all** of them: body drag to move; bounding corners to resize a
      rectangle or ellipse; **vertex** handles for everything else, with
      shift-click to delete a vertex and shift-click on an edge to insert one.
- [x] Fills, as QuPath's "Fill annotations" does — ear-clipped with holes, on a
      per-layer toggle, off by default as QuPath defaults it.
- [x] An optional **z range** (`zExtent`/`tExtent`), our deviation from both
      QuPath and OME-XML, declared in the group attributes and written only when
      set — so an ordinary file is byte-for-byte what QuPath would have written.
- [x] The ROI table stays, for ngio interop, and a save to one now **reports how
      many shapes it flattened** to bounding boxes rather than doing it quietly.

Acceptance: every tool draws its own geometry type, each is editable, and the
file that comes out is read back identically. Checked in a real browser — 31
assertions on top of the 22 and 29 the earlier phases already had.

#### The object model, finished afterwards

Reviewing what a round trip actually preserved turned up two members that were
neither read nor written, and two fields that round-tripped but did nothing:

- [x] **`nucleusGeometry`.** A QuPath *cell* carries a second ROI for its
      nucleus. Dropping it lost half of every cell a segmentation produced. Now
      read, written, and drawn as an inner outline.
- [x] **`isMissing`**, the TMA-core flag. Same treatment, narrower case.
- [x] **`isLocked` is now enforced.** A file that says "do not edit this" and a
      viewer that edits it anyway is worse than one that cannot edit at all.
      A locked shape offers no handles; the lock is a checkbox on the selection.
- [x] **`name` is editable.** It is per-object *identity* ("Region 3"), which is
      not the same thing as the per-category `classification` and is why QuPath
      has both. It was read and preserved but only ever visible in a tooltip.
- [x] **`objectType` is settable**, per layer for new shapes and per object for
      the selected one. It is QuPath's *processing role*, not the semantic kind
      — the kind is the classification — but it decides how QuPath treats the
      objects on the way back: a detection stays out of its annotation list and
      keeps it fast with thousands of them.

#### The hierarchy, shown and maintained

- [x] **Nesting is spatial, as QuPath's is.** A shape drawn inside another
      becomes its child, by the smallest-covering-shape rule
      (`containing_parent`), without anybody saying so.
- [x] **The list reads as a tree**: depth-first, indented by depth, with a count
      of what is inside each parent. `in_tree_order` is iterative and tolerates
      a cycle, because a hand-edited file can claim one and the panel still has
      to render.
- [x] **Deleting a parent lifts its children** rather than taking them with it:
      removing a region must not silently remove every cell inside it.
- [x] **Re-nest** rebuilds the hierarchy from where the shapes now are (editing
      moves things in and out of each other), and **Detach** lifts one out. Both
      are offered rather than applied automatically — silently re-nesting under
      the pointer mid-drag would be a surprise.

#### What the work turned up

* **A ring's seam has to be asked about before it is moved.** Once one end of a
  closed ring has shifted, the ring no longer *looks* closed, and the check that
  would have kept the two ends together fails.
* **A scale of exactly 1 must be a no-op, not a computation.** `ox + (p - ox)` is
  not always `p` in floating point, so a resize that ended where it began would
  otherwise move every vertex by an ULP.
* **Shift is the vertex modifier, so a shift-click that misses does nothing** —
  panning instead sends the picture sliding away from somebody who was aiming at
  a handle and was three pixels out.
* **A point has no corners**, so it is grabbed by its body; and a freehand trace
  is simplified as it is stored, because a mouse-move fires far more often than
  a hand moves a pixel.

#### Disk and S3, finished

The first cut of the GeoJSON path was filesystem-only, which left the *native*
annotation format unable to reach the stores every other layer kind could. Now
both:

- [x] `save_async` / `load_async` / `list_async` through `AsyncOpendalStore`,
      sharing `remote`, `attributes_at_async` and `remote_is_v3` with the ROI
      table rather than growing a second copy.
- [x] Targets keep their scheme (`split_uri_target`, `make_uri_target`), so an
      `s3://` target is routed as one rather than being treated as a directory
      called `s3:` and failing obscurely three layers down.
- [x] Remote sets are listed beside remote tables, so opening one is a click.
- [x] Remote writes stay behind `--allow-remote-writes`.
- [x] A bare `.geojson` *file* target remotely is refused with advice: a bucket
      has no files, only objects inside a store, so the set has to be named.

Verified against a live store served over HTTP: a polygon **with its hole**
opened through opendal, the sets listed, the write gate refused without the flag
and — with it — ran the async path and was refused by HTTP itself, which is the
transport saying no rather than the viewer. AnnData stays local-only: its rows
are zarr arrays, so a remote read would be a second decoder, where every other
form is one object through a different store.

**Still open, deliberately:** OME-XML export (a mechanical downgrade, per
`info_annotation_formats.md` §6), a raster brush — that is a `labels` image,
which is where a rasterised polygon would land — and reading an AnnData table
over `s3://`.

### Phase 11 — tables that are not geometry — **done**

Landed 2026-08-30, after asking what a viewer could actually *do* with an
AnnData table. The answer turned on a distinction worth stating: ngio's
**feature table** is per-*object* measurements — one row per label id — and its
**condition table** is the image-level one. Neither carries coordinates.

- [x] **`obsm["spatial"]` → points.** The scverse convention: an `(n_obs, 2|3)`
      array, the default key for scanpy and squidpy, and where a spatial-omics
      table keeps its positions since it has no `*_micrometer` columns at all.
      Taken *unscaled*, because an `obsm` array is already in the pixels of the
      image it was measured from, where a micrometre column must be divided by
      the factor it was written with.
- [x] **`region` + `instance_key` → a label join.** A feature table paints the
      label image it describes, colouring every id by one of its columns through
      the same ramp the object layer uses. This is the whole of how a table with
      no coordinates is *seen*, and it reuses the label LUT machinery that was
      already there.
- [x] **AnnData over `s3://`.** A second fetcher, sharing the assembly
      (`assemble_anndata`) — zarrs' sync and async storage traits are different
      types and no generic unifies them, so only the fetching is written twice.
- [x] **A table view.** `LayerKind::Table`, shown as a table, paged
      (`/api/tables/{layer}/rows`) because a feature table has a row per
      segmented object. Columns keep the *file's* order, not a sorted map's.
- [x] A declared `roi_table` with no coordinates is still refused by name; a
      feature table with none is the spec working as intended.

Acceptance: a feature table opens beside the label image it describes, paints it
by `area`, and reads back as a table. Checked in a browser — 11 assertions,
including that a low and a high value get different colours and that switching
the colouring off restores what the label image itself declared.

## 9. Upstream gap, stated once

`clearmap-ng` currently has **no way to write a point set to disk** —
`Workspace` writes `Value::Mask` as `.npy` and refuses every other kind by name,
on purpose (a serialisation decision belongs beside the stage that owns the
value). So phase 2's readers cover `blockflow`'s producers today, and cover
`clearmap-ng`'s cells only once something writes them.

The cheapest bridge, and the one to ask for: `cells` / `Placed` gains a
`Workspace` write as either a **`blockflow` table blob** (already the shape the
values have, already decodable by phase 2) or a `(N, 3+k)` `.npy` with a
sibling JSON naming the columns. Either is a small change there and zero change
here. Nothing in this plan depends on which.

## 10. Continuous integration

Not in the original plan, added once there was something to keep working.

* **`ci.yml`** — Linux, macOS and Windows, in parallel and `fail-fast: false`:
  build the frontend, `cargo test -p server`, clippy over the server and the
  wasm frontend with `-D warnings`, and build the desktop crate. Windows earns
  its place: the path-separator assumptions in `SourceSpec::short_name` and in
  project layer names are the kind of bug no other runner sees.
* **`release.yml`** — the desktop bundles. A `v*` tag attaches them to a
  **draft** release, so publishing stays a human decision; a manual run uploads
  them as artifacts, so "does it still bundle" can be asked without cutting a
  release.
* **Not committed:** `desktop/gen/schemas/`. Tauri regenerates it every build
  and writes a *different* set per platform, so committing it would have every
  CI runner dirty the tree.

## 11. Risks

| Risk | Mitigation |
|---|---|
| XZ/YZ planes are chunk-hostile (touch every chunk row) | **as built**: a pane reads one whole plane per channel at a level that fits it (<= 2048 px), cached; not tiled |
| Millions of objects | server-side grid index + deterministic decimation + honest "N of M" reporting; never silently truncate |
| f32 label corruption | fixed structurally in phase 1 by the `raw` encoding, not by clamping |
| `.npy` over S3 | supported but slow by construction — a remote `.npy` is read whole, once. `omezarr-convert` is the answer; the UI does **not** yet warn when one is opened from S3 |
| WebKitGTK WebGL2 | **settled**: checked first, renders correctly under Xvfb on WebKitGTK 2.50.4 |
| Scope creep into a 3D viewer | §2 decision held: slices, ortho panes and a projection slab covered every case phases 3-6 met, and volume rendering stayed undone |
