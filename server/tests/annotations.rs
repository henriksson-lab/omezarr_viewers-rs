//! Annotations, from a click to a file and back into a session.
//!
//! The claim these tests make is the one the feature rests on: boxes drawn over
//! an image, written into that image's own store as an ngio ROI table, come
//! back out of a *fresh session* at the same world coordinates. Anything that
//! quietly rescales — a voxel size applied once on the way out and not undone
//! on the way in, a `len_*` column read as a max corner — moves them, and the
//! second half of each test is where that shows.

use omezarr_viewer_common::{Annotation, Plane, WorldScale};
use omezarr_viewer_server::annotations::{roi_table, AnnotationSet};
use omezarr_viewer_server::objects::ObjectSpace;
use omezarr_viewer_server::session::{LayerRole, Session};
use omezarr_viewer_server::source::{SourceRegistry, SourceSpec};
use omezarr_viewer_server::synthetic;

const SHAPE: (u64, u64, u64) = (8, 128, 128);

/// A synthetic image store, the thing annotations are drawn over.
fn image_store() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("image.zarr");
    let blobs = synthetic::blobs(SHAPE, 3);
    synthetic::write_image(&path, SHAPE, &blobs).expect("write image");
    (dir, path)
}

fn point(z: i32, y: f64, x: f64, class: &str) -> Annotation {
    Annotation {
        label: class.to_string(),
        ..Annotation::point(x, y, Plane::at(z, 0))
    }
}

/// A box at `(x, y)` of size `(w, h)`, on plane `z` spanning `dz` further.
fn boxed(z: i32, y: f64, x: f64, dz: u32, h: f64, w: f64, class: &str) -> Annotation {
    Annotation {
        label: class.to_string(),
        z_extent: dz,
        ..Annotation::rect(x, y, x + w, y + h, Plane::at(z, 0))
    }
}

#[actix_web::test]
async fn boxes_and_points_survive_a_save_and_a_fresh_session() {
    let (_dir, store) = image_store();
    let registry = SourceRegistry::new();

    // Draw over the image, as clicks would.
    let mut session = Session::new();
    session
        .add(
            &registry,
            SourceSpec::File(store.clone()),
            LayerRole::Image,
            None,
            ObjectSpace::default(),
        )
        .await
        .expect("open image");
    let layer = session.add_annotations(Some("drawn".into()), AnnotationSet::new());

    let drawn = {
        let set = session.annotations_mut(&layer).expect("annotation layer");
        vec![
            set.add(point(3, 40.0, 60.0, "cell")),
            set.add(boxed(1, 10.0, 20.0, 2, 30.0, 40.0, "region")),
            // Drawn right-to-left, as half of all drags are: `Geometry::rect`
            // puts the corners the right way round.
            set.add(Annotation {
                ..Annotation::rect(100.0, 100.0, 75.0, 80.0, Plane::default())
            }),
        ]
    };
    assert_eq!(
        drawn[2].bounds(),
        Some([75.0, 80.0, 100.0, 100.0]),
        "a backwards drag"
    );

    // Save into the image's own store, at the scale the image declares.
    let scale = session
        .reference_dataset()
        .map(roi_table::world_scale_of)
        .unwrap_or_default();
    let rows: Vec<Annotation> = session
        .get(&layer)
        .and_then(|l| l.data.annotations())
        .expect("annotations")
        .items()
        .to_vec();
    let target = roi_table::write(&store, "drawn", &rows, scale).expect("write roi table");

    // The table is listed where a reader would look for it.
    assert_eq!(roi_table::list(&store).expect("list"), vec!["drawn"]);

    // A fresh session, opening the table by its target and nothing else.
    let mut reopened = Session::new();
    let read = reopened
        .add(
            &registry,
            SourceSpec::parse(&target).expect("target parses"),
            LayerRole::Annotations,
            None,
            ObjectSpace::default(),
        )
        .await
        .unwrap_or_else(|e| panic!("reopening {target}: {e:#}"));

    let back = reopened
        .get(&read)
        .and_then(|l| l.data.annotations())
        .expect("annotations came back");
    assert_eq!(back.len(), 3);
    assert_eq!(back.target(), Some(target.as_str()));

    for (before, after) in drawn.iter().zip(back.items()) {
        assert_eq!(before.bounds(), after.bounds(), "bounds of {before:?}");
        assert_eq!(before.plane.z, after.plane.z, "plane of {before:?}");
        assert_eq!(before.z_extent, after.z_extent, "depth of {before:?}");
        assert_eq!(before.label, after.label, "class of {before:?}");
        assert_eq!(
            before.is_point(),
            after.is_point(),
            "a point must not become a box, or the reverse"
        );
    }
}

#[actix_web::test]
async fn a_declared_voxel_size_does_not_move_anything() {
    let (_dir, store) = image_store();

    // A store whose pixels are 0.325 um across writes 0.325-um numbers, and
    // reading them back has to undo exactly that — this is the failure mode a
    // round trip through pixels alone would never catch.
    let scale = WorldScale {
        voxel: [4.0, 0.325, 0.325],
        seconds: 1.0,
    };
    let rows = vec![
        boxed(2, 64.0, 32.0, 1, 16.0, 8.0, "region"),
        point(0, 1.0, 1.0, "corner"),
    ];
    roi_table::write(&store, "scaled", &rows, scale).expect("write");

    let read = roi_table::read(&store, "scaled").expect("read");
    assert_eq!(read.scale, scale, "the factors are recorded, not assumed");
    for (before, after) in rows.iter().zip(&read.rows) {
        assert_eq!(before.bounds(), after.bounds());
        assert_eq!(before.plane.z, after.plane.z);
        assert_eq!(before.z_extent, after.z_extent);
    }
}

#[actix_web::test]
async fn a_saved_table_sits_beside_labels_and_leaves_the_image_alone() {
    let (_dir, store) = image_store();

    let before: Vec<String> = std::fs::read_dir(&store)
        .expect("read store")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();

    roi_table::write(
        &store,
        "drawn",
        &[point(0, 1.0, 2.0, "x")],
        WorldScale::default(),
    )
    .expect("write");

    let after: Vec<String> = std::fs::read_dir(&store)
        .expect("read store")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .filter(|name| !before.contains(name))
        .collect();
    assert_eq!(
        after,
        vec!["tables".to_string()],
        "saving must add `tables` and touch nothing else"
    );
    assert!(store.join("tables/drawn/table.csv").is_file());
}

#[actix_web::test]
async fn a_table_that_is_not_an_roi_table_is_refused_by_name() {
    let (_dir, store) = image_store();
    roi_table::write(&store, "drawn", &[], WorldScale::default()).expect("write");
    // A table group with the right attributes but somebody else's columns.
    std::fs::write(store.join("tables/drawn/table.csv"), b"id,area\n1,42\n")
        .expect("overwrite payload");

    let error = roi_table::read(&store, "drawn").unwrap_err().to_string();
    assert!(error.contains("not an ROI table"), "{error}");
}

#[actix_web::test]
async fn opening_something_that_is_not_a_table_says_so() {
    let (_dir, store) = image_store();
    let registry = SourceRegistry::new();
    let mut session = Session::new();

    // The store itself, not a table inside it.
    let error = session
        .add(
            &registry,
            SourceSpec::File(store.clone()),
            LayerRole::Annotations,
            None,
            ObjectSpace::default(),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("tables"), "{error}");
    assert!(session.is_empty(), "a failed open must add no layer");
}

#[actix_web::test]
async fn an_annotation_layer_reports_itself_in_the_session() {
    let mut session = Session::new();
    let layer = session.add_annotations(Some("drawn".into()), AnnotationSet::new());
    session
        .annotations_mut(&layer)
        .expect("layer")
        .add(point(0, 5.0, 5.0, "cell"));

    let info = session.info();
    assert_eq!(info.layers.len(), 1);
    assert_eq!(info.layers[0].name, "drawn");
    let omezarr_viewer_common::LayerKind::Annotations {
        annotations,
        target,
    } = &info.layers[0].kind
    else {
        panic!("not an annotation layer: {:?}", info.layers[0].kind);
    };
    assert_eq!(annotations.len(), 1);
    assert_eq!(annotations[0].label, "cell");
    assert_eq!(annotations[0].id, 1, "ids reach the client");
    assert!(target.is_none(), "an unsaved layer has nowhere to save to");
    assert_eq!(session.layers()[0].role(), "annotations");
}

#[actix_web::test]
async fn scanning_a_run_finds_the_tables_drawn_over_its_images() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();
    let store = root.join("image.zarr");
    let blobs = synthetic::blobs(SHAPE, 3);
    synthetic::write_image(&store, SHAPE, &blobs).expect("write image");
    roi_table::write(
        &store,
        "drawn",
        &[point(0, 1.0, 2.0, "cell")],
        WorldScale::default(),
    )
    .expect("write");

    let project = omezarr_viewer_server::project::Project::scan(root).expect("scan");
    let annotations: Vec<&omezarr_viewer_server::project::ProjectLayer> = project
        .layers
        .iter()
        .filter(|l| l.role.as_deref() == Some("annotations"))
        .collect();
    assert_eq!(annotations.len(), 1, "layers: {:?}", project.layers);
    assert!(
        annotations[0].source.ends_with("tables/drawn"),
        "{}",
        annotations[0].source
    );

    // And it is the *only* thing the table contributed: the payload inside the
    // store must never be picked up a second time as a stray CSV.
    let objects = project
        .layers
        .iter()
        .filter(|l| l.role.as_deref() == Some("objects"))
        .count();
    assert_eq!(objects, 0, "table.csv must not also be an object layer");

    // The scanned project opens, with the annotations on it.
    let registry = SourceRegistry::new();
    let mut session = Session::new();
    project.open(&registry, &mut session).await.expect("open");
    let drawn = session
        .layers()
        .iter()
        .find(|l| l.role() == "annotations")
        .expect("an annotation layer");
    assert_eq!(drawn.data.annotations().expect("rows").len(), 1);
}

#[actix_web::test]
async fn an_unsaved_annotation_layer_stays_out_of_a_saved_project() {
    let (_dir, store) = image_store();
    let registry = SourceRegistry::new();
    let mut session = Session::new();
    session
        .add(
            &registry,
            SourceSpec::File(store.clone()),
            LayerRole::Image,
            None,
            ObjectSpace::default(),
        )
        .await
        .expect("open image");

    // One layer that has been saved somewhere, and one that has not.
    let saved = session.add_annotations(
        Some("saved".into()),
        AnnotationSet::from_rows(
            vec![point(0, 1.0, 2.0, "cell")],
            Some(roi_table::make_target(&store, "saved")),
        ),
    );
    let unsaved = session.add_annotations(Some("unsaved".into()), AnnotationSet::new());

    let project = omezarr_viewer_server::project::Project {
        name: None,
        layers: session
            .layers()
            .iter()
            .filter(|layer| !layer.spec.is_unsaved())
            .map(|layer| omezarr_viewer_server::project::ProjectLayer {
                source: layer.spec.uri(),
                role: Some(layer.role().to_string()),
                name: Some(layer.name.clone()),
                scale: layer.object_scale(),
                offset: None,
            })
            .collect(),
    };

    let names: Vec<&str> = project
        .layers
        .iter()
        .filter_map(|l| l.name.as_deref())
        .collect();
    assert!(names.contains(&"saved"), "{names:?}");
    assert!(
        !names.contains(&"unsaved"),
        "a layer with nowhere to be read from must not become a source: {names:?}"
    );
    assert!(session.get(&saved).is_some() && session.get(&unsaved).is_some());
}

#[actix_web::test]
async fn a_remote_target_is_told_apart_from_a_local_one() {
    // The two write paths are chosen by this predicate and nothing else, so it
    // is the one place a mistake would silently send a save down the wrong one.
    assert!(roi_table::is_remote("s3://bucket/run.zarr/tables/drawn"));
    assert!(roi_table::is_remote("https://host/run.zarr/tables/drawn"));
    assert!(!roi_table::is_remote("/data/run.zarr/tables/drawn"));
    assert!(!roi_table::is_remote("file:///data/run.zarr/tables/drawn"));

    let (store, name) = roi_table::split_uri_target("s3://bucket/run.zarr/tables/drawn").unwrap();
    assert_eq!(store, "s3://bucket/run.zarr");
    assert_eq!(name, "drawn");
    assert_eq!(
        roi_table::make_uri_target(&store, &name),
        "s3://bucket/run.zarr/tables/drawn"
    );

    // A profile nobody configured is refused by name rather than by panic.
    let registry = SourceRegistry::new();
    let error = roi_table::write_async(
        &registry,
        "s3://bucket/run.zarr",
        "drawn",
        &[],
        WorldScale::default(),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("profile"), "{error}");
}

#[actix_web::test]
async fn a_label_store_hands_over_what_it_says_about_each_id() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("labels.zarr");
    let blobs = synthetic::blobs(SHAPE, 3);
    synthetic::write_labels(&path, SHAPE, &blobs).expect("write labels");

    // `image-label` as the spec has it: colours and ragged per-id properties,
    // with a store that already declares itself a label image.
    let attrs_path = path.join("zarr.json");
    let mut attrs: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&attrs_path).expect("read")).expect("parse");
    let target = attrs
        .get_mut("attributes")
        .expect("attributes")
        .as_object_mut()
        .expect("object");
    target.insert(
        "image-label".into(),
        serde_json::json!({
            "version": "0.5",
            "colors": [{"label-value": 1, "rgba": [0, 128, 0, 128]}],
            "properties": [
                {"label-value": 1, "class": "cell", "area (pixels)": 1650},
                {"label-value": 2, "class": "vessel"},
            ],
        }),
    );
    std::fs::write(&attrs_path, serde_json::to_vec_pretty(&attrs).unwrap()).expect("write attrs");

    let registry = SourceRegistry::new();
    let mut session = Session::new();
    session
        .add(
            &registry,
            SourceSpec::File(path),
            LayerRole::Auto,
            None,
            ObjectSpace::default(),
        )
        .await
        .expect("open labels");

    let info = session.info();
    let omezarr_viewer_common::LayerKind::Labels {
        colors, properties, ..
    } = &info.layers[0].kind
    else {
        panic!("auto-detection did not make it a label layer");
    };
    assert_eq!(colors.as_ref().expect("colours").len(), 1);

    let properties = properties.as_ref().expect("properties");
    assert_eq!(properties.len(), 2);
    assert_eq!(properties[0].label_value, 1.0);
    assert_eq!(properties[0].fields["class"], "cell");
    assert_eq!(properties[0].fields["area (pixels)"], 1650);
    // Ragged on purpose: the spec says rows need not share keys, so the second
    // row having no area must not make it unparseable.
    assert_eq!(properties[1].fields["class"], "vessel");
    assert!(!properties[1].fields.contains_key("area (pixels)"));
}

#[actix_web::test]
async fn a_table_written_by_another_tool_opens_as_a_layer() {
    let (_dir, store) = image_store();

    // A JSON-backend table, as ngio would leave one: the group declares the
    // backend and the rows sit beside it. Nothing here went through our writer.
    let group = store.join("tables").join("foreign");
    std::fs::create_dir_all(&group).expect("mkdir");
    std::fs::write(
        store.join("tables/zarr.json"),
        br#"{"zarr_format":3,"node_type":"group","attributes":{"tables":["foreign"]}}"#,
    )
    .expect("index");
    std::fs::write(
        group.join("zarr.json"),
        br#"{"zarr_format":3,"node_type":"group","attributes":{
            "type":"roi_table","table_version":"1","backend":"experimental_json_v1"}}"#,
    )
    .expect("attrs");
    std::fs::write(
        group.join("table.json"),
        br#"[{"x_micrometer":10,"y_micrometer":20,"z_micrometer":1,
             "len_x_micrometer":30,"len_y_micrometer":40,"len_z_micrometer":0,
             "class":"cell"},
            {"x_micrometer":50,"y_micrometer":60,"z_micrometer":0,
             "len_x_micrometer":0,"len_y_micrometer":0,"len_z_micrometer":0,
             "class":"spot"}]"#,
    )
    .expect("payload");

    // It is offered by a scan…
    let listed = roi_table::list(&store).expect("list");
    assert_eq!(listed, vec!["foreign"]);

    // …and opens as an annotation layer with its rows in world coordinates.
    let registry = SourceRegistry::new();
    let mut session = Session::new();
    let id = session
        .add(
            &registry,
            SourceSpec::File(store.join("tables/foreign")),
            LayerRole::Annotations,
            None,
            ObjectSpace::default(),
        )
        .await
        .expect("open the foreign table");

    let set = session
        .get(&id)
        .and_then(|l| l.data.annotations())
        .expect("annotations");
    assert_eq!(set.len(), 2);
    assert_eq!(set.items()[0].bounds(), Some([10.0, 20.0, 40.0, 60.0]));
    assert_eq!(set.items()[0].label, "cell");
    assert!(set.items()[1].is_point());
    assert_eq!(set.items()[1].label, "spot");

    // Saving it back writes *our* backend, and the rows survive the change.
    let target =
        roi_table::write(&store, "foreign", set.items(), WorldScale::default()).expect("rewrite");
    assert!(target.ends_with("tables/foreign"));
    let back = roi_table::read(&store, "foreign").expect("read back");
    assert_eq!(back.backend, "csv", "a rewrite converts the backend");
    assert_eq!(back.rows.len(), 2);
    assert_eq!(back.rows[0].bounds(), Some([10.0, 20.0, 40.0, 60.0]));
    assert_eq!(back.rows[1].label, "spot");
}

#[actix_web::test]
async fn a_polygon_with_a_hole_survives_a_geojson_round_trip_through_a_session() {
    use omezarr_viewer_server::annotations::geojson;

    let (_dir, store) = image_store();
    let registry = SourceRegistry::new();
    let mut session = Session::new();
    let layer = session.add_annotations(Some("drawn".into()), AnnotationSet::new());

    // A ring with a hole, a freehand line, a multipoint and an ellipse — the
    // shapes an ROI table cannot hold and the reason this format exists.
    let ring = vec![
        vec![
            [0.0, 0.0],
            [100.0, 0.0],
            [100.0, 100.0],
            [0.0, 100.0],
            [0.0, 0.0],
        ],
        vec![
            [40.0, 40.0],
            [60.0, 40.0],
            [60.0, 60.0],
            [40.0, 60.0],
            [40.0, 40.0],
        ],
    ];
    let drawn = {
        let set = session.annotations_mut(&layer).expect("layer");
        vec![
            set.add(Annotation {
                geometry: omezarr_viewer_common::Geometry::Polygon(ring),
                label: "tissue".into(),
                plane: Plane::at(2, 0),
                z_extent: 4,
                ..Default::default()
            }),
            set.add(Annotation {
                geometry: omezarr_viewer_common::Geometry::LineString(vec![
                    [10.0, 10.0],
                    [20.0, 30.0],
                    [40.0, 25.0],
                ]),
                label: "trace".into(),
                ..Default::default()
            }),
            set.add(Annotation {
                geometry: omezarr_viewer_common::Geometry::MultiPoint(vec![[5.0, 5.0], [6.0, 7.0]]),
                label: "cells".into(),
                ..Default::default()
            }),
            set.add(Annotation {
                is_ellipse: true,
                label: "nucleus".into(),
                ..Annotation::rect(200.0, 200.0, 240.0, 220.0, Plane::default())
            }),
        ]
    };

    let rows: Vec<Annotation> = session
        .get(&layer)
        .and_then(|l| l.data.annotations())
        .expect("annotations")
        .items()
        .to_vec();
    let target = geojson::save(&store, "drawn", &rows).expect("save");
    assert!(target.ends_with("annotations/drawn"));
    assert!(store
        .join("annotations/drawn/annotations.geojson")
        .is_file());
    assert_eq!(geojson::list(&store).expect("list"), vec!["drawn"]);

    // A fresh session, opening the set by its target and nothing else.
    let mut reopened = Session::new();
    let read = reopened
        .add(
            &registry,
            SourceSpec::File(store.join("annotations/drawn")),
            LayerRole::Annotations,
            None,
            ObjectSpace::default(),
        )
        .await
        .unwrap_or_else(|e| panic!("reopening {target}: {e:#}"));
    let back = reopened
        .get(&read)
        .and_then(|l| l.data.annotations())
        .expect("annotations came back");

    assert_eq!(back.len(), 4);
    for (before, after) in drawn.iter().zip(back.items()) {
        assert_eq!(
            before.geometry, after.geometry,
            "geometry of {}",
            before.label
        );
        assert_eq!(before.label, after.label);
        assert_eq!(before.plane, after.plane);
        assert_eq!(before.z_extent, after.z_extent);
        assert_eq!(before.is_ellipse, after.is_ellipse);
    }
    // The hole is still a hole after the round trip, which no ROI table could
    // have managed.
    assert!(!back.items()[0].contains(50.0, 50.0, 0.0));
    assert!(back.items()[0].contains(10.0, 10.0, 0.0));
}

#[actix_web::test]
async fn a_bare_qupath_export_opens_as_a_layer() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("export.geojson");
    std::fs::write(
        &path,
        br#"{"type":"FeatureCollection","features":[
          {"type":"Feature","id":"abc",
           "geometry":{"type":"Polygon","coordinates":[[[0,0],[9,0],[9,9],[0,9],[0,0]]],
                       "plane":{"c":-1,"z":2,"t":0}},
           "properties":{"objectType":"annotation",
                         "classification":{"name":"Tumor","color":[200,0,0]}}}]}"#,
    )
    .expect("write");

    let registry = SourceRegistry::new();
    let mut session = Session::new();
    let id = session
        .add(
            &registry,
            SourceSpec::File(path),
            LayerRole::Annotations,
            None,
            ObjectSpace::default(),
        )
        .await
        .expect("open the export");

    let set = session
        .get(&id)
        .and_then(|l| l.data.annotations())
        .expect("rows");
    assert_eq!(set.len(), 1);
    assert_eq!(set.items()[0].label, "Tumor");
    assert_eq!(set.items()[0].class_color, Some([200, 0, 0]));
    assert_eq!(set.items()[0].plane.z, 2);
    assert_eq!(set.items()[0].uuid.as_deref(), Some("abc"));
}

#[actix_web::test]
async fn an_roi_table_says_how_many_shapes_it_had_to_flatten() {
    use omezarr_viewer_server::annotations::roi_table::lossy_rows;

    let square = omezarr_viewer_common::Geometry::Polygon(vec![vec![
        [0.0, 0.0],
        [10.0, 0.0],
        [10.0, 10.0],
        [0.0, 10.0],
        [0.0, 0.0],
    ]]);
    let triangle = omezarr_viewer_common::Geometry::Polygon(vec![vec![
        [0.0, 0.0],
        [10.0, 0.0],
        [5.0, 10.0],
        [0.0, 0.0],
    ]]);
    let rows = vec![
        point(0, 1.0, 1.0, ""),
        Annotation {
            geometry: square,
            ..Default::default()
        },
        Annotation {
            geometry: triangle,
            ..Default::default()
        },
    ];
    // A point and an axis-aligned rectangle lose nothing; a triangle does.
    assert_eq!(lossy_rows(&rows), 1);
}

#[actix_web::test]
async fn a_qupath_cell_keeps_its_nucleus_name_and_type_through_the_viewer() {
    use omezarr_viewer_common::ObjectType;
    use omezarr_viewer_server::annotations::geojson;

    let (_dir, store) = image_store();
    let registry = SourceRegistry::new();

    // A file as QuPath's cell segmentation writes one: a membrane, a nucleus
    // beside it, a name, a type and a lock.
    let path = store.join("cells.geojson");
    std::fs::write(
        &path,
        br#"{"type":"FeatureCollection","features":[{
          "type":"Feature","id":"cell-1",
          "geometry":{"type":"Polygon",
                      "coordinates":[[[10,10],[30,10],[30,30],[10,30],[10,10]]]},
          "nucleusGeometry":{"type":"Polygon",
                      "coordinates":[[[16,16],[24,16],[24,24],[16,24],[16,16]]]},
          "properties":{"objectType":"cell","name":"Cell 1","isLocked":true,
                        "classification":{"name":"Tumor"},
                        "measurements":{"Area":123.5},
                        "metadata":{"note":"checked"}}}]}"#,
    )
    .expect("write");

    let mut session = Session::new();
    let id = session
        .add(
            &registry,
            SourceSpec::File(path),
            LayerRole::Annotations,
            None,
            ObjectSpace::default(),
        )
        .await
        .expect("open the cell file");

    let rows = session
        .get(&id)
        .and_then(|l| l.data.annotations())
        .expect("rows")
        .items()
        .to_vec();
    assert_eq!(rows.len(), 1);
    let cell = &rows[0];
    assert_eq!(cell.object_type, ObjectType::Cell);
    assert_eq!(cell.name.as_deref(), Some("Cell 1"));
    assert_eq!(
        cell.display_name(),
        "Cell 1",
        "a name identifies, a class classifies"
    );
    assert!(cell.locked);
    assert_eq!(cell.label, "Tumor");
    let nucleus = cell
        .nucleus
        .as_ref()
        .expect("the nucleus survived the open");
    assert_eq!(nucleus.bounds(), Some([16.0, 16.0, 24.0, 24.0]));

    // Saving it back reproduces every one of those.
    let target = geojson::save(&store, "cells", &rows).expect("save");
    assert!(target.ends_with("annotations/cells"));
    let back = geojson::load(&store, "cells").expect("load");
    let after = &back.rows[0];
    assert_eq!(after.object_type, cell.object_type);
    assert_eq!(after.name, cell.name);
    assert_eq!(after.locked, cell.locked);
    assert_eq!(after.nucleus, cell.nucleus);
    assert_eq!(after.measurements, cell.measurements);
    assert_eq!(after.metadata, cell.metadata);

    // And the file itself carries QuPath's own spellings, so QuPath reads it.
    let text =
        std::fs::read_to_string(store.join("annotations/cells/annotations.geojson")).expect("read");
    let doc: serde_json::Value = serde_json::from_str(&text).expect("parse");
    let feature = &doc["features"][0];
    assert_eq!(feature["properties"]["objectType"], "cell");
    assert_eq!(feature["properties"]["name"], "Cell 1");
    assert_eq!(feature["properties"]["isLocked"], true);
    assert_eq!(feature["nucleusGeometry"]["type"], "Polygon");
}

#[actix_web::test]
async fn a_shape_drawn_inside_another_becomes_its_child() {
    use omezarr_viewer_common::{in_tree_order, Geometry};

    let mut set = AnnotationSet::new();
    let region = set.add_nested(Annotation {
        geometry: Geometry::Polygon(vec![vec![
            [0.0, 0.0],
            [100.0, 0.0],
            [100.0, 100.0],
            [0.0, 100.0],
            [0.0, 0.0],
        ]]),
        label: "tissue".into(),
        ..Default::default()
    });
    assert_eq!(region.parent, None, "nothing contains the first shape");

    let inner = set.add_nested(Annotation {
        geometry: Geometry::Polygon(vec![vec![
            [10.0, 10.0],
            [50.0, 10.0],
            [50.0, 50.0],
            [10.0, 50.0],
            [10.0, 10.0],
        ]]),
        label: "gland".into(),
        ..Default::default()
    });
    assert_eq!(inner.parent, Some(region.id));

    let cell = set.add_nested(Annotation {
        geometry: Geometry::Point([20.0, 20.0]),
        label: "cell".into(),
        ..Default::default()
    });
    assert_eq!(
        cell.parent,
        Some(inner.id),
        "the smallest thing covering it"
    );

    // The list reads as a tree.
    let order: Vec<(&str, usize)> = in_tree_order(set.items())
        .into_iter()
        .map(|(a, depth)| (a.label.as_str(), depth))
        .collect();
    assert_eq!(order, vec![("tissue", 0), ("gland", 1), ("cell", 2)]);

    // Deleting the middle one lifts its children rather than taking them with
    // it: removing a region must not silently remove every cell inside it.
    assert!(set.remove(inner.id));
    assert_eq!(set.len(), 2);
    assert_eq!(
        set.items().iter().find(|a| a.id == cell.id).unwrap().parent,
        Some(region.id),
        "the cell moved up one level"
    );

    // Detaching makes it top-level, and a re-nest puts it back.
    assert!(set.detach(cell.id));
    assert_eq!(
        set.items().iter().find(|a| a.id == cell.id).unwrap().parent,
        None
    );
    set.renest();
    assert_eq!(
        set.items().iter().find(|a| a.id == cell.id).unwrap().parent,
        Some(region.id)
    );
}

#[actix_web::test]
async fn nesting_survives_a_geojson_round_trip() {
    use omezarr_viewer_server::annotations::geojson;

    let (_dir, store) = image_store();
    let mut set = AnnotationSet::new();
    let region = set.add_nested(Annotation {
        geometry: omezarr_viewer_common::Geometry::Polygon(vec![vec![
            [0.0, 0.0],
            [100.0, 0.0],
            [100.0, 100.0],
            [0.0, 100.0],
            [0.0, 0.0],
        ]]),
        label: "tissue".into(),
        ..Default::default()
    });
    let cell = set.add_nested(Annotation {
        geometry: omezarr_viewer_common::Geometry::Point([20.0, 20.0]),
        label: "cell".into(),
        ..Default::default()
    });
    assert_eq!(cell.parent, Some(region.id));

    geojson::save(&store, "nested", set.items()).expect("save");
    // The child nests inside the parent's `childObjects`, as QuPath writes it.
    let doc: serde_json::Value = serde_json::from_slice(
        &std::fs::read(store.join("annotations/nested/annotations.geojson")).expect("read"),
    )
    .expect("parse");
    assert_eq!(doc["features"].as_array().unwrap().len(), 1);
    assert_eq!(
        doc["features"][0]["properties"]["childObjects"][0]["properties"]["classification"]["name"],
        "cell"
    );

    // And it comes back a child, with the ids the new set hands out.
    let back = AnnotationSet::from_rows(geojson::load(&store, "nested").expect("load").rows, None);
    assert_eq!(back.len(), 2);
    assert_eq!(back.items()[0].parent, None);
    assert_eq!(back.items()[1].parent, Some(back.items()[0].id));
}

#[actix_web::test]
async fn a_remote_annotation_target_is_told_apart_and_addressed_correctly() {
    use omezarr_viewer_server::annotations::geojson;

    // The two write paths are chosen by these predicates and nothing else, so
    // they are the one place a mistake would send a save down the wrong one —
    // and an `s3://` target treated as a directory name fails obscurely.
    for target in [
        "s3://bucket/run/image.zarr/annotations/drawn",
        "https://host/run/image.zarr/annotations/drawn",
    ] {
        assert!(geojson::is_annotation_target(target), "{target}");
        assert!(geojson::target_is_remote(target), "{target}");
        let (store, name) = geojson::split_uri_target(target).expect(target);
        assert!(store.ends_with("image.zarr"), "{store}");
        assert_eq!(name, "drawn");
        assert_eq!(geojson::make_uri_target(&store, &name), target);
    }

    let local = "/data/run/image.zarr/annotations/drawn";
    assert!(geojson::is_annotation_target(local));
    assert!(!geojson::target_is_remote(local));
    let (root, name) = geojson::split_target(local).expect("local");
    assert_eq!(root, std::path::Path::new("/data/run/image.zarr"));
    assert_eq!(name, "drawn");

    // An ROI table target is not an annotation target, and the reverse.
    assert!(!geojson::is_annotation_target(
        "/data/run/image.zarr/tables/boxes"
    ));
    assert!(!roi_table::is_remote(local));

    // A profile nobody configured is refused by name rather than by silently
    // writing a directory called `s3:`.
    let registry = SourceRegistry::new();
    let error = geojson::save_async(&registry, "s3://bucket/image.zarr", "drawn", &[])
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("profile"), "{error}");
}
