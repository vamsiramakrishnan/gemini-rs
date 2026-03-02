//! Built-in server-side tools that modify LLM requests rather than executing locally.

pub mod google_search;

pub use google_search::GoogleSearchTool;
