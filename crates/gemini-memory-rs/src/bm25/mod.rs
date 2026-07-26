//! Local lexical retrieval.
//!
//! The default retrieval engine is BM25 over the compiled OKF corpus: warm, in
//! process, and fast enough that memory can be prepared speculatively while the
//! user is still speaking. Semantic retrieval is a fallback for the queries
//! lexical search genuinely cannot serve, not the default path.

pub mod explain;
pub mod index;
pub mod schema;

pub use explain::{BoostKind, ScoreComponent, SearchExplanation};
pub use index::{MemoryIndex, Query, SearchHit};
pub use schema::{tokenize, Field, IndexedMemory, MemoryOrigin};
