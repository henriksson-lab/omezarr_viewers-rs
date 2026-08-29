//! One handle over the two things a pixel layer can be.
//!
//! An enum rather than a trait object, because the set is closed and small and
//! because the methods are `async` — a `dyn` trait with async methods needs a
//! macro crate, and two variants need no dispatch machinery at all. Adding a
//! third backend means adding a variant here and the compiler naming every
//! place that has to answer for it.

use anyhow::Result;
use omezarr_viewer_common::DatasetInfo;
use std::sync::Arc;

use crate::npy_volume::NpyVolume;
use crate::zarr_reader::{PlaneAxis, PlaneBytes, PlaneRequest, TileBytes, TileRequest, ZarrStore};

/// A source of pixels: a multiscale zarr store, or a flat `.npy` array.
#[derive(Clone)]
pub enum Volume {
    Zarr(Arc<ZarrStore>),
    Npy(Arc<NpyVolume>),
}

impl Volume {
    pub fn metadata(&self) -> &DatasetInfo {
        match self {
            Volume::Zarr(store) => store.metadata(),
            Volume::Npy(volume) => volume.metadata(),
        }
    }

    pub fn level_dtype(&self, level: usize) -> Result<String> {
        match self {
            Volume::Zarr(store) => store.level_dtype(level),
            Volume::Npy(volume) => volume.level_dtype(level),
        }
    }

    pub fn axis_extent(&self, level: usize, name: &str) -> Result<u64> {
        match self {
            Volume::Zarr(store) => store.axis_extent(level, name),
            Volume::Npy(volume) => volume.axis_extent(level, name),
        }
    }

    pub fn plane_shape(&self, level: usize, axis: PlaneAxis) -> Result<(u64, u64)> {
        match self {
            Volume::Zarr(store) => store.plane_shape(level, axis),
            Volume::Npy(volume) => volume.plane_shape(level, axis),
        }
    }

    pub async fn read_tile_bytes(&self, request: &TileRequest) -> Result<TileBytes> {
        match self {
            Volume::Zarr(store) => store.read_tile_bytes(request).await,
            Volume::Npy(volume) => volume.read_tile_bytes(request),
        }
    }

    pub async fn read_plane(&self, request: &PlaneRequest) -> Result<PlaneBytes> {
        match self {
            Volume::Zarr(store) => store.read_plane(request).await,
            Volume::Npy(volume) => volume.read_plane(request),
        }
    }

    /// The zarr group attributes, for the questions only a store can answer.
    pub fn attributes(&self) -> Option<&serde_json::Map<String, serde_json::Value>> {
        match self {
            Volume::Zarr(store) => Some(store.attributes()),
            Volume::Npy(_) => None,
        }
    }
}
