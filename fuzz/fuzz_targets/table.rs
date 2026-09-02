//! `blockflow` table blobs — the format most worth fuzzing here.
//!
//! Every count in it is a `u64` the file chooses: the column count, the row
//! count, and each name's length, all used to allocate and to multiply. The
//! deterministic sweep in `server/tests/parser_fuzz.rs` found a capacity
//! overflow here on its first run.
#![no_main]

use libfuzzer_sys::fuzz_target;
use omezarr_viewer_server::objects::table;

fuzz_target!(|data: &[u8]| {
    let _ = table::read(data);
});
