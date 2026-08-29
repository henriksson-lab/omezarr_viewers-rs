//! `.npy` to OME-Zarr — the answer to "why is my mask slow over S3".

use clap::Parser;

#[derive(Parser)]
#[command(
    name = "omezarr-convert",
    about = "Convert a .npy volume into a chunked, pyramidal OME-Zarr store"
)]
struct Cli {
    /// The `.npy` to read.
    input: std::path::PathBuf,
    /// The store to write. Must not exist.
    output: std::path::PathBuf,
    /// How coarser levels are derived: `mean` for intensity, `nearest` for
    /// labels. Defaults by dtype — wide integers are sampled, the rest averaged.
    #[arg(long)]
    reduce: Option<String>,
    /// The most pyramid levels to write.
    #[arg(long, default_value_t = 8)]
    levels: usize,
    /// Chunk size in y and x.
    #[arg(long, default_value_t = 256)]
    chunk: u64,
}

fn main() -> anyhow::Result<()> {
    env_logger::init_from_env(env_logger::Env::default().default_filter_or("info"));
    let cli = Cli::parse();
    let reduce = omezarr_viewer_server::convert::Reduce::parse(cli.reduce.as_deref());
    let shapes = omezarr_viewer_server::convert::npy_to_zarr(
        &cli.input,
        &cli.output,
        reduce,
        cli.levels,
        cli.chunk,
    )?;
    println!("wrote {} to {}", cli.input.display(), cli.output.display());
    for (level, shape) in shapes.iter().enumerate() {
        println!("  level {level}: {shape:?}");
    }
    Ok(())
}
