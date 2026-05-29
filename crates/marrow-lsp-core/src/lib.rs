//! Transport-free language intelligence for Marrow tooling.
//!
//! The LSP server, the MCP server, and the DAP adapter are thin transports over
//! this one library. It reuses the canonical marrow crates for all real analysis
//! and never reimplements the parser, checker, or type system.

pub mod diagnostics;
pub mod documents;
pub mod hover;
pub mod positions;
pub mod symbols;
pub mod workspace;
