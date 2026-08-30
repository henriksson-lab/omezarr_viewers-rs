//! NumPy arrays of points — ClearMap's own shape, and `clearmap-ng`'s when it
//! grows a writer for one (PLAN.md §9).
//!
//! Two layouts, because both are in the wild:
//!
//! * a plain 2D array, `(N, 2)` / `(N, 3)` / `(N, k)`. The first two or three
//!   columns are the position and the rest are unnamed columns `c3`, `c4`, …;
//! * a **structured** 1D array, whose dtype names its fields — ClearMap's cell
//!   tables are these, with fields like `x`, `y`, `z`, `size`, `source`.
//!
//! Only the header is parsed here. The rest of `.npy` — Fortran order, byte
//! order, every scalar width — is handled where it costs a line, and refused by
//! name where it does not.

use anyhow::{bail, Context, Result};

use super::{ColumnData, NamedColumn, ObjectStore};

/// A field of the array's dtype.
#[derive(Debug, Clone)]
struct Field {
    name: String,
    /// `u`, `i` or `f`.
    kind: char,
    width: usize,
    little_endian: bool,
}

impl Field {
    fn parse(name: &str, descr: &str) -> Result<Self> {
        let bytes: Vec<char> = descr.chars().collect();
        let (endian, rest) = match bytes.first() {
            Some('<') | Some('|') => (true, &descr[1..]),
            Some('>') => (false, &descr[1..]),
            _ => (true, descr),
        };
        let kind = rest
            .chars()
            .next()
            .with_context(|| format!("dtype `{descr}` names no kind"))?;
        let width: usize = rest[1..]
            .parse()
            .with_context(|| format!("dtype `{descr}` names no width"))?;
        if !matches!(kind, 'u' | 'i' | 'f') {
            bail!("dtype `{descr}` is not a number this reads");
        }
        if !matches!(
            (kind, width),
            ('f', 4) | ('f', 8) | (_, 1) | (_, 2) | (_, 4) | (_, 8)
        ) {
            bail!("dtype `{descr}` has a width this does not read");
        }
        Ok(Self {
            name: name.to_string(),
            kind,
            width,
            little_endian: endian,
        })
    }

    fn read(&self, bytes: &[u8]) -> f64 {
        let mut buf = [0u8; 8];
        buf[..self.width].copy_from_slice(&bytes[..self.width]);
        if !self.little_endian {
            buf[..self.width].reverse();
        }
        // Destructured rather than `buf[..n].try_into().unwrap()`: the widths
        // are fixed and known here, so the compiler can carry the proof instead
        // of a runtime check that can only ever be a panic.
        let [b0, b1, b2, b3, ..] = buf;
        match (self.kind, self.width) {
            ('f', 4) => f32::from_le_bytes([b0, b1, b2, b3]) as f64,
            ('f', 8) => f64::from_le_bytes(buf),
            ('i', 1) => i8::from_le_bytes([b0]) as f64,
            ('i', 2) => i16::from_le_bytes([b0, b1]) as f64,
            ('i', 4) => i32::from_le_bytes([b0, b1, b2, b3]) as f64,
            ('i', 8) => i64::from_le_bytes(buf) as f64,
            ('u', 1) => b0 as f64,
            ('u', 2) => u16::from_le_bytes([b0, b1]) as f64,
            ('u', 4) => u32::from_le_bytes([b0, b1, b2, b3]) as f64,
            ('u', 8) => u64::from_le_bytes(buf) as f64,
            _ => f64::NAN,
        }
    }

    fn is_integral(&self) -> bool {
        matches!(self.kind, 'u' | 'i')
    }
}

pub fn read(bytes: &[u8]) -> Result<ObjectStore> {
    let (header, data) = split_header(bytes)?;
    let descr = value_of(&header, "descr").context("the header names no dtype")?;
    let fortran = value_of(&header, "fortran_order")
        .map(|v| v.contains("True"))
        .unwrap_or(false);
    if fortran {
        bail!("this array is Fortran-ordered; write it C-ordered (np.ascontiguousarray)");
    }
    let shape = parse_shape(&header)?;

    let fields = parse_fields(&descr)?;
    let (positions, mut columns) = if fields.len() == 1 && fields[0].name.is_empty() {
        plain(&fields[0], &shape, data)?
    } else {
        structured(&fields, &shape, data)?
    };

    // A z of exactly zero everywhere is a plane, not a missing axis.
    let has_z = positions.iter().any(|p| p[0] != 0.0);
    columns.retain(|column| !column.data.is_empty());
    ObjectStore::new(positions, columns, has_z)
}

/// A plain `(N, k)` array: the first columns are the position.
fn plain(field: &Field, shape: &[usize], data: &[u8]) -> Result<(Vec<[f32; 3]>, Vec<NamedColumn>)> {
    let (rows, width) = match shape {
        [rows, width] => (*rows, *width),
        [rows] => (*rows, 1),
        other => bail!("a point array is 2D; this one is {other:?}"),
    };
    if width < 2 {
        bail!("a point array needs at least two columns; this one has {width}");
    }
    let stride = field.width;
    if data.len() < rows * width * stride {
        bail!("the array data is shorter than its shape");
    }

    // Three columns are `(z, y, x)` — the axis order every volume in this
    // pipeline uses. Two are `(y, x)` in a plane.
    let has_z = width >= 3;
    let mut positions = Vec::with_capacity(rows);
    let mut extra: Vec<Vec<f64>> = vec![Vec::with_capacity(rows); width.saturating_sub(3)];
    for row in 0..rows {
        let at = row * width * stride;
        let value = |column: usize| field.read(&data[at + column * stride..]);
        let position = if has_z {
            [value(0) as f32, value(1) as f32, value(2) as f32]
        } else {
            [0.0, value(0) as f32, value(1) as f32]
        };
        positions.push(position);
        for (index, column) in extra.iter_mut().enumerate() {
            column.push(value(3 + index));
        }
    }

    let columns = extra
        .into_iter()
        .enumerate()
        .map(|(index, values)| NamedColumn {
            name: format!("c{}", index + 3),
            data: column_data(field.is_integral(), values),
        })
        .collect();
    Ok((positions, columns))
}

/// A structured array: the dtype's field names say which column is which.
fn structured(
    fields: &[Field],
    shape: &[usize],
    data: &[u8],
) -> Result<(Vec<[f32; 3]>, Vec<NamedColumn>)> {
    let rows = match shape {
        [rows] => *rows,
        [rows, 1] => *rows,
        other => bail!("a structured point array is 1D; this one is {other:?}"),
    };
    let stride: usize = fields.iter().map(|f| f.width).sum();
    if data.len() < rows * stride {
        bail!("the array data is shorter than its shape");
    }

    let find = |names: &[&str]| {
        fields
            .iter()
            .position(|f| names.iter().any(|n| f.name.eq_ignore_ascii_case(n)))
    };
    let x = find(&["x", "centroid_x", "col"]);
    let y = find(&["y", "centroid_y", "row"]);
    let z = find(&["z", "centroid_z", "plane"]);
    let (Some(x), Some(y)) = (x, y) else {
        bail!(
            "no x/y fields among [{}]",
            fields
                .iter()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    };

    let mut offsets = Vec::with_capacity(fields.len());
    let mut at = 0;
    for field in fields {
        offsets.push(at);
        at += field.width;
    }

    let mut positions = Vec::with_capacity(rows);
    let mut values: Vec<Vec<f64>> = vec![Vec::with_capacity(rows); fields.len()];
    for row in 0..rows {
        let base = row * stride;
        for (index, field) in fields.iter().enumerate() {
            values[index].push(field.read(&data[base + offsets[index]..]));
        }
        positions.push([
            z.map(|i| values[i][row]).unwrap_or(0.0) as f32,
            values[y][row] as f32,
            values[x][row] as f32,
        ]);
    }

    let columns = fields
        .iter()
        .enumerate()
        .filter(|(index, _)| {
            Some(*index) != Some(x) && Some(*index) != Some(y) && Some(*index) != z
        })
        .map(|(index, field)| NamedColumn {
            name: field.name.clone(),
            data: column_data(field.is_integral(), values[index].clone()),
        })
        .collect();
    Ok((positions, columns))
}

fn column_data(integral: bool, values: Vec<f64>) -> ColumnData {
    if integral && values.iter().all(|v| *v >= 0.0 && v.fract() == 0.0) {
        ColumnData::U64(values.into_iter().map(|v| v as u64).collect())
    } else {
        ColumnData::F64(values)
    }
}

/// Split the `.npy` header dictionary from the data.
fn split_header(bytes: &[u8]) -> Result<(String, &[u8])> {
    if bytes.len() < 10 || &bytes[..6] != b"\x93NUMPY" {
        bail!("this file does not start with the NumPy magic");
    }
    let major = bytes[6];
    let (header_len, start) = if major == 1 {
        (u16::from_le_bytes([bytes[8], bytes[9]]) as usize, 10usize)
    } else {
        (
            u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize,
            12usize,
        )
    };
    let end = start + header_len;
    if bytes.len() < end {
        bail!("the header runs past the end of the file");
    }
    let header = String::from_utf8_lossy(&bytes[start..end]).into_owned();
    Ok((header, &bytes[end..]))
}

/// The value of a key in the header dict, as raw text.
fn value_of(header: &str, key: &str) -> Option<String> {
    let needle = format!("'{key}':");
    let at = header.find(&needle)? + needle.len();
    let rest = header[at..].trim_start();
    let end = match rest.chars().next()? {
        '(' => rest.find(')')? + 1,
        '[' => rest.rfind(']')? + 1,
        '\'' => rest[1..].find('\'')? + 2,
        _ => rest.find(',').unwrap_or(rest.len()),
    };
    Some(rest[..end].trim().to_string())
}

fn parse_shape(header: &str) -> Result<Vec<usize>> {
    let text = value_of(header, "shape").context("the header names no shape")?;
    let inner = text.trim_start_matches('(').trim_end_matches(')');
    Ok(inner
        .split(',')
        .filter_map(|part| part.trim().parse::<usize>().ok())
        .collect())
}

/// The dtype: either one scalar (a plain array) or a list of named fields.
fn parse_fields(descr: &str) -> Result<Vec<Field>> {
    let descr = descr.trim();
    if descr.starts_with('\'') {
        let text = descr.trim_matches('\'');
        return Ok(vec![Field::parse("", text)?]);
    }
    if !descr.starts_with('[') {
        bail!("dtype `{descr}` is neither a scalar nor a field list");
    }
    let mut fields = Vec::new();
    let mut rest = &descr[1..];
    while let Some(open) = rest.find('(') {
        let Some(close) = rest[open..].find(')') else {
            break;
        };
        let tuple = &rest[open + 1..open + close];
        let parts: Vec<&str> = tuple.split(',').map(|p| p.trim()).collect();
        if parts.len() >= 2 {
            let name = parts[0].trim_matches(|c| c == '\'' || c == '"');
            let kind = parts[1].trim_matches(|c| c == '\'' || c == '"');
            fields.push(Field::parse(name, kind)?);
        }
        rest = &rest[open + close + 1..];
    }
    if fields.is_empty() {
        bail!("dtype `{descr}` names no fields");
    }
    Ok(fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write a `.npy` the way NumPy does, v1 header.
    fn npy(descr: &str, shape: &str, data: &[u8]) -> Vec<u8> {
        let dict = format!("{{'descr': {descr}, 'fortran_order': False, 'shape': {shape}, }}");
        let mut header = dict.into_bytes();
        while !(10 + header.len() + 1).is_multiple_of(64) {
            header.push(b' ');
        }
        header.push(b'\n');
        let mut out = Vec::new();
        out.extend_from_slice(b"\x93NUMPY\x01\x00");
        out.extend_from_slice(&(header.len() as u16).to_le_bytes());
        out.extend_from_slice(&header);
        out.extend_from_slice(data);
        out
    }

    #[test]
    fn reads_an_n_by_3_float_array_as_zyx() {
        let values: Vec<f64> = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        let data: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        let bytes = npy("'<f8'", "(2, 3)", &data);
        let store = read(&bytes).expect("read");
        assert_eq!(store.len(), 2);
        assert_eq!(store.world_position(0).unwrap(), [1.0, 2.0, 3.0]);
        assert_eq!(store.world_position(1).unwrap(), [4.0, 5.0, 6.0]);
        assert!(store.columns().is_empty());
    }

    #[test]
    fn extra_columns_become_unnamed_columns() {
        let values: Vec<f32> = vec![1.0, 2.0, 3.0, 40.0, 4.0, 5.0, 6.0, 50.0];
        let data: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        let bytes = npy("'<f4'", "(2, 4)", &data);
        let store = read(&bytes).expect("read");
        assert_eq!(store.columns().len(), 1);
        assert_eq!(store.columns()[0].name, "c3");
        assert_eq!(store.columns()[0].data.at(1), Some(50.0));
    }

    #[test]
    fn a_two_column_array_is_a_plane() {
        let values: Vec<f64> = vec![7.0, 8.0];
        let data: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        let bytes = npy("'<f8'", "(1, 2)", &data);
        let store = read(&bytes).expect("read");
        assert_eq!(store.world_position(0).unwrap(), [0.0, 7.0, 8.0]);
        assert!(!store.schema().has_z);
    }

    #[test]
    fn reads_a_structured_cell_table_by_field_name() {
        // ClearMap's shape: x, y, z as u16 and a size as u32.
        let mut data = Vec::new();
        for (x, y, z, size) in [(10u16, 20u16, 30u16, 400u32), (11, 21, 31, 500)] {
            data.extend_from_slice(&x.to_le_bytes());
            data.extend_from_slice(&y.to_le_bytes());
            data.extend_from_slice(&z.to_le_bytes());
            data.extend_from_slice(&size.to_le_bytes());
        }
        let bytes = npy(
            "[('x', '<u2'), ('y', '<u2'), ('z', '<u2'), ('size', '<u4')]",
            "(2,)",
            &data,
        );
        let store = read(&bytes).expect("read");
        assert_eq!(store.world_position(0).unwrap(), [30.0, 20.0, 10.0]);
        assert_eq!(store.columns().len(), 1);
        assert_eq!(store.columns()[0].name, "size");
        assert_eq!(store.columns()[0].data.at(1), Some(500.0));
        assert_eq!(store.columns()[0].data.kind(), "u64");
    }

    #[test]
    fn big_endian_fields_are_read_as_big_endian() {
        let mut data = Vec::new();
        data.extend_from_slice(&1u16.to_be_bytes());
        data.extend_from_slice(&2u16.to_be_bytes());
        let bytes = npy("[('x', '>u2'), ('y', '>u2')]", "(1,)", &data);
        let store = read(&bytes).expect("read");
        assert_eq!(store.world_position(0).unwrap(), [0.0, 2.0, 1.0]);
    }

    #[test]
    fn fortran_order_is_refused_by_name() {
        let dict = "{'descr': '<f8', 'fortran_order': True, 'shape': (1, 3), }";
        let mut header = dict.as_bytes().to_vec();
        header.push(b'\n');
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"\x93NUMPY\x01\x00");
        bytes.extend_from_slice(&(header.len() as u16).to_le_bytes());
        bytes.extend_from_slice(&header);
        bytes.extend_from_slice(&[0u8; 24]);
        let err = read(&bytes).expect_err("refused");
        assert!(format!("{err}").contains("Fortran"), "{err}");
    }

    #[test]
    fn a_file_that_is_not_npy_is_refused() {
        assert!(read(b"not a numpy file at all").is_err());
    }
}
