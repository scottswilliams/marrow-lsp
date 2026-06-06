//! Signature help for incomplete call expressions, driven from cached analysis.

use std::path::Path;

use lsp_types::SignatureHelp;
use marrow_check::{AnalysisSnapshot, CheckedProgram};
use marrow_syntax::LexedSource;

mod active_call;
mod render;
mod signatures;

pub fn signature_help(
    program: &CheckedProgram,
    docs: Option<&AnalysisSnapshot>,
    file: &Path,
    source: &str,
    lexed: &LexedSource,
    offset: usize,
) -> Option<SignatureHelp> {
    let call = active_call::active_call(source, lexed, offset)?;
    let signature = signatures::signature_for(program, docs, file, &call.segments)?;
    Some(render::help(
        signature,
        call.active_parameter,
        call.named_argument,
    ))
}

#[cfg(test)]
mod tests;
