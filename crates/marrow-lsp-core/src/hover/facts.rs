use std::path::Path;

use marrow_check::{
    AnalysisSnapshot, BindingIndex, CatalogEntryKind, CheckedFacts, MarrowType, SymbolKind,
    SymbolRef, UseSiteKind,
    tooling::{
        self, CallableSignature, CallableSignatureKind, SavedPlaceHoverFact,
        SourceCallableHoverFact, StoreRootHoverFact, saved_place_hover_fact_at,
        source_callable_hover_fact_at, source_symbol_docs_at, store_root_hover_fact_at,
    },
    type_at,
};
use marrow_schema::{EnumSchema, ResourceSchema};
use marrow_syntax::{Keyword, Token, TokenKind, lex_source};

use crate::callables::render_callable_signature;
use crate::language_facts;

use super::source;
use super::tokens;

pub(super) enum HoverFact<'a> {
    CanonicalLibraryText(String),
    SourceCallable {
        fact: SourceCallableHoverFact,
        checked_facts: &'a CheckedFacts,
    },
    StoreRoot(StoreRootHoverFact),
    Resource {
        schema: &'a ResourceSchema,
    },
    Enum {
        schema: &'a EnumSchema,
    },
    EnumMember {
        schema: &'a EnumSchema,
        ordinal: usize,
    },
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
    if blocked_module_prefix_hover(snapshot, index, file, offset) {
        return None;
    }
    if let Some(fact) = store_root_hover_fact_at(snapshot, file, offset) {
        return Some(HoverFact::StoreRoot(fact));
    }
    if let Some(fact) = enum_annotation_fact(snapshot, file, offset) {
        return Some(fact);
    }
    if let Some(fact) = rich_symbol_fact(snapshot, index, file, offset) {
        return Some(fact);
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

fn blocked_module_prefix_hover(
    snapshot: &AnalysisSnapshot,
    index: &BindingIndex,
    file: &Path,
    offset: usize,
) -> bool {
    let Some(analyzed) = snapshot.files.iter().find(|f| f.path == file) else {
        return false;
    };
    let Some((segments, cursor_segment, declaration)) =
        tokens::module_path_at_with_position(&analyzed.source, offset)
    else {
        return false;
    };
    if declaration || cursor_segment + 1 == segments.len() {
        return false;
    }
    if let Some(symbol) = index.definition(file, offset)
        && matches!(symbol.kind, SymbolKind::Enum | SymbolKind::EnumMember)
    {
        return false;
    }
    true
}

fn enum_annotation_fact<'a>(
    snapshot: &'a AnalysisSnapshot,
    file: &Path,
    offset: usize,
) -> Option<HoverFact<'a>> {
    let site = snapshot
        .use_sites()
        .iter()
        .filter(|site| site.kind == UseSiteKind::Enum)
        .filter(|site| site.file == file && source::span_covers(site.span, offset))
        .min_by_key(|site| site.span.end_byte.saturating_sub(site.span.start_byte))?;
    let declaration = snapshot.catalog_declaration(&site.catalog_id)?;
    if declaration.kind != CatalogEntryKind::Enum {
        return None;
    }
    let schema = enum_schema_in_file(snapshot, &declaration.file, &declaration.name)?;
    Some(HoverFact::Enum { schema })
}

fn rich_symbol_fact<'a>(
    snapshot: &'a AnalysisSnapshot,
    index: &BindingIndex,
    file: &Path,
    offset: usize,
) -> Option<HoverFact<'a>> {
    let symbol = index.definition(file, offset)?;
    if !is_rich_symbol_hover_target(snapshot, index, file, offset, &symbol) {
        return None;
    }
    match symbol.kind {
        SymbolKind::Resource => {
            let schema = resource_schema_for_symbol(snapshot, &symbol)?;
            Some(HoverFact::Resource { schema })
        }
        SymbolKind::Enum => {
            let schema = enum_schema_for_symbol(snapshot, &symbol)?;
            Some(HoverFact::Enum { schema })
        }
        SymbolKind::EnumMember => {
            let (schema, ordinal) = enum_member_schema_for_symbol(snapshot, &symbol)?;
            Some(HoverFact::EnumMember { schema, ordinal })
        }
        _ => None,
    }
}

fn default_library_call_text(
    snapshot: &AnalysisSnapshot,
    file: &Path,
    offset: usize,
) -> Option<String> {
    let analyzed = snapshot.files.iter().find(|f| f.path == file)?;
    let tokens = lex_source(&analyzed.source).tokens;
    let index = tokens::path_segment_index_at(&tokens, offset)?;
    if let Some(value) = std_library_path_text(snapshot, file, &tokens, &analyzed.source, index) {
        return Some(value);
    }

    let (segments, leaf_index) = tokens::callable_path_at(&tokens, &analyzed.source, index)?;
    (index == leaf_index)
        .then(|| tooling::intrinsic_callable_signature_for_file(snapshot, file, &segments))?
        .map(|signature| render_callable_text(&signature))
}

fn std_library_path_text(
    snapshot: &AnalysisSnapshot,
    file: &Path,
    tokens: &[Token],
    source: &str,
    index: usize,
) -> Option<String> {
    let (module_index, op_index) = tokens::std_operation_call_for_segment(tokens, source, index)?;
    if index == op_index {
        return None;
    }
    let module = tokens[module_index].text(source);
    let op = tokens[op_index].text(source);
    let segments = ["std", module, op]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    tooling::intrinsic_callable_signature_for_file(snapshot, file, &segments)?;
    if index + 2 == module_index {
        return Some(language_facts::std_namespace_hover());
    }
    if index == module_index {
        return language_facts::std_module_hover(module);
    }
    None
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

fn is_rich_symbol_hover_target(
    snapshot: &AnalysisSnapshot,
    index: &BindingIndex,
    file: &Path,
    offset: usize,
    symbol: &SymbolRef,
) -> bool {
    match symbol.kind {
        SymbolKind::Resource => {
            source::is_named_symbol_reference(snapshot, index, file, offset, symbol)
                || source::is_symbol_declaration_name(snapshot, file, offset, symbol)
        }
        SymbolKind::Enum => {
            !blocked_enum_type_symbol_hover(snapshot, file, offset, symbol)
                && (source::is_named_symbol_reference(snapshot, index, file, offset, symbol)
                    || source::is_symbol_declaration_name(snapshot, file, offset, symbol))
        }
        SymbolKind::EnumMember => {
            source::is_named_symbol_reference(snapshot, index, file, offset, symbol)
                || source::is_symbol_declaration_name(snapshot, file, offset, symbol)
        }
        _ => false,
    }
}

fn blocked_enum_type_symbol_hover(
    snapshot: &AnalysisSnapshot,
    file: &Path,
    offset: usize,
    symbol: &SymbolRef,
) -> bool {
    let Some(analyzed) = snapshot.files.iter().find(|f| f.path == file) else {
        return false;
    };
    if !crate::type_context::type_annotation_at(&analyzed.parsed.file, offset) {
        return false;
    }
    let Some((segments, _)) = tokens::qualified_name_at_with_position(&analyzed.source, offset)
    else {
        return false;
    };
    segments.len() > 1 || symbol.file != file
}

fn resource_schema_for_symbol<'a>(
    snapshot: &'a AnalysisSnapshot,
    symbol: &SymbolRef,
) -> Option<&'a ResourceSchema> {
    let analyzed = snapshot.files.iter().find(|f| f.path == symbol.file)?;
    let resource = source::resource_at(&analyzed.parsed.file, symbol.span)?;
    snapshot
        .program
        .modules
        .iter()
        .find(|module| module.source_file == symbol.file)?
        .resources
        .iter()
        .find(|schema| schema.name == resource.name)
}

fn enum_schema_for_symbol<'a>(
    snapshot: &'a AnalysisSnapshot,
    symbol: &SymbolRef,
) -> Option<&'a EnumSchema> {
    let analyzed = snapshot.files.iter().find(|f| f.path == symbol.file)?;
    let enum_decl = source::enum_at(&analyzed.parsed.file, symbol.span)?;
    enum_schema_in_file(snapshot, &symbol.file, &enum_decl.name)
}

fn enum_member_schema_for_symbol<'a>(
    snapshot: &'a AnalysisSnapshot,
    symbol: &SymbolRef,
) -> Option<(&'a EnumSchema, usize)> {
    let analyzed = snapshot.files.iter().find(|f| f.path == symbol.file)?;
    let (enum_name, path) = source::enum_member_path_at(&analyzed.parsed.file, symbol.span)?;
    let schema = enum_schema_in_file(snapshot, &symbol.file, &enum_name)?;
    let path = path.iter().map(String::as_str).collect::<Vec<_>>();
    let marrow_schema::MemberPathResolution::Found(ordinal) = schema.walk_member_path(&path) else {
        return None;
    };
    Some((schema, ordinal))
}

fn enum_schema_in_file<'a>(
    snapshot: &'a AnalysisSnapshot,
    file: &Path,
    name: &str,
) -> Option<&'a EnumSchema> {
    snapshot
        .program
        .modules
        .iter()
        .find(|module| module.source_file == file)?
        .enums
        .iter()
        .find(|enum_schema| enum_schema.name == name)
}
