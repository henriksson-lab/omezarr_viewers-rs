//! A synthetic image + label pair, written as OME-Zarr.
//!
//! Development data with a *known* answer: every blob's centre, radius and id
//! are computed here, so "is the label layer aligned with the image" is a
//! question that can be answered by looking, and "is this id the one under the
//! cursor" is a question that can be answered by arithmetic.
//!
//! Deliberately not random: the same call writes the same bytes, so a picture
//! that changed means the viewer changed.

use anyhow::{Context, Result};
use std::path::Path;
use std::sync::Arc;
use zarrs::array::{ArrayBuilder, DataType, FillValue};
use zarrs::array_subset::ArraySubset;
use zarrs::filesystem::FilesystemStore;
use zarrs::group::GroupBuilder;

/// One synthetic object: a sphere with an id and a brightness.
#[derive(Clone, Copy, Debug)]
pub struct Blob {
    pub z: f64,
    pub y: f64,
    pub x: f64,
    pub radius: f64,
    pub id: u32,
    pub intensity: f64,
}

/// The blobs a demo volume of `shape` = `(z, y, x)` holds.
///
/// Laid out on a lattice with a per-blob radius that varies with position, so
/// a size column has something to sort by and neighbouring ids differ.
pub fn blobs(shape: (u64, u64, u64), count_xy: u64) -> Vec<Blob> {
    let mut blobs = Vec::new();
    let mut id = 1u32;
    for iz in 0..shape.0.clamp(1, 4) {
        for iy in 0..count_xy {
            for ix in 0..count_xy {
                let z = (iz as f64 + 0.5) * shape.0 as f64 / shape.0.clamp(1, 4) as f64;
                let y = (iy as f64 + 0.5) * shape.1 as f64 / count_xy as f64;
                let x = (ix as f64 + 0.5) * shape.2 as f64 / count_xy as f64;
                let radius = 6.0 + ((ix * 7 + iy * 13 + iz * 3) % 11) as f64;
                blobs.push(Blob {
                    z,
                    y,
                    x,
                    radius,
                    id,
                    intensity: 0.35 + ((id % 7) as f64) / 10.0,
                });
                id += 1;
            }
        }
    }
    blobs
}

/// Write a two-level `(c, z, y, x)` `uint16` image: a background gradient plus
/// the blobs, one channel bright on the blobs and one on the background.
pub fn write_image(path: &Path, shape: (u64, u64, u64), blobs: &[Blob]) -> Result<()> {
    let store = Arc::new(FilesystemStore::new(path).context("create image store")?);
    let mut datasets = Vec::new();
    for (level, step) in [(0usize, 1u64), (1, 2), (2, 4)] {
        let level_shape = [2, shape.0, shape.1 / step, shape.2 / step];
        let values: Vec<u16> = (0..2)
            .flat_map(|c| {
                (0..level_shape[1]).flat_map(move |z| {
                    (0..level_shape[2]).flat_map(move |y| {
                        (0..level_shape[3]).map(move |x| {
                            let (fz, fy, fx) = (z as f64, (y * step) as f64, (x * step) as f64);
                            let inside = blobs.iter().find(|b| {
                                let dz = (b.z - fz) / (b.radius * 0.5);
                                let dy = (b.y - fy) / b.radius;
                                let dx = (b.x - fx) / b.radius;
                                dz * dz + dy * dy + dx * dx <= 1.0
                            });
                            let gradient = fx / shape.2 as f64 * 0.25 + fy / shape.1 as f64 * 0.1;
                            let value = match (c, inside) {
                                (0, Some(blob)) => blob.intensity + 0.4,
                                (0, None) => gradient * 0.3,
                                (_, Some(_)) => gradient * 0.2,
                                (_, None) => gradient,
                            };
                            (value.clamp(0.0, 1.0) * 4000.0) as u16
                        })
                    })
                })
            })
            .collect();
        write_level(
            &store,
            &level.to_string(),
            DataType::UInt16,
            FillValue::from(0u16),
            &level_shape,
            &values,
        )?;
        datasets.push(dataset_entry(&level.to_string(), step));
    }

    let attributes = serde_json::json!({
        "multiscales": [{
            "name": "synthetic",
            "axes": axes(),
            "datasets": datasets,
        }],
        "omero": {
            "channels": [
                {"active": true, "label": "blobs", "color": "00FF00",
                 "window": {"start": 0.0, "end": 4000.0, "min": 0.0, "max": 65535.0}},
                {"active": true, "label": "background", "color": "FF00FF",
                 "window": {"start": 0.0, "end": 1500.0, "min": 0.0, "max": 65535.0}},
            ],
        },
    });
    write_group(&store, attributes)
}

/// Write the label volume for the same blobs: one `uint32` id per blob, at
/// half the image's resolution, so the viewer's cross-resolution overlay is
/// exercised rather than assumed.
pub fn write_labels(path: &Path, shape: (u64, u64, u64), blobs: &[Blob]) -> Result<()> {
    let store = Arc::new(FilesystemStore::new(path).context("create label store")?);
    let mut datasets = Vec::new();
    for (level, step) in [(0usize, 2u64), (1, 4)] {
        let level_shape = [1, shape.0, shape.1 / step, shape.2 / step];
        let values: Vec<u32> = (0..level_shape[1])
            .flat_map(|z| {
                (0..level_shape[2]).flat_map(move |y| {
                    (0..level_shape[3]).map(move |x| {
                        let (fz, fy, fx) = (z as f64, (y * step) as f64, (x * step) as f64);
                        blobs
                            .iter()
                            .find(|b| {
                                let dz = (b.z - fz) / (b.radius * 0.5);
                                let dy = (b.y - fy) / b.radius;
                                let dx = (b.x - fx) / b.radius;
                                dz * dz + dy * dy + dx * dx <= 1.0
                            })
                            .map(|b| b.id)
                            .unwrap_or(0)
                    })
                })
            })
            .collect();
        write_level(
            &store,
            &level.to_string(),
            DataType::UInt32,
            FillValue::from(0u32),
            &level_shape,
            &values,
        )?;
        datasets.push(dataset_entry(&level.to_string(), step));
    }

    // A colour table for the first few ids only: everything above falls back
    // to the hash colouring, which is the case a sparse table has to exercise.
    let colors: Vec<serde_json::Value> = blobs
        .iter()
        .take(3)
        .enumerate()
        .map(|(i, blob)| {
            let rgba = [[255, 64, 64, 255], [64, 255, 64, 255], [64, 128, 255, 255]][i % 3];
            serde_json::json!({"label-value": blob.id, "rgba": rgba})
        })
        .collect();

    let attributes = serde_json::json!({
        "multiscales": [{
            "name": "synthetic-labels",
            "axes": axes(),
            "datasets": datasets,
        }],
        "image-label": {
            "version": "0.4",
            "colors": colors,
            "source": {"image": "../image.zarr"},
        },
    });
    write_group(&store, attributes)
}

/// Write both stores under `root`, as `image.zarr` and `labels.zarr`.
pub fn write_demo(root: &Path, shape: (u64, u64, u64)) -> Result<Vec<Blob>> {
    std::fs::create_dir_all(root).context("create demo root")?;
    let blobs = blobs(shape, 6);
    write_image(&root.join("image.zarr"), shape, &blobs)?;
    write_labels(&root.join("labels.zarr"), shape, &blobs)?;
    Ok(blobs)
}

fn axes() -> serde_json::Value {
    serde_json::json!([
        {"name": "c", "type": "channel"},
        {"name": "z", "type": "space", "unit": "micrometer"},
        {"name": "y", "type": "space", "unit": "micrometer"},
        {"name": "x", "type": "space", "unit": "micrometer"},
    ])
}

fn dataset_entry(path: &str, step: u64) -> serde_json::Value {
    serde_json::json!({
        "path": path,
        "coordinateTransformations": [
            {"type": "scale", "scale": [1.0, 1.0, step as f64, step as f64]}
        ],
    })
}

fn write_group(store: &Arc<FilesystemStore>, attributes: serde_json::Value) -> Result<()> {
    let group = GroupBuilder::new()
        .attributes(attributes.as_object().unwrap().clone())
        .build(store.clone(), "/")
        .context("build group")?;
    group.store_metadata().context("store group metadata")?;
    Ok(())
}

fn write_level<T: zarrs::array::Element>(
    store: &Arc<FilesystemStore>,
    path: &str,
    data_type: DataType,
    fill: FillValue,
    shape: &[u64; 4],
    values: &[T],
) -> Result<()> {
    let chunk = vec![1, 1, shape[2].clamp(1, 128), shape[3].clamp(1, 128)];
    let array = ArrayBuilder::new(
        shape.to_vec(),
        data_type,
        chunk.try_into().map_err(|e| anyhow::anyhow!("{e:?}"))?,
        fill,
    )
    .build(store.clone(), &format!("/{path}"))
    .context("build array")?;
    array.store_metadata().context("store array metadata")?;
    array
        .store_array_subset_elements(&ArraySubset::new_with_shape(shape.to_vec()), values)
        .context("store array elements")?;
    Ok(())
}

/// Write the same blobs as object tables, one per reader.
///
/// Three files with the *same* rows, so a mismatch between the readers shows
/// up as points in different places rather than as a passing test:
///
/// * `cells.csv` — the shape `blockflow::yolo` writes, plus z and size;
/// * `cells.npy` — a structured array, the shape ClearMap's cell tables have;
/// * `cells.blob` — a `blockflow::table` blob, the shape `model_segment` writes.
pub fn write_objects(root: &Path, blobs: &[Blob]) -> Result<()> {
    std::fs::create_dir_all(root).context("create demo root")?;

    let mut csv = String::from("id,x,y,z,size,intensity\n");
    for blob in blobs {
        let size = (4.0 / 3.0 * std::f64::consts::PI * blob.radius.powi(3)).round();
        csv.push_str(&format!(
            "{},{},{},{},{},{:.3}\n",
            blob.id, blob.x, blob.y, blob.z, size, blob.intensity
        ));
    }
    std::fs::write(root.join("cells.csv"), csv).context("write cells.csv")?;

    // A structured `.npy`: x, y, z as u16 and size as u32, C-ordered.
    let mut rows = Vec::with_capacity(blobs.len() * 10);
    for blob in blobs {
        rows.extend_from_slice(&(blob.x as u16).to_le_bytes());
        rows.extend_from_slice(&(blob.y as u16).to_le_bytes());
        rows.extend_from_slice(&(blob.z as u16).to_le_bytes());
        let size = (4.0 / 3.0 * std::f64::consts::PI * blob.radius.powi(3)) as u32;
        rows.extend_from_slice(&size.to_le_bytes());
    }
    let descr = "[('x', '<u2'), ('y', '<u2'), ('z', '<u2'), ('size', '<u4')]";
    let dict = format!(
        "{{'descr': {descr}, 'fortran_order': False, 'shape': ({},), }}",
        blobs.len()
    );
    let mut header = dict.into_bytes();
    while !(10 + header.len() + 1).is_multiple_of(64) {
        header.push(b' ');
    }
    header.push(b'\n');
    let mut npy = Vec::new();
    npy.extend_from_slice(b"\x93NUMPY\x01\x00");
    npy.extend_from_slice(&(header.len() as u16).to_le_bytes());
    npy.extend_from_slice(&header);
    npy.extend_from_slice(&rows);
    std::fs::write(root.join("cells.npy"), npy).context("write cells.npy")?;

    // A blockflow table blob: `id` and `count` as u64, `intensity` as f64 bits.
    let magic = u64::from_be_bytes(*b"BFTABLE\0");
    let columns: [(&str, u64); 3] = [("id", 1), ("count", 1), ("intensity", 2)];
    let mut words = vec![magic, 1, columns.len() as u64, blobs.len() as u64];
    for (name, code) in columns {
        words.push(code);
        words.push(name.len() as u64);
        for chunk in name.as_bytes().chunks(8) {
            let mut padded = [0u8; 8];
            padded[..chunk.len()].copy_from_slice(chunk);
            words.push(u64::from_le_bytes(padded));
        }
    }
    for blob in blobs {
        let count = (4.0 / 3.0 * std::f64::consts::PI * blob.radius.powi(3)) as u64;
        words.extend_from_slice(&[
            blob.z as u64,
            blob.y as u64,
            blob.x as u64,
            blob.id as u64,
            count,
            blob.intensity.to_bits(),
        ]);
    }
    let blob_bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    std::fs::write(root.join("cells.blob"), blob_bytes).context("write cells.blob")?;

    Ok(())
}
