//! Reading QuPath's GeoJSON dialect.
//!
//! Everything the reader understands is **preserved**, including members
//! nothing in this viewer displays — UUID, measurements, metadata. A round trip
//! must not flatten somebody else's work, which is why the tests that pin that
//! live beside the module rather than here: they are a claim about read *and*
//! write together, and this half cannot make it alone.

use anyhow::{bail, Context, Result};
use omezarr_viewer_common::{Annotation, Geometry, ObjectType, Plane};
use serde_json::{Map, Value};

use super::{DENSE_REGION, STROKE_WIDTH, T_EXTENT, Z_EXTENT};

/// Parse a `FeatureCollection` — or a bare feature, or an array of them.
///
/// All three are in the wild: QuPath writes a collection by default but can
/// write an array, and hand-made files are often a single feature. Accepting
/// each costs one match and saves the user finding out by error message.
pub fn parse(bytes: &[u8]) -> Result<Vec<Annotation>> {
    let value: Value = serde_json::from_slice(bytes).context("parsing the annotations as JSON")?;
    let features = match &value {
        Value::Object(map) => match map.get("type").and_then(Value::as_str) {
            Some("FeatureCollection") => map
                .get("features")
                .and_then(Value::as_array)
                .cloned()
                .context("a FeatureCollection with no `features` array")?,
            Some("Feature") => vec![value.clone()],
            _ => bail!("not GeoJSON: the root object is neither a Feature nor a FeatureCollection"),
        },
        Value::Array(items) => items.clone(),
        _ => bail!("not GeoJSON: the root is neither an object nor an array"),
    };

    let mut out = Vec::new();
    for feature in &features {
        read_feature(feature, None, &mut out)?;
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

fn read_feature(feature: &Value, parent: Option<u64>, out: &mut Vec<Annotation>) -> Result<()> {
    let Some(object) = feature.as_object() else {
        bail!("a feature that is not an object");
    };
    // A `root` object is QuPath's hierarchy anchor and has no geometry of its
    // own; its children are what matter, so it is descended into and dropped.
    let geometry_value = object.get("geometry");
    let is_root = object
        .get("properties")
        .and_then(|p| p.get("objectType"))
        .and_then(Value::as_str)
        == Some("root");

    let mut id = None;
    if let Some(geometry_value) = geometry_value.filter(|v| !v.is_null()) {
        if !is_root {
            let mut annotation = read_geometry(geometry_value)?;
            // A cell object's nucleus rides beside its main geometry. Its plane
            // and ellipse flag are the object's, not its own.
            annotation.nucleus = object
                .get("nucleusGeometry")
                .filter(|v| !v.is_null())
                .and_then(|v| serde_json::from_value(v.clone()).ok());
            annotation.parent = parent;
            annotation.uuid = object.get("id").and_then(Value::as_str).map(str::to_string);
            if let Some(properties) = object.get("properties").and_then(Value::as_object) {
                read_properties(properties, &mut annotation);
            }
            // The index within this batch stands in for an id until the set
            // assigns real ones; `AnnotationSet::from_rows` rewrites both this
            // and the children's `parent` to match.
            annotation.id = out.len() as u64;
            id = Some(annotation.id);
            out.push(annotation);
        }
    }

    if let Some(children) = object
        .get("properties")
        .and_then(|p| p.get("childObjects"))
        .and_then(Value::as_array)
    {
        for child in children {
            read_feature(child, id.or(parent), out)?;
        }
    }
    Ok(())
}

/// One GeoJSON geometry, plus QuPath's two foreign members.
fn read_geometry(value: &Value) -> Result<Annotation> {
    let geometry: Geometry = serde_json::from_value(value.clone()).with_context(|| match value
        .get("type")
        .and_then(Value::as_str)
    {
        Some(kind) => format!("`{kind}` is not a geometry this viewer draws"),
        None => "a geometry with no `type`".to_string(),
    })?;

    let plane = value
        .get("plane")
        .and_then(|p| {
            Some(Plane {
                c: p.get("c").and_then(Value::as_i64).unwrap_or(-1) as i32,
                z: p.get("z").and_then(Value::as_i64)? as i32,
                t: p.get("t").and_then(Value::as_i64).unwrap_or(0) as i32,
            })
        })
        .unwrap_or_default();

    Ok(Annotation {
        is_ellipse: value
            .get("isEllipse")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        geometry,
        plane,
        ..Default::default()
    })
}

fn read_properties(properties: &Map<String, Value>, into: &mut Annotation) {
    if let Some(kind) = properties.get("objectType").and_then(Value::as_str) {
        into.object_type = ObjectType::parse(kind);
    }
    into.name = properties
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_string);
    into.color = properties.get("color").and_then(read_color);
    into.locked = properties
        .get("isLocked")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    into.missing = properties
        .get("isMissing")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    if let Some(classification) = properties.get("classification") {
        // QuPath writes a simple class as `name` and a derived one as `names`;
        // joining with ": " is how QuPath itself renders the derived form, so
        // the round trip through a single text field is exact.
        into.label = classification
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                let names = classification.get("names")?.as_array()?;
                Some(
                    names
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(": "),
                )
            })
            .unwrap_or_default();
        into.class_color = classification.get("color").and_then(read_color);
    }

    if let Some(measurements) = properties.get("measurements") {
        into.measurements = read_measurements(measurements);
    }
    if let Some(metadata) = properties.get("metadata").and_then(Value::as_object) {
        into.metadata = metadata
            .iter()
            .map(|(key, value)| {
                let text = match value {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                (key.clone(), text)
            })
            .collect();
    }
    into.z_extent = properties
        .get(Z_EXTENT)
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    into.t_extent = properties
        .get(T_EXTENT)
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    // Only a positive, finite width is a stroke. A zero or a NaN in the file is
    // a line that says nothing about area, which is exactly what `None` means,
    // so it is not carried through as a width nobody can rasterise.
    into.dense_region = properties
        .get(DENSE_REGION)
        .and_then(Value::as_bool)
        .unwrap_or(false);
    into.stroke_width = properties
        .get(STROKE_WIDTH)
        .and_then(Value::as_f64)
        .filter(|w| w.is_finite() && *w > 0.0);
}

/// `[r, g, b]` or `[r, g, b, a]`; the alpha is dropped, since nothing here
/// draws a per-object alpha.
fn read_color(value: &Value) -> Option<[u8; 3]> {
    let array = value.as_array()?;
    let channel = |i: usize| -> u8 {
        array
            .get(i)
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .clamp(0, 255) as u8
    };
    (array.len() >= 3).then(|| [channel(0), channel(1), channel(2)])
}

/// An object of name → number (QuPath ≥ 0.4), or the older array of
/// `{"name":…, "value":…}`. Both are accepted, as QuPath accepts both.
///
/// Non-finite values arrive as the *strings* `"NaN"`, `"Infinity"`,
/// `"-Infinity"`, which is what QuPath writes; they are dropped rather than
/// turned into a number that would be wrong in a different way.
fn read_measurements(value: &Value) -> std::collections::BTreeMap<String, f64> {
    let mut out = std::collections::BTreeMap::new();
    match value {
        Value::Object(map) => {
            for (name, value) in map {
                if let Some(number) = value.as_f64().filter(|n| n.is_finite()) {
                    out.insert(name.clone(), number);
                }
            }
        }
        Value::Array(rows) => {
            for row in rows {
                let (Some(name), Some(number)) = (
                    row.get("name").and_then(Value::as_str),
                    row.get("value").and_then(Value::as_f64),
                ) else {
                    continue;
                };
                if number.is_finite() {
                    out.insert(name.to_string(), number);
                }
            }
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::QUPATH;
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_everything_qupath_writes() {
        let rows = parse(QUPATH.as_bytes()).unwrap();
        assert_eq!(rows.len(), 3, "two features plus a nested child");

        let region = &rows[0];
        assert_eq!(
            region.uuid.as_deref(),
            Some("e3b0c442-98fc-1c14-9afb-4c8996fb9242")
        );
        assert_eq!(region.plane, Plane { c: -1, z: 3, t: 1 });
        assert_eq!(region.name.as_deref(), Some("Region 1"));
        assert_eq!(region.color, Some([255, 0, 0]));
        assert_eq!(region.label, "Tumor: Positive", "a derived class joins");
        assert_eq!(region.class_color, Some([200, 0, 0]));
        assert!(region.locked);
        assert_eq!(
            region.metadata.get("note").map(String::as_str),
            Some("checked")
        );
        assert_eq!(region.measurements.get("Area"), Some(&1234.5));
        assert!(
            !region.measurements.contains_key("Bad"),
            "`NaN` is a string in this format and is not a measurement"
        );
        // The hole survived, which is the whole reason for this format.
        let Geometry::Polygon(rings) = &region.geometry else {
            panic!("not a polygon");
        };
        assert_eq!(rings.len(), 2);
        assert!(!region.geometry.contains(175.0, 275.0, 0.0), "in the hole");
        assert!(region.geometry.contains(280.0, 380.0, 0.0), "in the ring");

        // The child came through as a child.
        let cell = &rows[1];
        assert_eq!(cell.parent, Some(region.id));
        assert_eq!(cell.object_type, ObjectType::Cell);
        assert_eq!(cell.label, "Cell");
        assert!(matches!(cell.geometry, Geometry::Point(_)));
        // A cell's second geometry. Dropping it would lose half of every cell
        // a QuPath segmentation produced.
        let nucleus = cell.nucleus.as_ref().expect("the nucleus came through");
        assert_eq!(nucleus.bounds(), Some([148.0, 258.0, 152.0, 262.0]));

        // And the ellipse flag, which cannot be recovered from the vertices.
        assert!(rows[2].is_ellipse);
        assert_eq!(rows[2].plane, Plane::default());
    }
    #[test]
    fn a_width_that_cannot_be_rasterised_is_not_a_width() {
        // Zero and negative reach us from files this viewer did not write, and
        // each is a line rather than a stroke: there is no set of pixels within
        // `w / 2` of the path for either. Non-finite is not tested because JSON
        // cannot express one — `NaN` and `Infinity` are not JSON numbers, so a
        // width that survives parsing is already finite.
        for bad in [json!(0), json!(-3.5)] {
            let feature = json!({
                "type": "Feature",
                "geometry": {"type": "LineString", "coordinates": [[0, 0], [5, 5]]},
                "properties": {"strokeWidth": bad},
            });
            let back = parse(feature.to_string().as_bytes()).unwrap();
            assert_eq!(back[0].stroke_width, None, "width {bad} is not a stroke");
        }
    }
    #[test]
    fn a_bare_feature_and_an_array_of_features_are_both_accepted() {
        let one = r#"{"type":"Feature","geometry":{"type":"Point","coordinates":[1,2]},
                      "properties":{}}"#;
        assert_eq!(parse(one.as_bytes()).unwrap().len(), 1);
        let array = format!("[{one},{one}]");
        assert_eq!(parse(array.as_bytes()).unwrap().len(), 2);
    }
    #[test]
    fn a_root_object_contributes_its_children_and_not_itself() {
        // QuPath's hierarchy export wraps everything in a geometry-less root.
        let text = r#"{"type":"FeatureCollection","features":[{
            "type":"Feature","geometry":null,
            "properties":{"objectType":"root","childObjects":[
              {"type":"Feature","geometry":{"type":"Point","coordinates":[1,2]},
               "properties":{"objectType":"annotation"}}
            ]}}]}"#;
        let rows = parse(text.as_bytes()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].parent, None, "the root is not a parent to keep");
        assert!(matches!(rows[0].geometry, Geometry::Point(_)));
    }
    #[test]
    fn something_that_is_not_geojson_is_refused_by_name() {
        let error = parse(br#"{"tables":["boxes"]}"#).unwrap_err().to_string();
        assert!(error.contains("Feature"), "{error}");

        let bad_geometry = r#"{"type":"Feature","properties":{},
            "geometry":{"type":"Sphere","coordinates":[1,2,3]}}"#;
        let error = parse(bad_geometry.as_bytes()).unwrap_err().to_string();
        assert!(error.contains("Sphere"), "{error}");
    }
}
