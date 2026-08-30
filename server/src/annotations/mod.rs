//! Annotation layers: boxes and points a person drew, in world coordinates.
//!
//! Every other layer kind in this viewer is read-only — it shows what a
//! pipeline wrote. This one is the exception: the rows come from clicks, live
//! in memory, and are written out on request.
//!
//! # Why boxes, and why only boxes
//!
//! OME-Zarr specifies exactly one annotation form, and it is pixel data: a
//! `labels` image of integer ids (`info_roi.md` §2). There is no vector
//! geometry in the spec at all. The convention the ecosystem settled on for
//! regions is the ngio/Fractal **ROI table** — a `tables/<name>` group beside
//! `labels/` whose rows are *axis-aligned bounding boxes* and nothing else.
//!
//! So the model here is one axis-aligned box per row, and a point is a box with
//! zero extent. That is not a simplification we chose; it is the shape of the
//! only on-disk form that other tools will read back. A polygon has no home in
//! OME-Zarr, and inventing one here would produce a file only this viewer
//! understands.
//!
//! # Coordinates
//!
//! Annotations are held in **world** pixels — the reference image layer's
//! full-resolution x/y, the same space a click arrives in. That is also exactly
//! QuPath's convention (full resolution, origin top-left, y down), so GeoJSON
//! is written unconverted. Only the ROI table needs a conversion, to its
//! `*_micrometer` columns, through a `WorldScale` taken from the store's own
//! `coordinateTransformations`.

pub mod geojson;
pub mod roi_table;

use anyhow::{bail, Result};
use omezarr_viewer_common::{containing_parent, Annotation};

/// The annotations of one layer, plus where they came from.
#[derive(Debug, Default)]
pub struct AnnotationSet {
    items: Vec<Annotation>,
    /// Ids are handed out here and never reused, so a stale client reference
    /// names nothing rather than naming somebody else's box.
    next_id: u64,
    /// The ROI table this set was read from, or last written to.
    target: Option<String>,
}

impl AnnotationSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a set from rows read off disk, assigning ids.
    ///
    /// A reader numbers rows by their position in the file and points a child's
    /// `parent` at one of those numbers; this rewrites both to the ids the set
    /// hands out, so the hierarchy survives being renumbered.
    pub fn from_rows(rows: Vec<Annotation>, target: Option<String>) -> Self {
        let mut set = Self {
            items: Vec::with_capacity(rows.len()),
            next_id: 0,
            target,
        };
        let assigned: Vec<u64> = (0..rows.len() as u64).map(|i| i + 1).collect();
        for mut row in rows {
            row.parent = row
                .parent
                .and_then(|old| assigned.get(old as usize).copied());
            set.add(row);
        }
        set
    }

    pub fn items(&self) -> &[Annotation] {
        &self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn target(&self) -> Option<&str> {
        self.target.as_deref()
    }

    pub fn set_target(&mut self, target: impl Into<String>) {
        self.target = Some(target.into());
    }

    /// Append one annotation, nesting it under whatever it was drawn inside.
    ///
    /// QuPath's hierarchy is *spatial*: an object goes under the smallest
    /// annotation that covers it, without anybody saying so. This is the path a
    /// new shape from the viewer takes; [`AnnotationSet::add`] is the literal
    /// one, for rows read off disk that already know their parent.
    pub fn add_nested(&mut self, mut annotation: Annotation) -> Annotation {
        if annotation.parent.is_none() {
            annotation.parent = containing_parent(&self.items, &annotation);
        }
        self.add(annotation)
    }

    /// Recompute every parent from the shapes' current geometry.
    ///
    /// Editing moves shapes in and out of each other, and a hierarchy that was
    /// right when it was drawn can be wrong afterwards. Offered rather than
    /// applied on every edit: silently re-nesting under the pointer would be a
    /// surprise mid-drag.
    pub fn renest(&mut self) {
        let snapshot = self.items.clone();
        for (index, item) in self.items.iter_mut().enumerate() {
            let mut without_self = snapshot.clone();
            without_self.remove(index);
            // A shape cannot become its own descendant's child: dropping the
            // subtree first is what stops a re-nest from making a cycle.
            let descendants = descendants_of(&snapshot, item.id);
            without_self.retain(|other| !descendants.contains(&other.id));
            item.parent = containing_parent(&without_self, item);
        }
    }

    /// Append one annotation, assigning it an id, and return it as stored.
    pub fn add(&mut self, mut annotation: Annotation) -> Annotation {
        self.next_id += 1;
        annotation.id = self.next_id;
        self.items.push(annotation.clone());
        annotation
    }

    /// Replace one annotation, keeping its id.
    ///
    /// The parent link is kept from the stored row rather than taken from the
    /// caller: a client editing a shape sends the shape, and should not be able
    /// to re-parent it by omission.
    pub fn update(&mut self, id: u64, mut annotation: Annotation) -> Result<Annotation> {
        let Some(slot) = self.items.iter_mut().find(|item| item.id == id) else {
            bail!("no annotation {id}");
        };
        annotation.id = id;
        annotation.parent = slot.parent;
        *slot = annotation.clone();
        Ok(annotation)
    }

    /// Remove one annotation, and lift its children to where it was.
    ///
    /// Not a cascade: deleting a region should not silently delete every cell
    /// inside it. The children keep existing, one level further out.
    pub fn remove(&mut self, id: u64) -> bool {
        let Some(gone) = self.items.iter().find(|item| item.id == id) else {
            return false;
        };
        let inherits = gone.parent;
        self.items.retain(|item| item.id != id);
        for item in &mut self.items {
            if item.parent == Some(id) {
                item.parent = inherits;
            }
        }
        true
    }

    /// Detach one annotation from its parent, making it top-level.
    pub fn detach(&mut self, id: u64) -> bool {
        match self.items.iter_mut().find(|item| item.id == id) {
            Some(item) if item.parent.is_some() => {
                item.parent = None;
                true
            }
            _ => false,
        }
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// Axis-aligned bounds over every annotation, `[x0, y0, x1, y1]`.
    pub fn bounds(&self) -> Option<[f64; 4]> {
        let mut lo = [f64::INFINITY; 2];
        let mut hi = [f64::NEG_INFINITY; 2];
        for item in &self.items {
            let Some([x0, y0, x1, y1]) = item.bounds() else {
                continue;
            };
            lo[0] = lo[0].min(x0);
            lo[1] = lo[1].min(y0);
            hi[0] = hi[0].max(x1);
            hi[1] = hi[1].max(y1);
        }
        lo[0].is_finite().then_some([lo[0], lo[1], hi[0], hi[1]])
    }
}

/// Every id at or below `root`, so a re-nest cannot make a shape its own
/// descendant's child.
fn descendants_of(items: &[Annotation], root: u64) -> Vec<u64> {
    let mut found = vec![root];
    let mut changed = true;
    while changed {
        changed = false;
        for item in items {
            if let Some(parent) = item.parent {
                if found.contains(&parent) && !found.contains(&item.id) {
                    found.push(item.id);
                    changed = true;
                }
            }
        }
    }
    found
}

/// Every class present in the set, in first-seen order.
pub fn classes(items: &[Annotation]) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for item in items {
        if !seen.iter().any(|c| c == &item.label) {
            seen.push(item.label.clone());
        }
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;
    use omezarr_viewer_common::{Geometry, Plane};

    fn rect(x: f64, y: f64) -> Annotation {
        Annotation::rect(x, y, x + 10.0, y + 10.0, Plane::default())
    }

    #[test]
    fn ids_are_assigned_and_never_reused() {
        let mut set = AnnotationSet::new();
        let a = set.add(rect(1.0, 1.0));
        let b = set.add(rect(2.0, 2.0));
        assert_eq!((a.id, b.id), (1, 2));
        assert!(set.remove(a.id));
        let c = set.add(rect(3.0, 3.0));
        assert_eq!(c.id, 3, "a freed id must not come back");
        assert!(!set.remove(a.id));
    }

    #[test]
    fn reading_rows_renumbers_them_and_their_parents_together() {
        // A reader numbers rows by position and points children at those
        // numbers; the set hands out its own ids and has to rewrite both.
        let rows = vec![
            Annotation {
                id: 0,
                geometry: Geometry::Point([1.0, 1.0]),
                ..Default::default()
            },
            Annotation {
                id: 1,
                geometry: Geometry::Point([2.0, 2.0]),
                parent: Some(0),
                ..Default::default()
            },
        ];
        let set = AnnotationSet::from_rows(rows, None);
        assert_eq!(set.items()[0].id, 1);
        assert_eq!(set.items()[1].id, 2);
        assert_eq!(
            set.items()[1].parent,
            Some(1),
            "the child points at the parent's *new* id"
        );
    }

    #[test]
    fn an_update_cannot_re_parent_by_omission() {
        let mut set = AnnotationSet::new();
        let parent = set.add(rect(0.0, 0.0));
        let mut child = rect(1.0, 1.0);
        child.parent = Some(parent.id);
        let child = set.add(child);

        // A client editing the shape sends the shape, with no parent on it.
        let mut edited = child.clone();
        edited.parent = None;
        edited.geometry = Geometry::Point([5.0, 5.0]);
        let stored = set.update(child.id, edited).unwrap();
        assert_eq!(
            stored.parent,
            Some(parent.id),
            "the link is the set's, not the client's"
        );
        assert!(
            matches!(stored.geometry, Geometry::Point(_)),
            "the edit landed"
        );
    }

    #[test]
    fn bounds_span_every_shape() {
        let mut set = AnnotationSet::new();
        assert!(set.bounds().is_none());
        set.add(rect(10.0, 10.0));
        set.add(Annotation {
            geometry: Geometry::Point([2.0, 30.0]),
            ..Default::default()
        });
        assert_eq!(set.bounds(), Some([2.0, 10.0, 20.0, 30.0]));
    }

    #[test]
    fn classes_are_listed_once_in_the_order_they_appear() {
        let mut set = AnnotationSet::new();
        for label in ["cell", "vessel", "cell", ""] {
            let mut item = rect(0.0, 0.0);
            item.label = label.to_string();
            set.add(item);
        }
        assert_eq!(classes(set.items()), vec!["cell", "vessel", ""]);
    }
}
