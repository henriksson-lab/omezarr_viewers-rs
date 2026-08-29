use actix_files::Files;
use actix_web::{web, App, HttpServer};
use clap::Parser;
use tokio::sync::RwLock;

use omezarr_viewer_server::api::{self, AppState};
use omezarr_viewer_server::cache::TileCache;
use omezarr_viewer_server::objects::ObjectSpace;
use omezarr_viewer_server::ontology::Ontology;
use omezarr_viewer_server::project::Project;
use omezarr_viewer_server::session::{LayerRole, Session};
use omezarr_viewer_server::source::{S3Profile, SourceRegistry, SourceSpec};
use omezarr_viewer_server::zarr_reader::S3Config;

#[derive(Parser)]
/// Command-line arguments for the server.
#[command(name = "omezarr-viewer")]
#[command(about = "OME-Zarr web viewer server")]
struct Cli {
    /// Path or URL to an OME-Zarr store (local path, http:// URL, or s3://bucket/key).
    /// If omitted and S3 args are provided, no dataset is loaded until selected in the UI.
    #[arg(long)]
    store: Option<String>,

    /// Additional layers to open, as `source[:role]` where role is image,
    /// labels or objects.
    /// Repeatable; layers are drawn in the order given, above --store.
    #[arg(long = "layer")]
    layers: Vec<String>,

    /// A run directory to scan, or a project `.json` to open.
    ///
    /// A directory is walked for zarr stores, `.npy` volumes and object tables
    /// — which is what a `clearmap-ng` workspace is — and every one becomes a
    /// layer.
    #[arg(long)]
    project: Option<std::path::PathBuf>,

    /// An atlas ontology (JSONL) naming the regions a label layer holds.
    #[arg(long)]
    ontology: Option<std::path::PathBuf>,

    /// Bind address
    #[arg(long, default_value = "127.0.0.1:8078")]
    bind: String,

    /// Tile cache size in megabytes. 0 disables it.
    #[arg(long, default_value_t = 512)]
    cache_mb: usize,

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

/// Start the actix-web server, open the configured layers, and serve the API +
/// static files.
#[actix_web::main]
async fn main() -> std::io::Result<()> {
    env_logger::init_from_env(env_logger::Env::default().default_filter_or("info"));

    let cli = Cli::parse();

    let access_key = cli
        .access_key
        .clone()
        .or_else(|| std::env::var("AWS_ACCESS_KEY_ID").ok())
        .unwrap_or_default();
    let secret_key = cli
        .secret_key
        .clone()
        .or_else(|| std::env::var("AWS_SECRET_ACCESS_KEY").ok())
        .unwrap_or_default();

    // The CLI's S3 flags are the `default` profile. Sources that name another
    // profile need a config file, which is a later phase; until then `default`
    // is the only one there is, and it is enough to open `s3://…` layers with.
    let registry = SourceRegistry::new().with_profile(
        "default",
        S3Profile {
            endpoint: cli.endpoint.clone(),
            region: cli.region.clone(),
            access_key: access_key.clone(),
            secret_key: secret_key.clone(),
        },
    );

    let s3_config = cli.bucket.as_ref().map(|bucket| S3Config {
        bucket: bucket.clone(),
        endpoint: cli.endpoint.clone(),
        region: cli.region.clone(),
        access_key,
        secret_key,
        prefix: cli.prefix.clone(),
    });

    let mut session = Session::new();
    if let Some(path) = &cli.project {
        let project = if path.is_dir() {
            Project::scan(path).expect("scanning the project directory")
        } else {
            Project::read(path).expect("reading the project file")
        };
        log::info!(
            "Opening project `{}` with {} layer(s)",
            project.name.clone().unwrap_or_default(),
            project.layers.len()
        );
        let opened = project
            .open(&registry, &mut session)
            .await
            .expect("opening the project");
        log::info!("Opened {opened} of {} layer(s)", project.layers.len());
    }
    if let Some(source) = &cli.store {
        log::info!("Opening store: {source}");
        let spec = SourceSpec::parse(source).expect("invalid --store");
        session
            .add(&registry, spec, LayerRole::Auto, None, ObjectSpace::default())
            .await
            .expect("Failed to open zarr store");
    }
    for entry in &cli.layers {
        let (source, role) = split_role(entry);
        log::info!("Opening layer: {source} ({role:?})");
        let spec = SourceSpec::parse(source).expect("invalid --layer");
        session
            .add(&registry, spec, role, None, ObjectSpace::default())
            .await
            .expect("Failed to open layer");
    }

    for layer in session.layers() {
        log::info!("Layer {} `{}` <- {}", layer.id, layer.name, layer.spec.uri());
        if let Some(store) = layer.data.store() {
            let info = store.metadata();
            if let Some(omero) = &info.metadata.omero {
                log::info!("  {} channels", omero.channels.len());
            }
            for (i, arr) in info.arrays.iter().enumerate() {
                log::info!("  Level {}: shape={:?}, dtype={}", i, arr.shape, arr.dtype);
            }
        }
    }
    if session.is_empty() {
        log::info!("No layers open; use the UI to select a dataset");
    }

    let ontology = cli.ontology.as_ref().map(|path| {
        std::sync::Arc::new(Ontology::read(path).expect("reading the ontology"))
    });

    let data = web::Data::new(AppState {
        session: RwLock::new(session),
        registry,
        cache: TileCache::new(cli.cache_mb),
        s3_config,
        ontology,
    });

    log::info!("Starting server at http://{}", cli.bind);

    HttpServer::new(move || {
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
            .service(Files::new("/", "./dist/").index_file("index.html"))
    })
    .bind(&cli.bind)?
    .run()
    .await
}

/// Split a `--layer` argument into its source and its role.
///
/// The role is a trailing `:image` / `:labels`, which cannot be confused with a
/// scheme because a scheme is followed by `//`.
fn split_role(entry: &str) -> (&str, LayerRole) {
    for suffix in [":image", ":labels", ":objects", ":points"] {
        if let Some(source) = entry.strip_suffix(suffix) {
            return (source, LayerRole::parse(Some(&suffix[1..])));
        }
    }
    (entry, LayerRole::Auto)
}
