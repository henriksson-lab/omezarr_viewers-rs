//! What both volume readers do to a buffer of decoded pixels.
//!
//! `project` and `f32_bytes` were byte-for-byte identical in `zarr_reader` and
//! `npy_volume` — 38 lines of the same arithmetic in two files. The readers
//! genuinely differ (one walks zarr chunks, the other a memory-mapped `.npy`),
//! but by the time they hold `&[f32]` they are doing the same thing, and the
//! z-projection is a rule worth having in one place: a max over a set of planes
//! that drifted between the two would show as one layer kind projecting
//! differently from another.

use crate::zarr_reader::Projection;

/// Reduce `planes` consecutive planes of `plane` pixels each to one.
///
/// Returns the input untouched when there is nothing to project — a single
/// plane, or a zero-sized one — so the caller does not have to special-case it.
pub fn project(pixels: &[f32], plane: usize, planes: u64, projection: Projection) -> Vec<f32> {
    let planes = planes as usize;
    if plane == 0 || planes <= 1 {
        return pixels.to_vec();
    }
    let mut out = vec![
        match projection {
            Projection::Max => f32::NEG_INFINITY,
            Projection::Mean => 0.0,
        };
        plane
    ];
    for index in 0..planes {
        let at = index * plane;
        // A short final plane means the volume ended; take what there is.
        let Some(slice) = pixels.get(at..at + plane) else {
            break;
        };
        for (accumulator, value) in out.iter_mut().zip(slice) {
            match projection {
                Projection::Max => *accumulator = accumulator.max(*value),
                Projection::Mean => *accumulator += *value,
            }
        }
    }
    if projection == Projection::Mean {
        for value in out.iter_mut() {
            *value /= planes as f32;
        }
    }
    out
}

/// Little-endian `f32` bytes, the wire form of every intensity answer.
pub fn f32_bytes(pixels: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(pixels.len() * 4);
    for value in pixels {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}
