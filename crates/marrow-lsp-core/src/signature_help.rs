//! Signature help for incomplete call expressions, driven from cached analysis.

use std::path::Path;

use lsp_types::SignatureHelp;
use marrow_check::{AnalysisSnapshot, CheckedProgram};
use marrow_syntax::LexedSource;

mod render;

pub fn signature_help(
    program: &CheckedProgram,
    snapshot: Option<&AnalysisSnapshot>,
    file: &Path,
    source: &str,
    lexed: &LexedSource,
    offset: usize,
) -> Option<SignatureHelp> {
    let fact = marrow_check::tooling::source_signature_help_fact_at(
        program, snapshot, file, source, lexed, offset,
    )?;
    Some(render::help(program, fact))
}

#[cfg(test)]
mod tests;
