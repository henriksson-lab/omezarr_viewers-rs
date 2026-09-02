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

use crate::annotations::roi_table::classes::LabelClasses;
use crate::annotations::AnnotationSet;
use crate::npy_volume::NpyVolume;
use crate::objects::{self, ObjectSpace, ObjectStore};
use crate::source::{SourceRegistry, SourceSpec};
use crate::volume::Volume;
use crate::zarr_reader::ZarrStore;

/// How many rows of a table travel with the session.
///
/// A feature table has a row per segmented object and there can be a hundred
/// thousand of them; the rest is paged rather than pushed into every client on
/// every session read.
pub const PREVIEW_ROWS: usize = 200;

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
    /// Boxes and points drawn here — the one layer kind the viewer writes.
    Annotations,
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
            Some("annotations") | Some("roi") => LayerRole::Annotations,
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
        properties: Option<Vec<omezarr_viewer_common::LabelProperty>>,
        /// What each id *is*, when a curator has said. The label image itself
        /// stays untouched — this is an assertion about somebody else's raster,
        /// not an edit of it, which is why it lives here and saves to a table
        /// beside the labels rather than into them.
        classes: LabelClasses,
    },
    Objects(Arc<ObjectStore>),
    /// Mutable, unlike every other kind: this is the one a click edits.
    Annotations(AnnotationSet),
    /// Rows with no geometry of their own — a feature or condition table.
    Table(Box<crate::annotations::roi_table::RoiTable>),
}

impl LayerData {
    /// The pixels behind this layer, for the kinds that have any.
    pub fn store(&self) -> Option<&Volume> {
        match self {
            LayerData::Image(store) => Some(store),
            LayerData::Labels { store, .. } => Some(store),
            LayerData::Objects(_) | LayerData::Annotations(_) | LayerData::Table(_) => None,
        }
    }

    /// The object table behind this layer, for the kind that has one.
    pub fn objects(&self) -> Option<&Arc<ObjectStore>> {
        match self {
            LayerData::Objects(store) => Some(store),
            _ => None,
        }
    }

    /// The annotations behind this layer, for the kind that has any.
    pub fn annotations(&self) -> Option<&AnnotationSet> {
        match self {
            LayerData::Annotations(set) => Some(set),
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
    /// Whether a viewer should draw this on arrival. See [`LayerInfo::visible`].
    pub visible: bool,
}

impl Layer {
    /// What this layer was opened as, in the vocabulary `--layer` and
    /// `/api/layers` use.
    pub fn role(&self) -> &'static str {
        match &self.data {
            LayerData::Image(_) => "image",
            LayerData::Labels { .. } => "labels",
            LayerData::Objects(_) => "objects",
            LayerData::Annotations(_) => "annotations",
            LayerData::Table(_) => "table",
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
            LayerData::Labels {
                store,
                colors,
                properties,
                ..
            } => LayerKind::Labels {
                dataset: store.metadata().clone(),
                colors: colors.clone(),
                properties: properties.clone(),
            },
            LayerData::Objects(store) => LayerKind::Objects {
                schema: store.schema(),
                count: store.len() as u64,
            },
            LayerData::Annotations(set) => LayerKind::Annotations {
                annotations: set.items().to_vec(),
                target: set.target().map(str::to_string),
            },
            LayerData::Table(table) => LayerKind::Table {
                table: table.info(PREVIEW_ROWS),
            },
        };
        LayerInfo {
            id: self.id.clone(),
            name: self.name.clone(),
            source: self.spec.uri(),
            visible: self.visible,
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

    /// The table behind one layer, for the kind that is one.
    pub fn table(&self, id: &str) -> Option<&crate::annotations::roi_table::RoiTable> {
        match &self.get(id)?.data {
            LayerData::Table(table) => Some(table),
            _ => None,
        }
    }

    /// What a label layer's ids have been classed as.
    pub fn label_classes(&self, id: &str) -> Option<&LabelClasses> {
        match &self.get(id)?.data {
            LayerData::Labels { classes, .. } => Some(classes),
            _ => None,
        }
    }

    /// The same, mutably. A label layer is otherwise read-only: this classes
    /// the ids in somebody else's raster without touching the raster.
    pub fn label_classes_mut(&mut self, id: &str) -> Option<&mut LabelClasses> {
        match &mut self.layers.iter_mut().find(|layer| layer.id == id)?.data {
            LayerData::Labels { classes, .. } => Some(classes),
            _ => None,
        }
    }

    /// The annotations of one layer, mutably — the only editable thing in a
    /// session, and the only reason `Session` is ever borrowed for writing
    /// outside `add`/`remove`.
    pub fn annotations_mut(&mut self, id: &str) -> Option<&mut AnnotationSet> {
        match &mut self.layers.iter_mut().find(|layer| layer.id == id)?.data {
            LayerData::Annotations(set) => Some(set),
            _ => None,
        }
    }

    /// The layer an annotation is drawn *over*: the first image layer, whose
    /// full-resolution pixels are the world every annotation is held in.
    pub fn reference_dataset(&self) -> Option<&omezarr_viewer_common::DatasetInfo> {
        self.default_layer()?.data.store().map(|s| s.metadata())
    }

    /// Open an annotation source: a GeoJSON set, a bare `.geojson` file, or an
    /// ngio ROI table.
    ///
    /// Which of the three is decided by the *shape of the path*, not by a flag:
    /// `<store>/annotations/<name>` and `<store>/tables/<name>` are the two
    /// conventions, and anything ending `.geojson` or `.json` is a file
    /// somebody exported. Asking the user to say which would be asking them to
    /// repeat what they already typed.
    async fn add_annotation_source(
        &mut self,
        registry: &SourceRegistry,
        spec: SourceSpec,
        name: Option<String>,
    ) -> Result<String> {
        use crate::annotations::{geojson, roi_table};
        let uri = spec.uri();

        if geojson::is_annotation_target(&uri) {
            let (read, set_name, target) = if geojson::target_is_remote(&uri) {
                let (store, set_name) = geojson::split_uri_target(&uri)?;
                let read = geojson::load_async(registry, &store, &set_name)
                    .await
                    .with_context(|| format!("reading annotations {uri}"))?;
                let target = geojson::make_uri_target(&store, &set_name);
                (read, set_name, target)
            } else {
                let (root, set_name) = geojson::split_target(&uri)?;
                let read = geojson::load(&root, &set_name)
                    .with_context(|| format!("reading annotations {uri}"))?;
                let target = geojson::make_target(&root, &set_name);
                (read, set_name, target)
            };
            log::info!("{uri} holds {} annotation(s)", read.rows.len());
            if !read.declared_space {
                log::warn!(
                    "{uri} declares no coordinate space; reading it as full-resolution pixels"
                );
            }
            let set = AnnotationSet::from_rows(read.rows, Some(target));
            return Ok(self.add_annotations(name.or(Some(set_name)), set));
        }

        if matches!(spec.extension().as_deref(), Some("geojson") | Some("json")) {
            let SourceSpec::File(path) = &spec else {
                anyhow::bail!("a GeoJSON file can only be opened from a local path");
            };
            let read =
                geojson::load_file(path).with_context(|| format!("reading annotations {uri}"))?;
            log::info!("{uri} holds {} annotation(s)", read.rows.len());
            let set = AnnotationSet::from_rows(read.rows, Some(path.display().to_string()));
            return Ok(self.add_annotations(name.or_else(|| Some(spec.short_name())), set));
        }

        let (read, table, target) = if roi_table::is_remote(&uri) {
            let (store, table) = roi_table::split_uri_target(&uri)?;
            let read = roi_table::read_async(registry, &store, &table)
                .await
                .with_context(|| format!("reading ROI table {uri}"))?;
            let target = roi_table::make_uri_target(&store, &table);
            (read, table, target)
        } else {
            let (root, table) = roi_table::split_target(&uri)?;
            let read = roi_table::read(&root, &table)
                .with_context(|| format!("reading ROI table {uri}"))?;
            let target = roi_table::make_target(&root, &table);
            (read, table, target)
        };
        if !read.is_geometry() {
            // A feature table is per-object measurements keyed to a label image
            // and a condition table is experiment metadata; neither has
            // anywhere to be drawn. Opening one as an empty annotation layer
            // would be the wrong answer told quietly.
            log::info!(
                "{uri} is a {} with {} row(s) and no geometry of its own",
                read.table_type,
                read.columns.row_count()
            );
            return Ok(self.push(spec, LayerData::Table(Box::new(read)), name.or(Some(table))));
        }
        log::info!(
            "{uri} holds {} annotation(s), read from its {} backend{}",
            read.rows.len(),
            read.backend,
            if read.from_obsm {
                " via obsm[\"spatial\"]"
            } else {
                ""
            }
        );
        let set = AnnotationSet::from_rows(read.rows, Some(target));
        Ok(self.add_annotations(name.or(Some(table)), set))
    }

    /// Append an empty annotation layer, ready to be drawn into.
    pub fn add_annotations(&mut self, name: Option<String>, set: AnnotationSet) -> String {
        let spec = match set.target() {
            Some(target) => SourceSpec::File(std::path::PathBuf::from(target)),
            None => SourceSpec::unsaved(),
        };
        let name = name.or_else(|| {
            set.target()
                .and_then(|t| t.rsplit(std::path::is_separator).next())
                .map(str::to_string)
        });
        self.push(spec, LayerData::Annotations(set), name)
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

    /// Open a source and append it as one or more layers.
    ///
    /// What the source *is* decides the layer kind: a table becomes objects, a
    /// `.npy` becomes a volume, a zarr store with `image-label` metadata
    /// becomes labels, and everything else is an image. `role` overrides the
    /// guess where the file cannot say for itself.
    ///
    /// **More than one layer** when the source is a `bioformats2raw` container:
    /// its root holds no pixels, and each numbered subgroup is an image. They
    /// are expanded here rather than inside `ZarrStore` so that a layer's spec
    /// is the *series* — which is what decides where its annotations are
    /// written, and a coordinate space declared at a container root would be a
    /// claim about pixels that are not there.
    pub async fn add(
        &mut self,
        registry: &SourceRegistry,
        spec: SourceSpec,
        role: LayerRole,
        name: Option<String>,
        space: ObjectSpace,
    ) -> Result<Vec<String>> {
        // Only a zarr group can be a container, so the probe is skipped for
        // anything naming a file — a `.npy` volume, a `.csv` or a table blob.
        // `.zarr` itself is *not* such a name: it is the conventional suffix on
        // a store directory, and gating this on "has no extension" is how the
        // first version of this silently never ran.
        let probe = matches!(spec.extension().as_deref(), None | Some("zarr"));
        if probe {
            if let Some(series) = ZarrStore::series_of(registry, &spec)
                .await
                .with_context(|| format!("opening {}", spec.uri()))?
            {
                let mut ids = Vec::new();
                for (index, one) in series.iter().enumerate() {
                    // A container holding a single image is how every
                    // single-scene conversion comes out; there is nothing to
                    // disambiguate, so it keeps the store's own name.
                    let layer_name = match (&name, series.len()) {
                        (Some(given), 1) => given.clone(),
                        (Some(given), _) => format!("{given}[{one}]"),
                        (None, 1) => spec.short_name(),
                        (None, _) => format!("{}[{}]", spec.short_name(), one),
                    };
                    let opened = Box::pin(self.add(
                        registry,
                        spec.child(one),
                        role,
                        Some(layer_name),
                        space,
                    ))
                    .await?;
                    // Every series after the first arrives hidden. They are
                    // alternative scenes, not things to overlay: stacked image
                    // layers composite *additively*, so leaving them all on
                    // sums unrelated pictures into one that means nothing — and
                    // they do not even share a coordinate space, since the
                    // world is the first image's and a second scene is rarely
                    // the same size.
                    if index > 0 {
                        for id in &opened {
                            if let Some(layer) = self.layers.iter_mut().find(|l| &l.id == id) {
                                layer.visible = false;
                            }
                        }
                    }
                    ids.push(opened);
                }
                return Ok(ids.into_iter().flatten().collect());
            }
        }
        self.add_one(registry, spec, role, name, space).await
    }

    /// Open exactly one source as exactly one layer.
    async fn add_one(
        &mut self,
        registry: &SourceRegistry,
        spec: SourceSpec,
        role: LayerRole,
        name: Option<String>,
        space: ObjectSpace,
    ) -> Result<Vec<String>> {
        // An object table is not a zarr store, so it is answered here rather
        // than by `zarrs` failing to find multiscales metadata in a CSV.
        //
        // `.npy` is the interesting case: `clearmap-ng` writes masks *and* cell
        // tables under that extension, so the file's own header decides, not
        // its name. Reading the header is a kilobyte, and a range request over
        // S3 rather than the whole object.
        // An ROI table is a group inside a store, not a store: `zarrs` would
        // find no multiscales metadata under it and say so in the wrong words.
        if matches!(role, LayerRole::Annotations) {
            return Ok(vec![
                self.add_annotation_source(registry, spec, name).await?,
            ]);
        }

        let extension = spec.extension().unwrap_or_default();
        let object_source = match role {
            LayerRole::Objects => true,
            LayerRole::Image | LayerRole::Labels | LayerRole::Annotations => false,
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
            return Ok(vec![self.push(
                spec,
                LayerData::Objects(Arc::new(store)),
                name,
            )]);
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
                colors: image_label_field(&store, "colors"),
                properties: image_label_field(&store, "properties"),
                store,
                classes: LabelClasses::default(),
            }
        } else {
            LayerData::Image(store)
        };
        Ok(vec![self.push(spec, data, name)])
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
            // Shown unless something says otherwise; `add` hides the series
            // after the first when it expands a container.
            visible: true,
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

/// One array out of the store's `image-label` object, parsed.
///
/// Both `colors` and `properties` are optional and both are arrays of per-id
/// objects, so one accessor serves both; a store that declares neither, or
/// declares one this build cannot parse, gets `None` rather than an error —
/// a label image with no colour table is the common case, not a broken file.
fn image_label_field<T: serde::de::DeserializeOwned>(store: &Volume, field: &str) -> Option<T> {
    let attrs = store.attributes()?;
    let label = attrs
        .get("image-label")
        .or_else(|| attrs.get("ome").and_then(|ome| ome.get("image-label")))?;
    serde_json::from_value(label.get(field)?.clone()).ok()
}
