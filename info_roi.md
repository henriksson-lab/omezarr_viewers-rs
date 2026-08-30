# What OME-Zarr supports for annotations — research notes

Written 2026-08-29, against the **0.5** spec (the current release) and the RFC
index as of that date. This is background for the annotation work; it is not a
design document. `PLAN.md` remains the design document.

Nothing here implies a Python dependency. Where a tool is named (napari, vizarr,
MoBIE, ngio, SpatialData) it is named as *a reader of the bytes we would write*
— evidence that a layout is the conventional one — never as something this repo
would link against or shell out to. The implementation is Rust, via `zarrs`.

---

## 1. The spec defines exactly one annotation form

The complete list of metadata keys OME-Zarr 0.5 defines:

`axes`, `bioformats2raw.layout`, `coordinateTransformations`, `multiscales`,
`omero`, `labels`, `plate`, `well`.

**There is no vector / ROI / shape / point / table specification.** 0.6 is not
released (`https://ngff.openmicroscopy.org/0.6/` is a 404).

### The editor's draft — checked, and it changes nothing here

`https://ngff.openmicroscopy.org/specifications/dev/index.html` is **0.6rc0**.
Its section list is:

`coordinateSystems` · `bioformats2raw.layout` (transitional) ·
`coordinateTransformations` · `multiscales` · `omero` (transitional) · `labels`
· `plate` · `well` · **`scene`**

So the draft adds two things — a first-class `coordinateSystems` object with a
much richer transformation vocabulary (RFC-5 landed here: affine, sequence,
displacement/coordinate fields, a transformation *graph*), and `scene`, a group
above several images describing how they sit relative to one another. **It still
has no ROI, shape, polygon, point or table section.** `labels` remains the only
annotation form in the draft as in the release.

One line in the draft is directly relevant to the decision below, though —
under `coordinateTransformations`:

> Some applications might prefer to define points, regions-of-interest or
> transformation parameters in array coordinates (also referred to as pixel
> coordinates) rather than physical units. Because transformations are agnostic
> to whether they operate on array or physical coordinates, indicating that
> choice explicitly will be important for interoperability.

That is the spec editors saying, in as many words, that ROIs in pixel units are
a legitimate thing to write **provided you say so explicitly** — which is
exactly what §5's `world_pixel_size_zyx` attribute does.

Polygons remain a later problem; there is nowhere in OME-Zarr to put one.

The RFC pipeline at the time of writing:

| RFC | Title | Status |
|---|---|---|
| 0 | Original consensus model | — |
| 1 | RFC Process | Adopted |
| 2 | Zarr V3 Support | Adopted |
| 3 | Remove axis restrictions | Under review |
| 4 | Axis Anatomical Orientation | Under review |
| 5 | Coordinate systems and transformations | Under review |
| 6 | Flattening the multiscales array | Superseded |
| 7 | Channel provenance | TBD |
| 8 | Collections | Under review |
| 9 | Zipped OME-Zarr | Under review |
| 10 | NGFF Governance | Under review |

None of them is annotations. RFC-5 is the one people cite as the prerequisite
for a future ROI spec, because an ROI is meaningless without a stated coordinate
system — which is the same problem `ObjectSpace` solves for us today.

So: one in-spec form, plus two de-facto conventions.

---

## 2. `labels` / `image-label` — pixel annotation (in-spec)

A `labels` group sits **inside** the image group, a sibling of the resolution
levels. Its attributes MUST carry the list of label images:

```json
{"attributes": {"ome": {"version": "0.5", "labels": ["my_annotation"]}}}
```

(In 0.4 — zarr v2 — the same JSON lives in `.zattrs` without the `ome` nesting.)

Each label image:

* MUST implement `multiscales`, with **the same number of levels** as the source
  image;
* MUST have an integer dtype — one of
  `uint8, int8, uint16, int16, uint32, int32, uint64, int64`;
* SHOULD carry an `image-label` object alongside `multiscales`.

```json5
"image-label": {
  "version": "0.5",
  // SHOULD. One entry per unique id. `rgba` is MAY, 4 ints 0-255.
  "colors": [{"label-value": 1, "rgba": [0, 128, 0, 128]}],
  // MAY. Arbitrary keys per id, and rows need not share keys.
  "properties": [{"label-value": 1, "class": "cell", "area (pixels)": 1650}],
  // MAY. Relative path back to the source image; default is "../../".
  "source": {"image": "../../"}
}
```

Intermediate groups between `labels` and the images are allowed but MUST NOT
carry metadata. Image names under `labels` are arbitrary.

`properties` is the escape hatch worth remembering: arbitrary per-id key/values,
ragged. A class name, a note, an author all fit there without inventing a
format.

---

## 3. `tables/` — ngio / Fractal ROI tables (de-facto, *not* in the spec)

The convention the ecosystem actually uses for regions and per-object rows,
deliberately modelled on the shape of the `labels` group. It originated in
Fractal and is now specified and maintained by `ngio`.

```
image.zarr
├── 0 … N          multiscale levels
├── labels/        label images   (in-spec)
└── tables/        tables         (convention)
    ├── table_1/
    └── table_2/
```

Group attributes on `tables/`, so tables are discoverable without listing:

```json
{"tables": ["table_1", "table_2"]}
```

Group attributes on each table:

```json5
{
  "type": "roi_table",       // roi_table | masking_roi_table | feature_table
                             // | condition_table | (anything else -> generic)
  "table_version": "1",
  "backend": "anndata_v1",   // anndata_v1 | parquet | csv | json
  "index_key": "FieldIndex", // column that identifies a row
  "index_type": "str"        // "str" | "int"
}
```

### ROI table v1 columns

Required — **axis-aligned bounding boxes, in micrometres**:

* `x_micrometer`, `y_micrometer`, `z_micrometer` — top-left (min) corner
* `len_x_micrometer`, `len_y_micrometer`, `len_z_micrometer` — extent

Optional, carried through unchanged by readers that do not know them:

* `t_second`, `len_t_second`
* `x_micrometer_original`, `y_micrometer_original`, `z_micrometer_original`
* `translation_x`, `translation_y`, `translation_z` (multiplexing registration)
* `plate_name`, `row`, `column`, `path_in_well`, `path_in_plate`,
  `acquisition_id`, `acquisition_name`
* `FieldIndex`, `label` — the two names treated as index keys

Extra columns are preserved but uninterpreted (readers log a warning per
unrecognised column). **There is no point type and no polygon type**: a point is
a box with zero extent, and a polygon has no representation at all.

### Backends

Four, all of which are "a payload inside the table's zarr group":

* **AnnData** (the default, `anndata_v1`) — a zarr group; numeric columns become
  `X`, categorical/integer columns become `obs`, the index column is cast to
  string and stored as the `obs` index. Also writes `encoding-type: "anndata"`,
  `encoding-version: "0.1.0"`.
* **Parquet** — a `.parquet` file inside the group.
* **CSV** — a plain `.csv` file inside the group.
* **JSON** — a `.json` file inside the group.

The last three are literally one extra key in the store next to `zarr.json`.

---

## 4. The two routes for real polygons

Neither is NGFF, and we are not doing either yet.

* **`bioformats2raw.layout`** — a *transitional but in-spec* key that points at
  an `OME/METADATA.ome.xml` sidecar. OME-XML carries the full ROI model: Point,
  Line, Rectangle, Ellipse, Polygon, Polyline, Mask, Label, each with T/Z/C
  indices, grouped into ROIs as a union of shapes. This is where OMERO's ROIs go
  on export. XML, not arrays.
* **SpatialData** — stores shapes as **GeoParquet** (WKB or geoarrow encoding)
  inside the zarr store, migrating away from an older geopandas ragged-array
  representation. Entirely outside NGFF.

---

## 5. Can `zarrs` write all this?

Yes — and the hard part already exists in this repo.

`zarrs` 0.18.3 is fully write-capable:

* `GroupBuilder::new().attributes(map).build(store, path)?` then
  `group.store_metadata()?`
* `ArrayBuilder::new(shape, dtype, chunk_shape, fill_value).build(store, path)?`
  then `array.store_metadata()?` and `store_chunk_elements` /
  `store_array_subset*` (sync and async variants both exist —
  `array_sync_writable.rs`)
* `WritableStorageTraits::set(&StoreKey, Bytes)`
  (`zarrs_storage-0.4.5/src/storage_sync.rs:161`) writes an **arbitrary key**
  inside a zarr group — which is exactly what a CSV/JSON/Parquet table backend
  needs.

`server/src/convert.rs:150-216` is already this pattern, for the `.npy` →
OME-Zarr converter.

Consequences per form:

| Form | Cost in this repo |
|---|---|
| `labels` + `image-label` | **No new dependency.** A JSON attribute map plus an integer pyramid — `convert.rs` already writes one, with `Reduce::Nearest`, for exactly this reason. Painting is `store_chunk_elements` on the edited chunks. |
| `tables/` with **CSV or JSON** backend | **No new dependency.** Group attrs via `GroupBuilder`, payload via `set()`. `csv` is already a dependency. |
| `tables/` with **AnnData** backend | Hand-rolling the AnnData encoding (`X`, `obs/_index`, `encoding-type`). String columns need zarr-v3 `DataType::String`, which zarrs 0.18 does have — but AnnData-in-zarr-v2 uses object dtype plus a codec, and that is where it gets ugly. |
| `tables/` with **Parquet** backend | Pulls in `arrow` + `parquet`. |

### A defect this turned up

`server/src/convert.rs:150` builds a zarr **v3** group (`GroupBuilder` defaults
to `GroupMetadataV3`) but places `multiscales` at the top of `attributes`
instead of under `attributes.ome` with `"version": "0.5"`. That is 0.4 metadata
in a 0.5 container. Our own reader accepts both shapes
(`server/src/zarr_reader.rs:690-698`), so nothing breaks today — but anything we
newly *write* for another tool to read has to pick a lane.

---

## 6. Where this lands

Two annotation kinds, each matching a layer kind the viewer already renders:

1. **Painted annotation → a `labels/<name>` label image**, with
   `image-label.colors` for the palette and `image-label.properties` for the
   class or note per id. Fully in-spec. The read path already exists — label
   layers, `R32UI` textures.
2. **Points and boxes → an ngio-style `tables/<name>` ROI table**, CSV backend.
   The object layer already has a row model and a CSV reader
   (`server/src/objects/csv.rs`).

Polygons have no home in OME-Zarr. Either rasterise into (1), or store GeoJSON
as our own sidecar and say plainly that it is ours — which is the call `PLAN.md`
§2 deferred: "a sidecar *spec* may follow later, once real use has shown the
shape".

**Decided: (2) first.** Points and boxes, an ROI table per annotation layer.

---

## Sources

* NGFF 0.5 specification — <https://ngff.openmicroscopy.org/0.5/index.html>
* NGFF 0.6rc0, the editor's draft — <https://ngff.openmicroscopy.org/specifications/dev/index.html>
* NGFF RFC index — <https://ngff.openmicroscopy.org/rfc/index.html>
* ngio table specifications — <https://biovisioncenter.github.io/ngio/stable/table_specs/overview/>
* Fractal table specs — <https://fractal-analytics-platform.github.io/fractal-tasks-core/tables/>
* image.sc, "Polygon and other ROI annotations in ome-zarr" — <https://forum.image.sc/t/polygon-and-other-roi-annotations-in-ome-zarr/47990>
* SpatialData design document — <https://github.com/scverse/spatialdata/blob/main/docs/design_doc.md>
