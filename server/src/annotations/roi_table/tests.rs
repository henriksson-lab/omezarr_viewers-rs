//! Round trips through the writer, and reads of tables it did not write.
//!
//! The AnnData and Parquet cases build their fixtures here rather than checking
//! in binaries, so what the reader is being asked to cope with is visible in the
//! test rather than opaque in `tests/data/`.

use super::store::filesystem;
use super::*;
use omezarr_viewer_common::{Annotation, Axis, Multiscale, MultiscaleDataset, Plane};
use std::path::Path;
use zarrs::array_subset::ArraySubset;

/// A box at `(x, y)` of size `(w, h)` on plane `z`.
fn boxed(id: u64, x: f64, y: f64, w: f64, h: f64, z: i32, label: &str) -> Annotation {
    Annotation {
        id,
        label: label.to_string(),
        ..Annotation::rect(x, y, x + w, y + h, Plane::at(z, 0))
    }
}

/// A point at `(x, y)` on plane `z`.
fn dot(id: u64, x: f64, y: f64, z: i32, label: &str) -> Annotation {
    Annotation {
        id,
        label: label.to_string(),
        ..Annotation::point(x, y, Plane::at(z, 0))
    }
}

/// A minimal zarr group at `root`, so the writer has a store to sit inside.
fn store_at(root: &Path, v3: bool) {
    std::fs::create_dir_all(root).unwrap();
    if v3 {
        std::fs::write(
            root.join("zarr.json"),
            br#"{"zarr_format":3,"node_type":"group"}"#,
        )
        .unwrap();
    } else {
        std::fs::write(root.join(".zgroup"), br#"{"zarr_format":2}"#).unwrap();
    }
}

// ---------------------------------------------------------------------------
// The CSV round trip
// ---------------------------------------------------------------------------

#[test]
fn a_round_trip_returns_the_same_boxes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("image.zarr");
    store_at(&root, false);

    let rows = vec![
        dot(1, 200.0, 100.0, 3, "cell"),
        boxed(2, 20.25, 10.5, 40.0, 30.0, 0, "vessel"),
    ];
    let target = write(&root, "boxes", &rows, WorldScale::default()).unwrap();
    // What the target *parses back into*, not how it is spelled: `make_target`
    // joins with the platform's own separator, so a `tables/boxes` suffix is a
    // claim about Unix rather than about the round trip. `split_target` is what
    // actually consumes this, and it takes both separators.
    let (parsed_root, parsed_name) = split_target(&target).expect("a table target");
    assert_eq!(parsed_root, root);
    assert_eq!(parsed_name, "boxes");

    let back = read(&root, "boxes").unwrap();
    assert_eq!(back.backend, "csv");
    assert_eq!(back.rows.len(), 2);
    assert!(
        back.rows[0].is_point(),
        "a zero-size box comes back a point"
    );
    assert_eq!(back.rows[0].bounds(), Some([200.0, 100.0, 200.0, 100.0]));
    assert_eq!(back.rows[0].plane.z, 3);
    assert_eq!(back.rows[0].label, "cell");
    assert_eq!(back.rows[1].bounds(), Some([20.25, 10.5, 60.25, 40.5]));
    assert_eq!(back.rows[1].label, "vessel");
}

#[test]
fn a_world_scale_survives_the_round_trip_exactly() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("image.zarr");
    store_at(&root, false);

    let scale = WorldScale {
        voxel: [5.0, 0.325, 0.325],
        seconds: 2.5,
    };
    let mut row = boxed(1, 400.0, 200.0, 40.0, 20.0, 4, "");
    row.z_extent = 1;
    row.plane.t = 3;
    row.t_extent = 2;
    write(&root, "boxes", &[row], scale).unwrap();

    // The file itself is in the declared units…
    let csv = std::fs::read_to_string(root.join("tables/boxes/table.csv")).unwrap();
    assert!(csv.contains("130"), "400 px * 0.325 = 130 um in x: {csv}");
    assert!(csv.contains("65"), "200 px * 0.325 = 65 um in y: {csv}");
    assert!(csv.contains("7.5"), "frame 3 * 2.5 s = 7.5 s: {csv}");

    // …and reading it back undoes exactly that, because the factors were
    // written down rather than assumed.
    let back = read(&root, "boxes").unwrap();
    assert_eq!(back.scale, scale);
    assert_eq!(back.rows[0].bounds(), Some([400.0, 200.0, 440.0, 220.0]));
    assert_eq!(back.rows[0].plane.z, 4);
    assert_eq!(back.rows[0].z_extent, 1);
    assert_eq!(back.rows[0].plane.t, 3);
    assert_eq!(back.rows[0].t_extent, 2);
}

#[test]
fn the_table_group_declares_the_ngio_attributes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("image.zarr");
    store_at(&root, false);
    write(&root, "boxes", &[], WorldScale::default()).unwrap();

    let attrs: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("tables/boxes/.zattrs")).unwrap()).unwrap();
    assert_eq!(attrs["type"], "roi_table");
    assert_eq!(attrs["table_version"], "1");
    assert_eq!(attrs["backend"], "csv");
    assert_eq!(attrs["index_key"], "FieldIndex");
    assert_eq!(attrs["omezarr_viewer"]["world_seconds_per_frame"], 1.0);

    let index: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("tables/.zattrs")).unwrap()).unwrap();
    assert_eq!(index["tables"], serde_json::json!(["boxes"]));
}

#[test]
fn a_second_table_does_not_erase_the_first_from_the_index() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("image.zarr");
    store_at(&root, false);
    write(&root, "first", &[], WorldScale::default()).unwrap();
    write(&root, "second", &[], WorldScale::default()).unwrap();
    // And rewriting one must not duplicate its name.
    write(&root, "first", &[], WorldScale::default()).unwrap();
    assert_eq!(list(&root).unwrap(), vec!["first", "second"]);
}

#[test]
fn a_v3_store_gets_v3_table_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("image.zarr");
    store_at(&root, true);
    write(&root, "boxes", &[], WorldScale::default()).unwrap();
    assert!(root.join("tables/zarr.json").exists());
    assert!(root.join("tables/boxes/zarr.json").exists());
    assert!(!root.join("tables/boxes/.zattrs").exists());
    // And it still reads back, because the reader asks zarrs, not the path.
    assert_eq!(list(&root).unwrap(), vec!["boxes"]);
}

#[test]
fn an_roi_table_without_position_columns_is_refused_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("image.zarr");
    store_at(&root, false);
    // The group declares `roi_table`, so the columns are not optional.
    write(&root, "boxes", &[], WorldScale::default()).unwrap();
    std::fs::write(root.join("tables/boxes/table.csv"), b"id,area\n1,42\n").unwrap();

    let error = read(&root, "boxes").unwrap_err().to_string();
    assert!(error.contains("not an ROI table"), "{error}");
    // And it says what it *did* find, so the next question is answerable.
    assert!(error.contains("area"), "{error}");
}

#[test]
fn a_feature_table_opens_with_no_geometry_and_keeps_its_link() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("image.zarr");
    store_at(&root, false);
    foreign_table(
        &root,
        "features",
        "csv",
        "table.csv",
        b"label,area,intensity_mean,cell_type\n1,120.5,33.0,tumour\n2,80.25,41.5,stroma\n",
    );
    // Declared a feature table, keyed to a label image.
    std::fs::write(
        root.join("tables/features/.zattrs"),
        br#"{"type":"feature_table","table_version":"1","backend":"csv",
             "region":{"path":"../labels/nuclei"},"instance_key":"label"}"#,
    )
    .unwrap();

    let table = read(&root, "features").unwrap();
    assert!(
        !table.is_geometry(),
        "a feature table carries no coordinates at all"
    );
    assert_eq!(table.table_type, "feature_table");
    let region = table.region.as_ref().expect("the label image it describes");
    assert_eq!(region.path, "../labels/nuclei");
    assert_eq!(region.instance_key, "label");

    // The schema is what a table view shows.
    let info = table.info(10);
    assert_eq!(info.rows, 2);
    let names: Vec<&str> = info.columns.iter().map(|c| c.name.as_str()).collect();
    // The file's own order, not a sorted map's.
    assert_eq!(names, vec!["label", "area", "intensity_mean", "cell_type"]);
    assert!(info
        .columns
        .iter()
        .find(|c| c.name == "area")
        .unwrap()
        .is_number());
    assert!(!info
        .columns
        .iter()
        .find(|c| c.name == "cell_type")
        .unwrap()
        .is_number());
    assert_eq!(
        info.columns
            .iter()
            .find(|c| c.name == "area")
            .unwrap()
            .range,
        Some([80.25, 120.5])
    );
    assert_eq!(info.preview.len(), 2);

    // And the join: ids from `instance_key`, values from the named column.
    let (ids, values) = table.column_by_label("area").expect("a numeric column");
    assert_eq!(ids, vec![1, 2]);
    assert_eq!(values, vec![120.5, 80.25]);
    assert!(
        table.column_by_label("cell_type").is_none(),
        "a text column cannot colour a label image"
    );
}

#[test]
fn a_spatial_omics_table_takes_its_positions_from_obsm() {
    use zarrs::array::{ArrayBuilder, DataType, FillValue};

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("image.zarr");
    store_at(&root, false);
    let store = filesystem(&root).unwrap();

    group_for(
        store.clone(),
        false,
        "/tables",
        serde_json::json!({ "tables": ["spots"] }),
    )
    .unwrap()
    .store_metadata()
    .unwrap();
    group_for(
        store.clone(),
        false,
        "/tables/spots",
        serde_json::json!({ "backend": "anndata_v1", "encoding-type": "anndata" }),
    )
    .unwrap()
    .store_metadata()
    .unwrap();

    // A scverse table: counts in X, and positions in obsm["spatial"] — there
    // are no `*_micrometer` columns anywhere.
    let numbers = |path: &str, shape: Vec<u64>, values: &[f64]| {
        let array = ArrayBuilder::new(
            shape.clone(),
            DataType::Float64,
            shape.clone().try_into().unwrap(),
            FillValue::from(0.0f64),
        )
        .build(store.clone(), path)
        .unwrap();
        array.store_metadata().unwrap();
        array
            .store_array_subset_elements::<f64>(&ArraySubset::new_with_shape(shape), values)
            .unwrap();
    };
    numbers("/tables/spots/X", vec![3, 1], &[7.0, 8.0, 9.0]);
    numbers(
        "/tables/spots/obsm/spatial",
        vec![3, 2],
        &[10.0, 20.0, 30.0, 40.0, 50.0, 60.0],
    );
    let names = ["gene_a"];
    let array = ArrayBuilder::new(
        vec![1],
        DataType::String,
        vec![1].try_into().unwrap(),
        FillValue::from(""),
    )
    .build(store.clone(), "/tables/spots/var/_index")
    .unwrap();
    array.store_metadata().unwrap();
    array
        .store_array_subset_elements::<String>(
            &ArraySubset::new_with_shape(vec![1]),
            &names.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
        )
        .unwrap();

    let table = read(&root, "spots").unwrap();
    assert!(table.from_obsm, "the positions came from obsm[\"spatial\"]");
    assert_eq!(table.rows.len(), 3);
    // (x, y) in scanpy's order, taken as world pixels without a scale — an
    // `obsm` array is already in the image's own pixels.
    assert_eq!(table.rows[0].bounds(), Some([10.0, 20.0, 10.0, 20.0]));
    assert_eq!(table.rows[2].bounds(), Some([50.0, 60.0, 50.0, 60.0]));
    assert!(table.rows[0].is_point());
}

#[test]
fn an_unknown_backend_is_refused_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("image.zarr");
    store_at(&root, false);
    write(&root, "boxes", &[], WorldScale::default()).unwrap();
    std::fs::write(
        root.join("tables/boxes/.zattrs"),
        br#"{"type":"roi_table","backend":"feather"}"#,
    )
    .unwrap();
    let error = read(&root, "boxes").unwrap_err().to_string();
    assert!(error.contains("feather"), "{error}");
}

// ---------------------------------------------------------------------------
// Backends this viewer reads but does not write
// ---------------------------------------------------------------------------

/// The rows every foreign-backend fixture below encodes, so the three reads can
/// be compared against one expectation.
fn fixture_rows() -> Vec<(f64, f64, f64, f64, f64, f64, &'static str)> {
    vec![
        (10.0, 20.0, 1.0, 30.0, 40.0, 2.0, "cell"),
        (50.0, 60.0, 0.0, 0.0, 0.0, 0.0, "spot"),
    ]
}

fn assert_fixture(table: &RoiTable, backend: &str) {
    assert_eq!(table.backend, backend);
    assert_eq!(table.rows.len(), 2, "{backend}");
    assert_eq!(
        table.rows[0].bounds(),
        Some([10.0, 20.0, 40.0, 60.0]),
        "{backend} row 0"
    );
    assert_eq!(table.rows[0].plane.z, 1, "{backend} row 0");
    assert_eq!(table.rows[0].z_extent, 2, "{backend} row 0");
    assert_eq!(table.rows[0].label, "cell", "{backend} row 0");
    assert_eq!(
        table.rows[1].bounds(),
        Some([50.0, 60.0, 50.0, 60.0]),
        "{backend} row 1"
    );
    assert!(table.rows[1].is_point(), "{backend} row 1 is a point");
    assert_eq!(table.rows[1].label, "spot", "{backend} row 1");
}

/// Write a table group by hand, declaring `backend`, with `payload` inside it.
fn foreign_table(root: &Path, name: &str, backend: &str, payload: &str, bytes: &[u8]) {
    let group = root.join("tables").join(name);
    std::fs::create_dir_all(&group).unwrap();
    std::fs::write(root.join("tables/.zgroup"), br#"{"zarr_format":2}"#).unwrap();
    std::fs::write(
        root.join("tables/.zattrs"),
        format!(r#"{{"tables":["{name}"]}}"#).as_bytes(),
    )
    .unwrap();
    std::fs::write(group.join(".zgroup"), br#"{"zarr_format":2}"#).unwrap();
    std::fs::write(
        group.join(".zattrs"),
        format!(r#"{{"type":"roi_table","table_version":"1","backend":"{backend}"}}"#).as_bytes(),
    )
    .unwrap();
    std::fs::write(group.join(payload), bytes).unwrap();
}

#[test]
fn the_json_backend_reads_an_array_of_rows() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("image.zarr");
    store_at(&root, false);
    let rows: Vec<serde_json::Value> = fixture_rows()
        .iter()
        .map(|(x, y, z, lx, ly, lz, class)| {
            serde_json::json!({
                "x_micrometer": x, "y_micrometer": y, "z_micrometer": z,
                "len_x_micrometer": lx, "len_y_micrometer": ly, "len_z_micrometer": lz,
                "class": class,
            })
        })
        .collect();
    foreign_table(
        &root,
        "foreign",
        "experimental_json_v1",
        "table.json",
        serde_json::to_string(&rows).unwrap().as_bytes(),
    );
    assert_fixture(&read(&root, "foreign").unwrap(), "json");
}

#[test]
fn the_json_backend_also_reads_an_object_of_columns() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("image.zarr");
    store_at(&root, false);
    let f = fixture_rows();
    let columns = serde_json::json!({
        "x_micrometer": f.iter().map(|r| r.0).collect::<Vec<_>>(),
        "y_micrometer": f.iter().map(|r| r.1).collect::<Vec<_>>(),
        "z_micrometer": f.iter().map(|r| r.2).collect::<Vec<_>>(),
        "len_x_micrometer": f.iter().map(|r| r.3).collect::<Vec<_>>(),
        "len_y_micrometer": f.iter().map(|r| r.4).collect::<Vec<_>>(),
        "len_z_micrometer": f.iter().map(|r| r.5).collect::<Vec<_>>(),
        "class": f.iter().map(|r| r.6).collect::<Vec<_>>(),
    });
    foreign_table(
        &root,
        "foreign",
        "json",
        "table.json",
        columns.to_string().as_bytes(),
    );
    assert_fixture(&read(&root, "foreign").unwrap(), "json");
}

#[test]
fn the_parquet_backend_reads_a_table_written_by_something_else() {
    use parquet::data_type::{ByteArray, ByteArrayType, DoubleType};
    use parquet::file::properties::WriterProperties;
    use parquet::file::writer::SerializedFileWriter;
    use parquet::schema::parser::parse_message_type;

    let schema = parse_message_type(
        "message roi {
            REQUIRED DOUBLE x_micrometer;
            REQUIRED DOUBLE y_micrometer;
            REQUIRED DOUBLE z_micrometer;
            REQUIRED DOUBLE len_x_micrometer;
            REQUIRED DOUBLE len_y_micrometer;
            REQUIRED DOUBLE len_z_micrometer;
            REQUIRED BYTE_ARRAY class (UTF8);
        }",
    )
    .unwrap();

    let f = fixture_rows();
    let doubles: Vec<Vec<f64>> = vec![
        f.iter().map(|r| r.0).collect(),
        f.iter().map(|r| r.1).collect(),
        f.iter().map(|r| r.2).collect(),
        f.iter().map(|r| r.3).collect(),
        f.iter().map(|r| r.4).collect(),
        f.iter().map(|r| r.5).collect(),
    ];
    let classes: Vec<ByteArray> = f.iter().map(|r| ByteArray::from(r.6)).collect();

    let mut buffer = Vec::new();
    {
        let mut writer = SerializedFileWriter::new(
            &mut buffer,
            std::sync::Arc::new(schema),
            std::sync::Arc::new(WriterProperties::builder().build()),
        )
        .unwrap();
        let mut group = writer.next_row_group().unwrap();
        for column in &doubles {
            let mut w = group.next_column().unwrap().unwrap();
            w.typed::<DoubleType>()
                .write_batch(column, None, None)
                .unwrap();
            w.close().unwrap();
        }
        let mut w = group.next_column().unwrap().unwrap();
        w.typed::<ByteArrayType>()
            .write_batch(&classes, None, None)
            .unwrap();
        w.close().unwrap();
        group.close().unwrap();
        writer.close().unwrap();
    }

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("image.zarr");
    store_at(&root, false);
    foreign_table(&root, "foreign", "parquet", "table.parquet", &buffer);
    assert_fixture(&read(&root, "foreign").unwrap(), "parquet");
}

#[test]
fn the_anndata_backend_reads_x_plus_a_categorical_obs_column() {
    use zarrs::array::{ArrayBuilder, DataType, FillValue};

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("image.zarr");
    store_at(&root, false);
    let store = filesystem(&root).unwrap();

    // The table group, declaring AnnData, and the `tables` index beside it.
    group_for(
        store.clone(),
        false,
        "/tables",
        serde_json::json!({ "tables": ["foreign"] }),
    )
    .unwrap()
    .store_metadata()
    .unwrap();
    group_for(
        store.clone(),
        false,
        "/tables/foreign",
        serde_json::json!({
            "type": "roi_table",
            "table_version": "1",
            "backend": "anndata_v1",
            "encoding-type": "anndata",
            "encoding-version": "0.1.0",
        }),
    )
    .unwrap()
    .store_metadata()
    .unwrap();

    let f = fixture_rows();
    // ngio's normalisation: every float column lands in X, one per `var` entry.
    let names = [
        "x_micrometer",
        "y_micrometer",
        "z_micrometer",
        "len_x_micrometer",
        "len_y_micrometer",
        "len_z_micrometer",
    ];
    let mut x = Vec::new();
    for row in &f {
        x.extend_from_slice(&[row.0, row.1, row.2, row.3, row.4, row.5]);
    }
    let array = ArrayBuilder::new(
        vec![f.len() as u64, names.len() as u64],
        DataType::Float64,
        vec![f.len() as u64, names.len() as u64].try_into().unwrap(),
        FillValue::from(0.0f64),
    )
    .build(store.clone(), "/tables/foreign/X")
    .unwrap();
    array.store_metadata().unwrap();
    array
        .store_array_subset_elements::<f64>(
            &ArraySubset::new_with_shape(vec![f.len() as u64, names.len() as u64]),
            &x,
        )
        .unwrap();

    let strings = |path: &str, values: &[&str]| {
        let array = ArrayBuilder::new(
            vec![values.len() as u64],
            DataType::String,
            vec![values.len() as u64].try_into().unwrap(),
            FillValue::from(""),
        )
        .build(store.clone(), path)
        .unwrap();
        array.store_metadata().unwrap();
        array
            .store_array_subset_elements::<String>(
                &ArraySubset::new_with_shape(vec![values.len() as u64]),
                &values.iter().map(|v| (*v).to_string()).collect::<Vec<_>>(),
            )
            .unwrap();
    };

    group_for(
        store.clone(),
        false,
        "/tables/foreign/var",
        serde_json::json!({ "encoding-type": "dataframe" }),
    )
    .unwrap()
    .store_metadata()
    .unwrap();
    strings("/tables/foreign/var/_index", &names);

    // `class` as pandas stores a categorical: categories plus codes into them.
    group_for(
        store.clone(),
        false,
        "/tables/foreign/obs",
        serde_json::json!({ "column-order": ["class"], "_index": "_index" }),
    )
    .unwrap()
    .store_metadata()
    .unwrap();
    strings("/tables/foreign/obs/_index", &["roi_1", "roi_2"]);
    group_for(
        store.clone(),
        false,
        "/tables/foreign/obs/class",
        serde_json::json!({ "encoding-type": "categorical" }),
    )
    .unwrap()
    .store_metadata()
    .unwrap();
    strings("/tables/foreign/obs/class/categories", &["cell", "spot"]);
    let codes = ArrayBuilder::new(
        vec![2],
        DataType::Int8,
        vec![2].try_into().unwrap(),
        FillValue::from(-1i8),
    )
    .build(store.clone(), "/tables/foreign/obs/class/codes")
    .unwrap();
    codes.store_metadata().unwrap();
    codes
        .store_array_subset_elements::<i8>(&ArraySubset::new_with_shape(vec![2]), &[0, 1])
        .unwrap();

    assert_fixture(&read(&root, "foreign").unwrap(), "anndata");
}

// ---------------------------------------------------------------------------
// Targets and scales
// ---------------------------------------------------------------------------

#[test]
fn a_target_splits_into_store_and_name() {
    let (root, name) = split_target("file:///data/image.zarr/tables/boxes").unwrap();
    assert_eq!(root, Path::new("/data/image.zarr"));
    assert_eq!(name, "boxes");
    assert!(split_target("/data/image.zarr/boxes").is_err());

    // A remote target keeps its scheme, which is what tells the two write paths
    // apart.
    let (store, name) = split_uri_target("s3://bucket/run/image.zarr/tables/boxes").unwrap();
    assert_eq!(store, "s3://bucket/run/image.zarr");
    assert_eq!(name, "boxes");
    assert!(is_remote("s3://bucket/x"));
    assert!(!is_remote("/data/image.zarr/tables/boxes"));
}

#[test]
fn a_windows_target_splits_the_same_way() {
    // Spelled with backslashes on every platform, so the case that only Windows
    // CI can produce is one Linux can fail on too. `make_target` joins with the
    // platform separator, and a test that asserted a `tables/name` suffix was a
    // claim about Unix rather than about the round trip — which is exactly how
    // this was found.
    let (root, name) = split_target(r"C:\data\image.zarr\tables\boxes").unwrap();
    assert_eq!(root, Path::new(r"C:\data\image.zarr"));
    assert_eq!(name, "boxes");

    // A trailing separator is a target somebody typed, not a different table.
    assert_eq!(
        split_target(r"C:\data\image.zarr\tables\boxes\").unwrap().1,
        "boxes"
    );

    // And the parent still has to be `tables`.
    assert!(split_target(r"C:\data\image.zarr\boxes").is_err());
}

#[test]
fn world_scale_comes_from_the_level_0_transformation() {
    let metadata = DatasetMetadata {
        multiscales: vec![Multiscale {
            axes: vec![
                Axis {
                    name: "t".into(),
                    axis_type: Some("time".into()),
                    unit: Some("second".into()),
                },
                Axis {
                    name: "c".into(),
                    axis_type: Some("channel".into()),
                    unit: None,
                },
                Axis {
                    name: "z".into(),
                    axis_type: Some("space".into()),
                    unit: None,
                },
                Axis {
                    name: "y".into(),
                    axis_type: Some("space".into()),
                    unit: None,
                },
                Axis {
                    name: "x".into(),
                    axis_type: Some("space".into()),
                    unit: None,
                },
            ],
            datasets: vec![MultiscaleDataset {
                path: "0".into(),
                coordinate_transformations: Some(vec![CoordinateTransformation::Scale {
                    scale: vec![30.0, 1.0, 5.0, 0.5, 0.5],
                }]),
            }],
            name: None,
        }],
        omero: None,
    };
    let scale = world_scale(&metadata);
    assert_eq!(scale.voxel, [5.0, 0.5, 0.5]);
    assert_eq!(scale.seconds, 30.0);

    // A store that says nothing gets pixels and frames, stated as 1.
    let silent = DatasetMetadata {
        multiscales: vec![Multiscale {
            axes: vec![],
            datasets: vec![],
            name: None,
        }],
        omero: None,
    };
    assert_eq!(world_scale(&silent), WorldScale::default());
}
