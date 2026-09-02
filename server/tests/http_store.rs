//! The async reader, against a store served over **HTTP from a temp directory**.
//!
//! This codebase has two code paths almost everywhere: `zarrs::filesystem`
//! synchronously for `file://`, and an `AsyncOpendalStore` for `http(s)://` and
//! `s3://`. Every other fixture in this suite reads from a directory, so the
//! async half had almost no positive coverage — `public_stores.rs` covers async
//! *images*, but it reaches the public internet and is `#[ignore]`d, and it
//! cannot cover annotations at all because no public store carries our
//! annotation groups.
//!
//! So the fixture here is a real HTTP server, in process, over a temp
//! directory: everything is written with the *local* writer, read back with the
//! *async* reader, and the two are compared. That makes these tests a gate in
//! CI rather than a weather report about somebody else's uptime.
//!
//! # What this cannot cover: writing
//!
//! opendal's HTTP backend is a **read** service. It has no `write`, so
//! `geojson::save_async` and `roi_table::write_async` cannot be exercised this
//! way — verified, not assumed, and pinned below by
//! `writing_to_an_http_store_is_refused_by_the_backend`. Those two functions
//! therefore still have **no successful execution anywhere in the test suite**,
//! and CLAUDE.md's capability table promises them for remote stores. Proving
//! them needs an S3 emulator; see QUALITY.md task 21.

use actix_web::{App, HttpServer};
use omezarr_viewer_common::{Annotation, Geometry, Plane, WorldScale};
use omezarr_viewer_server::annotations::{geojson, roi_table};
use omezarr_viewer_server::source::{SourceRegistry, SourceSpec};
use omezarr_viewer_server::synthetic;
use omezarr_viewer_server::zarr_reader::{TileEncoding, TileRequest, ZarrStore};
use std::path::{Path, PathBuf};

const SHAPE: (u64, u64, u64) = (8, 128, 128);

/// A directory served over HTTP on a port the OS chose.
///
/// The port is **read back from the listener**, never derived from the pid or
/// picked by hand: two suites on one machine that each compute a port are two
/// suites that eventually collide, and the failure reads as a broken reader
/// rather than as a busy socket. `tests/browser/cdp.py::free_port` says the
/// same thing about the CDP port, for the same reason.
///
/// The `TempDir` is held here so the files outlive the server: dropping it
/// first would leave a server answering 404 for a store the test just wrote.
struct Served {
    base: String,
    _dir: tempfile::TempDir,
}

impl Served {
    fn new(dir: tempfile::TempDir) -> Self {
        let root: PathBuf = dir.path().to_path_buf();
        let server = HttpServer::new(move || {
            App::new().service(
                actix_files::Files::new("/", root.clone())
                    // A zarr v2 store is `.zgroup`, `.zattrs`, `.zarray` — all
                    // dotfiles, which `actix_files` hides by default. Without
                    // this every v2 store 404s and the reader is blamed.
                    .use_hidden_files(),
            )
        })
        // One worker: the fixture serves a handful of small files, and a
        // thread per core for that is noise in every test's output.
        .workers(1)
        .bind(("127.0.0.1", 0))
        .expect("binding 127.0.0.1:0");
        let port = server.addrs()[0].port();
        actix_web::rt::spawn(server.run());
        Served {
            base: format!("http://127.0.0.1:{port}"),
            _dir: dir,
        }
    }

    /// The URI of something inside the served directory.
    fn uri(&self, name: &str) -> String {
        format!("{}/{name}", self.base)
    }
}

/// A synthetic image on disk, and that same directory served over HTTP.
fn served_image() -> (Served, PathBuf) {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = dir.path().join("image.zarr");
    synthetic::write_image(&store, SHAPE, &synthetic::blobs(SHAPE, 3)).expect("write image");
    (Served::new(dir), store)
}

async fn open_http(registry: &SourceRegistry, uri: &str) -> ZarrStore {
    let spec = SourceSpec::parse(uri).expect("a URL");
    ZarrStore::open_spec(registry, &spec)
        .await
        .unwrap_or_else(|e| panic!("opening {uri}: {e:#}"))
}

/// The same store, read twice: `zarrs::filesystem` and `AsyncOpendalStore`.
///
/// The claim is not "an HTTP read succeeds" but that the two paths return the
/// *same picture*. A remote read that lost the channel offset, or that fetched
/// the wrong chunk for a level, comes back a valid tile of the wrong pixels —
/// which is why this compares bytes against the local read rather than
/// asserting the response is well-formed.
#[actix_web::test]
async fn an_image_read_over_http_is_the_same_image_as_on_disk() {
    let (served, path) = served_image();
    let registry = SourceRegistry::new();

    let local = ZarrStore::open_local(&path).expect("open on disk");
    let remote = open_http(&registry, &served.uri("image.zarr")).await;

    let (here, there) = (&local.metadata().arrays, &remote.metadata().arrays);
    assert_eq!(there.len(), here.len(), "a different number of levels");
    for (level, (a, b)) in here.iter().zip(there).enumerate() {
        assert_eq!(b.shape, a.shape, "level {level} shape");
        assert_eq!(b.dtype, a.dtype, "level {level} dtype");
    }
    assert_eq!(
        remote.attributes().keys().collect::<Vec<_>>(),
        local.attributes().keys().collect::<Vec<_>>(),
        "the attributes are what `image-label` detection and the omero window \
         are read from"
    );

    // Every level, because level choice is where the two paths build an array
    // key differently: the sync one has a path, the async one a prefix.
    for level in 0..here.len() {
        let request = TileRequest::new(level, 4, 8, 16, 24).at(0, 1, 3);
        let want = local.read_tile_bytes(&request).await.expect("local tile");
        let got = remote.read_tile_bytes(&request).await.expect("remote tile");

        assert_eq!(got.dtype, want.dtype, "level {level} tile dtype");
        // Not a fill-value tile on both sides, which would compare equal while
        // proving nothing about either read.
        assert!(
            want.bytes.windows(4).any(|w| w != &want.bytes[..4]),
            "level {level} is uniform on disk, so equality proves nothing"
        );
        assert_eq!(got.bytes, want.bytes, "level {level} pixels differ");
    }

    // And in the labels' encoding, which takes a different branch out of the
    // reader: an id above 2^24 does not survive f32, so a label tile is raw.
    let request = TileRequest::new(0, 0, 0, 8, 8)
        .at(0, 0, 2)
        .encoded(TileEncoding::Raw);
    let want = local.read_tile_bytes(&request).await.expect("local raw");
    let got = remote.read_tile_bytes(&request).await.expect("remote raw");
    assert_eq!(got.dtype, want.dtype);
    assert_eq!(got.bytes, want.bytes, "raw pixels differ");
}

/// A container laid out the way `bioformats2raw` and `img2omezarr` write one.
///
/// Re-derived rather than shared with `tests/bioformats2raw.rs`: that file
/// covers the *local* decision, this one covers the same decision taken over
/// the network, and a helper reaching across two test binaries is a coupling
/// neither of them asked for.
fn container(root: &Path, series: &[&str]) -> PathBuf {
    let store = root.join("container.zarr");
    std::fs::create_dir_all(store.join("OME")).expect("OME group");
    write_json(
        &store.join(".zgroup"),
        &serde_json::json!({"zarr_format": 2}),
    );
    write_json(
        &store.join(".zattrs"),
        &serde_json::json!({"bioformats2raw.layout": 3}),
    );
    write_json(
        &store.join("OME").join(".zgroup"),
        &serde_json::json!({"zarr_format": 2}),
    );
    write_json(
        &store.join("OME").join(".zattrs"),
        &serde_json::json!({"series": series}),
    );
    for name in series {
        synthetic::write_image(&store.join(name), SHAPE, &synthetic::blobs(SHAPE, 3))
            .expect("a series");
    }
    store
}

fn write_json(path: &Path, value: &serde_json::Value) {
    std::fs::write(path, serde_json::to_vec_pretty(value).expect("json")).expect("write");
}

/// The async half of the container resolution, which shipped with no test that
/// runs in CI: `public_stores.rs` covers it against the IDR, and that suite is
/// `#[ignore]`d because it reaches the internet.
///
/// A container root has no pixels of its own, so getting a pyramid back is the
/// whole claim: the layout key was found, `OME/.zattrs` was read, and the image
/// group beneath was opened — all over HTTP.
#[actix_web::test]
async fn a_container_resolves_to_its_series_over_http() {
    let dir = tempfile::tempdir().expect("temp dir");
    container(dir.path(), &["0"]);
    let served = Served::new(dir);
    let registry = SourceRegistry::new();

    let store = open_http(&registry, &served.uri("container.zarr")).await;

    let level = &store.metadata().arrays[0];
    assert_eq!(
        level.shape[level.shape.len() - 2..],
        [SHAPE.1, SHAPE.2],
        "the pixels are the series', not the container's — it has none"
    );
    assert!(
        store.attributes().contains_key("multiscales"),
        "got the container's attributes instead of the series': {:?}",
        store.attributes().keys().collect::<Vec<_>>()
    );

    // Metadata resolution and tile reading take different routes to an array,
    // and a store opened at a container root once described its pyramid
    // perfectly and could not fetch a single tile (QUALITY.md task 19).
    let tile = store
        .read_tile_bytes(&TileRequest::new(0, 0, 0, 8, 8).at(0, 0, 3))
        .await
        .expect("a tile from the series");
    assert_eq!(tile.bytes.len(), 8 * 8 * 4);
}

/// A polygon with a hole: the shape OME-XML's ROI model cannot express, and so
/// the one that has to survive every path the GeoJSON form takes.
fn ring_with_a_hole() -> Annotation {
    Annotation {
        geometry: Geometry::Polygon(vec![
            vec![
                [10.0, 10.0],
                [90.0, 10.0],
                [90.0, 90.0],
                [10.0, 90.0],
                [10.0, 10.0],
            ],
            vec![
                [30.0, 30.0],
                [60.0, 30.0],
                [60.0, 60.0],
                [30.0, 60.0],
                [30.0, 30.0],
            ],
        ]),
        plane: Plane::at(2, 0),
        ..Annotation::default()
    }
}

/// `list_async` and `load_async`, neither of which had ever been called
/// successfully anywhere in this suite.
#[actix_web::test]
async fn an_annotation_set_written_locally_reads_back_over_http() {
    let (served, path) = served_image();
    let registry = SourceRegistry::new();

    let rows = vec![
        Annotation {
            label: "region".into(),
            name: Some("the one with a hole".into()),
            ..ring_with_a_hole()
        },
        Annotation {
            label: "cell".into(),
            ..Annotation::point(40.0, 60.0, Plane::at(3, 0))
        },
    ];
    geojson::save(&path, "drawn", &rows).expect("save locally");

    let uri = served.uri("image.zarr");

    // The index the group's attributes carry, which is what the reopen panel
    // offers — a set on disk that the group does not list is a set nobody can
    // find.
    assert_eq!(
        geojson::list_async(&registry, &uri).await.expect("list"),
        vec!["drawn".to_string()],
    );

    let file = geojson::load_async(&registry, &uri, "drawn")
        .await
        .expect("load over http");
    assert!(
        file.declared_space,
        "the group's `coordinate_space` did not come back, so the reader is \
         guessing what the coordinates mean"
    );
    assert_eq!(file.rows.len(), 2);

    // The coordinates are the point of the format: GeoJSON is written
    // unconverted because QuPath's pixels and this viewer's world already
    // agree, so anything that moves is a bug rather than a rounding choice.
    let local = geojson::load(&path, "drawn").expect("load locally");
    for (remote, local) in file.rows.iter().zip(&local.rows) {
        assert_eq!(remote.geometry, local.geometry, "geometry differs");
        assert_eq!(remote.label, local.label);
        assert_eq!(remote.name, local.name);
        assert_eq!(remote.plane, local.plane);
    }
    assert_eq!(
        file.rows[0].bounds(),
        Some([10.0, 10.0, 90.0, 90.0]),
        "the hole is inside the ring, so it must not change the bounds"
    );
}

/// A set the group does not hold: the error names it rather than coming back
/// as an empty set, because an empty set looks exactly like a set that saved
/// nothing.
#[actix_web::test]
async fn a_missing_annotation_set_is_an_error_over_http_and_names_itself() {
    let (served, _path) = served_image();
    let registry = SourceRegistry::new();

    let error = geojson::load_async(&registry, &served.uri("image.zarr"), "never-saved")
        .await
        .expect_err("a set that was never written")
        .to_string();
    assert!(error.contains("never-saved"), "{error}");
}

/// `roi_table::read_async` and `list_async`, likewise never called
/// successfully before this file.
#[actix_web::test]
async fn an_roi_table_written_locally_reads_back_over_http() {
    let (served, path) = served_image();
    let registry = SourceRegistry::new();

    // A voxel size, because the ROI table is the one form that stores
    // micrometres: the writer divides by the scale and the reader multiplies
    // back, and a remote read that skipped the group attributes would take the
    // default scale of 1 and land every box in the wrong place.
    let scale = WorldScale {
        voxel: [2.0, 0.5, 0.5],
        seconds: 1.0,
    };
    let rows = vec![
        Annotation {
            label: "region".into(),
            z_extent: 2,
            ..Annotation::rect(20.0, 10.0, 60.0, 40.0, Plane::at(1, 0))
        },
        Annotation {
            label: "other".into(),
            ..Annotation::rect(75.0, 80.0, 100.0, 100.0, Plane::at(4, 0))
        },
    ];
    roi_table::write(&path, "boxes", &rows, scale).expect("write locally");

    let uri = served.uri("image.zarr");
    assert_eq!(
        roi_table::list_async(&registry, &uri).await.expect("list"),
        vec!["boxes".to_string()],
    );

    let table = roi_table::read_async(&registry, &uri, "boxes")
        .await
        .expect("read over http");
    assert_eq!(table.backend, "csv");
    assert!(table.is_geometry(), "an ROI table is boxes");
    assert_eq!(table.scale, scale, "the scale the writer recorded");

    let local = roi_table::read(&path, "boxes").expect("read locally");
    assert_eq!(table.rows.len(), local.rows.len());
    for (remote, local) in table.rows.iter().zip(&local.rows) {
        assert_eq!(remote.bounds(), local.bounds(), "a box moved");
        assert_eq!(remote.plane, local.plane);
        assert_eq!(remote.z_extent, local.z_extent);
    }
    assert_eq!(
        table.rows[0].bounds(),
        Some([20.0, 10.0, 60.0, 40.0]),
        "the world coordinates the box was drawn at"
    );
}

/// **The limit, pinned rather than papered over.**
///
/// opendal's HTTP backend is a read service: it implements `read` and `stat`
/// and no `write`. So the remote *write* path can be reached over HTTP but
/// never completes, and neither `geojson::save_async` nor
/// `roi_table::write_async` has a successful execution anywhere in this suite.
///
/// This test exists to say that in code. If it ever starts failing because a
/// write succeeded, the capability table stopped being aspirational and the
/// module documentation above needs rewriting.
#[actix_web::test]
async fn writing_to_an_http_store_is_refused_by_the_backend() {
    let (served, _path) = served_image();
    let registry = SourceRegistry::new();
    let uri = served.uri("image.zarr");

    for error in [
        format!(
            "{:#}",
            geojson::save_async(&registry, &uri, "drawn", &[])
                .await
                .expect_err("http has no write")
        ),
        format!(
            "{:#}",
            roi_table::write_async(&registry, &uri, "boxes", &[], WorldScale::default())
                .await
                .expect_err("http has no write")
        ),
    ] {
        // Not a 405 dressed up as a parse failure, and not a panic: the
        // operator says the operation does not exist, and the context says
        // which write was being attempted.
        assert!(
            error.contains("not supported"),
            "the refusal should name the missing operation: {error}"
        );
        assert!(error.contains("writing the"), "{error}");
    }
}
