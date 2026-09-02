//! One server, in process, driven over HTTP.
//!
//! The tests beside this file are the only ones that go through `api.rs` at
//! all: everything else in `server/tests/` calls the library directly, which
//! verifies what a read returns but says nothing about routing, query parsing,
//! status codes, or the headers the frontend's contracts rest on.
//!
//! Two things here are deliberate:
//!
//! * **The routes come from [`api::configure`]**, the same list `main` uses, in
//!   the same order. Order is behaviour — `/tables` and `/layers` are literal
//!   segments that also match `/{layer}` — so a harness with its own list would
//!   be testing a server nobody runs.
//! * **The service is rebuilt per request, over shared state.** `web::Data` is
//!   an `Arc`, so a POST and the GET that checks it see the same session;
//!   rebuilding avoids naming `init_service`'s opaque type just to store it.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use actix_web::http::StatusCode;
use actix_web::{test, web, App};
use omezarr_viewer_server::api::{self, AppState};
use omezarr_viewer_server::cache::TileCache;
use omezarr_viewer_server::objects::ObjectSpace;
use omezarr_viewer_server::session::{LayerRole, Session};
use omezarr_viewer_server::source::{SourceRegistry, SourceSpec};
use omezarr_viewer_server::synthetic;
use tempfile::TempDir;
use tokio::sync::RwLock;

/// The fixture's shape, `(z, y, x)`.
pub const SHAPE: (u64, u64, u64) = (8, 128, 128);

/// One response, already read, so a test can look at all of it.
pub struct Res {
    pub status: StatusCode,
    pub headers: actix_web::http::header::HeaderMap,
    pub body: Vec<u8>,
}

impl Res {
    /// The body as JSON. Panics with the body text when it is not JSON, which
    /// is what an unexpected error page looks like.
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| {
            panic!("expected JSON, got {e}: {}", self.text());
        })
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// A response header, as a string.
    pub fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    }

    pub fn is_ok(&self) -> bool {
        self.status.is_success()
    }
}

/// A server with a temp store behind it.
pub struct Api {
    pub state: web::Data<AppState>,
    /// Kept alive: dropping it deletes the store the session has open.
    pub dir: TempDir,
    /// The image store on disk, for tests that need to look at files.
    pub store: PathBuf,
}

impl Api {
    /// A server with nothing open.
    pub async fn empty() -> Self {
        Self::build(false)
    }

    /// A server with one image layer open, over a synthetic store.
    pub async fn image() -> Self {
        Self::opened(false).await
    }

    /// [`Api::image`], but started with `--allow-remote-writes`.
    ///
    /// A separate constructor rather than a setter: the flag is read straight
    /// off the shared `AppState`, so it has to be right before any request is
    /// made rather than swapped underneath one.
    pub async fn image_writable() -> Self {
        Self::opened(true).await
    }

    /// An image layer and a label layer over it, in that order.
    pub async fn with_labels() -> Self {
        let api = Self::image().await;
        let labels = api.dir.path().join("labels.zarr");
        let blobs = synthetic::blobs(SHAPE, 3);
        synthetic::write_labels(&labels, SHAPE, &blobs).expect("write labels");
        api.open(&labels, LayerRole::Labels).await;
        api
    }

    fn build(allow_remote_writes: bool) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = dir.path().join("image.zarr");
        Self {
            state: state(Session::new(), allow_remote_writes),
            dir,
            store,
        }
    }

    async fn opened(allow_remote_writes: bool) -> Self {
        let api = Self::build(allow_remote_writes);
        write_image(&api.store);
        api.open(&api.store.clone(), LayerRole::Image).await;
        api
    }

    /// An image layer and an object layer of detections over it.
    ///
    /// The blobs are the same ones the image was drawn from, so a row's
    /// position is a place where there really is something.
    pub async fn with_objects() -> Self {
        let api = Self::image().await;
        let blobs = synthetic::blobs(SHAPE, 3);
        synthetic::write_objects(api.dir.path(), &blobs).expect("write objects");
        api.open(&api.dir.path().join("cells.csv"), LayerRole::Objects)
            .await;
        api
    }

    /// An image layer and an empty annotation layer, as "New layer" makes one.
    pub async fn with_annotations() -> Self {
        let api = Self::image().await;
        api.post(
            "/api/annotations/layers",
            serde_json::json!({"name": "drawn"}),
        )
        .await;
        api
    }

    /// The id of the first layer of a kind, as `/api/session` reports it.
    pub async fn layer_of_kind(&self, kind: &str) -> String {
        self.state
            .session
            .read()
            .await
            .info()
            .layers
            .iter()
            .find(|l| serde_json::to_value(&l.kind).unwrap()["kind"] == kind)
            .unwrap_or_else(|| panic!("no {kind} layer is open"))
            .id
            .clone()
    }

    /// Open a source as a layer, and return the id the session gave it.
    pub async fn open(&self, path: &Path, role: LayerRole) -> String {
        self.state
            .session
            .write()
            .await
            .add(
                &self.state.registry,
                SourceSpec::File(path.to_path_buf()),
                role,
                None,
                ObjectSpace::default(),
            )
            .await
            .map(only)
            .expect("open layer")
    }

    /// The ids of every open layer, in draw order.
    pub async fn layer_ids(&self) -> Vec<String> {
        self.state
            .session
            .read()
            .await
            .info()
            .layers
            .iter()
            .map(|l| l.id.clone())
            .collect()
    }

    // -- requests ------------------------------------------------------------

    pub async fn get(&self, uri: &str) -> Res {
        self.send(test::TestRequest::get().uri(uri)).await
    }

    pub async fn post(&self, uri: &str, body: serde_json::Value) -> Res {
        self.send(test::TestRequest::post().uri(uri).set_json(body))
            .await
    }

    /// A POST with no body, for the routes that take none.
    pub async fn post_empty(&self, uri: &str) -> Res {
        self.send(test::TestRequest::post().uri(uri)).await
    }

    pub async fn put(&self, uri: &str, body: serde_json::Value) -> Res {
        self.send(test::TestRequest::put().uri(uri).set_json(body))
            .await
    }

    pub async fn delete(&self, uri: &str) -> Res {
        self.send(test::TestRequest::delete().uri(uri)).await
    }

    async fn send(&self, req: test::TestRequest) -> Res {
        let app = test::init_service(
            App::new()
                .app_data(self.state.clone())
                .configure(api::configure),
        )
        .await;
        let response = test::call_service(&app, req.to_request()).await;
        let status = response.status();
        let headers = response.headers().clone();
        let body = test::read_body(response).await.to_vec();
        Res {
            status,
            headers,
            body,
        }
    }
}

/// Write the synthetic image every fixture starts from.
pub fn write_image(path: &Path) {
    let blobs = synthetic::blobs(SHAPE, 3);
    synthetic::write_image(path, SHAPE, &blobs).expect("write image");
}

fn state(session: Session, allow_remote_writes: bool) -> web::Data<AppState> {
    web::Data::new(AppState {
        session: RwLock::new(session),
        registry: SourceRegistry::new(),
        cache: TileCache::new(8),
        s3_config: None,
        ontology: None,
        allow_remote_writes,
    })
}

/// The id of the single layer a source opened as.
///
/// `Session::add` returns a list because a `bioformats2raw` container expands
/// into one layer per series. Every fixture here is one image, so this says so
/// and fails loudly if that ever stops being true.
fn only(ids: Vec<String>) -> String {
    assert_eq!(ids.len(), 1, "expected one layer, got {ids:?}");
    ids.into_iter().next().expect("one layer")
}
