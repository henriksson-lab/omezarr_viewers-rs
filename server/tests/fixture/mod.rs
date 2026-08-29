//! A synthetic OME-Zarr store, written to a temp directory.
//!
//! The tests need a dataset whose every pixel value is *known*, so that a tile
//! read can be checked against arithmetic rather than against another read of
//! the same bytes. `value_at` is that arithmetic, and it is the only place the
//! fixture's content is defined.

use std::sync::Arc;

use tempfile::TempDir;
use zarrs::array::{ArrayBuilder, DataType, FillValue};
use zarrs::filesystem::FilesystemStore;
use zarrs::group::GroupBuilder;

/// The value the fixture holds at `(c, z, y, x)`.
///
/// Distinct in every axis, so a tile read that transposes two of them produces
/// different numbers rather than the same ones in a different order — and
/// small enough that the whole range fits an `int16`, so one function
/// describes the fixture in every dtype but `uint8`, which truncates.
pub fn value_at(c: u64, z: u64, y: u64, x: u64) -> u64 {
    x + 20 * y + 400 * z + 8000 * c
}

/// The offset [`write_wide`] adds: past `2^24`, where `f32` stops being exact
/// over the integers.
pub const WIDE: u64 = 1 << 24;

/// The value a wide fixture holds at `(c, z, y, x)`.
pub fn wide_value_at(c: u64, z: u64, y: u64, x: u64) -> u64 {
    WIDE + value_at(c, z, y, x)
}

/// A written fixture, kept alive by its temp directory.
pub struct Fixture {
    pub dir: TempDir,
}

impl Fixture {
    pub fn path(&self) -> &std::path::Path {
        self.dir.path()
    }
}

/// Write a two-level `(c, z, y, x)` OME-Zarr store in `dtype`.
///
/// Level 1 is a plain 2× decimation of level 0 in y and x — sampled, not
/// averaged, so `value_at` describes it too.
pub fn write(dtype: &str, shape: [u64; 4]) -> Fixture {
    write_offset(dtype, shape, 0)
}

/// A `uint32` fixture whose ids are past `2^24`, for the label path.
pub fn write_wide(shape: [u64; 4]) -> Fixture {
    write_offset("uint32", shape, WIDE)
}

fn write_offset(dtype: &str, shape: [u64; 4], offset: u64) -> Fixture {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = Arc::new(FilesystemStore::new(dir.path()).expect("filesystem store"));

    let datasets = [("0", 1u64), ("1", 2u64)];
    let mut dataset_meta = Vec::new();
    for (path, step) in datasets {
        let level_shape = [shape[0], shape[1], shape[2] / step, shape[3] / step];
        write_level(&store, path, dtype, level_shape, step, offset);
        dataset_meta.push(serde_json::json!({
            "path": path,
            "coordinateTransformations": [
                {"type": "scale", "scale": [1.0, 1.0, step as f64, step as f64]}
            ],
        }));
    }

    let attributes = serde_json::json!({
        "multiscales": [{
            "name": "fixture",
            "axes": [
                {"name": "c", "type": "channel"},
                {"name": "z", "type": "space"},
                {"name": "y", "type": "space"},
                {"name": "x", "type": "space"},
            ],
            "datasets": dataset_meta,
        }],
        "omero": {
            "channels": (0..shape[0]).map(|c| serde_json::json!({
                "active": true,
                "label": format!("ch{c}"),
                "color": "FFFFFF",
                "window": {"start": 0.0, "end": 255.0, "min": 0.0, "max": 255.0},
            })).collect::<Vec<_>>(),
        },
    });

    let group = GroupBuilder::new()
        .attributes(attributes.as_object().unwrap().clone())
        .build(store.clone(), "/")
        .expect("group");
    group.store_metadata().expect("group metadata");

    Fixture { dir }
}

/// Add an OME-NGFF `image-label` block to a written fixture, making it a label
/// image as far as auto-detection is concerned.
pub fn mark_as_labels(fixture: &Fixture) {
    let path = fixture.path().join("zarr.json");
    let text = std::fs::read_to_string(&path).expect("group metadata");
    let mut json: serde_json::Value = serde_json::from_str(&text).expect("group json");
    json["attributes"]["image-label"] = serde_json::json!({
        "version": "0.4",
        "colors": [
            {"label-value": 1.0, "rgba": [255, 0, 0, 255]},
            {"label-value": 2.0, "rgba": [0, 255, 0, 255]},
        ],
    });
    std::fs::write(&path, serde_json::to_string(&json).unwrap()).expect("write group metadata");
}

fn write_level(
    store: &Arc<FilesystemStore>,
    path: &str,
    dtype: &str,
    shape: [u64; 4],
    step: u64,
    offset: u64,
) {
    let (data_type, fill) = match dtype {
        "uint8" => (DataType::UInt8, FillValue::from(0u8)),
        "uint16" => (DataType::UInt16, FillValue::from(0u16)),
        "uint32" => (DataType::UInt32, FillValue::from(0u32)),
        "uint64" => (DataType::UInt64, FillValue::from(0u64)),
        "int16" => (DataType::Int16, FillValue::from(0i16)),
        "int32" => (DataType::Int32, FillValue::from(0i32)),
        "float32" => (DataType::Float32, FillValue::from(0.0f32)),
        "float64" => (DataType::Float64, FillValue::from(0.0f64)),
        other => panic!("fixture has no writer for dtype {other}"),
    };
    let chunk = vec![1, 1, shape[2].min(8), shape[3].min(8)];
    let array = ArrayBuilder::new(shape.to_vec(), data_type, chunk.try_into().unwrap(), fill)
        .build(store.clone(), &format!("/{path}"))
        .expect("array");
    array.store_metadata().expect("array metadata");

    let subset = zarrs::array_subset::ArraySubset::new_with_shape(shape.to_vec());
    let values: Vec<u64> = (0..shape[0])
        .flat_map(|c| {
            (0..shape[1]).flat_map(move |z| {
                (0..shape[2]).flat_map(move |y| {
                    (0..shape[3]).map(move |x| offset + value_at(c, z, y * step, x * step))
                })
            })
        })
        .collect();

    match dtype {
        "uint8" => store_elements(&array, &subset, values.iter().map(|&v| v as u8).collect()),
        "uint16" => store_elements(&array, &subset, values.iter().map(|&v| v as u16).collect()),
        "uint32" => store_elements(&array, &subset, values.iter().map(|&v| v as u32).collect()),
        "uint64" => store_elements(&array, &subset, values.clone()),
        "int16" => store_elements(&array, &subset, values.iter().map(|&v| v as i16).collect()),
        "int32" => store_elements(&array, &subset, values.iter().map(|&v| v as i32).collect()),
        "float32" => store_elements(&array, &subset, values.iter().map(|&v| v as f32).collect()),
        "float64" => store_elements(&array, &subset, values.iter().map(|&v| v as f64).collect()),
        other => panic!("fixture has no writer for dtype {other}"),
    }
}

fn store_elements<T: zarrs::array::Element>(
    array: &zarrs::array::Array<FilesystemStore>,
    subset: &zarrs::array_subset::ArraySubset,
    values: Vec<T>,
) {
    array
        .store_array_subset_elements(subset, &values)
        .expect("store elements");
}
