//! Signature help for incomplete call expressions, driven from cached analysis.

use std::path::Path;

use lsp_types::SignatureHelp;
use marrow_check::{AnalysisSnapshot, CheckedProgram};
use marrow_syntax::LexedSource;

mod render;
mod signatures;

pub fn signature_help(
    program: &CheckedProgram,
    snapshot: Option<&AnalysisSnapshot>,
    file: &Path,
    source: &str,
    lexed: &LexedSource,
    offset: usize,
) -> Option<SignatureHelp> {
    let parsed = marrow_syntax::parse_source(source);
    let call = marrow_check::tooling::active_callable_context(source, lexed, &parsed, offset)?;
    let signature = signatures::signature_for(program, snapshot, file, &call.callee_path_segments)?;
    Some(render::help(
        signature,
        call.active_argument,
        call.named_argument,
    ))
}

#[cfg(test)]
mod tests;
