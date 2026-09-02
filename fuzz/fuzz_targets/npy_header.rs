//! `.npy` headers: a length-prefixed blob followed by a Python dict literal.
//!
//! Two parsers rather than one, because `classify` reads what `split` returns
//! and a header that splits cleanly can still describe a shape nothing can use.
#![no_main]

use libfuzzer_sys::fuzz_target;
use omezarr_viewer_server::npy_header;

fuzz_target!(|data: &[u8]| {
    let _ = npy_header::split(data);
    let _ = npy_header::classify(data);
});
