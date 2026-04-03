mod api;
mod zarr_reader;

use actix_files::Files;
use actix_web::{App, HttpServer, web};
use clap::Parser;
use std::sync::Arc;

use api::AppState;
use zarr_reader::ZarrStore;

const DEFAULT_STORE: &str = "http://localhost:8079/zarr-test/2079_R1.zarr";

#[derive(Parser)]
#[command(name = "omezarr-viewer")]
#[command(about = "OME-Zarr web viewer server")]
struct Cli {
    /// Path or URL to the OME-Zarr store (local path or http:// URL)
    #[arg(long, default_value = DEFAULT_STORE)]
    store: String,

    /// Bind address
    #[arg(long, default_value = "127.0.0.1:8078")]
    bind: String,
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    env_logger::init_from_env(env_logger::Env::default().default_filter_or("info"));

    let cli = Cli::parse();

    log::info!("Opening zarr store at: {}", cli.store);
    let store = ZarrStore::open(&cli.store)
        .await
        .expect("Failed to open zarr store");
    let info = store.metadata();
    log::info!(
        "Loaded dataset with {} resolution levels",
        info.arrays.len()
    );
    if let Some(ref omero) = info.metadata.omero {
        log::info!("  {} channels", omero.channels.len());
    }
    for (i, arr) in info.arrays.iter().enumerate() {
        log::info!("  Level {}: shape={:?}, dtype={}", i, arr.shape, arr.dtype);
    }

    let data = web::Data::new(AppState {
        store: Arc::new(store),
    });

    log::info!("Starting server at http://{}", cli.bind);

    HttpServer::new(move || {
        App::new()
            .app_data(data.clone())
            .service(api::info)
            .service(api::tile)
            .service(Files::new("/", "./dist/").index_file("index.html"))
    })
    .bind(&cli.bind)?
    .run()
    .await
}
