//! Does the slice grid read the same chunks twice?
//!
//! The 2x2 grid is three readers, not four: the `xy` view asks for tiles, the
//! two orthogonal panes ask for whole planes, and the 3D box fetches nothing at
//! all (`app/src/cube_pane.rs` — a box and three cuts are decided from the
//! volume's extent, which the client already has). The response cache
//! ([`crate::cache::TileCache`]) already makes an *identical* repeat free, so
//! what is left to measure is the overlap it cannot see: a tile at `z=k` and a
//! plane at `y=j` are different keys that read some of the same chunks.
//!
//! Measured rather than argued about, because `ortho_pane`'s "a plane crosses
//! every chunk row of the store" is a claim about how many chunks a plane
//! *touches*, not about how many of them anything else touches.
//!
//! # What the measurement says, and why there is no chunk cache
//!
//! * With every panel on level 0 and chunks the size of a tile — the best case
//!   the hypothesis can have — 21% of chunk reads are repeats, and *none* of
//!   them are a panel re-reading its own chunks.
//! * Move the panes to the level they actually read at on any store wider than
//!   2048 px and the `xy` view shares nothing with them at all: different
//!   levels are different arrays. What is left is the one chunk column where
//!   the two planes cross — 16 reads here, one per z plane, whatever the
//!   store's width. Its share of the work is therefore `1 / (2 * chunks
//!   across)` and falls as the store grows: 14.3% at 4 chunks across, 5.0% at
//!   8.
//! * The largest duplicate source found is not between panels at all. A store
//!   chunked at 512 read by a client that tiles at 256 makes the `xy` view
//!   decode every chunk four times — 12 of 16 reads in a frame, inside one
//!   panel. Fixing that is a tile-size question for `app/`, not a cache.
//!
//! And a chunk cache could not be put where the saving would be worth most:
//! `zarrs` 0.18.3's `ChunkCache` is bounded by `ReadableStorageTraits` and its
//! source carries a bare `// TODO: AsyncChunkCache`, so the `s3://` and
//! `http(s)://` path — the one where a duplicate chunk read costs a request
//! rather than a warm page-cache hit and a decode — cannot use it. A cache
//! available only on the path where duplicates are cheapest, saving a column of
//! chunks per crosshair position, is not worth a second thing to size, report
//! and reason about beside the response cache.
//!
//! # The request mix
//!
//! Taken from `app/`, so this measures the client that exists:
//!
//! * tiles are [`TILE`] pixels square (`layers::TileGrid`, `chunk.clamp(256,
//!   2048)`), and a fitted view asks for every tile of the plane;
//! * a pane reads one whole plane per axis, at the first level that fits
//!   2048 px (`app/src/app/tiles.rs::ortho_level`);
//! * scrubbing z moves the `xy` view and leaves the panes' indices alone, so
//!   the panes' second frame is a response-cache hit and never reaches a chunk.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use actix_web::{test, web, App};
use tempfile::TempDir;
use tokio::sync::RwLock;
use zarrs::array::{ArrayBuilder, DataType, FillValue};
use zarrs::array_subset::ArraySubset;
use zarrs::filesystem::FilesystemStore;
use zarrs::group::GroupBuilder;

use crate::api::{self, AppState};
use crate::cache::TileCache;
use crate::chunk_probe::{self, Report};
use crate::objects::ObjectSpace;
use crate::session::{LayerRole, Session};
use crate::source::{SourceRegistry, SourceSpec};

/// The tile edge the client asks in, in pixels.
const TILE: u64 = 256;

/// The fixture's shape, `(c, z, y, x)`.
///
/// Small enough to write in a test and large enough to have a chunk grid worth
/// counting: 4x4 chunks across a plane at [`CHUNK_ALIGNED`], 16 planes deep.
const SHAPE: [u64; 4] = [1, 16, 1024, 1024];

/// A chunk edge equal to the client's tile: one tile is one chunk.
const CHUNK_ALIGNED: u64 = 256;

/// A chunk edge of twice the client's tile — the common OME-Zarr 512 chunking,
/// against a client that tiles in 256.
const CHUNK_WIDE: u64 = 512;

/// A written fixture, kept alive by its temp directory.
struct Fixture {
    _dir: TempDir,
    store: PathBuf,
}

/// Write a two-level `uint8` store whose values are arithmetic; only the shape
/// of the reads matters here, so the pixels are the cheapest thing that is not
/// a constant.
fn write_fixture(chunk: u64) -> Fixture {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("image.zarr");
    let store = Arc::new(FilesystemStore::new(&path).expect("filesystem store"));

    let mut datasets = Vec::new();
    for (level, step) in [(0usize, 1u64), (1, 2)] {
        let shape = [SHAPE[0], SHAPE[1], SHAPE[2] / step, SHAPE[3] / step];
        let chunk_shape = vec![1, 1, chunk.min(shape[2]), chunk.min(shape[3])];
        let array = ArrayBuilder::new(
            shape.to_vec(),
            DataType::UInt8,
            chunk_shape.try_into().expect("chunk shape"),
            FillValue::from(0u8),
        )
        .build(store.clone(), &format!("/{level}"))
        .expect("array");
        array.store_metadata().expect("array metadata");
        let values: Vec<u8> = (0..shape.iter().product::<u64>())
            .map(|i| (i % 251) as u8)
            .collect();
        array
            .store_array_subset_elements(&ArraySubset::new_with_shape(shape.to_vec()), &values)
            .expect("store elements");
        datasets.push(serde_json::json!({
            "path": level.to_string(),
            "coordinateTransformations": [
                {"type": "scale", "scale": [1.0, 1.0, step as f64, step as f64]}
            ],
        }));
    }

    let attributes = serde_json::json!({
        "multiscales": [{
            "name": "chunk-reuse",
            "axes": [
                {"name": "c", "type": "channel"},
                {"name": "z", "type": "space"},
                {"name": "y", "type": "space"},
                {"name": "x", "type": "space"},
            ],
            "datasets": datasets,
        }],
    });
    let group = GroupBuilder::new()
        .attributes(attributes.as_object().unwrap().clone())
        .build(store.clone(), "/")
        .expect("group");
    group.store_metadata().expect("group metadata");

    Fixture {
        _dir: dir,
        store: path,
    }
}

/// One session, driven over the real routes.
///
/// Through HTTP rather than through [`crate::zarr_reader`] directly, because
/// the response cache sits between the two and the whole question is what it
/// leaves behind.
struct Grid {
    state: web::Data<AppState>,
}

impl Grid {
    async fn open(store: &Path) -> Self {
        let state = web::Data::new(AppState {
            session: RwLock::new(Session::new()),
            registry: SourceRegistry::new(),
            // The server's own default, so the measurement is of the server as
            // it ships rather than of a cache nobody runs.
            cache: TileCache::new(512),
            s3_config: None,
            ontology: None,
            allow_remote_writes: false,
        });
        state
            .session
            .write()
            .await
            .add(
                &state.registry,
                SourceSpec::File(store.to_path_buf()),
                LayerRole::Image,
                None,
                ObjectSpace::default(),
            )
            .await
            .expect("open image layer");
        Self { state }
    }

    async fn get(&self, uri: &str) {
        let app = test::init_service(
            App::new()
                .app_data(self.state.clone())
                .configure(api::configure),
        )
        .await;
        let response =
            test::call_service(&app, test::TestRequest::get().uri(uri).to_request()).await;
        assert!(
            response.status().is_success(),
            "{uri} -> {}",
            response.status()
        );
    }

    /// Every tile of a fitted `xy` view at one z.
    async fn xy_view(&self, level: usize, z: u64) {
        let (h, w) = (SHAPE[2] >> level, SHAPE[3] >> level);
        for y in (0..h).step_by(TILE as usize) {
            for x in (0..w).step_by(TILE as usize) {
                self.get(&format!(
                    "/api/tile?level={level}&z={z}&y={y}&x={x}&h={}&w={}",
                    TILE.min(h - y),
                    TILE.min(w - x)
                ))
                .await;
            }
        }
    }

    /// The two orthogonal panes, cut through the same point.
    async fn ortho_panes(&self, level: usize, y: u64, x: u64) {
        self.get(&format!("/api/slice?axis=y&index={y}&level={level}"))
            .await;
        self.get(&format!("/api/slice?axis=x&index={x}&level={level}"))
            .await;
    }
}
/// Drive one grid session and report what it read.
///
/// `z_values` is the scrub: the panes are cut at the centre and stay there,
/// which is what moving through z in the client does.
async fn measure(chunk: u64, xy_level: usize, ortho_level: usize, z_values: &[u64]) -> Report {
    let fixture = write_fixture(chunk);
    let grid = Grid::open(&fixture.store).await;
    chunk_probe::start();
    for &z in z_values {
        grid.xy_view(xy_level, z).await;
        grid.ortho_panes(
            ortho_level,
            (SHAPE[2] >> ortho_level) / 2,
            (SHAPE[3] >> ortho_level) / 2,
        )
        .await;
    }
    chunk_probe::finish()
}

/// One panel's counts, by the name [`crate::chunk_probe`] tags it with.
fn panel(report: &Report, name: &str) -> crate::chunk_probe::RouteCounts {
    report
        .per_route
        .iter()
        .find(|(route, _)| *route == name)
        .unwrap_or_else(|| panic!("no reads from {name}"))
        .1
}

/// The best case for the hypothesis: every panel at level 0, chunks exactly the
/// size of a tile, three z values scrubbed.
///
/// The numbers are arithmetic rather than observation. The `xz` plane at
/// `y=512` covers the 16x4 chunks of chunk-row `y=2`; the `yz` plane at `x=512`
/// covers the 16x4 of chunk-column `x=2`; they meet in the 16 chunks of the
/// column `(y=2, x=2)`, and each `xy` frame meets each plane in 4 chunks and
/// both in 1. Scrubbing z costs nothing on the panes — their requests do not
/// change, so the response cache answers them and the reader never runs — so
/// each further frame adds only its own 16 chunks, 7 of which the planes have
/// already read.
#[actix_web::test]
async fn every_panel_at_one_level_repeats_a_fifth_of_its_chunk_reads() {
    let report = measure(CHUNK_ALIGNED, 0, 0, &[4, 8, 12]).await;
    println!(
        "aligned chunks, every panel at level 0:\n{}",
        report.table()
    );

    let xy = panel(&report, "tile:xy");
    let xz = panel(&report, "slice:xz");
    let yz = panel(&report, "slice:yz");

    // 16 tiles a frame of one chunk each, three frames; a plane is 64 chunks
    // and is read once however often it is asked for.
    assert_eq!((xy.reads, xz.reads, yz.reads), (48, 64, 64));
    assert_eq!(report.reads, 176);
    assert_eq!(report.unique, 139);

    // Every repeat is between panels; no panel re-reads a chunk of its own.
    assert_eq!((xy.repeat_own, xz.repeat_own, yz.repeat_own), (0, 0, 0));
    assert_eq!(
        (xy.repeat_cross, xz.repeat_cross, yz.repeat_cross),
        (14, 4, 19)
    );

    // 21%, and the bulk of it is the 16-chunk column where the two planes
    // cross — not the tiles meeting the planes.
    assert!((report.duplicate_rate() - 0.21).abs() < 0.005);
}

/// The same mix with the panes one level down, which is what the client does on
/// any store whose level 0 is wider than 2048 px (`ortho_level`).
///
/// Different levels are different arrays, so the `xy` view shares nothing with
/// either pane and the only repeat left is the two planes crossing. That
/// crossing is a *column*: one chunk per z, whatever the store's width — so it
/// is 16 reads here at both chunk sizes, while everything around it doubles.
/// This is the case a chunk cache has to be justified against rather than by:
/// on a store big enough for the panes to drop a level, the between-panel
/// overlap the hypothesis is about is one chunk column and nothing else.
#[actix_web::test]
async fn a_pane_at_a_coarser_level_shares_only_the_column_where_the_planes_cross() {
    for (chunk, reads) in [(CHUNK_ALIGNED, 112), (CHUNK_WIDE / 4, 320)] {
        let report = measure(chunk, 0, 1, &[4, 8, 12]).await;
        println!("{chunk}px chunks, panes at level 1:\n{}", report.table());

        let xy = panel(&report, "tile:xy");
        assert_eq!((xy.repeat_own, xy.repeat_cross), (0, 0));
        // The `yz` plane re-reading the column it shares with `xz`: one chunk
        // per z plane, 16 of them, at either chunk size.
        assert_eq!(panel(&report, "slice:yz").repeat_cross, 16);
        assert_eq!(report.reads - report.unique, 16);
        assert_eq!(report.reads, reads);
    }
}

/// A store chunked at 512 read by a client that tiles at 256.
///
/// One frame, so the count is unambiguous: four tiles land inside one chunk, so
/// the `xy` view alone decodes 16 chunks' worth of work over 4 distinct chunks.
/// That is a bigger duplicate source than every panel overlap put together, and
/// it is inside one panel — which is the finding, because it says a chunk cache
/// would be paying for the client's tile size rather than for the grid.
#[actix_web::test]
async fn a_chunk_wider_than_a_tile_is_reread_within_the_xy_view() {
    let report = measure(CHUNK_WIDE, 0, 0, &[4]).await;
    println!(
        "512 chunks, 256 tiles, every panel at level 0:\n{}",
        report.table()
    );

    let xy = panel(&report, "tile:xy");
    assert_eq!(xy.reads, 16);
    assert_eq!(xy.first, 4, "a 2x2 chunk grid holds the whole frame");
    assert_eq!(xy.repeat_own, 12, "each chunk decoded four times over");
    assert_eq!(xy.repeat_cross, 0, "and none of it is the panes' doing");
}
