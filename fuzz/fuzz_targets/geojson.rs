//! QuPath GeoJSON, as read from a file this viewer did not write.
//!
//! `serde_json` handles the JSON itself and enforces its own nesting limit, so
//! what is under test here is everything after that: the walk over
//! `childObjects`, the coordinate arrays, and the property readers.
#![no_main]

use libfuzzer_sys::fuzz_target;
use omezarr_viewer_server::annotations::geojson;

fuzz_target!(|data: &[u8]| {
    // The contract is that this returns. Anything a file can contain must come
    // back as an error naming the reason, never as a panic.
    let _ = geojson::parse(data);
});
