# omezarr-viewer-rs

A web-based OME-Zarr image viewer built with Rust. The server reads OME-Zarr datasets (local or remote) and
the frontend renders multi-channel microscopy data in the browser using WebGL2.

## Features

- Multi-channel rendering with per-channel color, contrast, and opacity controls
- Pyramid level selection based on zoom — automatically picks the appropriate resolution
- Tiled rendering with progressive loading and fallback from coarser levels
- Pan and zoom with mouse (scroll to zoom around cursor, drag to pan)
- Z-slice and time point navigation
- Supports OME-NGFF metadata (V2 and V3) including OMERO channel info
- Reads any numeric dtype (uint8/16/32, int16, float32/64)
- Local filesystem and HTTP-backed Zarr stores

## Prerequisites

- Rust toolchain (stable)
- `trunk` for building the WASM frontend: `cargo install trunk`
- WASM target: `rustup target add wasm32-unknown-unknown`

## Quick start

```bash
make serve STORE=/path/to/dataset.zarr BIND=127.0.0.1:8078
```

Then open http://127.0.0.1:8078 in the browser.

For remote stores:

```bash
make serve STORE=http://example.com/data.zarr
```

## Build commands

```bash
make build          # Build frontend + server, output in dist/
make serve          # Build and run (default: localhost:8078)
make dev-app        # Watch-rebuild frontend only
make dev-server     # Watch-rebuild server only
make clean          # Remove build artifacts
```

## Architecture

Cargo workspace with three crates:

| Crate | Path | Description |
|-------|------|-------------|
| `omezarr-viewer-common` | `src/` | Shared types for the server/frontend API contract |
| `server` | `server/` | actix-web server with tile and metadata endpoints |
| `app` | `app/` | Yew 0.21 WASM frontend with WebGL2 renderer |

### API

- `GET /api/info` — dataset metadata (multiscales, channels, per-level shapes)
- `GET /api/tile?level=&t=&c=&z=&y=&x=&h=&w=` — raw float32 tile data

### Rendering

The frontend fetches tiles aligned to the Zarr chunk grid and renders each as a WebGL2 quad with an R32F texture.
A fragment shader handles per-channel contrast normalization, colorization, and compositing in a single pass (up to 6 channels).
Pyramid levels are selected to match screen pixel density, and a tile cache retains coarser-level tiles as fallbacks during level transitions.

## License

MIT license


Some of the code may be derived from https://github.com/hms-dbmi/vizarr, but a rather large rewrite has been performed
to fit the Rust frontend framework, and Rust backend libraries. The license of vizarr is in either case compatible
