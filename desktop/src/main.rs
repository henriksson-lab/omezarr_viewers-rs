//! The viewer as a desktop app.
//!
//! The web build and the desktop build run **the same server and the same
//! frontend**. Tauri starts `actix-web` in-process on `127.0.0.1:0`, asks the
//! OS which port it got, and points the webview at that URL; the frontend is
//! compiled into the binary and served from memory, so a bundled app has no
//! `dist/` beside it.
//!
//! What the desktop adds is not a second API — it is a file dialog. A path the
//! user picks goes back through the *same* `POST /api/layers` the browser uses,
//! so there is one way to open a layer rather than two that can disagree.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::net::SocketAddr;
use std::sync::mpsc;

use actix_web::{web, App, HttpResponse, HttpServer};
use clap::Parser;
use rust_embed::RustEmbed;
use server::api::{self, AppState};
use server::cache::TileCache;
use server::objects::ObjectSpace;
use server::project::Project;
use server::session::{LayerRole, Session};
use server::source::{SourceRegistry, SourceSpec};
use tokio::sync::RwLock;

/// The frontend, compiled in.
///
/// `trunk build` must have run: the bundle is the app, and a desktop build
/// pointing at a `dist/` on disk would work on the developer's machine and
/// nowhere else.
#[derive(RustEmbed)]
#[folder = "../dist/"]
struct Frontend;

#[derive(Parser, Debug, Clone)]
#[command(name = "omezarr-viewer-desktop", about = "OME-Zarr viewer, desktop")]
struct Cli {
    /// A run directory or project file to open at startup.
    #[arg(long)]
    project: Option<std::path::PathBuf>,
    /// A store to open at startup.
    #[arg(long)]
    store: Option<String>,
    /// An atlas ontology (JSONL) naming the regions a label layer holds.
    #[arg(long)]
    ontology: Option<std::path::PathBuf>,
    /// Tile cache size in megabytes.
    #[arg(long, default_value_t = 512)]
    cache_mb: usize,
}

fn main() -> anyhow::Result<()> {
    env_logger::init_from_env(env_logger::Env::default().default_filter_or("info"));
    let cli = Cli::parse();

    // The server runs on its own thread with its own runtime: Tauri owns the
    // main thread, and on macOS it must.
    let (tx, rx) = mpsc::channel::<SocketAddr>();
    let serve = cli.clone();
    std::thread::spawn(move || {
        if let Err(e) = run_server(serve, tx) {
            log::error!("server: {e:#}");
        }
    });
    let address = rx
        .recv()
        .map_err(|_| anyhow::anyhow!("the server stopped before it was listening"))?;
    log::info!("serving on http://{address}");

    let url = format!("http://{address}");
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![pick_folder, pick_file])
        .setup(move |app| {
            let external = url.parse().expect("a valid url");
            tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::External(external))
                .title("OME-Zarr Viewer")
                .inner_size(1400.0, 900.0)
                .build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(())
}

/// Ask the OS for a directory. The frontend sends what comes back to
/// `POST /api/layers` as a `run folder`.
#[tauri::command]
async fn pick_folder(app: tauri::AppHandle) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().pick_folder(move |path| {
        let _ = tx.send(path.map(|p| p.to_string()));
    });
    rx.await.ok().flatten()
}

/// Ask the OS for a file — a store, a `.npy`, or a table.
#[tauri::command]
async fn pick_file(app: tauri::AppHandle) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .add_filter(
            "viewer data",
            &["zarr", "npy", "csv", "tsv", "blob", "json"],
        )
        .pick_file(move |path| {
            let _ = tx.send(path.map(|p| p.to_string()));
        });
    rx.await.ok().flatten()
}

/// Start the API and the embedded frontend, and report the bound address.
fn run_server(cli: Cli, report: mpsc::Sender<SocketAddr>) -> anyhow::Result<()> {
    actix_web::rt::System::new().block_on(async move {
        let registry = SourceRegistry::new();
        let mut session = Session::new();

        if let Some(path) = &cli.project {
            let project = if path.is_dir() {
                Project::scan(path)?
            } else {
                Project::read(path)?
            };
            let opened = project.open(&registry, &mut session).await?;
            log::info!("opened {opened} of {} layer(s)", project.layers.len());
        }
        if let Some(source) = &cli.store {
            let spec = SourceSpec::parse(source)?;
            session
                .add(
                    &registry,
                    spec,
                    LayerRole::Auto,
                    None,
                    ObjectSpace::default(),
                )
                .await?;
        }

        let data = web::Data::new(AppState {
            session: RwLock::new(session),
            registry,
            cache: TileCache::new(cli.cache_mb),
            s3_config: None,
            ontology: match cli.ontology.as_ref() {
                Some(path) => Some(std::sync::Arc::new(server::ontology::Ontology::read(path)?)),
                None => None,
            },
        });

        let server = HttpServer::new(move || {
            App::new()
                .app_data(data.clone())
                .service(api::info)
                .service(api::session_info)
                .service(api::stats)
                .service(api::tile)
                .service(api::slice)
                .service(api::voxel_value)
                .service(api::objects)
                .service(api::object_at)
                .service(api::regions)
                .service(api::datasets)
                .service(api::open_dataset)
                .service(api::save_project)
                .service(api::open_project)
                .service(api::add_layer)
                .service(api::remove_layer)
                .default_service(web::to(embedded))
        })
        // Port 0: the OS picks, and nothing has to be free for the app to start.
        .bind(("127.0.0.1", 0))?;

        let address = server
            .addrs()
            .first()
            .copied()
            .ok_or_else(|| anyhow::anyhow!("the server bound no address"))?;
        report
            .send(address)
            .map_err(|_| anyhow::anyhow!("nobody was waiting for the address"))?;
        server.run().await?;
        Ok::<(), anyhow::Error>(())
    })
}

/// Serve the compiled-in frontend.
async fn embedded(request: actix_web::HttpRequest) -> HttpResponse {
    let path = request.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    match Frontend::get(path) {
        Some(file) => HttpResponse::Ok()
            .content_type(mime_guess::from_path(path).first_or_octet_stream().as_ref())
            .body(file.data.into_owned()),
        None => HttpResponse::NotFound().body(format!("no such file: {path}")),
    }
}
