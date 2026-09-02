//! The parsers, against bytes nobody meant them to read.
//!
//! Three of them take input this viewer did not write — a GeoJSON file a person
//! exported from QuPath, a `blockflow` table blob, a `.npy` header — and each
//! reads them from `file://`, `http(s)://` or `s3://` alike. A malformed one
//! should produce an **error**, because an error names the file and the number
//! that was wrong. Panics here unwind — nothing sets `panic = "abort"` — so the
//! server survives one; what it costs is a dropped connection and a log line
//! reading `capacity overflow` instead of which file to go and look at.
//!
//! This is the cheap half of fuzzing: a fixed seed, a fixed budget, and it runs
//! on every `make test`. `fuzz/` holds libFuzzer targets over the same three
//! entry points for the deep runs that find what a few thousand deterministic
//! mutations do not. The corpus here is what those runs found plus the shapes
//! worth trying on purpose, so a bug found once cannot come back unnoticed.
//!
//! Determinism is the point: a fuzz failure nobody can reproduce is a flake,
//! and a flake in CI gets muted. Every input below is a pure function of the
//! seed, so a failure prints the seed and the case index and re-running is
//! exact.

use omezarr_viewer_server::annotations::geojson;
use omezarr_viewer_server::npy_header;
use omezarr_viewer_server::objects::table;

/// xorshift64*. Small, deterministic, and good enough to smear bytes around —
/// this is not a random-number generator anybody's statistics depend on.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            0
        } else {
            (self.next() % bound as u64) as usize
        }
    }
}

/// Corrupt `seed_bytes` one way, chosen by the generator.
///
/// Mutations rather than random noise: random bytes are rejected by the first
/// length check and never reach the code that indexes. What finds bugs is a
/// *nearly* valid file — the magic word intact, one count absurd.
fn mutate(rng: &mut Rng, seed_bytes: &[u8]) -> Vec<u8> {
    let mut bytes = seed_bytes.to_vec();
    if bytes.is_empty() {
        return bytes;
    }
    match rng.below(6) {
        // Flip some bits somewhere.
        0 => {
            for _ in 0..1 + rng.below(8) {
                let at = rng.below(bytes.len());
                bytes[at] ^= 1 << rng.below(8);
            }
        }
        // Cut it short: the commonest real corruption, and the one that makes a
        // length read earlier in the file describe bytes that are not there.
        1 => {
            let keep = rng.below(bytes.len());
            bytes.truncate(keep);
        }
        // Splice a run of bytes out of the middle.
        2 => {
            let at = rng.below(bytes.len());
            let len = rng.below(bytes.len() - at);
            bytes.drain(at..at + len);
        }
        // Make it longer than it says it is.
        3 => {
            let extra = rng.below(64);
            bytes.extend(std::iter::repeat_n(rng.next() as u8, extra));
        }
        // Set an aligned word to something extreme. This is the mutation that
        // matters for a length-driven format: every count in a table blob is a
        // `u64` a file gets to choose, and the interesting values are the ones
        // that overflow when multiplied rather than the ones that are merely
        // large.
        4 => {
            if bytes.len() >= 8 {
                let word = rng.below(bytes.len() / 8);
                let value = match rng.below(6) {
                    0 => u64::MAX,
                    1 => u64::MAX / 2,
                    2 => 1 << 60,
                    3 => 1 << 32,
                    4 => usize::MAX as u64 - 3,
                    _ => rng.next(),
                };
                bytes[word * 8..word * 8 + 8].copy_from_slice(&value.to_le_bytes());
            }
        }
        // Overwrite a stretch with one repeated byte.
        _ => {
            let at = rng.below(bytes.len());
            let len = rng.below(bytes.len() - at);
            let value = rng.next() as u8;
            bytes[at..at + len].fill(value);
        }
    }
    bytes
}

/// How many mutations each parser gets, and where the generator starts.
///
/// Both overridable, so the same test is a fast gate by default and a long run
/// on demand: `FUZZ_CASES=5000000 FUZZ_SEED=7 cargo test -p server --test
/// parser_fuzz`. The defaults are small enough that `make test` does not notice
/// and large enough to have caught something — the deep search is `fuzz/`'s job.
fn budget(name: &str, fallback: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(fallback)
}

fn hammer(name: &str, seed_bytes: &[u8], parse: impl Fn(&[u8])) {
    let cases = budget("FUZZ_CASES", 20_000);
    let seed = budget("FUZZ_SEED", 0x5EED_1234_ABCD_0001);
    let mut rng = Rng(seed);
    for case in 0..cases {
        let bytes = mutate(&mut rng, seed_bytes);
        // The assertion is that this returns at all. A panic here fails the
        // test with the case index, and `case` plus the seed above reproduces
        // the exact bytes.
        parse(&bytes);
        let _ = (name, case, seed);
    }
}

/// A valid table blob: the golden fixture the rasteriser is pinned to, so the
/// mutations start from bytes that are real rather than bytes a test made up.
fn table_blob() -> Vec<u8> {
    std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/fragments.bftable"
    ))
    .expect("the golden fragment fixture")
}

fn geojson_file() -> Vec<u8> {
    use omezarr_viewer_common::{Annotation, Geometry};
    let rows = vec![
        Annotation {
            id: 1,
            geometry: Geometry::Polygon(vec![
                vec![
                    [0.0, 0.0],
                    [10.0, 0.0],
                    [10.0, 10.0],
                    [0.0, 10.0],
                    [0.0, 0.0],
                ],
                vec![[2.0, 2.0], [4.0, 2.0], [4.0, 4.0], [2.0, 4.0], [2.0, 2.0]],
            ]),
            label: "Tumor".into(),
            ..Default::default()
        },
        Annotation {
            id: 2,
            geometry: Geometry::LineString(vec![[0.0, 0.0], [5.0, 5.0]]),
            stroke_width: Some(7.0),
            parent: Some(1),
            ..Default::default()
        },
    ];
    geojson::write(&rows).expect("writing the seed file")
}

/// A `.npy` header as numpy writes one.
fn npy_header_bytes() -> Vec<u8> {
    let text = "{'descr': '<u2', 'fortran_order': False, 'shape': (4, 8, 8), }";
    let mut header = text.as_bytes().to_vec();
    while !(10 + header.len() + 1).is_multiple_of(64) {
        header.push(b' ');
    }
    header.push(b'\n');
    let mut bytes = b"\x93NUMPY\x01\x00".to_vec();
    bytes.extend_from_slice(&(header.len() as u16).to_le_bytes());
    bytes.extend_from_slice(&header);
    bytes
}

#[test]
fn a_table_blob_is_refused_rather_than_panicked_at() {
    // Every count in this format is a u64 the file chooses: the column count,
    // the row count, and each name's length. All three are used to allocate and
    // to multiply, and none of them is bounded by anything but the blob itself.
    hammer("table", &table_blob(), |bytes| {
        let _ = table::read(bytes);
    });
}

#[test]
fn a_geojson_file_is_refused_rather_than_panicked_at() {
    hammer("geojson", &geojson_file(), |bytes| {
        let _ = geojson::parse(bytes);
    });
}

#[test]
fn an_npy_header_is_refused_rather_than_panicked_at() {
    let seed = npy_header_bytes();
    hammer("npy split", &seed, |bytes| {
        let _ = npy_header::split(bytes);
    });
    hammer("npy classify", &seed, |bytes| {
        let _ = npy_header::classify(bytes);
    });
}

#[test]
fn the_seeds_this_fuzzing_starts_from_are_actually_valid() {
    // Otherwise every case above is exercising the first error path and the run
    // proves nothing — the shape of vacuous fuzzing.
    assert!(table::read(&table_blob()).is_ok(), "the table seed");
    assert_eq!(
        geojson::parse(&geojson_file())
            .expect("the geojson seed")
            .len(),
        2
    );
    assert!(
        npy_header::split(&npy_header_bytes()).is_ok(),
        "the npy seed"
    );
}
