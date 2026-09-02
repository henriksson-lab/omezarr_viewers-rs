# Where these came from

Real documents, fetched rather than written here, because a fixture this repo
writes only ever exercises the shape this repo writes — and the parser's job is
to read what *other* tools produce. Three different producers are represented on
purpose: `omero-zarr`, Bio-Formats, and the specification's own examples.

| file | source | producer |
|---|---|---|
| `idr0062A-0.4.zattrs.json` | `https://uk1s3.embassy.ebi.ac.uk/idr/zarr/v0.4/idr0062A/6001240.zarr/.zattrs` | omero-zarr 0.4.0 |
| `bioformats2raw-root-0.4.zattrs.json` | `.../idr0048A/9846151.zarr/.zattrs` | bioformats2raw (container root) |
| `bioformats2raw-series-0.4.zattrs.json` | `.../idr0048A/9846151.zarr/0/.zattrs` | Bio-Formats 6.10.1 |
| `spec-0.5-multiscales.json` | `ome/ngff-spec@69b136f` `examples/multiscales_strict/multiscales_example.json` | the 0.5 specification |
| `spec-0.5-transformations.json` | `ome/ngff-spec@69b136f` `examples/multiscales_strict/multiscales_transformations.json` | the 0.5 specification |

The two spec files are pinned to submodule commit `69b136f1e64e68fead11216ac8dd3f1155668d04`,
which is what `ome/ngff` `specifications/0.5` pointed at. They are documentation
snippets and carried `//` comments, which are stripped here so they are valid
JSON; nothing else about them was changed.

To refresh one, re-fetch from the URL above. If a refreshed file stops parsing,
that is the spec or a producer moving and is exactly what these are for.
