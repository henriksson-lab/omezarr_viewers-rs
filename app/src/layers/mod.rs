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

use omezarr_viewer_common::{
    DatasetInfo, LabelColor, LabelProperty, LayerInfo, LayerKind, TableInfo,
};

use crate::controls::channel_panel;

mod annotations;
mod objects;

pub use annotations::AnnotUiState;
pub use objects::{ObjectData, ObjectUiState};

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
    /// `image-label` properties: what the store says about each id.
    pub properties: Option<Vec<LabelProperty>>,
    /// A feature table's column currently colouring these ids, as
    /// `(layer id, column name)`.
    ///
    /// This is what a feature table is *for*: it has no coordinates, only a row
    /// per label id, so the way to see it is to paint the ids it describes.
    pub colored_by: Option<(String, String)>,
}

impl LabelUiState {
    /// What the store says about one id, as a line of text.
    ///
    /// The spec lets each id carry a different set of keys, so this reports
    /// whatever is there rather than looking for fields it hopes exist.
    pub fn describe(&self, id: u64) -> Option<String> {
        let entry = self
            .properties
            .as_ref()?
            .iter()
            .find(|p| p.label_value as u64 == id)?;
        let described = entry
            .fields
            .iter()
            .map(|(key, value)| {
                let text = match value {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                format!("{key} {text}")
            })
            .collect::<Vec<_>>()
            .join(" \u{00b7} ");
        (!described.is_empty()).then_some(described)
    }
}

impl Default for LabelUiState {
    fn default() -> Self {
        Self {
            opacity: 0.6,
            outline: false,
            selected: 0,
            only_selected: false,
            colors: None,
            properties: None,
            colored_by: None,
        }
    }
}

/// UI state for a table layer — rows with no geometry of their own.
#[derive(Clone, PartialEq)]
pub struct TableUiState {
    pub table: TableInfo,
    /// Rows fetched so far, as text. The first page arrives with the session.
    pub rows: Vec<Vec<String>>,
    /// Where the next page starts.
    pub offset: usize,
    pub loading: bool,
    /// The column this table is currently painting a label layer with.
    pub coloring: Option<String>,
    /// The label layer it is painting, once one has been matched.
    pub target: Option<String>,
}

impl TableUiState {
    pub fn new(table: TableInfo) -> Self {
        Self {
            rows: table.preview.clone(),
            offset: table.preview.len(),
            table,
            loading: false,
            coloring: None,
            target: None,
        }
    }
}

/// The point shader's ramp, on the CPU: dark blue to teal to green to yellow.
///
/// The same one the object layer uses, so a measurement means the same colour
/// whether it is drawn as a point or painted onto a label image.
fn ramp(t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    let stops = [
        [0.27, 0.00, 0.33],
        [0.13, 0.42, 0.56],
        [0.15, 0.68, 0.49],
        [0.99, 0.91, 0.15],
    ];
    let scaled = t * 3.0;
    let i = (scaled.floor() as usize).min(2);
    let f = scaled - i as f32;
    let mut out = [0u8; 3];
    for channel in 0..3 {
        let value = stops[i][channel] + (stops[i + 1][channel] - stops[i][channel]) * f;
        out[channel] = (value * 255.0).clamp(0.0, 255.0) as u8;
    }
    out
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
    Annotations(AnnotUiState),
    Table(TableUiState),
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
                dataset,
                colors,
                properties,
            } => Some(Self {
                id: info.id.clone(),
                name: info.name.clone(),
                visible: true,
                ui: LayerUi::Labels(LabelUiState {
                    colors: colors.clone(),
                    properties: properties.clone(),
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
            LayerKind::Table { table } => Some(Self {
                id: info.id.clone(),
                name: info.name.clone(),
                visible: true,
                dataset: None,
                ui: LayerUi::Table(TableUiState::new(table.clone())),
            }),
            LayerKind::Annotations {
                annotations,
                target,
            } => Some(Self {
                id: info.id.clone(),
                name: info.name.clone(),
                visible: true,
                dataset: None,
                ui: LayerUi::Annotations(AnnotUiState::new(annotations.clone(), target.clone())),
            }),
        }
    }

    pub fn is_labels(&self) -> bool {
        matches!(self.ui, LayerUi::Labels(_))
    }

    pub fn is_objects(&self) -> bool {
        matches!(self.ui, LayerUi::Objects(_))
    }

    pub fn is_annotations(&self) -> bool {
        matches!(self.ui, LayerUi::Annotations(_))
    }

    pub fn is_table(&self) -> bool {
        matches!(self.ui, LayerUi::Table(_))
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
            // An annotation layer never defines the world: it is drawn *onto*
            // one, and a session of nothing but annotations has no pixels to
            // put a camera on.
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
    /// A colour table built from a feature column: id → a ramp position.
    ///
    /// This is how a table with no coordinates gets drawn — every id the table
    /// describes takes the colour of its measurement, and the label image it
    /// describes becomes a heat map of that column.
    pub fn measurement_lut(labels: &[u64], values: &[f64]) -> Option<Vec<u8>> {
        let max_id = labels.iter().copied().max()?;
        if max_id > 65_535 {
            // The same ceiling the colour table has: an atlas with ids in the
            // millions wants a hash map on the GPU, not a 4 MB texture of holes.
            return None;
        }
        let (lo, hi) = values
            .iter()
            .filter(|v| v.is_finite())
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
                (lo.min(*v), hi.max(*v))
            });
        let span = (hi - lo).max(f64::MIN_POSITIVE);
        let mut rgba = vec![0u8; (max_id as usize + 1) * 4];
        for (id, value) in labels.iter().zip(values) {
            if !value.is_finite() {
                continue;
            }
            let [r, g, b] = ramp(((value - lo) / span) as f32);
            let at = *id as usize * 4;
            rgba[at] = r;
            rgba[at + 1] = g;
            rgba[at + 2] = b;
            // Opaque, so the shader knows the table names this id — a zero
            // alpha is how it says "not in the table".
            rgba[at + 3] = 255;
        }
        Some(rgba)
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Alpha is how the shader knows the table names an id at all.
    fn named(lut: &[u8], id: usize) -> bool {
        lut[id * 4 + 3] == 255
    }

    #[test]
    fn a_measurement_lut_maps_every_id_and_leaves_the_rest_transparent() {
        let lut = LayerState::measurement_lut(&[1, 3], &[0.0, 100.0]).expect("a table");
        assert_eq!(lut.len(), 4 * 4, "sized to the largest id");
        let named: Vec<bool> = (0..4).map(|id| named(&lut, id)).collect();
        assert_eq!(named, vec![false, true, false, true], "only the listed ids");
        // Ends of the ramp differ, which is the whole point of colouring by it.
        assert_ne!(&lut[4..7], &lut[12..15]);
    }

    #[test]
    fn a_measurement_lut_refuses_ids_too_big_for_a_texture() {
        // An atlas with ids in the millions wants a hash map on the GPU, not a
        // texture of holes; the honest answer is to decline.
        assert!(LayerState::measurement_lut(&[70_000], &[1.0]).is_none());
        assert!(LayerState::measurement_lut(&[], &[]).is_none());
    }

    #[test]
    fn a_non_finite_measurement_colours_nothing() {
        let lut = LayerState::measurement_lut(&[1, 2], &[f64::NAN, 5.0]).expect("a table");
        assert!(!named(&lut, 1), "NaN leaves the id unnamed");
        assert!(named(&lut, 2));
    }
}
