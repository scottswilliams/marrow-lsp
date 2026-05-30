//! Transport-free language intelligence for Marrow tooling.
//!
//! The LSP server, the MCP server, and the DAP adapter are thin transports over
//! this one library. It reuses the canonical marrow crates for all real analysis
//! and never reimplements the parser, checker, or type system.

pub mod code_lens;
pub mod completion;
pub mod data_explorer;
pub mod diagnostics;
pub mod documents;
pub mod formatting;
pub mod hover;
pub mod positions;
pub mod semantic_tokens;
pub mod store;
pub mod symbols;
pub mod workspace;
