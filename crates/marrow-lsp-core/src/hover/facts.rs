use std::path::Path;

use marrow_check::{
    AnalysisSnapshot, BindingIndex, CheckedFacts, MarrowType,
    tooling::{
        self, CallableSignature, CallableSignatureKind, SavedPlaceHoverFact,
        SourceCallableHoverFact, SourceModulePathHoverFact, SourceSchemaHoverFact,
        StoreRootHoverFact, saved_place_hover_fact_at, source_callable_hover_fact_at,
        source_module_path_hover_fact_at, source_schema_hover_fact_at, source_symbol_docs_at,
        store_root_hover_fact_at,
    },
    type_at,
};
use marrow_syntax::{Keyword, TokenKind, lex_source};

use crate::callables::render_callable_signature;
use crate::language_facts;

use super::tokens;

pub(super) enum HoverFact<'a> {
    CanonicalLibraryText(String),
    SourceCallable {
        fact: SourceCallableHoverFact,
        checked_facts: &'a CheckedFacts,
    },
    SourceModulePath(SourceModulePathHoverFact),
    StoreRoot(StoreRootHoverFact),
    SourceSchema(SourceSchemaHoverFact),
    SavedPlace(SavedPlaceHoverFact),
    Type {
        ty: MarrowType,
        docs: Option<Vec<String>>,
    },
}

pub(super) fn collect<'a>(
    snapshot: &'a AnalysisSnapshot,
    index: &'a BindingIndex,
    file: &Path,
    offset: usize,
) -> Option<HoverFact<'a>> {
    if let Some(value) = default_library_call_text(snapshot, file, offset) {
        return Some(HoverFact::CanonicalLibraryText(value));
    }
    if let Some(value) = operator_text(snapshot, file, offset) {
        return Some(HoverFact::CanonicalLibraryText(value));
    }
    if let Some(fact) = source_callable_hover_fact_at(snapshot, index, file, offset) {
        return Some(HoverFact::SourceCallable {
            fact,
            checked_facts: &snapshot.program.facts,
        });
    }
    if let Some(fact) = source_module_path_hover_fact_at(snapshot, index, file, offset) {
        return Some(HoverFact::SourceModulePath(fact));
    }
    if let Some(fact) = store_root_hover_fact_at(snapshot, file, offset) {
        return Some(HoverFact::StoreRoot(fact));
    }
    if let Some(fact) = source_schema_hover_fact_at(snapshot, index, file, offset) {
        return Some(HoverFact::SourceSchema(fact));
    }
    if let Some(fact) = saved_place_hover_fact_at(snapshot, index, file, offset) {
        return Some(HoverFact::SavedPlace(fact));
    }

    let docs = source_symbol_docs_at(snapshot, index, file, offset).map(|docs| docs.lines);
    checked_type_at(snapshot, file, offset).map(|ty| HoverFact::Type { ty, docs })
}

pub(super) fn checked_type_at(
    snapshot: &AnalysisSnapshot,
    file: &Path,
    offset: usize,
) -> Option<MarrowType> {
    let analyzed = snapshot.files.iter().find(|f| f.path == file)?;
    type_at(&snapshot.program, file, &analyzed.parsed, offset)
}

fn default_library_call_text(
    snapshot: &AnalysisSnapshot,
    file: &Path,
    offset: usize,
) -> Option<String> {
    let analyzed = snapshot.files.iter().find(|f| f.path == file)?;
    let tokens = lex_source(&analyzed.source).tokens;
    let index = tokens::path_segment_index_at(&tokens, offset)?;
    let (segments, leaf_index) = tokens::callable_path_at(&tokens, &analyzed.source, index)?;
    (index == leaf_index)
        .then(|| tooling::intrinsic_callable_signature_for_file(snapshot, file, &segments))?
        .map(|signature| render_callable_text(&signature))
}

fn render_callable_text(callable: &CallableSignature) -> String {
    let mut sections = vec![callable_context(callable.kind).to_string()];
    if let Some(docs) = join_callable_docs(&callable.docs) {
        sections.push(docs);
    }
    format!(
        "{}\n\n{}",
        render_callable_signature(callable),
        sections.join("\n\n")
    )
}

fn callable_context(kind: CallableSignatureKind) -> &'static str {
    match kind {
        CallableSignatureKind::Builtin => "default library builtin.",
        CallableSignatureKind::ScalarConversion => "default library scalar conversion.",
        CallableSignatureKind::ErrorConstructor => "default library Error constructor.",
        CallableSignatureKind::IdentityConstructor => "default library Id constructor.",
        CallableSignatureKind::StandardLibrary => "default library std operation.",
    }
}

fn join_callable_docs(lines: &[String]) -> Option<String> {
    let joined = lines
        .iter()
        .map(|line| line.trim())
        .collect::<Vec<_>>()
        .join(" ");
    let joined = joined.trim();
    (!joined.is_empty()).then(|| joined.to_string())
}

fn operator_text(snapshot: &AnalysisSnapshot, file: &Path, offset: usize) -> Option<String> {
    let analyzed = snapshot.files.iter().find(|f| f.path == file)?;
    let tokens = lex_source(&analyzed.source).tokens;
    let (index, token) = tokens
        .iter()
        .enumerate()
        .find(|(_, token)| token.span.start_byte <= offset && offset <= token.span.end_byte)?;
    type_at(&snapshot.program, file, &analyzed.parsed, offset)?;
    if is_operator_keyword_non_expression(&tokens, index) {
        return None;
    }
    language_facts::operator_hover(operator_spelling(token.kind, token.text(&analyzed.source))?)
}

fn is_operator_keyword_non_expression(tokens: &[marrow_syntax::Token], index: usize) -> bool {
    matches!(
        tokens[index].kind,
        TokenKind::Keyword(Keyword::Not | Keyword::And | Keyword::Or | Keyword::Is)
    ) && {
        let (previous, next) = tokens::significant_neighbors(tokens, index);
        previous.is_some_and(|kind| kind == TokenKind::DoubleColon)
            || next.is_some_and(|kind| kind == TokenKind::DoubleColon)
            || previous.is_some_and(|kind| {
                matches!(kind, TokenKind::Keyword(Keyword::Module | Keyword::Use))
            })
    }
}

fn operator_spelling(kind: TokenKind, text: &str) -> Option<&str> {
    match kind {
        TokenKind::Keyword(Keyword::Not | Keyword::And | Keyword::Or | Keyword::Is)
        | TokenKind::DotDot
        | TokenKind::DotDotEqual
        | TokenKind::EqualEqual
        | TokenKind::BangEqual
        | TokenKind::QuestionDot
        | TokenKind::QuestionQuestion
        | TokenKind::Less
        | TokenKind::LessEqual
        | TokenKind::Greater
        | TokenKind::GreaterEqual
        | TokenKind::Plus
        | TokenKind::Minus
        | TokenKind::Star
        | TokenKind::Slash
        | TokenKind::Percent => Some(text),
        _ => None,
    }
}
