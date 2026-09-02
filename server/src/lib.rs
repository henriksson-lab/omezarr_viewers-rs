//! The viewer server: sources, sessions, zarr reading and the HTTP API.
//!
//! Split out of the binary so integration tests can drive the same code the
//! server runs. `src/main.rs` is the CLI around it and nothing else.

pub mod annotations;
pub mod api;
pub mod cache;
/// Measurement scaffolding for the chunk-reuse question; see the module docs.
#[cfg(test)]
mod chunk_probe;
#[cfg(test)]
mod chunk_reuse;
pub mod convert;
pub mod npy_header;
pub mod npy_volume;
pub mod objects;
pub mod ontology;
pub mod pixels;
pub mod project;
pub mod session;
pub mod source;
pub mod synthetic;
pub mod volume;
pub mod zarr_reader;
