//! The viewer server: sources, sessions, zarr reading and the HTTP API.
//!
//! Split out of the binary so integration tests can drive the same code the
//! server runs. `src/main.rs` is the CLI around it and nothing else.

pub mod api;
pub mod cache;
pub mod convert;
pub mod npy_volume;
pub mod objects;
pub mod ontology;
pub mod project;
pub mod session;
pub mod source;
pub mod synthetic;
pub mod volume;
pub mod zarr_reader;
