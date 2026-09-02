//! The OME-NGFF metadata structs, against files this repo did not write.
//!
//! Every other fixture in this workspace comes from `synthetic.rs` — which is
//! *this repo's writer*. So the reader has only ever been shown the exact shape
//! our own writer produces, and a field that is too strict, or a `default` that
//! is missing, would show up as the viewer refusing a perfectly good store and
//! nobody finding out until they opened one.
//!
//! The documents here are real: two from the IDR, written by `omero-zarr` and by
//! Bio-Formats, and two from the specification's own examples. `SOURCES.md`
//! beside them records the URL each came from. Three producers on purpose — the
//! risk is not that we misread our own output, it is a writer we have never seen.
//!
//! What each one is here to pin is written on the test, because a fixture whose
//! point is not stated becomes a file nobody dares to touch.

use omezarr_viewer_common::{Axis, CoordinateTransformation, DatasetMetadata};

fn document(name: &str) -> serde_json::Value {
    let path = format!("{}/tests/data/ngff/{name}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path} is not JSON: {e}"))
}

/// A 0.4 store keeps its metadata at the root of `.zattrs`.
fn v04(name: &str) -> DatasetMetadata {
    serde_json::from_value(document(name)).expect("a 0.4 .zattrs")
}

/// A 0.5 store keeps it under `ome` in the group's `zarr.json` attributes —
/// the same fields, one level down. Both paths exist in `parse_multiscales`,
/// and until now only the first was ever exercised.
fn v05(name: &str) -> DatasetMetadata {
    let ome = document(name)["attributes"]["ome"].clone();
    serde_json::from_value(ome).expect("a 0.5 zarr.json")
}

fn axis<'a>(metadata: &'a DatasetMetadata, name: &str) -> &'a Axis {
    metadata.multiscales[0]
        .axes
        .iter()
        .find(|a| a.name == name)
        .unwrap_or_else(|| panic!("no {name} axis"))
}

#[test]
fn a_real_omero_zarr_export_reads() {
    let metadata = v04("idr0062A-0.4.zattrs.json");
    assert_eq!(metadata.multiscales.len(), 1);
    assert_eq!(metadata.multiscales[0].datasets.len(), 3, "three levels");

    // The case the synthetic fixtures never produce: a **channel axis carries no
    // `unit`**, because a channel is not a length. An `Axis` that required one
    // would refuse this file, and this file is what the IDR serves.
    assert_eq!(axis(&metadata, "c").unit, None);
    assert_eq!(axis(&metadata, "c").axis_type.as_deref(), Some("channel"));
    assert_eq!(axis(&metadata, "x").unit.as_deref(), Some("micrometer"));

    // The scale is what places a level in the world, so a level whose transform
    // did not parse is a layer drawn at the wrong size.
    let CoordinateTransformation::Scale { scale } = &metadata.multiscales[0].datasets[0]
        .coordinate_transformations
        .as_ref()
        .expect("level 0 has a transform")[0]
    else {
        panic!("level 0's transform is not a scale");
    };
    assert_eq!(scale.len(), 4, "one factor per axis");
    assert_eq!(scale[0], 1.0, "no scaling along c");

    // `_creator` is a member nothing here models; unknown keys must be ignored
    // rather than refused, or every producer's extras become a parse failure.
    assert!(document("idr0062A-0.4.zattrs.json")
        .get("_creator")
        .is_some());

    let omero = metadata.omero.expect("this export carries omero settings");
    assert_eq!(omero.channels.len(), 2);
    let window = omero.channels[0].window.as_ref().expect("a window");
    assert!(window.start <= window.end && window.min <= window.max);
}

#[test]
fn a_bio_formats_export_reads_though_nothing_here_wrote_it() {
    // A second producer, and a different shape: `unit: "pixel"` where the other
    // says micrometer, and a `metadata` member on the multiscale that nothing
    // here models.
    let metadata = v04("bioformats2raw-series-0.4.zattrs.json");
    assert_eq!(metadata.multiscales.len(), 1);
    assert_eq!(axis(&metadata, "x").unit.as_deref(), Some("pixel"));
    assert_eq!(axis(&metadata, "t").unit, None, "time, unitless here");
    let omero = metadata
        .omero
        .expect("Bio-Formats writes rendering settings too, in its own shape");
    assert_eq!(omero.channels.len(), 3);
    assert!(
        document("bioformats2raw-series-0.4.zattrs.json")["multiscales"][0]
            .get("metadata")
            .is_some(),
        "and it carries a member we ignore"
    );
}

#[test]
fn the_specifications_own_0_5_example_reads_from_under_ome() {
    let metadata = v05("spec-0.5-multiscales.json");
    assert_eq!(metadata.multiscales.len(), 1);
    assert_eq!(metadata.multiscales[0].axes.len(), 5, "t, c, z, y, x");
    assert_eq!(metadata.multiscales[0].name.as_deref(), Some("example"));
    assert_eq!(metadata.multiscales[0].datasets.len(), 3);
}

#[test]
fn a_multiscale_level_transformation_is_read_and_is_not_the_datasets_own() {
    // Every synthetic fixture puts its scale on the dataset, so the *other*
    // place the spec allows one had never been read. A transform that is
    // dropped puts every level at the wrong size — which looks like a
    // calibration mistake in the data rather than a parsing bug here.
    let metadata = v05("spec-0.5-transformations.json");
    let multiscale = &metadata.multiscales[0];

    let CoordinateTransformation::Scale { scale: per_dataset } = &multiscale.datasets[0]
        .coordinate_transformations
        .as_ref()
        .expect("the dataset has one")[0]
    else {
        panic!("expected a scale");
    };
    assert_eq!(per_dataset, &vec![1.0, 1.0]);

    let CoordinateTransformation::Scale { scale: for_all } = &multiscale
        .coordinate_transformations
        .as_ref()
        .expect("and so does the multiscale — this is the bug this file found")[0]
    else {
        panic!("expected a scale");
    };
    assert_eq!(for_all, &vec![10.0, 10.0]);

    // Stated rather than asserted about the renderer, because the renderer does
    // not do it yet: the pixel size of this image is **ten**, not one, and the
    // viewer currently reads only the dataset's own transform. A store written
    // this way is drawn ten times too small and says nothing.
    assert_ne!(
        per_dataset, for_all,
        "if these ever match, this fixture has stopped testing anything"
    );
}

/// A **known limitation**, pinned rather than fixed.
///
/// `bioformats2raw` — the converter most microscopy pipelines actually run —
/// writes a container whose root holds only `{"bioformats2raw.layout": 3}`, with
/// each series in a numbered subgroup. Pointing this viewer at such a root
/// finds no `multiscales` and fails.
///
/// `info_roi.md` describes the key; no code reads it. The test asserts today's
/// behaviour so that supporting the layout is a deliberate change with a test
/// that flips, rather than something that quietly starts working.
#[test]
fn a_bioformats2raw_container_root_is_not_an_image_and_says_so() {
    let root = document("bioformats2raw-root-0.4.zattrs.json");
    assert_eq!(
        root.as_object().map(|o| o.len()),
        Some(1),
        "the root carries the layout key and nothing else: {root}"
    );
    assert!(root.get("bioformats2raw.layout").is_some());
    assert!(
        serde_json::from_value::<DatasetMetadata>(root).is_err(),
        "there is no image here; the image is in the series subgroups"
    );

    // And the series inside it is an ordinary image, which is what a reader
    // following the layout would land on.
    assert_eq!(
        v04("bioformats2raw-series-0.4.zattrs.json")
            .multiscales
            .len(),
        1
    );
}
