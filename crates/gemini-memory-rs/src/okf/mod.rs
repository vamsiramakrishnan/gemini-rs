//! The canonical memory format: human-readable OKF Markdown.
//!
//! The durable asset is the corpus, not the index. A user's memory is a set of
//! Markdown files they can read, diff, hand-edit and delete; every retrieval
//! structure in this crate is derived from them and can be rebuilt from
//! scratch.

pub mod document;
pub mod record;
pub mod repository;
pub mod store;
pub mod yaml;

pub use document::OkfDocument;
pub use record::{from_document, to_document, OKF_VERSION};
pub use repository::{
    category_path, ManifestEntry, MemoryManifest, MemoryRepository, MemoryTransaction, MemoryWrite,
    OkfRepository, ReconciliationSelector, Tombstone, MANIFEST_SCHEMA_VERSION,
};
pub use store::{FsStore, MemoryStore, OkfStore};
pub use yaml::{Yaml, YamlError};
