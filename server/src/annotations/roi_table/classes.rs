//! A class per label id, as an ngio **feature table** beside the label image.
//!
//! This is the other half of what the viewer already does with a feature table.
//! It reads one to *colour* a label image by a column; this writes one to say
//! what each label **is**. Where a shape's class travels with its geometry, a
//! segmented object has no geometry here — it is an id in a raster somebody else
//! produced — so the class travels in a table joined to it by that id, which is
//! exactly what `region` and `instance_key` are for.
//!
//! # Why this is the cheap half of annotation for training
//!
//! An object classifier's training data is a class per instance, and the
//! instances already exist: a segmentation is the input, not the thing being
//! drawn. So the annotation is a *table write*, not a raster write — no brush,
//! no rasterisation rule, nothing to resample. It is also the form that survives
//! a re-segmentation worst: the ids are the other tool's, and if it renumbers
//! them the join is silently wrong. `source_labels` records what was joined to,
//! so a mismatch can at least be noticed.
//!
//! # Unassigned is not a class
//!
//! An id with no row has not been looked at; an id with an empty class has been
//! looked at and found to be nothing in particular. Collapsing the two would
//! make "I have not started" indistinguishable from "none of these are cells",
//! which is the same partial-supervision distinction the annotation side keeps
//! between a stroke and the pixels around it.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Context, Result};
use zarrs::storage::WritableStorageTraits;

use super::store::{attributes_at, check_name, filesystem, store_is_v3};
use super::{group_for, make_target, merged_index, payload_key, table_path, CSV_PAYLOAD};

/// The class of each label id that has one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LabelClasses {
    by_id: BTreeMap<u64, String>,
}

impl LabelClasses {
    pub fn set(&mut self, id: u64, class: impl Into<String>) {
        self.by_id.insert(id, class.into());
    }

    /// Forget an id, which is not the same as classing it as nothing.
    pub fn clear(&mut self, id: u64) {
        self.by_id.remove(&id);
    }

    pub fn get(&self, id: u64) -> Option<&str> {
        self.by_id.get(&id).map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (u64, &str)> {
        self.by_id.iter().map(|(id, class)| (*id, class.as_str()))
    }

    /// Every class in use, in first-seen id order.
    pub fn classes(&self) -> Vec<String> {
        let mut seen: Vec<String> = Vec::new();
        for class in self.by_id.values() {
            if !seen.iter().any(|c| c == class) {
                seen.push(class.clone());
            }
        }
        seen
    }
}

/// What the table declares itself to be.
///
/// `feature_table` rather than `roi_table` because it carries no coordinates:
/// where a row *is* is wherever its id sits in the label image `region` names.
/// A table claiming to be an ROI table without coordinates is a broken file, and
/// this viewer refuses those by name — so it must not write one.
fn attributes(region: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "feature_table",
        "table_version": "1",
        "backend": "csv",
        "region": {"path": region},
        "instance_key": INSTANCE_KEY,
        "index_key": INSTANCE_KEY,
        "index_type": "int",
    })
}

const INSTANCE_KEY: &str = "label";
const CLASS_COLUMN: &str = "class";

/// Write `classes` as `<root>/tables/<name>`, joined to the label image at
/// `region` — a path relative to the table group, e.g. `../labels/nuclei`.
pub fn write(root: &Path, name: &str, region: &str, classes: &LabelClasses) -> Result<String> {
    check_name(name)?;
    if !root.is_dir() {
        bail!("{} is not a directory", root.display());
    }
    if region.trim().is_empty() {
        bail!("a class table has to name the label image it describes");
    }
    let v3 = store_is_v3(root);
    let store = filesystem(root)?;

    let index = merged_index(&attributes_at(&store, "/tables"), name);
    group_for(store.clone(), v3, "/tables", index)?
        .store_metadata()
        .context("writing the tables group")?;
    group_for(store.clone(), v3, &table_path(name), attributes(region))?
        .store_metadata()
        .context("writing the class table group")?;
    store
        .set(&payload_key(name, CSV_PAYLOAD)?, encode(classes).into())
        .context("writing the class table payload")?;

    Ok(make_target(root, name))
}

/// `label,class`, one row per assigned id, in id order.
fn encode(classes: &LabelClasses) -> Vec<u8> {
    let mut csv = format!("{INSTANCE_KEY},{CLASS_COLUMN}\n");
    for (id, class) in classes.iter() {
        csv.push_str(&format!("{id},{}\n", quoted(class)));
    }
    csv.into_bytes()
}

/// A class name is a person's words, so it may hold a comma or a quote.
fn quoted(value: &str) -> String {
    if value.contains([',', '"', '\n']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_at(root: &Path) {
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(root.join(".zgroup"), br#"{"zarr_format":2}"#).unwrap();
        std::fs::write(root.join(".zattrs"), b"{}").unwrap();
    }

    #[test]
    fn what_we_write_is_what_our_own_reader_understands() {
        // The whole value of writing this table is that something reads it. The
        // nearest reader is ours, and it is the one that already joins a feature
        // table to a label image, so it is the honest first check.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("image.zarr");
        store_at(&root);

        let mut classes = LabelClasses::default();
        classes.set(1, "tumour");
        classes.set(2, "stroma");
        classes.set(7, "tumour");
        let target = write(&root, "cell_types", "../labels/nuclei", &classes).unwrap();
        assert_eq!(super::super::split_target(&target).unwrap().1, "cell_types");

        let back = super::super::read(&root, "cell_types").unwrap();
        assert_eq!(back.table_type, "feature_table");
        let region = back.region.as_ref().expect("the label image it describes");
        assert_eq!(region.path, "../labels/nuclei");
        assert_eq!(region.instance_key, "label");

        // The metadata being right is not the point; the join is. Read the two
        // columns back and rebuild the id -> class map, which is what any
        // consumer of this table has to do.
        assert_eq!(back.columns.row_count(), 3);
        assert_eq!(back.columns.names(), vec!["label", "class"]);
        let rebuilt: Vec<(String, String)> = (0..back.columns.row_count())
            .map(|row| {
                (
                    back.columns.string("label", row).unwrap(),
                    back.columns.string("class", row).unwrap(),
                )
            })
            .collect();
        assert_eq!(
            rebuilt,
            vec![
                ("1".to_string(), "tumour".to_string()),
                ("2".to_string(), "stroma".to_string()),
                ("7".to_string(), "tumour".to_string()),
            ],
            "ids and classes, in id order"
        );
    }

    #[test]
    fn an_unassigned_id_has_no_row_and_an_empty_class_does() {
        // Two different statements: "not looked at" and "looked at, nothing in
        // particular". A table that wrote a row for every id would lose the
        // first, which is the one that says where the work stopped.
        let mut classes = LabelClasses::default();
        classes.set(3, "");
        let csv = String::from_utf8(encode(&classes)).unwrap();
        assert_eq!(csv, "label,class\n3,\n", "{csv}");
        assert_eq!(classes.get(3), Some(""));
        assert_eq!(classes.get(4), None, "an id nobody classed");

        classes.clear(3);
        assert_eq!(classes.get(3), None, "cleared is unassigned again");
    }

    #[test]
    fn a_class_a_person_typed_survives_being_a_csv_field() {
        // Class names are somebody's words. A comma in one would otherwise shift
        // every later column of that row, silently.
        let mut classes = LabelClasses::default();
        classes.set(1, "tumour, invasive");
        classes.set(2, r#"the "odd" ones"#);
        let csv = String::from_utf8(encode(&classes)).unwrap();
        assert!(csv.contains("\"tumour, invasive\""), "{csv}");
        assert!(csv.contains(r#""the ""odd"" ones""#), "{csv}");
    }

    #[test]
    fn a_table_with_no_label_image_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("image.zarr");
        store_at(&root);
        let error = write(&root, "orphan", "  ", &LabelClasses::default())
            .unwrap_err()
            .to_string();
        assert!(error.contains("label image"), "{error}");
    }

    #[test]
    fn the_classes_in_use_are_listed_once_each() {
        let mut classes = LabelClasses::default();
        classes.set(5, "b");
        classes.set(1, "a");
        classes.set(9, "a");
        assert_eq!(classes.classes(), vec!["a", "b"], "by id, deduplicated");
    }
}
