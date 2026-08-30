//! CSV, JSON and Parquet: bytes in, columns out, and nothing else.

use anyhow::{bail, Context, Result};

use super::*;

// ---------------------------------------------------------------------------
// Backends: bytes in, columns out
// ---------------------------------------------------------------------------

/// A column is numeric when *every* value in it parses as one.
///
/// Per column rather than per cell: a column with one unparseable row is a text
/// column that mostly looks numeric, and silently turning that row into a zero
/// is how a class named `2` becomes a coordinate.
pub(crate) fn columns_from_records(names: &[String], records: &[Vec<String>]) -> Columns {
    let mut columns = Columns::default();
    for (index, name) in names.iter().enumerate() {
        let cells: Vec<&str> = records
            .iter()
            .map(|record| record.get(index).map(String::as_str).unwrap_or(""))
            .collect();
        let numbers: Option<Vec<f64>> = cells
            .iter()
            .map(|cell| cell.trim().parse::<f64>().ok())
            .collect();
        match numbers {
            Some(numbers) => columns.push_numeric(name.clone(), numbers),
            None => columns.push_text(
                name.clone(),
                cells.iter().map(|cell| (*cell).to_string()).collect(),
            ),
        }
    }
    columns.rows = columns.rows.max(records.len());
    columns
}

pub(crate) fn csv_columns(bytes: &[u8]) -> Result<Columns> {
    let mut reader = csv::Reader::from_reader(bytes);
    let names: Vec<String> = reader
        .headers()
        .context("reading the table header")?
        .iter()
        .map(|h| h.trim().to_string())
        .collect();
    let mut records = Vec::new();
    for record in reader.records() {
        let record = record.context("reading a table row")?;
        records.push(record.iter().map(str::to_string).collect());
    }
    Ok(columns_from_records(&names, &records))
}

/// The JSON backend: an array of row objects, or an object of column arrays.
///
/// Both shapes are in the wild — pandas writes either, depending on `orient` —
/// and telling them apart costs one match on the outer value.
pub(crate) fn json_columns(bytes: &[u8]) -> Result<Columns> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).context("parsing the table as JSON")?;
    let mut columns = Columns::default();

    fn put(columns: &mut Columns, name: &str, cells: &[serde_json::Value]) {
        let numbers: Option<Vec<f64>> = cells.iter().map(|c| c.as_f64()).collect();
        match numbers {
            Some(numbers) => columns.push_numeric(name, numbers),
            None => columns.push_text(
                name,
                cells
                    .iter()
                    .map(|c| match c {
                        serde_json::Value::String(s) => s.clone(),
                        serde_json::Value::Null => String::new(),
                        other => other.to_string(),
                    })
                    .collect(),
            ),
        }
    }

    match value {
        serde_json::Value::Array(rows) => {
            let mut names: Vec<String> = Vec::new();
            for row in &rows {
                for key in row.as_object().map(|o| o.keys()).into_iter().flatten() {
                    if !names.iter().any(|n| n == key) {
                        names.push(key.clone());
                    }
                }
            }
            for name in &names {
                let cells: Vec<serde_json::Value> = rows
                    .iter()
                    .map(|row| row.get(name).cloned().unwrap_or(serde_json::Value::Null))
                    .collect();
                put(&mut columns, name, &cells);
            }
            columns.rows = columns.rows.max(rows.len());
        }
        serde_json::Value::Object(map) => {
            for (name, cells) in map {
                let Some(cells) = cells.as_array() else {
                    continue;
                };
                put(&mut columns, &name, cells);
            }
        }
        _ => bail!("the JSON table is neither an array of rows nor an object of columns"),
    }
    Ok(columns)
}

/// The Parquet backend, read through the record API rather than through Arrow.
///
/// `parquet` with `default-features = false` drops the whole `arrow` stack,
/// which is a lot of crates to carry for reading a few hundred rows of a table
/// somebody else wrote. The record iterator is slower per row, which at this
/// size is not a number anyone can measure.
pub(crate) fn parquet_columns(bytes: &[u8]) -> Result<Columns> {
    use parquet::file::reader::{FileReader, SerializedFileReader};
    use parquet::record::Field;

    let reader = SerializedFileReader::new(bytes::Bytes::copy_from_slice(bytes))
        .context("opening the parquet table")?;
    let mut names: Vec<String> = Vec::new();
    let mut records: Vec<Vec<String>> = Vec::new();
    for row in reader.get_row_iter(None).context("reading parquet rows")? {
        let row = row.context("reading a parquet row")?;
        let mut cells = Vec::new();
        for (index, (name, field)) in row.get_column_iter().enumerate() {
            if names.len() <= index {
                names.push(name.clone());
            }
            cells.push(match field {
                Field::Null => String::new(),
                Field::Str(text) => text.clone(),
                other => other.to_string(),
            });
        }
        records.push(cells);
    }
    Ok(columns_from_records(&names, &records))
}

/// Decode a table's payload according to the backend it declares.
pub(crate) fn columns_from_payload(backend: &str, name: &str, bytes: &[u8]) -> Result<Columns> {
    match backend {
        "csv" => csv_columns(bytes),
        "json" => json_columns(bytes),
        "parquet" => parquet_columns(bytes),
        other => bail!("table `{name}` declares the unknown backend `{other}`"),
    }
}

/// The file a byte-payload backend keeps its rows in. AnnData has none — its
/// rows are zarr arrays, and the group is the table.
pub(crate) fn payload_name(backend: &str) -> Option<&'static str> {
    match backend {
        "csv" => Some(CSV_PAYLOAD),
        "json" => Some(JSON_PAYLOAD),
        "parquet" => Some(PARQUET_PAYLOAD),
        _ => None,
    }
}
