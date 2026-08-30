//! The `.npy` container format: the magic, the version, the header dictionary,
//! and the two things that dictionary says before either reader is chosen —
//! what dtype and what shape.
//!
//! One file rather than a directory, because there is one small format here and
//! every function below is about the same fixed preamble and the Python dict
//! literal that follows it. The *readers* are genuinely different and stay
//! where they are: `npy_volume` slices a picture out of a flat buffer, while
//! `objects::npy` walks rows and understands structured dtypes, which nothing
//! here does.
//!
//! `classify` lives here rather than in either reader because deciding whether
//! a `.npy` is a volume or a table is a question about the header, asked before
//! a reader exists — `clearmap-ng` writes masks and cell tables under the same
//! extension.

use anyhow::{bail, Context, Result};

/// What a `.npy` file holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NpyKind {
    /// A 2D or 3D array of pixels — a mask, a density, a plane.
    Volume,
    /// A table of objects: a structured array, or `(N, k)` with a small `k`.
    Objects,
}

/// A `.npy` file cut into its header dictionary and its data.
#[derive(Debug)]
pub struct Split<'a> {
    /// The header dictionary, as the Python source text NumPy wrote.
    pub dict: String,
    /// Byte offset of the first element, into the whole file.
    pub offset: usize,
    /// The data, from `offset` to the end.
    pub data: &'a [u8],
}

/// Split a `.npy` at its header.
pub fn split(bytes: &[u8]) -> Result<Split<'_>> {
    if bytes.len() < 10 || &bytes[..6] != b"\x93NUMPY" {
        bail!("this file does not start with the NumPy magic");
    }
    // v1 states the header length in two bytes, v2 and later in four.
    let (header_len, start) = if bytes[6] == 1 {
        (u16::from_le_bytes([bytes[8], bytes[9]]) as usize, 10usize)
    } else if bytes.len() >= 12 {
        (
            u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize,
            12usize,
        )
    } else {
        bail!("the .npy header runs past the end of the file");
    };
    let end = start + header_len;
    if bytes.len() < end {
        bail!("the .npy header runs past the end of the file");
    }
    Ok(Split {
        dict: String::from_utf8_lossy(&bytes[start..end]).into_owned(),
        offset: end,
        data: &bytes[end..],
    })
}

/// The value of a key in the header dict, as raw text.
pub fn value_of(header: &str, key: &str) -> Option<String> {
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

/// The `descr` value, still quoted or bracketed as it was written: a scalar
/// dtype and a structured field list are told apart by that punctuation.
pub fn descr(header: &str) -> Result<String> {
    value_of(header, "descr").context("the header names no dtype")
}

/// Refuse a Fortran-ordered array. Neither reader transposes; both slice as if
/// the rows were contiguous, and a silent wrong picture is worse than a stop.
pub fn require_c_order(header: &str) -> Result<()> {
    let fortran = value_of(header, "fortran_order")
        .map(|v| v.contains("True"))
        .unwrap_or(false);
    if fortran {
        bail!("this array is Fortran-ordered; write it C-ordered (np.ascontiguousarray)");
    }
    Ok(())
}

/// The shape tuple. A dimension that does not parse is dropped rather than
/// refused, which is what makes `(2,)` a one-element shape and not a two.
pub fn shape(header: &str) -> Result<Vec<u64>> {
    Ok(value_of(header, "shape")
        .context("the header names no shape")?
        .trim_start_matches('(')
        .trim_end_matches(')')
        .split(',')
        .filter_map(|part| part.trim().parse::<u64>().ok())
        .collect())
}

/// A NumPy scalar descriptor as `(dtype name, width, little-endian)`.
///
/// The quotes are stripped here, so a `descr` may be passed as it was read.
pub fn scalar(descr: &str) -> Result<(String, usize, bool)> {
    let descr = descr.trim().trim_matches(|c| c == '\'' || c == '"');
    let (little_endian, rest) = match descr.chars().next() {
        Some('>') => (false, &descr[1..]),
        Some('<') | Some('|') | Some('=') => (true, &descr[1..]),
        _ => (true, descr),
    };
    let kind = rest.chars().next().context("dtype names no kind")?;
    let width: usize = rest[1..].parse().context("dtype names no width")?;
    let name = match (kind, width) {
        ('u', 1) => "uint8",
        ('u', 2) => "uint16",
        ('u', 4) => "uint32",
        ('u', 8) => "uint64",
        ('i', 1) => "int8",
        ('i', 2) => "int16",
        ('i', 4) => "int32",
        ('i', 8) => "int64",
        ('f', 4) => "float32",
        ('f', 8) => "float64",
        ('b', 1) => "uint8",
        _ => bail!("dtype `{descr}` is not one this reads"),
    };
    Ok((name.to_string(), width, little_endian))
}

/// Decide which reader a `.npy` belongs to, from its header alone.
///
/// The distinction is real and cannot be guessed from the extension:
/// `clearmap-ng` writes masks and cell tables as `.npy` alike. The rules, in
/// order:
///
/// * a **structured** dtype names fields — that is a table, always;
/// * `(N,)` is a table of one column;
/// * `(N, k)` with `k <= 4` is a point list — a volume that narrow is not a
///   picture of anything;
/// * anything else is a volume.
pub fn classify(header_bytes: &[u8]) -> Result<NpyKind> {
    let header = split(header_bytes)?.dict;
    if descr(&header)?.trim_start().starts_with('[') {
        return Ok(NpyKind::Objects);
    }
    Ok(match shape(&header)?.as_slice() {
        [_] => NpyKind::Objects,
        [_, k] if *k <= 4 => NpyKind::Objects,
        _ => NpyKind::Volume,
    })
}

/// Write a `.npy` the way NumPy does, v1 header.
///
/// `descr` is the dict value **as it appears in the file**, quotes included:
/// `'<u2'` for a scalar, `[('x', '<u2'), …]` for a structured dtype. Only that
/// form can express both, and a helper that quoted for you could not write a
/// field list at all.
#[cfg(test)]
pub(crate) fn write(descr: &str, shape: &str, data: &[u8]) -> Vec<u8> {
    let dict = format!("{{'descr': {descr}, 'fortran_order': False, 'shape': {shape}, }}");
    let mut header = dict.into_bytes();
    // NumPy pads the header so the data starts on a 64-byte boundary.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_a_v1_header_from_its_data() {
        let bytes = write("'<u2'", "(1, 2)", &[1, 0, 2, 0]);
        let split = split(&bytes).expect("split");
        assert!(split.dict.starts_with("{'descr': '<u2'"));
        assert_eq!(split.data, &[1, 0, 2, 0]);
        assert_eq!(split.offset, bytes.len() - 4);
        assert!(split.offset.is_multiple_of(64), "NumPy aligns the data");
    }

    #[test]
    fn a_file_without_the_magic_is_refused_by_name() {
        let err = split(b"not a numpy file at all").expect_err("refused");
        assert!(format!("{err}").contains("NumPy magic"), "{err}");
    }

    #[test]
    fn a_header_longer_than_the_file_is_refused_by_name() {
        let mut bytes = b"\x93NUMPY\x01\x00".to_vec();
        bytes.extend_from_slice(&999u16.to_le_bytes());
        let err = split(&bytes).expect_err("refused");
        assert!(format!("{err}").contains("past the end"), "{err}");
    }

    #[test]
    fn values_come_back_with_their_own_punctuation() {
        let bytes = write("[('x', '<u2')]", "(2,)", &[]);
        let header = split(&bytes).expect("split").dict;
        assert_eq!(descr(&header).unwrap(), "[('x', '<u2')]");
        assert_eq!(value_of(&header, "shape").unwrap(), "(2,)");
        assert_eq!(shape(&header).unwrap(), vec![2]);
    }

    #[test]
    fn scalars_carry_their_width_and_byte_order() {
        assert_eq!(scalar("'<u2'").unwrap(), ("uint16".to_string(), 2, true));
        assert_eq!(scalar(">f8").unwrap(), ("float64".to_string(), 8, false));
        assert!(scalar("<c8").is_err(), "complex is not one this reads");
    }

    #[test]
    fn classify_tells_a_picture_from_a_table() {
        let volume = write("'<u2'", "(3, 4, 5)", &[]);
        assert_eq!(classify(&volume).unwrap(), NpyKind::Volume);
        let narrow = write("'<f8'", "(100, 3)", &[]);
        assert_eq!(classify(&narrow).unwrap(), NpyKind::Objects);
        let structured = write("[('x', '<u2'), ('y', '<u2')]", "(2,)", &[]);
        assert_eq!(classify(&structured).unwrap(), NpyKind::Objects);
    }
}
