//! Face recognition support for anytopdf.
//!
//! Detection and embedding run in runtime plugins; this crate holds what both
//! sides share: landmark alignment, the workspace hand-off for embeddings,
//! and (with the `index` feature) the local SQLite face index plus the host
//! step that names faces in a graph. Identities come only from people the
//! user enrolls or names. Nothing here estimates age, gender, emotion or any
//! other trait.

pub mod align;
pub mod paths;
pub mod vector;
pub mod workspace;

#[cfg(feature = "index")]
pub mod index;
#[cfg(feature = "index")]
pub mod recognize;
