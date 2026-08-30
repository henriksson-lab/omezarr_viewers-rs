# A format for manual annotation: QuPath, OME-XML, and what to write

Written 2026-08-29, against QuPath's source at `qupath/qupath@main` and the
OME-XML **2016-06** schema, both read directly rather than from documentation.
Companion to `info_roi.md`, which covered what OME-Zarr specifies (one thing,
and it is pixels) and what the ngio/Fractal `tables/` convention offers.

**The question this answers:** what should the viewer write so that it can do
the annotation work people currently do in QuPath, in JSON rather than XML,
without inventing a format nobody else can read.

**Status:** implemented, PLAN.md phase 10 (2026-08-30). This document is the
analysis that decided it; the decisions in §7 are the ones that were taken.

**The answer, up front:** adopt **QuPath's GeoJSON dialect**. It is already
JSON, already a published standard underneath (RFC 7946), already the format
QuPath itself calls its preferred export, and it is a strict superset of
OME-XML's *vector* ROI model apart from two primitives and the raster `Mask` —
both of which have clean answers. §7 has the recommendation and §8 the cost.

---

## 1. Why the ROI table does not answer this

Phase 8/9 write an ngio ROI table: axis-aligned boxes with `*_micrometer`
columns. That convention came from high-content screening, where the rows are
**algorithm output** — one per segmented object, with a feature matrix attached.
Its natural payload is AnnData, because for that community the imaging output
*is* the single-cell input.

Manual annotation is a different job with different requirements:

| | ROI table | Manual annotation |
|---|---|---|
| Rows | thousands, machine-made | tens to hundreds, hand-drawn |
| Geometry | axis-aligned box only | polygons, holes, freehand |
| Per-row payload | a feature vector | a class, a name, a note |
| Structure | flat | hierarchical (region ⊃ cells) |
| Edited after writing | no | constantly |

A polygon with a hole — "tissue, excluding the lumen" — is routine in pathology
and has **no representation at all** in an ROI table. That is the gap.

---

## 2. QuPath's object model

Everything in QuPath is a `PathObject`, in a hierarchy under a root:

- `PathAnnotationObject` — what a person draws
- `PathDetectionObject` — what an algorithm finds
- `PathCellObject` — a detection with a *second* ROI for the nucleus
- `PathTileObject`, `TMACoreObject`, `PathRootObject`

Each carries: an `id` (UUID), a `ROI`, an optional `name`, an optional `color`,
an optional `PathClass` (the classification), a measurement list, a string→string
metadata map, a locked flag, and **child objects**.

Its ROI types are `Points` (a multipoint), `Line`, `Polyline`, `Polygon`,
`Rectangle`, `Ellipse`, `Area`, and `Geometry` (an arbitrary JTS geometry —
which is what carries holes and multi-part shapes). Every ROI sits on an
`ImagePlane`: a `(c, z, t)` triple, where `c = -1` means "all channels".

---

## 3. QuPath's GeoJSON dialect, exactly

From `QuPathTypeAdapters.java` and `ROITypeAdapters.java`. This is what
`File → Object data… → Export as GeoJSON` writes, and what QuPath reads back.

```json5
{
  "type": "FeatureCollection",
  "features": [{
    "type": "Feature",
    "id": "e3b0c442-98fc-1c14-9afb-4c8996fb9242",   // UUID, since v0.4
    "geometry": {
      "type": "Polygon",                            // RFC 7946 geometry
      "coordinates": [
        [[1000, 2000], [1100, 2000], [1100, 2100], [1000, 2000]],   // exterior
        [[1030, 2030], [1070, 2030], [1070, 2070], [1030, 2030]]    // a HOLE
      ],
      "plane":     {"c": -1, "z": 3, "t": 0},       // foreign member, omitted if default
      "isEllipse": true                             // foreign member, see below
    },
    "nucleusGeometry": { /* … */ },                 // cell objects only
    "properties": {
      "objectType":     "annotation",               // annotation|detection|cell|tile|tmaCore|root
      "name":           "Region 1",
      "color":          [255, 0, 0],                // RGB, 0-255
      "classification": {"name": "Tumor", "color": [200, 0, 0]},
      "isLocked":       true,                       // written only when true
      "isMissing":      false,                      // TMA cores only
      "measurements":   {"Area µm^2": 12345.6},     // object of name -> number
      "metadata":       {"note": "checked"},        // string -> string
      "childObjects":   [ /* nested Features */ ]   // only in hierarchy mode
    }
  }]
}
```

Details that matter and are not obvious:

- **Geometry types written:** `Point`, `MultiPoint`, `LineString`,
  `MultiLineString`, `Polygon`, `MultiPolygon`, `GeometryCollection`. Plain
  RFC 7946, no extensions.
- **Coordinates are pixels**, origin `(0, 0)` at the top-left of the
  **full-resolution** image, y increasing downward. This is a deliberate
  deviation from RFC 7946, which says coordinates are WGS84 lon/lat. Everybody
  doing bioimage GeoJSON deviates the same way; RFC 7946 removed the `crs`
  member, so there is nowhere to say so and the convention is simply understood.
- **Ellipses become polygons**, and QuPath marks them with the foreign member
  `isEllipse: true`; on read it takes the polygon's bounding box and rebuilds
  the ellipse. A polygonised ellipse is not recoverable from its vertices, so
  the flag is load-bearing.
- **Rectangles need no flag.** On read, QuPath checks `polygon.isRectangle()`
  and routes those through shape reconstruction, so rectangle-ness survives by
  inspection. Ellipse-ness cannot, which is why only it gets a member.
- **A polygon with holes, or a multi-part shape, becomes a `GeometryROI`** on
  read — QuPath keeps it, it is just not one of the simple editable types.
- **`measurements` is an object** of name → number since v0.4; an array of
  `{"name":…, "value":…}` is still accepted on read (the pre-0.4 form).
  Non-finite values are written as the *strings* `"NaN"`, `"Infinity"`,
  `"-Infinity"`.
- **`classification`** is `{"name": …}` for a simple class, or
  `{"names": [...]}` for a derived one — QuPath classes nest, `Tumor: Positive`
  splitting into `["Tumor", "Positive"]`. Colour is `[r,g,b]` or `[r,g,b,a]`.

---

## 4. OME-XML's ROI model, exactly

From `ome.xsd` 2016-06.

```
ROI                     ID (required), Name (optional)
  ├── Union             (required, 1..n shapes — this is the only composition operator)
  │     └── Shape…
  ├── AnnotationRef*    (link to a MapAnnotation / TagAnnotation / XMLAnnotation)
  └── Description       (optional element)
```

Every shape shares these attributes:

`ID`, `TheZ`, `TheT`, `TheC`, `Locked`, `Text`, `FillColor`, `FillRule`
(`EvenOdd`|`NonZero`), `StrokeColor`, `StrokeWidth`, `StrokeWidthUnit`,
`StrokeDashArray`, `FontFamily`, `FontSize`, `FontSizeUnit`, `FontStyle`, plus a
`Transform` child (a 6-value affine) and `AnnotationRef` children.

The eight shapes and their own attributes:

| Shape | Attributes |
|---|---|
| `Rectangle` | `X`, `Y`, `Width`, `Height` |
| `Ellipse` | `X`, `Y`, `RadiusX`, `RadiusY` |
| `Point` | `X`, `Y` |
| `Line` | `X1`, `Y1`, `X2`, `Y2`, `MarkerStart`, `MarkerEnd` |
| `Polyline` | `Points` (`"x,y x,y …"`), `MarkerStart`, `MarkerEnd` |
| `Polygon` | `Points` (`"x,y x,y …"`, implicitly closed) |
| `Mask` | `X`, `Y`, `Width`, `Height` + a `BinData` child (inline base64 bitmap) |
| `Label` | `X`, `Y` (the baseline start of `Text`) |

`TheZ`/`TheT`/`TheC` absent means *all* z / t / channels. Each shape is 2D on a
single plane; the schema's own answer for a 3D ROI is "a Union of shapes across
planes".

---

## 5. Compatibility matrix

| Capability | QuPath | OME-XML | GeoJSON (QuPath dialect) |
|---|---|---|---|
| Point / multipoint | ✅ | ✅ `Point` (one each) | ✅ `Point`, `MultiPoint` |
| Line, polyline | ✅ | ✅ `Line`, `Polyline` | ✅ `LineString` |
| Rectangle | ✅ | ✅ exact | ➖ polygon, recovered by inspection |
| **Ellipse** | ✅ | ✅ exact | ➖ polygon + `isEllipse` flag |
| Polygon | ✅ | ✅ | ✅ |
| **Polygon with holes** | ✅ | ❌ **no representation** | ✅ interior rings |
| Multi-part shape | ✅ | ➖ `Union` (union, not difference) | ✅ `MultiPolygon` |
| **Raster mask** | ❌ (uses label images) | ✅ `Mask` + `BinData` | ❌ |
| Text label as a shape | ➖ name/`Text` | ✅ `Label` | ➖ a property |
| z / t / c | ✅ `plane` | ✅ `TheZ`/`TheT`/`TheC` | ✅ `plane` foreign member |
| "all z" / "all channels" | ✅ `c: -1` | ✅ attribute absent | ✅ same as QuPath |
| Classification | ✅ nested `PathClass` | ➖ `Text`, or `AnnotationRef` | ✅ `classification` |
| Measurements | ✅ | ➖ `AnnotationRef` → `MapAnnotation` | ✅ `measurements` |
| Free metadata | ✅ | ➖ via annotations | ✅ `metadata` |
| **Hierarchy** | ✅ `childObjects` | ❌ flat | ✅ `childObjects` |
| Stable id | ✅ UUID | ✅ `ID` | ✅ `id` |
| Locked | ✅ | ✅ `Locked` | ✅ `isLocked` |
| Colour | ✅ RGB | ✅ Fill/Stroke RGBA | ✅ RGB(A) |
| Stroke style, fonts | ❌ | ✅ | ❌ |
| Per-shape affine | ❌ (baked into coords) | ✅ `Transform` | ❌ |

## 6. The five real asymmetries

Everything else is a naming difference. These are the ones that cost data:

1. **OME-XML cannot express a hole.** Its only composition operator is `Union`;
   there is no difference. "Tissue minus lumen" is a routine pathology
   annotation and it simply does not fit. This is the single strongest argument
   against OME-XML as the native form — it is not a serialisation quibble, it is
   a shape the model cannot hold.
2. **GeoJSON cannot express an ellipse or a rectangle exactly** — but QuPath has
   already solved both, and the solutions are two lines: a foreign member for
   the ellipse, shape inspection for the rectangle.
3. **OME-XML has `Mask`; GeoJSON has nothing.** In an OME-Zarr store this is not
   a loss — a raster annotation belongs in `labels/`, which *is* in the spec and
   which this viewer already reads. A `Mask` should become a label image, not a
   feature.
4. **OME-XML has no hierarchy.** QuPath's model is hierarchical to its core
   (an annotation contains detections contain cells), so a flat export loses the
   containment relation. GeoJSON carries it in `childObjects`.
5. **OME-XML has a per-shape `Transform`; QuPath and GeoJSON do not.** Losing it
   is harmless as long as coordinates are written already transformed, which is
   what QuPath does.

Note the asymmetry is not symmetric: **GeoJSON → OME-XML is a mechanical,
documented downgrade** (polygonise the ellipse, drop holes or approximate them,
flatten the hierarchy, move measurements to a `MapAnnotation`). **OME-XML →
GeoJSON is lossless** except `Mask`, which should have gone to `labels/` anyway.
So writing GeoJSON does not close the OME-XML door; writing OME-XML would close
the hole door permanently.

---

## 7. Recommendation

### Write QuPath-dialect GeoJSON

The reasons, in order of weight:

1. **It is what QuPath reads and writes.** If the goal is to replace QuPath,
   then during the years that takes, work has to move both ways. Speaking its
   preferred interchange format exactly means a user can start an annotation
   here and finish it there, or the reverse, with no converter.
2. **The coordinate systems already agree.** QuPath uses full-resolution pixels
   with the origin at the top-left and y increasing downward. This viewer's
   **world** is the reference layer's full-resolution x/y, same origin, same
   direction. There is *no conversion* — unlike the `*_micrometer` dance the ROI
   table forced, which needed a scale factor recorded in the attributes to be
   reversible at all.
3. **It is a real standard underneath.** RFC 7946, with parsers in every
   language, rather than a format we invented.
4. **The hard cases are already decided.** Ellipse, plane, classification,
   hierarchy — QuPath hit all four and its answers are in its source. Adopting
   them costs nothing and inheriting them avoids four arguments.
5. **It subsumes what we already write.** An ROI table box is a `Polygon` with
   four corners; the ROI table stays for ngio interop, but nothing is lost.

### Where it goes in the store

Nothing in OME-Zarr says. Mirror the `labels/` and `tables/` pattern, which is
what every other convention here has done:

```text
image.zarr
├── 0 … N                  multiscale levels
├── labels/                label images                (in the spec)
├── tables/                ngio ROI/feature tables     (convention, §info_roi)
└── annotations/           zattrs: {"annotations": ["my_regions"]}
    └── my_regions/        zattrs: see below
        └── annotations.geojson
```

The table group's attributes have to say what the coordinates *mean*, because
the file itself cannot:

```json5
{
  "type": "geojson_annotations",
  "version": "1",
  "dialect": "qupath",           // the foreign members above are honoured
  "coordinate_space": {
    "axes": ["x", "y"],          // z and t live in each feature's `plane`
    "units": "pixel",            // full-resolution pixels of the image below
    "level": 0,
    "origin": "top-left",
    "y_axis": "down"
  },
  "omezarr_viewer": { "written_by": "…" }
}
```

Stating the coordinate space explicitly is the lesson from the ROI table: the
`*_micrometer` columns were unrecoverable without recording the factor used, and
GeoJSON's pixel convention is exactly the same kind of unwritten assumption.
Writing it down costs six keys.

### Keep the ROI table too

They are not competitors. `tables/` is machine output and HCS interop;
`annotations/` is hand-drawn work and QuPath interop. A box is expressible in
both, so the "save" button can offer either.

---

## 8. What it costs here

The current `Annotation` is a box: `position: [f64;3]`, `extent: [f64;3]`. The
change is to make geometry an enum, and it ripples:

| Piece | Work |
|---|---|
| `Annotation` → a geometry enum (point / multipoint / polyline / polygon-with-holes / multipolygon) | Moderate; the wire type, `contains`, `pick_annotation`, `max_corner` all follow the geometry |
| GeoJSON read/write | Small — `serde_json` is already a dependency and the schema is plain. **No new crate needed**, though `geojson` exists if wanted |
| Renderer: polygon outlines | **Already done** — the line program takes arbitrary segments; a polygon ring is the same buffer a box outline is |
| Renderer: filled polygons | Needs triangulation (ear clipping, ~150 lines, or the `earcut` crate). *Optional* — outlines may be enough, and outlines are what keeps overlapping regions readable |
| Drawing tools: polygon (click-to-add-vertex), freehand (drag), and the existing box/point | The real UI work: vertex editing, closing a ring, undo per vertex |
| Hierarchy, classification, measurements | Mostly panel work; the data model is a parent id and a map |
| OME-XML export | Optional, and a mechanical downgrade per §6 |

The renderer is the pleasant surprise: the `GL_LINES` program written for box
outlines draws an arbitrary polygon ring unchanged.

---

## 9. Decisions taken

Recorded here as they were settled, so the reasoning is not lost:

1. **Fills, or outlines only?** — **both**, as QuPath has: an always-drawn
   outline plus a translucent fill on a per-layer toggle, default off, which is
   QuPath's own default. Ear-clipped with hole support.
2. **How much of QuPath's object model to adopt** — **all of it on read**.
   `objectType`, the UUID, measurements, metadata and the hierarchy are carried
   through even where nothing renders them, so a round trip is non-destructive.
3. **Does a shape get a z range or one plane?** — **an optional range**, our
   deviation from both QuPath and OME-XML, written as `zExtent`/`tExtent` only
   when set and declared in the group attributes. A file of ordinary one-plane
   shapes is byte-for-byte what QuPath would have written.
4. **Whether to read `.qpdata`** — **no**. It is Java serialisation, not an
   interchange format, and QuPath's own advice is to export GeoJSON.

## 9b. Still open

Tracked in `README.md` under "Known gaps"; the reasoning is here and in
`PLAN.md`.

1. **Fills, or outlines only?** Outlines need no triangulation and keep stacked
   regions legible; QuPath fills with translucency. Suggest outlines first, with
   fill as a per-layer toggle later.
2. **How much of QuPath's object model to adopt.** `objectType` and
   `childObjects` are cheap to carry through even before the viewer does
   anything with them — and carrying them through is what makes a round trip
   through this viewer non-destructive. Suggest: preserve everything on read,
   even the parts not rendered.
3. **Does a polygon get a z *range*, or one plane?** QuPath and OME-XML both say
   one plane per shape, with a 3D region being several shapes. Phase 9 gave
   boxes a z extent, which is *more* expressive than either. Suggest keeping our
   extent internally and writing one feature per plane on export — or accept the
   deviation and record it in the group attributes.
4. **Whether to read `.qpdata`.** No — it is Java serialisation, not an
   interchange format, and QuPath's own advice is to export GeoJSON.

---

## Sources

* QuPath object/GeoJSON serialisation — `qupath-core/src/main/java/qupath/lib/io/QuPathTypeAdapters.java`, `ROITypeAdapters.java`, `GsonTools.java`, `qupath/lib/roi/GeometryTools.java` (read at `main`, 2026-08-29)
* QuPath, exporting annotations — <https://qupath.readthedocs.io/en/stable/docs/advanced/exporting_annotations.html>
* OME-XML 2016-06 schema — <https://github.com/ome/ome-model/blob/master/specification/src/main/resources/released-schema/2016-06/ome.xsd>
* OME ROI model documentation — <https://ome-model.readthedocs.io/en/stable/developers/roi.html>
* RFC 7946, The GeoJSON Format — <https://datatracker.ietf.org/doc/html/rfc7946>
