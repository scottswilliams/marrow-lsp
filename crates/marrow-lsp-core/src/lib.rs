//! Transport-free language intelligence for Marrow tooling.
//!
//! The LSP server, the MCP server, and the DAP adapter are thin transports over
//! this one library. It reuses the canonical marrow crates for all real analysis
//! and never reimplements the parser, checker, or type system.

mod callables;
pub mod catalog_binding;
pub mod completion;
pub mod data_explorer;
pub mod data_integrity;
pub mod diagnostics;
pub mod documents;
pub mod formatting;
pub mod hover;
mod language_facts;
pub mod mcp;
pub mod navigation;
pub mod positions;
pub mod semantic_tokens;
pub mod signature_help;
pub mod store;
pub mod symbols;
mod type_context;
mod types;
pub mod workspace;
