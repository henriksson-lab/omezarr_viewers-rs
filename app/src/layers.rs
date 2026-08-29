//! Per-layer UI and geometry state.
//!
//! The viewer used to hold one dataset and a list of channels. It now holds a
//! list of layers, each with its own pyramid, its own tile grid and its own
//! level choice — a label volume may be half the resolution of the image it
//! sits on, and both are drawn at once.
//!
//! The coordinate system every layer is drawn in is the **world**: the
//! reference layer's full-resolution x/y size. A layer's pixels are mapped into
//! it by a per-level scale, which is the only reason a coarser label volume
//! lands on top of the image rather than in a corner of it.

use omezarr_viewer_common::{DatasetInfo, LabelColor, LayerInfo, LayerKind, ObjectSchema};

use crate::controls::channel_panel;

/// UI state for a single channel: visibility, color, contrast, and opacity.
#[derive(Clone, PartialEq)]
pub struct ChannelUiState {
    pub label: String,
    pub visible: bool,
    pub color: [f32; 3],
    pub contrast_min: f32,
    pub contrast_max: f32,
    pub opacity: f32,
    /// True until the contrast is known.
    ///
    /// A store with OMERO metadata says how to display itself and this is false
    /// from the start. A `.npy` mask says nothing — and shown at `0..255` a
    /// volume of zeros and ones is black — so the first tile that arrives sets
    /// the range, once, and never again.
    pub auto_contrast: bool,
}

/// UI state for a label layer.
#[derive(Clone, PartialEq)]
pub struct LabelUiState {
    pub opacity: f32,
    pub outline: bool,
    /// The id under the last click; 0 means nothing is selected.
    pub selected: u32,
    pub only_selected: bool,
    /// `image-label` colours, when the store declared them.
    pub colors: Option<Vec<LabelColor>>,
}

impl Default for LabelUiState {
    fn default() -> Self {
        Self {
            opacity: 0.6,
            outline: false,
            selected: 0,
            only_selected: false,
            colors: None,
        }
    }
}

/// UI state for an object layer.
#[derive(Clone, PartialEq)]
pub struct ObjectUiState {
    pub schema: ObjectSchema,
    pub count: u64,
    pub color: [f32; 3],
    pub opacity: f32,
    /// Sprite diameter in screen pixels.
    pub size: f32,
    /// Rings rather than discs, so the pixels underneath stay visible.
    pub hollow: bool,
    /// Which column colours the points, if any.
    pub color_by: Option<usize>,
    /// Per-column `(min, max)` filter, when one is set.
    pub filters: Vec<Option<(f32, f32)>>,
    /// How far from the current z a point may be before it fades out. Zero for
    /// a set with no z, which is every 2D detector's.
    pub slab: f32,
    /// The row the last click selected.
    pub selected_row: Option<u32>,
    /// What the last fetch returned, and how much matched before the cap.
    pub loaded: usize,
    pub total: usize,
    /// Rows filtered out on the client, of `loaded`.
    pub shown: usize,
}

impl ObjectUiState {
    fn new(schema: ObjectSchema, count: u64) -> Self {
        let filters = vec![None; schema.columns.len()];
        let slab = if schema.has_z { 8.0 } else { 0.0 };
        Self {
            schema,
            count,
            color: [1.0, 0.85, 0.2],
            opacity: 0.9,
            size: 9.0,
            hollow: false,
            color_by: None,
            filters,
            slab,
            selected_row: None,
            loaded: 0,
            total: 0,
            shown: 0,
        }
    }
}

/// The rows one object layer currently has on the client.
///
/// Held whole rather than only as a GPU buffer so that filtering and
/// colour-by are instant: they rebuild the buffer from these arrays without
/// another round trip.
#[derive(Clone, Default, PartialEq)]
pub struct ObjectData {
    pub positions: Vec<[f32; 3]>,
    pub rows: Vec<u32>,
    /// One array per schema column, in schema order.
    pub columns: Vec<Vec<f32>>,
}

impl ObjectData {
    pub fn len(&self) -> usize {
        self.positions.len()
    }

    /// Build the interleaved `(z, y, x, value, row)` buffer the point shader
    /// reads, applying the layer's filters.
    pub fn to_vertices(&self, state: &ObjectUiState) -> (Vec<f32>, usize) {
        let mut out = Vec::with_capacity(self.len() * 5);
        let mut shown = 0;
        for row in 0..self.len() {
            if !self.passes(state, row) {
                continue;
            }
            let position = self.positions[row];
            out.extend_from_slice(&[position[0], position[1], position[2]]);
            let value = state
                .color_by
                .and_then(|column| self.columns.get(column))
                .and_then(|values| values.get(row))
                .copied()
                .unwrap_or(0.0);
            out.push(value);
            out.push(self.rows.get(row).copied().unwrap_or(row as u32) as f32);
            shown += 1;
        }
        (out, shown)
    }

    fn passes(&self, state: &ObjectUiState, row: usize) -> bool {
        for (column, filter) in state.filters.iter().enumerate() {
            let Some((lo, hi)) = filter else { continue };
            let Some(value) = self.columns.get(column).and_then(|v| v.get(row)) else {
                continue;
            };
            if value.is_nan() || *value < *lo || *value > *hi {
                return false;
            }
        }
        true
    }
}

/// What kind of layer this is, and the state that kind carries.
#[derive(Clone, PartialEq)]
pub enum LayerUi {
    Image {
        channels: Vec<ChannelUiState>,
        dtype_max: f32,
    },
    Labels(LabelUiState),
    Objects(ObjectUiState),
}

/// One layer as the frontend holds it.
#[derive(Clone, PartialEq)]
pub struct LayerState {
    /// The server's layer id — what `layer=` carries.
    pub id: String,
    pub name: String,
    pub visible: bool,
    /// The pyramid behind this layer — absent for an object layer, which has
    /// rows rather than pixels.
    pub dataset: Option<DatasetInfo>,
    pub ui: LayerUi,
}

/// A level's tile grid.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TileGrid {
    pub img_w: u64,
    pub img_h: u64,
    pub tile_w: u64,
    pub tile_h: u64,
    pub num_tiles_x: u32,
    pub num_tiles_y: u32,
}

impl LayerState {
    /// Build the frontend state for a layer the server reported.
    ///
    /// Returns `None` for kinds this build cannot draw yet, so an object layer
    /// added by a later phase does not have to be special-cased here first.
    pub fn from_info(info: &LayerInfo) -> Option<Self> {
        match &info.kind {
            LayerKind::Image { dataset } => {
                let dtype_max = dtype_max(dataset);
                Some(Self {
                    id: info.id.clone(),
                    name: info.name.clone(),
                    visible: true,
                    ui: LayerUi::Image {
                        channels: channels_from(dataset, dtype_max),
                        dtype_max,
                    },
                    dataset: Some(dataset.clone()),
                })
            }
            LayerKind::Labels {
                dataset, colors, ..
            } => Some(Self {
                id: info.id.clone(),
                name: info.name.clone(),
                visible: true,
                ui: LayerUi::Labels(LabelUiState {
                    colors: colors.clone(),
                    ..LabelUiState::default()
                }),
                dataset: Some(dataset.clone()),
            }),
            LayerKind::Objects { schema, count } => Some(Self {
                id: info.id.clone(),
                name: info.name.clone(),
                visible: true,
                dataset: None,
                ui: LayerUi::Objects(ObjectUiState::new(schema.clone(), *count)),
            }),
        }
    }

    pub fn is_labels(&self) -> bool {
        matches!(self.ui, LayerUi::Labels(_))
    }

    pub fn is_objects(&self) -> bool {
        matches!(self.ui, LayerUi::Objects(_))
    }

    /// The length of a named axis at level 0, or 1 when the axis is absent.
    pub fn axis_len(&self, name: &str) -> u64 {
        self.axis_len_at(0, name)
    }

    /// The length of a named axis at `level`, or 1 when the axis is absent.
    pub fn axis_len_at(&self, level: usize, name: &str) -> u64 {
        let Some(dataset) = &self.dataset else {
            // An object layer's extent is its bounds, not an axis length.
            return match (&self.ui, name) {
                (LayerUi::Objects(state), "z") => state
                    .schema
                    .bounds
                    .map(|b| (b[3].ceil() as u64).max(1))
                    .unwrap_or(1),
                _ => 1,
            };
        };
        let axes = &dataset.metadata.multiscales[0].axes;
        let Some(arr) = dataset.arrays.get(level).or_else(|| dataset.arrays.first()) else {
            return 1;
        };
        axes.iter()
            .position(|axis| axis.name == name)
            .and_then(|i| arr.shape.get(i).copied())
            .unwrap_or(1)
    }

    /// `(width, height)` in this layer's own pixels at `level`.
    pub fn level_size(&self, level: usize) -> Option<(f32, f32)> {
        let dataset = self.dataset.as_ref()?;
        let arr = dataset.arrays.get(level)?;
        let axes = &dataset.metadata.multiscales[0].axes;
        let mut w = 1.0;
        let mut h = 1.0;
        for (i, axis) in axes.iter().enumerate() {
            match axis.name.as_str() {
                "x" => w = *arr.shape.get(i)? as f32,
                "y" => h = *arr.shape.get(i)? as f32,
                _ => {}
            }
        }
        Some((w, h))
    }

    /// The layer's full-resolution `(width, height)`.
    ///
    /// An object layer has no pixels, so its extent is the bounding box of its
    /// rows — which is what makes a session of nothing but detections still
    /// have somewhere to put the camera.
    pub fn world_size(&self) -> (f32, f32) {
        if let Some(size) = self.level_size(0) {
            return size;
        }
        match &self.ui {
            LayerUi::Objects(state) => state
                .schema
                .bounds
                .map(|b| ((b[5] + 1.0) as f32, (b[4] + 1.0) as f32))
                .unwrap_or((1.0, 1.0)),
            _ => (1.0, 1.0),
        }
    }

    pub fn num_levels(&self) -> usize {
        self.dataset.as_ref().map(|d| d.arrays.len()).unwrap_or(0)
    }

    /// The tile grid for a level: chunk-sized tiles, clamped to something a
    /// texture upload is happy with.
    pub fn tile_grid(&self, level: usize) -> Option<TileGrid> {
        let dataset = self.dataset.as_ref()?;
        let arr = dataset.arrays.get(level)?;
        let axes = &dataset.metadata.multiscales[0].axes;
        let mut img_w = 1u64;
        let mut img_h = 1u64;
        let mut chunk_w = 256u64;
        let mut chunk_h = 256u64;
        for (i, axis) in axes.iter().enumerate() {
            match axis.name.as_str() {
                "x" => {
                    img_w = arr.shape[i];
                    if let Some(&cw) = arr.chunks.get(i) {
                        chunk_w = cw;
                    }
                }
                "y" => {
                    img_h = arr.shape[i];
                    if let Some(&ch) = arr.chunks.get(i) {
                        chunk_h = ch;
                    }
                }
                _ => {}
            }
        }
        let tile_w = chunk_w.clamp(256, 2048);
        let tile_h = chunk_h.clamp(256, 2048);
        Some(TileGrid {
            img_w,
            img_h,
            tile_w,
            tile_h,
            num_tiles_x: img_w.div_ceil(tile_w) as u32,
            num_tiles_y: img_h.div_ceil(tile_h) as u32,
        })
    }

    /// The coarsest level whose pixels are still worth at least half a screen
    /// pixel, expressed in the world the camera works in.
    ///
    /// Taking the world rather than the layer's own size is what makes a
    /// half-resolution label volume choose a *coarser* level than the image
    /// under it, which is the correct answer: its pixels are twice as big.
    pub fn pick_level(&self, world: (f32, f32), zoom: f32, canvas: (f32, f32)) -> usize {
        if self.dataset.is_none() {
            return 0;
        }
        let fit = zoom * (canvas.0 / world.0).min(canvas.1 / world.1);
        let mut best = 0;
        for level in 0..self.num_levels() {
            let Some((lw, lh)) = self.level_size(level) else {
                break;
            };
            // Screen pixels per level pixel.
            let spp = (fit * world.0 / lw).min(fit * world.1 / lh);
            if spp > 2.0 {
                break;
            }
            best = level;
        }
        best
    }

    /// The scale from this layer's level pixels to world pixels.
    pub fn level_to_world(&self, level: usize, world: (f32, f32)) -> (f32, f32) {
        match self.level_size(level) {
            Some((lw, lh)) if lw > 0.0 && lh > 0.0 => (world.0 / lw, world.1 / lh),
            _ => (1.0, 1.0),
        }
    }

    /// The RGBA colour table for a label layer, flattened for the GPU, when
    /// the store named colours and the ids are small enough to index directly.
    ///
    /// A table is refused above 65536 entries: an atlas with ids in the
    /// millions would want a hash map on the GPU, and until one exists the hash
    /// colouring is the honest answer rather than a 4 MB texture of holes.
    pub fn label_lut(&self) -> Option<Vec<u8>> {
        let LayerUi::Labels(state) = &self.ui else {
            return None;
        };
        let colors = state.colors.as_ref()?;
        let max_id = colors
            .iter()
            .map(|c| c.label_value.max(0.0) as u64)
            .max()
            .unwrap_or(0);
        if colors.is_empty() || max_id > 65_535 {
            return None;
        }
        let mut rgba = vec![0u8; (max_id as usize + 1) * 4];
        for color in colors {
            let Some([r, g, b, a]) = color.rgba else {
                continue;
            };
            let id = color.label_value.max(0.0) as usize;
            let at = id * 4;
            rgba[at] = r.clamp(0, 255) as u8;
            rgba[at + 1] = g.clamp(0, 255) as u8;
            rgba[at + 2] = b.clamp(0, 255) as u8;
            // Alpha 0 means "not in the table", so a named colour is opaque
            // even when the store wrote a zero there.
            rgba[at + 3] = a.clamp(1, 255) as u8;
        }
        Some(rgba)
    }
}

/// The display maximum for a dtype, used as the contrast slider's top end.
pub fn dtype_max(dataset: &DatasetInfo) -> f32 {
    match dataset.arrays.first().map(|a| a.dtype.as_str()) {
        Some("uint8") => 255.0,
        Some("uint16") => 65535.0,
        Some("uint32") => 4294967295.0,
        Some("int8") => 127.0,
        Some("int16") => 32767.0,
        Some("float32") | Some("float64") => 1.0,
        _ => 255.0,
    }
}

/// Channel states from OMERO metadata, or defaults when there is none.
fn channels_from(dataset: &DatasetInfo, dtype_max: f32) -> Vec<ChannelUiState> {
    let axes = &dataset.metadata.multiscales[0].axes;
    let shape = &dataset.arrays[0].shape;
    let num_channels = axes
        .iter()
        .position(|axis| axis.name == "c")
        .and_then(|i| shape.get(i).copied())
        .unwrap_or(1);

    (0..num_channels)
        .map(|c| {
            let omero_channel = dataset
                .metadata
                .omero
                .as_ref()
                .and_then(|omero| omero.channels.get(c as usize));
            let label = omero_channel
                .and_then(|ch| ch.label.clone())
                .unwrap_or_else(|| format!("Ch {}", c));
            let color = omero_channel
                .and_then(|ch| ch.color.as_deref())
                .and_then(parse_hex_color)
                .unwrap_or_else(|| channel_panel::default_color(c as usize));
            let visible = match omero_channel {
                Some(ch) => ch.active,
                None => c == 0,
            };
            let window = omero_channel
                .and_then(|ch| ch.window.as_ref())
                .map(|w| (w.start as f32, w.end as f32));
            let (contrast_min, contrast_max) = window.unwrap_or((0.0, dtype_max));
            ChannelUiState {
                label,
                visible,
                color,
                contrast_min,
                contrast_max,
                opacity: if visible { 1.0 } else { 0.0 },
                auto_contrast: window.is_none(),
            }
        })
        .collect()
}

fn parse_hex_color(hex: &str) -> Option<[f32; 3]> {
    let hex = hex.trim_start_matches('#');
    if hex.len() < 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()? as f32 / 255.0;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()? as f32 / 255.0;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()? as f32 / 255.0;
    Some([r, g, b])
}
