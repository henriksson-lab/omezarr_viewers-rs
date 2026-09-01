//! `blockflow`'s table blob — what `model_segment` (cellpose, stardist) writes.
//!
//! The format is defined in `blockflow/src/table.rs` and reimplemented here
//! rather than depended on: that crate pulls burn, candle and a CUDA toolchain
//! behind it, and this needs sixty lines of little-endian words.
//!
//! ```text
//! [ MAGIC, VERSION, n_columns, n_rows ]                    four u64 words
//! per column: [ type_code, name_len, name words … ]        8 bytes/word, zero-padded
//! per row:    [ z, y, x, col0, col1, … ]                   3 + n_columns words
//! ```
//!
//! `type_code` is 1 for `u64` and 2 for `f64`, an `f64` travelling as
//! `to_bits`. Positions come first and are voxel coordinates.
//!
//! The tests below build blobs the way `RowBuilder::encode` does, including a
//! `model_segment` row with that op's own schema, and check that a bumped
//! `VERSION` or a foreign magic word is **refused** rather than decoded into
//! plausible nonsense — which is the failure a hand-written decoder of somebody
//! else's format has to be protected from.

use anyhow::{bail, Result};

use super::{ColumnData, NamedColumn, ObjectStore};

/// `b"BFTABLE\0"` big-endian, as `blockflow` writes it.
const MAGIC: u64 = u64::from_be_bytes(*b"BFTABLE\0");
const VERSION: u64 = 1;
/// A row's position words, always first.
const POSITION_WORDS: usize = 3;

pub fn read(bytes: &[u8]) -> Result<ObjectStore> {
    if !bytes.len().is_multiple_of(8) {
        bail!(
            "a table blob is a whole number of 8-byte words; this one is {} byte(s)",
            bytes.len()
        );
    }
    let words: Vec<u64> = bytes
        .as_chunks::<8>()
        .0
        .iter()
        .map(|word| u64::from_le_bytes(*word))
        .collect();

    if words.len() < 4 {
        bail!(
            "a table blob starts with four words; this one has {}",
            words.len()
        );
    }
    if words[0] != MAGIC {
        bail!("this blob does not start with a blockflow table's magic word");
    }
    if words[1] != VERSION {
        bail!(
            "this table blob is version {}, and this build reads version {VERSION}",
            words[1]
        );
    }
    let column_count = words[2] as usize;
    let row_count = words[3] as usize;

    let mut at = 4;
    let mut names = Vec::with_capacity(column_count);
    let mut kinds = Vec::with_capacity(column_count);
    for column in 0..column_count {
        let Some(&code) = words.get(at) else {
            bail!("the header ends inside column {column}");
        };
        let Some(&name_len) = words.get(at + 1) else {
            bail!("the header ends inside column {column}");
        };
        at += 2;
        let name_words = (name_len as usize).div_ceil(8);
        let Some(chunk) = words.get(at..at + name_words) else {
            bail!("the header ends inside column {column}'s name");
        };
        at += name_words;
        let mut name_bytes = Vec::with_capacity(name_words * 8);
        for word in chunk {
            name_bytes.extend_from_slice(&word.to_le_bytes());
        }
        name_bytes.truncate(name_len as usize);
        names.push(String::from_utf8_lossy(&name_bytes).into_owned());
        kinds.push(match code {
            1 => ColumnKind::U64,
            2 => ColumnKind::F64,
            other => bail!("column `{}` has unknown type code {other}", names[column]),
        });
    }

    let width = POSITION_WORDS + column_count;
    let rows = &words[at..];
    if rows.len() != row_count * width {
        bail!(
            "the header promises {row_count} row(s) of {width} word(s) and the blob holds {} word(s) of rows",
            rows.len()
        );
    }

    let mut positions = Vec::with_capacity(row_count);
    let mut raw: Vec<Vec<u64>> = vec![Vec::with_capacity(row_count); column_count];
    for row in rows.chunks_exact(width) {
        positions.push([row[0] as f32, row[1] as f32, row[2] as f32]);
        for (column, values) in raw.iter_mut().enumerate() {
            values.push(row[POSITION_WORDS + column]);
        }
    }

    let columns = names
        .into_iter()
        .zip(kinds)
        .zip(raw)
        .map(|((name, kind), values)| NamedColumn {
            name,
            data: match kind {
                ColumnKind::U64 => ColumnData::U64(values),
                ColumnKind::F64 => {
                    ColumnData::F64(values.into_iter().map(f64::from_bits).collect())
                }
            },
        })
        .collect();

    // A table's positions are voxel coordinates in three axes; a 2D producer
    // writes zeros in the first, which is a plane rather than a missing axis.
    let has_z = positions.iter().any(|p| p[0] != 0.0);
    ObjectStore::new(positions, columns, has_z)
}

/// Encode rows into a table blob, the way `blockflow`'s `RowBuilder::encode`
/// does — the same layout this module's `read` accepts.
///
/// `positions` are the routing key: whole voxels, because that is what a table
/// is keyed by and what decides which block a row belongs to. A caller whose
/// geometry is finer than a voxel must carry the exact coordinate in an `f64`
/// column and treat the key as an address, not as the value.
pub fn write(positions: &[[u64; 3]], columns: &[NamedColumn]) -> Result<Vec<u8>> {
    for column in columns {
        let len = match &column.data {
            ColumnData::U64(values) => values.len(),
            ColumnData::F64(values) => values.len(),
        };
        if len != positions.len() {
            bail!(
                "column `{}` has {len} value(s) for {} row(s)",
                column.name,
                positions.len()
            );
        }
    }

    let mut words: Vec<u64> = vec![MAGIC, VERSION, columns.len() as u64, positions.len() as u64];
    for column in columns {
        words.push(match &column.data {
            ColumnData::U64(_) => 1,
            ColumnData::F64(_) => 2,
        });
        let name = column.name.as_bytes();
        words.push(name.len() as u64);
        // Zero-padded to a whole number of words; `read` truncates back to the
        // declared length, so the padding never reaches a name.
        for chunk in name.chunks(8) {
            let mut word = [0u8; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            words.push(u64::from_le_bytes(word));
        }
    }
    for (row, position) in positions.iter().enumerate() {
        words.extend_from_slice(position);
        for column in columns {
            words.push(match &column.data {
                ColumnData::U64(values) => values[row],
                ColumnData::F64(values) => values[row].to_bits(),
            });
        }
    }

    let mut bytes = Vec::with_capacity(words.len() * 8);
    for word in words {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    Ok(bytes)
}

enum ColumnKind {
    U64,
    F64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_written_table_reads_back_as_itself() {
        // The round trip is the only check that matters for a format this
        // reimplements rather than depends on: the writer and the reader are two
        // hand-written statements of one layout, and a disagreement between them
        // is exactly the drift the module header warns about.
        let positions = vec![[0, 10, 20], [3, 40, 50], [0, 0, 0]];
        let columns = vec![
            NamedColumn {
                name: "shape".into(),
                data: ColumnData::U64(vec![1, 1, 7]),
            },
            NamedColumn {
                // Deliberately not a multiple of eight bytes, so the name
                // padding is exercised rather than assumed.
                name: "half_width".into(),
                data: ColumnData::F64(vec![5.5, 0.0, 379.59344482421875]),
            },
        ];
        let blob = write(&positions, &columns).unwrap();
        let back = read(&blob).unwrap();

        assert_eq!(back.len(), 3);
        // `world_position` applies the layer's space, which is the identity for
        // a store built straight from a blob.
        assert_eq!(back.world_position(1), Some([3.0, 40.0, 50.0]));
        let widths = match &back.columns()[1].data {
            ColumnData::F64(values) => values.clone(),
            _ => panic!("half_width came back as the wrong type"),
        };
        // Exact, not approximate: an `f64` travels as `to_bits`, and the whole
        // reason for that is a coordinate that must not move.
        assert_eq!(widths[2], 379.59344482421875);
        assert_eq!(back.columns()[0].name, "shape");
        assert_eq!(back.columns()[1].name, "half_width");
    }

    #[test]
    fn a_column_that_does_not_match_the_rows_is_refused() {
        let err = write(
            &[[0, 0, 0], [0, 1, 1]],
            &[NamedColumn {
                name: "short".into(),
                data: ColumnData::U64(vec![1]),
            }],
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("short"), "{err}");
    }

    /// Build a blob the way `blockflow::table::RowBuilder::encode` does.
    fn blob(columns: &[(&str, u64)], rows: &[Vec<u64>]) -> Vec<u8> {
        let mut words = vec![MAGIC, VERSION, columns.len() as u64, rows.len() as u64];
        for (name, code) in columns {
            words.push(*code);
            let bytes = name.as_bytes();
            words.push(bytes.len() as u64);
            for chunk in bytes.chunks(8) {
                let mut padded = [0u8; 8];
                padded[..chunk.len()].copy_from_slice(chunk);
                words.push(u64::from_le_bytes(padded));
            }
        }
        for row in rows {
            words.extend_from_slice(row);
        }
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    #[test]
    fn reads_a_model_segment_row() {
        // The schema `blockflow::model_segment::schema(0)` produces.
        let bytes = blob(
            &[
                ("id", 1),
                ("count", 1),
                ("sum_0", 1),
                ("sum_1", 1),
                ("sum_2", 1),
            ],
            &[
                vec![4, 10, 20, 7, 41, 28, 40, 80],
                vec![6, 12, 22, 9, 55, 54, 66, 132],
            ],
        );
        let store = read(&bytes).expect("read");
        assert_eq!(store.len(), 2);
        assert_eq!(store.world_position(0).unwrap(), [4.0, 10.0, 20.0]);
        let names: Vec<&str> = store.columns().iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["id", "count", "sum_0", "sum_1", "sum_2"]);
        assert!(matches!(store.columns()[0].data, ColumnData::U64(_)));
        assert_eq!(store.columns()[1].data.at(1), Some(55.0));
        assert!(store.schema().has_z);
    }

    #[test]
    fn f64_columns_come_back_from_their_bits() {
        let bytes = blob(&[("intensity", 2)], &[vec![0, 1, 2, 1.5f64.to_bits()]]);
        let store = read(&bytes).expect("read");
        assert_eq!(store.columns()[0].data.at(0), Some(1.5));
    }

    #[test]
    fn a_foreign_blob_is_refused_rather_than_decoded() {
        let mut bytes = blob(&[("id", 1)], &[vec![0, 0, 0, 1]]);
        bytes[0] ^= 0xff;
        let err = read(&bytes).expect_err("refused");
        assert!(format!("{err}").contains("magic"), "{err}");
    }

    #[test]
    fn a_version_bump_is_reported_not_guessed() {
        let mut words: Vec<u64> = blob(&[("id", 1)], &[vec![0, 0, 0, 1]])
            .as_chunks::<8>()
            .0
            .iter()
            .map(|w| u64::from_le_bytes(*w))
            .collect();
        words[1] = VERSION + 1;
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        let err = read(&bytes).expect_err("refused");
        assert!(format!("{err}").contains("version"), "{err}");
    }

    #[test]
    fn a_truncated_blob_says_so() {
        let mut bytes = blob(&[("id", 1)], &[vec![0, 0, 0, 1]]);
        bytes.truncate(bytes.len() - 8);
        assert!(read(&bytes).is_err());
    }
}
