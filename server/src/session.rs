//! The session: an ordered list of layers, bottom to top.
//!
//! The viewer used to hold one store and one dataset. What a run produces is
//! several things over the same coordinates — an intensity image, a label
//! volume, a mask, a set of detected cells — so the unit the server holds is a
//! *list* of layers, each with its own source, and a tile request names which
//! one it is asking about.
//!
//! Layer ids are assigned here and are stable for the life of the session: the
//! client uses them as cache keys and as `layer=` in every request.

use anyhow::{Context, Result};
use omezarr_viewer_common::{LayerInfo, LayerKind, SessionInfo};
use std::sync::Arc;

use crate::npy_volume::NpyVolume;
use crate::objects::{self, ObjectSpace, ObjectStore};
use crate::source::{SourceRegistry, SourceSpec};
use crate::volume::Volume;
use crate::zarr_reader::ZarrStore;

/// What kind of thing a layer was opened as.
///
/// The request is separate from the answer: a caller says "open this as
/// labels", and what comes back is a [`Layer`] whose data proves it could be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayerRole {
    Image,
    Labels,
    /// A table of objects: cells, detections, instances.
    Objects,
    /// Work out what it is from the source: a `.npy` is a volume, a `.csv` is
    /// objects, a zarr store with `image-label` metadata is labels.
    Auto,
}

impl LayerRole {
    pub fn parse(value: Option<&str>) -> Self {
        match value {
            Some("image") => LayerRole::Image,
            Some("labels") => LayerRole::Labels,
            Some("objects") | Some("points") => LayerRole::Objects,
            _ => LayerRole::Auto,
        }
    }
}

/// The opened backing of a layer.
pub enum LayerData {
    Image(Volume),
    Labels {
        store: Volume,
        colors: Option<Vec<omezarr_viewer_common::LabelColor>>,
    },
    Objects(Arc<ObjectStore>),
}

impl LayerData {
    /// The pixels behind this layer, for the kinds that have any.
    pub fn store(&self) -> Option<&Volume> {
        match self {
            LayerData::Image(store) => Some(store),
            LayerData::Labels { store, .. } => Some(store),
            LayerData::Objects(_) => None,
        }
    }

    /// The object table behind this layer, for the kind that has one.
    pub fn objects(&self) -> Option<&Arc<ObjectStore>> {
        match self {
            LayerData::Objects(store) => Some(store),
            _ => None,
        }
    }
}

/// One layer in the session.
pub struct Layer {
    pub id: String,
    pub name: String,
    pub spec: SourceSpec,
    pub data: LayerData,
}

impl Layer {
    /// What this layer was opened as, in the vocabulary `--layer` and
    /// `/api/layers` use.
    pub fn role(&self) -> &'static str {
        match &self.data {
            LayerData::Image(_) => "image",
            LayerData::Labels { .. } => "labels",
            LayerData::Objects(_) => "objects",
        }
    }

    /// An object layer's scale, in the `z,y,x` form a project file carries.
    pub fn object_scale(&self) -> Option<String> {
        let store = self.data.objects()?;
        let scale = store.space().scale;
        (scale != [1.0, 1.0, 1.0]).then(|| format!("{},{},{}", scale[0], scale[1], scale[2]))
    }

    /// The wire form for `/api/session`.
    pub fn info(&self) -> LayerInfo {
        let kind = match &self.data {
            LayerData::Image(store) => LayerKind::Image {
                dataset: store.metadata().clone(),
            },
            LayerData::Labels { store, colors } => LayerKind::Labels {
                dataset: store.metadata().clone(),
                colors: colors.clone(),
                properties: None,
            },
            LayerData::Objects(store) => LayerKind::Objects {
                schema: store.schema(),
                count: store.len() as u64,
            },
        };
        LayerInfo {
            id: self.id.clone(),
            name: self.name.clone(),
            source: self.spec.uri(),
            kind,
        }
    }
}

/// The layers currently open, in draw order.
#[derive(Default)]
pub struct Session {
    layers: Vec<Layer>,
    next_id: u64,
}

impl Session {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn info(&self) -> SessionInfo {
        SessionInfo {
            layers: self.layers.iter().map(Layer::info).collect(),
        }
    }

    pub fn layers(&self) -> &[Layer] {
        &self.layers
    }

    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }

    pub fn get(&self, id: &str) -> Option<&Layer> {
        self.layers.iter().find(|layer| layer.id == id)
    }

    /// The layer a request means when it names none: the first image layer, or
    /// failing that the first layer at all. This is what keeps the pre-session
    /// API (`/api/info`, `/api/tile` with no `layer=`) answering.
    pub fn default_layer(&self) -> Option<&Layer> {
        self.layers
            .iter()
            .find(|layer| matches!(layer.data, LayerData::Image(_)))
            .or_else(|| self.layers.first())
    }

    /// Resolve `layer=`: a named layer, or the default when unnamed.
    pub fn resolve(&self, id: Option<&str>) -> Option<&Layer> {
        match id {
            Some(id) if !id.is_empty() => self.get(id),
            _ => self.default_layer(),
        }
    }

    /// Drop every layer. The tile cache is keyed by layer id, so a caller that
    /// clears the session clears the cache too.
    pub fn clear(&mut self) {
        self.layers.clear();
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.layers.len();
        self.layers.retain(|layer| layer.id != id);
        self.layers.len() != before
    }

    /// Open a source and append it as a layer.
    ///
    /// What the source *is* decides the layer kind: a table becomes objects, a
    /// `.npy` becomes a volume, a zarr store with `image-label` metadata
    /// becomes labels, and everything else is an image. `role` overrides the
    /// guess where the file cannot say for itself.
    pub async fn add(
        &mut self,
        registry: &SourceRegistry,
        spec: SourceSpec,
        role: LayerRole,
        name: Option<String>,
        space: ObjectSpace,
    ) -> Result<String> {
        // An object table is not a zarr store, so it is answered here rather
        // than by `zarrs` failing to find multiscales metadata in a CSV.
        //
        // `.npy` is the interesting case: `clearmap-ng` writes masks *and* cell
        // tables under that extension, so the file's own header decides, not
        // its name. Reading the header is a kilobyte, and a range request over
        // S3 rather than the whole object.
        let extension = spec.extension().unwrap_or_default();
        let object_source = match role {
            LayerRole::Objects => true,
            LayerRole::Image | LayerRole::Labels => false,
            LayerRole::Auto => match extension.as_str() {
                "csv" | "tsv" | "blob" | "bin" | "table" => true,
                "npy" => {
                    let header = crate::source::read_bytes(registry, &spec, Some(4096)).await?;
                    matches!(
                        crate::npy_volume::classify(&header)?,
                        crate::npy_volume::NpyKind::Objects
                    )
                }
                _ => false,
            },
        };
        if object_source {
            let store = objects::open(registry, &spec)
                .await
                .with_context(|| format!("opening {}", spec.uri()))?
                .with_space(space);
            log::info!(
                "{} holds {} object(s) with {} column(s)",
                spec.uri(),
                store.len(),
                store.columns().len()
            );
            return Ok(self.push(spec, LayerData::Objects(Arc::new(store)), name));
        }

        // A `.npy` is not a zarr store; it is a flat array, and it is the only
        // volume form `clearmap-ng` writes today (PLAN.md §3).
        let store = if spec.extension().is_some_and(|ext| ext == "npy") {
            Volume::Npy(Arc::new(
                NpyVolume::open(registry, &spec)
                    .await
                    .with_context(|| format!("opening {}", spec.uri()))?,
            ))
        } else {
            Volume::Zarr(Arc::new(
                ZarrStore::open_spec(registry, &spec)
                    .await
                    .with_context(|| format!("opening {}", spec.uri()))?,
            ))
        };
        let labelled = matches!(role, LayerRole::Labels)
            || (matches!(role, LayerRole::Auto) && has_image_label(&store));
        let data = if labelled {
            LayerData::Labels {
                colors: label_colors(&store),
                store,
            }
        } else {
            LayerData::Image(store)
        };
        Ok(self.push(spec, data, name))
    }

    /// Append an already-opened layer, assigning it an id.
    pub fn push(&mut self, spec: SourceSpec, data: LayerData, name: Option<String>) -> String {
        let id = format!("L{}", self.next_id);
        self.next_id += 1;
        let name = name.unwrap_or_else(|| spec.short_name());
        self.layers.push(Layer {
            id: id.clone(),
            name,
            spec,
            data,
        });
        id
    }
}

/// Does this store declare itself an OME-NGFF label image?
fn has_image_label(store: &Volume) -> bool {
    let Some(attrs) = store.attributes() else {
        return false;
    };
    attrs.contains_key("image-label")
        || attrs
            .get("ome")
            .and_then(|ome| ome.get("image-label"))
            .is_some()
}

/// The `image-label` colour table, when the store carries one.
fn label_colors(store: &Volume) -> Option<Vec<omezarr_viewer_common::LabelColor>> {
    let attrs = store.attributes()?;
    let label = attrs
        .get("image-label")
        .or_else(|| attrs.get("ome").and_then(|ome| ome.get("image-label")))?;
    serde_json::from_value(label.get("colors")?.clone()).ok()
}
