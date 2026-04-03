mod api;
mod zarr_reader;

use actix_files::Files;
use actix_web::{App, HttpServer, web};
use clap::Parser;
use std::sync::Arc;
use tokio::sync::RwLock;

use api::AppState;
use zarr_reader::{S3Config, ZarrStore};

#[derive(Parser)]
/// Command-line arguments for the server.
#[command(name = "omezarr-viewer")]
#[command(about = "OME-Zarr web viewer server")]
struct Cli {
    /// Path or URL to an OME-Zarr store (local path or http:// URL).
    /// If omitted and S3 args are provided, no dataset is loaded until selected in the UI.
    #[arg(long)]
    store: Option<String>,

    /// Bind address
    #[arg(long, default_value = "127.0.0.1:8078")]
    bind: String,

    /// S3 bucket name
    #[arg(long)]
    bucket: Option<String>,

    /// S3 endpoint URL (for S3-compatible storage)
    #[arg(long, default_value = "")]
    endpoint: String,

    /// S3 region
    #[arg(long, default_value = "us-east-1")]
    region: String,

    /// S3 access key (overrides AWS_ACCESS_KEY_ID env var)
    #[arg(long)]
    access_key: Option<String>,

    /// S3 secret key (overrides AWS_SECRET_ACCESS_KEY env var)
    #[arg(long)]
    secret_key: Option<String>,

    /// S3 key prefix for dataset listing (e.g. "zarr-test/")
    #[arg(long, default_value = "")]
    prefix: String,
}

/// Start the actix-web server, open the Zarr store, and serve the API + static files.
#[actix_web::main]
async fn main() -> std::io::Result<()> {
    env_logger::init_from_env(env_logger::Env::default().default_filter_or("info"));

    let cli = Cli::parse();

    // Build S3 config if bucket is provided
    let s3_config = cli.bucket.as_ref().map(|bucket| {
        let access_key = cli.access_key.clone()
            .or_else(|| std::env::var("AWS_ACCESS_KEY_ID").ok())
            .unwrap_or_default();
        let secret_key = cli.secret_key.clone()
            .or_else(|| std::env::var("AWS_SECRET_ACCESS_KEY").ok())
            .unwrap_or_default();
        S3Config {
            bucket: bucket.clone(),
            endpoint: cli.endpoint.clone(),
            region: cli.region.clone(),
            access_key,
            secret_key,
            prefix: cli.prefix.clone(),
        }
    });

    // Open initial store if --store is provided
    let initial_store = if let Some(ref source) = cli.store {
        log::info!("Opening zarr store at: {}", source);
        let store = ZarrStore::open(source)
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
        Some(Arc::new(store))
    } else {
        log::info!("No --store specified; use the UI to select a dataset");
        None
    };

    let data = web::Data::new(AppState {
        store: RwLock::new(initial_store),
        s3_config,
    });

    log::info!("Starting server at http://{}", cli.bind);

    HttpServer::new(move || {
        App::new()
            .app_data(data.clone())
            .service(api::info)
            .service(api::tile)
            .service(api::datasets)
            .service(api::open_dataset)
            .service(Files::new("/", "./dist/").index_file("index.html"))
    })
    .bind(&cli.bind)?
    .run()
    .await
}
