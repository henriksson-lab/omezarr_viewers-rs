//! Region names for label ids.
//!
//! An atlas label volume holds numbers; what a person wants is "Primary
//! somatosensory area, layer 4". The mapping lives in the atlas's own table —
//! for the Allen atlas that `clearmap-ng` uses, `ABA_annotation_last.jsonl`,
//! one JSON object per line.
//!
//! **Parsed permissively, and that is load-bearing.** `clearmap-rs` declares
//! `st_level` and `parent_structure_id` as integers and the shipped file writes
//! them as JSON floats (`"st_level": 2.0`), so its own loader fails on the
//! resource it ships — recorded in `clearmap-ng`'s `Cargo.toml`. Every numeric
//! field here is read through a form that accepts both, and a line that cannot
//! be read is skipped rather than sinking the file.

use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;

/// One region.
#[derive(Debug, Clone, Serialize)]
pub struct Region {
    pub id: u64,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acronym: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<u64>,
}

/// An id-to-region table.
#[derive(Debug, Clone, Default)]
pub struct Ontology {
    regions: HashMap<u64, Region>,
}

impl Ontology {
    /// Read a JSONL ontology.
    ///
    /// Reports how many lines were skipped rather than silently thinning the
    /// table: a file half of which failed to parse is a fact worth seeing in
    /// the log.
    pub fn read(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let mut ontology = Ontology::default();
        let mut skipped = 0usize;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<serde_json::Value>(line) {
                Ok(value) => match region_of(&value) {
                    Some(region) => {
                        ontology.regions.insert(region.id, region);
                    }
                    None => skipped += 1,
                },
                Err(_) => skipped += 1,
            }
        }
        log::info!(
            "ontology {}: {} region(s), {skipped} line(s) skipped",
            path.display(),
            ontology.regions.len()
        );
        Ok(ontology)
    }

    pub fn len(&self) -> usize {
        self.regions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }

    pub fn get(&self, id: u64) -> Option<&Region> {
        self.regions.get(&id)
    }

    pub fn name(&self, id: u64) -> Option<&str> {
        self.regions.get(&id).map(|r| r.name.as_str())
    }
}

/// One region from a JSON object, accepting the shapes the shipped files use.
fn region_of(value: &serde_json::Value) -> Option<Region> {
    let id = number(value.get("id").or_else(|| value.get("structure_id"))?)?;
    let name = value
        .get("name")
        .or_else(|| value.get("safe_name"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| format!("region {id}"));
    Some(Region {
        id,
        name,
        acronym: value
            .get("acronym")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        parent: value
            .get("parent_structure_id")
            .or_else(|| value.get("parent"))
            .and_then(number),
    })
}

/// A number written as an integer, a float (`8.0`), or a string.
fn number(value: &serde_json::Value) -> Option<u64> {
    match value {
        serde_json::Value::Number(number) => number
            .as_u64()
            .or_else(|| number.as_f64().filter(|v| *v >= 0.0).map(|v| v as u64)),
        serde_json::Value::String(text) => text
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|v| *v >= 0.0)
            .map(|v| v as u64),
        _ => None,
    }
}

/// One row of the per-region tally.
#[derive(Debug, Clone, Serialize)]
pub struct RegionCount {
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acronym: Option<String>,
    pub count: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_shipped_shape_floats_and_all() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("atlas.jsonl");
        std::fs::write(
            &path,
            // The float-typed fields that break the reference loader.
            "{\"id\": 315, \"name\": \"Isocortex\", \"acronym\": \"CTX\", \"st_level\": 2.0, \"parent_structure_id\": 8.0}\n\
             {\"id\": 500.0, \"name\": \"Somatosensory\", \"parent_structure_id\": 315}\n\
             \n\
             not json at all\n",
        )
        .unwrap();

        let ontology = Ontology::read(&path).expect("read");
        assert_eq!(
            ontology.len(),
            2,
            "the unreadable line is skipped, not fatal"
        );
        assert_eq!(ontology.name(315), Some("Isocortex"));
        assert_eq!(ontology.get(315).unwrap().parent, Some(8));
        assert_eq!(
            ontology.name(500),
            Some("Somatosensory"),
            "an id written as a float is the same id"
        );
        assert_eq!(ontology.name(999), None);
    }

    #[test]
    fn a_region_without_a_name_still_has_an_id() {
        let value: serde_json::Value = serde_json::from_str("{\"id\": 7}").unwrap();
        let region = region_of(&value).expect("a region");
        assert_eq!(region.id, 7);
        assert_eq!(region.name, "region 7");
    }
}
