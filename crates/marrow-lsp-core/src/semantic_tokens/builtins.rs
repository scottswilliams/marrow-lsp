use std::{collections::HashMap, path::Path};

use marrow_check::{
    AnalysisSnapshot,
    tooling::{self, CallableSignatureKind},
};
use marrow_syntax::{LexedSource, ParsedSource, SourceSpan, Token, TokenKind};

use super::{
    ByteSpan, MOD_DEFAULT_LIBRARY, TYPE_FUNCTION, TYPE_NAMESPACE, TYPE_TYPE, TokenStyle,
    syntax::is_path_segment_token,
};

pub(super) fn builtin_overrides(
    lexed: &LexedSource,
    parsed: &ParsedSource,
    source: &str,
    analysis: Option<(&AnalysisSnapshot, &Path)>,
) -> HashMap<ByteSpan, TokenStyle> {
    let mut overrides = HashMap::new();
    let token_indices = token_indices_by_span(lexed);

    for call in tooling::callable_callee_contexts(source, lexed, parsed) {
        let Some(callable) = intrinsic_callable(analysis, &call.callee_path_segments) else {
            continue;
        };
        if callable == CallableSignatureKind::StandardLibrary {
            let leaf = token_indices
                .get(&byte_span(call.callee_leaf_span))
                .copied();
            for prefix in leaf
                .into_iter()
                .flat_map(|index| callee_prefix_tokens(lexed, index))
            {
                insert_builtin_namespace_prefix(&mut overrides, prefix);
            }
        }
        overrides.insert(
            byte_span(call.callee_leaf_span),
            TokenStyle {
                token_type: callable_token_type(callable),
                modifiers: MOD_DEFAULT_LIBRARY,
            },
        );
    }
    overrides
}

fn intrinsic_callable(
    analysis: Option<(&AnalysisSnapshot, &Path)>,
    segments: &[String],
) -> Option<CallableSignatureKind> {
    let signature = match analysis {
        Some((snapshot, file)) => {
            tooling::intrinsic_callable_signature_for_file(snapshot, file, segments)
        }
        None => tooling::intrinsic_callable_signature(segments),
    }?;
    Some(signature.kind)
}

fn callable_token_type(kind: CallableSignatureKind) -> u32 {
    match kind {
        CallableSignatureKind::ScalarConversion => TYPE_TYPE,
        CallableSignatureKind::Builtin
        | CallableSignatureKind::ErrorConstructor
        | CallableSignatureKind::IdentityConstructor
        | CallableSignatureKind::StandardLibrary => TYPE_FUNCTION,
    }
}

fn insert_builtin_namespace_prefix(overrides: &mut HashMap<ByteSpan, TokenStyle>, token: &Token) {
    overrides.insert(
        (token.span.start_byte, token.span.end_byte),
        TokenStyle {
            token_type: TYPE_NAMESPACE,
            modifiers: MOD_DEFAULT_LIBRARY,
        },
    );
}

fn byte_span(span: SourceSpan) -> ByteSpan {
    (span.start_byte, span.end_byte)
}

fn token_indices_by_span(lexed: &LexedSource) -> HashMap<ByteSpan, usize> {
    lexed
        .tokens
        .iter()
        .enumerate()
        .map(|(index, token)| ((token.span.start_byte, token.span.end_byte), index))
        .collect()
}

fn callee_prefix_tokens(lexed: &LexedSource, leaf: usize) -> Vec<&Token> {
    let mut indices = vec![leaf];
    let mut index = leaf;
    while index >= 2
        && lexed.tokens[index - 1].kind == TokenKind::DoubleColon
        && is_path_segment_token(lexed.tokens[index - 2].kind)
    {
        index -= 2;
        indices.push(index);
    }
    indices.reverse();
    indices.pop();
    indices
        .into_iter()
        .map(|index| &lexed.tokens[index])
        .collect()
}
