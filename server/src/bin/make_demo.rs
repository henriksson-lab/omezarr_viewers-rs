//! Write a synthetic image + label dataset, for developing against without a
//! real acquisition on hand.

use clap::Parser;

#[derive(Parser)]
#[command(
    name = "make-demo",
    about = "Write a synthetic OME-Zarr image + labels"
)]
struct Cli {
    /// Directory to write `image.zarr` and `labels.zarr` into.
    out: std::path::PathBuf,
    #[arg(long, default_value_t = 8)]
    z: u64,
    #[arg(long, default_value_t = 512)]
    y: u64,
    #[arg(long, default_value_t = 512)]
    x: u64,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let blobs = omezarr_viewer_server::synthetic::write_demo(&cli.out, (cli.z, cli.y, cli.x))?;
    omezarr_viewer_server::synthetic::write_objects(&cli.out, &blobs)?;
    println!(
        "wrote {} blobs to {out}/image.zarr, {out}/labels.zarr, {out}/cells.csv, \
         {out}/cells.npy and {out}/cells.blob",
        blobs.len(),
        out = cli.out.display(),
    );
    for blob in blobs.iter().take(5) {
        println!(
            "  id {:>3} at z={:.0} y={:.0} x={:.0} r={:.0}",
            blob.id, blob.z, blob.y, blob.x, blob.radius
        );
    }
    Ok(())
}
