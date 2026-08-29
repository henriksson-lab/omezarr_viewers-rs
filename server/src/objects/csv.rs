//! CSV detections — `blockflow`'s YOLO output and anything shaped like it.
//!
//! `blockflow::yolo` writes `id,x,y,confidence,class`. Nothing here is specific
//! to that: the position columns are found **by name**, every other numeric
//! column is kept, and a file with a `z` column is 3D while one without is a
//! plane. A column that is not numeric is dropped with a log line rather than
//! guessed at — a class *name* is a thing this viewer will want eventually, and
//! inventing an encoding for it now would be the wrong place to decide.

use anyhow::{bail, Context, Result};

use super::{ColumnData, NamedColumn, ObjectStore};

/// Names a position column may go by, most specific first.
const X_NAMES: &[&str] = &["x", "centroid_x", "pos_x", "x_um", "col"];
const Y_NAMES: &[&str] = &["y", "centroid_y", "pos_y", "y_um", "row"];
const Z_NAMES: &[&str] = &["z", "centroid_z", "pos_z", "z_um", "slice", "plane"];

pub fn read(bytes: &[u8], tab_separated: bool) -> Result<ObjectStore> {
    let mut reader = ::csv::ReaderBuilder::new()
        .delimiter(if tab_separated { b'\t' } else { b',' })
        .flexible(false)
        .from_reader(bytes);

    let headers: Vec<String> = reader
        .headers()
        .context("reading the header row")?
        .iter()
        .map(|h| h.trim().to_string())
        .collect();
    if headers.is_empty() {
        bail!("this file has no header row, so its columns have no names");
    }

    let x = find(&headers, X_NAMES);
    let y = find(&headers, Y_NAMES);
    let z = find(&headers, Z_NAMES);
    let (Some(x), Some(y)) = (x, y) else {
        bail!(
            "no x/y columns among [{}] — a position is what makes a row an object",
            headers.join(", ")
        );
    };

    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut raw: Vec<Vec<Option<f64>>> = vec![Vec::new(); headers.len()];

    for (line, record) in reader.records().enumerate() {
        let record = record.with_context(|| format!("reading row {}", line + 2))?;
        let parse = |index: usize| -> Option<f64> {
            record.get(index).and_then(|v| v.trim().parse::<f64>().ok())
        };
        let (Some(px), Some(py)) = (parse(x), parse(y)) else {
            // A row without a position is not an object; skipping it is the
            // only reading that does not invent one.
            continue;
        };
        let pz = z.and_then(parse).unwrap_or(0.0);
        positions.push([pz as f32, py as f32, px as f32]);
        for (index, cell) in raw.iter_mut().enumerate() {
            cell.push(record.get(index).and_then(|v| v.trim().parse::<f64>().ok()));
        }
    }

    let mut columns = Vec::new();
    for (index, name) in headers.iter().enumerate() {
        if Some(index) == Some(x) || Some(index) == Some(y) || Some(index) == z {
            continue;
        }
        let values = &raw[index];
        if values.iter().all(|v| v.is_none()) {
            log::info!("column `{name}` holds no numbers and is not shown");
            continue;
        }
        let integral = values
            .iter()
            .flatten()
            .all(|v| v.fract() == 0.0 && *v >= 0.0 && *v <= u64::MAX as f64);
        let data = if integral {
            ColumnData::U64(values.iter().map(|v| v.unwrap_or(0.0) as u64).collect())
        } else {
            ColumnData::F64(values.iter().map(|v| v.unwrap_or(f64::NAN)).collect())
        };
        columns.push(NamedColumn {
            name: name.clone(),
            data,
        });
    }

    ObjectStore::new(positions, columns, z.is_some())
}

fn find(headers: &[String], names: &[&str]) -> Option<usize> {
    for name in names {
        if let Some(index) = headers
            .iter()
            .position(|header| header.eq_ignore_ascii_case(name))
        {
            return Some(index);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_yolo_detection_file() {
        // The exact header `blockflow::yolo` writes.
        let text = "id,x,y,confidence,class\n1,10.5,20.5,0.9,0\n2,30.0,40.0,0.5,1\n";
        let store = read(text.as_bytes(), false).expect("read");
        assert_eq!(store.len(), 2);
        assert_eq!(store.world_position(0).unwrap(), [0.0, 20.5, 10.5]);
        let names: Vec<&str> = store.columns().iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["id", "confidence", "class"]);
        assert_eq!(store.columns()[0].data.kind(), "u64", "ids stay exact");
        assert_eq!(store.columns()[1].data.kind(), "f64");
        assert!(!store.schema().has_z, "a 2D detector's rows have no z");
    }

    #[test]
    fn a_z_column_makes_the_set_three_dimensional() {
        let text = "x,y,z,size\n1,2,3,10\n";
        let store = read(text.as_bytes(), false).expect("read");
        assert!(store.schema().has_z);
        assert_eq!(store.world_position(0).unwrap(), [3.0, 2.0, 1.0]);
    }

    #[test]
    fn a_file_without_a_position_is_refused_by_name() {
        let text = "id,confidence\n1,0.5\n";
        let err = read(text.as_bytes(), false).expect_err("refused");
        assert!(format!("{err}").contains("no x/y columns"), "{err}");
    }

    #[test]
    fn non_numeric_columns_are_dropped_rather_than_guessed_at() {
        let text = "x,y,label\n1,2,cell\n3,4,debris\n";
        let store = read(text.as_bytes(), false).expect("read");
        assert!(store.columns().is_empty());
        assert_eq!(store.len(), 2);
    }
}
