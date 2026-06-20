use std::path::Path;

use marrow_check::{
    AnalysisSnapshot, BindingIndex, CatalogEntryKind, CheckedConst, CheckedFacts, CheckedFunction,
    CheckedParam, DirectEffectFacts, FunctionFact, MarrowType, SymbolKind, SymbolRef, UseSiteKind,
    tooling::{self, CallableSignature, CallableSignatureKind},
    type_at,
};
use marrow_schema::{EnumSchema, ResourceSchema, StoreSchema};
use marrow_syntax::{
    FunctionDecl, IndexDecl, Keyword, ResourceMember, Token, TokenKind, lex_source,
};

use crate::callables::render_callable_signature;
use crate::language_facts;

use super::source;
use super::tokens;

pub(super) enum HoverFact<'a> {
    CanonicalLibraryText(String),
    Function {
        checked: &'a CheckedFunction,
        parsed: &'a FunctionDecl,
        effects: Option<DirectEffects<'a>>,
    },
    Parameter {
        checked: &'a CheckedParam,
        docs: &'a [String],
    },
    ModuleConst {
        checked: &'a CheckedConst,
        docs: &'a [String],
    },
    StoreRoot {
        store: &'a StoreSchema,
        resource: &'a ResourceSchema,
    },
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
    ResourceMember {
        member: &'a ResourceMember,
    },
    Index {
        index: &'a IndexDecl,
    },
    Type {
        ty: MarrowType,
        docs: Option<&'a [String]>,
    },
}

pub(super) struct DirectEffects<'a> {
    pub(super) facts: &'a CheckedFacts,
    pub(super) effects: &'a DirectEffectFacts,
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
    if let Some(fact) = function_fact(snapshot, index, file, offset) {
        return Some(fact);
    }
    if let Some(fact) = parameter_fact(snapshot, index, file, offset) {
        return Some(fact);
    }
    if let Some(fact) = module_const_fact(snapshot, index, file, offset) {
        return Some(fact);
    }
    if blocked_module_prefix_hover(snapshot, index, file, offset) {
        return None;
    }
    if let Some(fact) = store_root_declaration_fact(snapshot, file, offset) {
        return Some(fact);
    }
    if let Some(fact) = enum_annotation_fact(snapshot, file, offset) {
        return Some(fact);
    }
    if let Some(fact) = rich_symbol_fact(snapshot, index, file, offset) {
        return Some(fact);
    }
    if let Some(fact) = resource_member_fact(snapshot, index, file, offset) {
        return Some(fact);
    }

    let docs = symbol_docs_at_hover_target(snapshot, index, file, offset);
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

pub(super) fn symbol_docs_lines<'a>(
    snapshot: &'a AnalysisSnapshot,
    index: &BindingIndex,
    file: &Path,
    offset: usize,
) -> Option<&'a [String]> {
    let symbol = index.definition(file, offset)?;
    let analyzed = snapshot.files.iter().find(|f| f.path == symbol.file)?;
    source::declaration_docs(&analyzed.parsed.file, &symbol)
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

fn function_fact<'a>(
    snapshot: &'a AnalysisSnapshot,
    index: &BindingIndex,
    file: &Path,
    offset: usize,
) -> Option<HoverFact<'a>> {
    let symbol = index.definition(file, offset)?;
    if symbol.kind != SymbolKind::Function
        || !source::is_function_hover_target(snapshot, index, file, offset, &symbol)
    {
        return None;
    }

    let parsed_file = snapshot.files.iter().find(|f| f.path == symbol.file)?;
    let parsed = source::parsed_function_at(&parsed_file.parsed.file, symbol.span)?;
    let checked = checked_function_for_parsed(snapshot, &symbol, &parsed_file.parsed.file, parsed)?;
    let effects = function_fact_for_symbol(snapshot, &symbol, checked).map(|fact| DirectEffects {
        facts: &snapshot.program.facts,
        effects: &fact.direct_effects,
    });
    Some(HoverFact::Function {
        checked,
        parsed,
        effects,
    })
}

fn parameter_fact<'a>(
    snapshot: &'a AnalysisSnapshot,
    index: &BindingIndex,
    file: &Path,
    offset: usize,
) -> Option<HoverFact<'a>> {
    let symbol = index.definition(file, offset)?;
    if symbol.kind != SymbolKind::Param {
        return None;
    }

    let parameter = index.parameter_definition(&symbol)?;
    let function_fact = snapshot.program.facts.function(parameter.function);
    let parsed_file = snapshot.files.iter().find(|f| f.path == symbol.file)?;
    let parsed_function = source::parsed_function_at(&parsed_file.parsed.file, function_fact.span)?;
    let name = parameter_use_name(snapshot, file, offset, parsed_function)?;
    let checked_function = checked_function_for_fact(snapshot, function_fact)?;
    let checked = checked_function.params.get(parameter.index)?;
    if checked.name != name {
        return None;
    }
    let parsed_param = parsed_function.params.get(parameter.index)?;

    Some(HoverFact::Parameter {
        checked,
        docs: &parsed_param.docs,
    })
}

fn parameter_use_name<'a>(
    snapshot: &'a AnalysisSnapshot,
    file: &Path,
    offset: usize,
    function: &FunctionDecl,
) -> Option<&'a str> {
    if !source::span_covers(function.body.span, offset) {
        return None;
    }
    let analyzed = snapshot.files.iter().find(|f| f.path == file)?;
    let tokens = lex_source(&analyzed.source).tokens;
    let (index, token) = tokens.iter().enumerate().find(|(_, token)| {
        token.kind == TokenKind::Identifier && source::span_covers(token.span, offset)
    })?;
    let (previous, next) = tokens::significant_neighbors(&tokens, index);
    if previous.is_some_and(|kind| {
        matches!(
            kind,
            TokenKind::DoubleColon | TokenKind::Dot | TokenKind::QuestionDot
        )
    }) || next.is_some_and(|kind| kind == TokenKind::Colon)
    {
        return None;
    }
    Some(token.text(&analyzed.source))
}

fn module_const_fact<'a>(
    snapshot: &'a AnalysisSnapshot,
    index: &BindingIndex,
    file: &Path,
    offset: usize,
) -> Option<HoverFact<'a>> {
    let symbol = index.definition(file, offset)?;
    if symbol.kind != SymbolKind::ModuleConst || symbol.file != file {
        return None;
    }

    let parsed_file = snapshot.files.iter().find(|f| f.path == symbol.file)?;
    let parsed_const = source::parsed_const_at(&parsed_file.parsed.file, symbol.span)?;
    if !source::offset_is_on_declaration_name(
        &parsed_file.source,
        symbol.span,
        &parsed_const.name,
        offset,
    ) {
        return None;
    }
    let checked = checked_const_at(snapshot, &symbol)?;
    Some(HoverFact::ModuleConst {
        checked,
        docs: &parsed_const.docs,
    })
}

fn store_root_declaration_fact<'a>(
    snapshot: &'a AnalysisSnapshot,
    file: &Path,
    offset: usize,
) -> Option<HoverFact<'a>> {
    let analyzed = snapshot.files.iter().find(|f| f.path == file)?;
    let store = source::store_root_at(&analyzed.parsed.file, &analyzed.source, offset)?;
    let store_resource =
        marrow_check::resolve::resolve_store_by_root(&snapshot.program, &store.root.root)?;
    Some(HoverFact::StoreRoot {
        store: store_resource.store,
        resource: store_resource.resource,
    })
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

fn resource_member_fact<'a>(
    snapshot: &'a AnalysisSnapshot,
    index: &BindingIndex,
    file: &Path,
    offset: usize,
) -> Option<HoverFact<'a>> {
    let symbol = index.definition(file, offset)?;
    if !matches!(
        symbol.kind,
        SymbolKind::Field | SymbolKind::Layer | SymbolKind::Index
    ) || !source::is_resource_member_hover_target(snapshot, index, file, offset, &symbol)
    {
        return None;
    }

    let parsed_file = snapshot.files.iter().find(|f| f.path == symbol.file)?;
    if symbol.kind == SymbolKind::Index {
        let index = source::store_index_at(&parsed_file.parsed.file, symbol.span)?;
        Some(HoverFact::Index { index })
    } else {
        let member = source::resource_member_at(&parsed_file.parsed.file, symbol.span)?;
        Some(HoverFact::ResourceMember { member })
    }
}

fn symbol_docs_at_hover_target<'a>(
    snapshot: &'a AnalysisSnapshot,
    index: &BindingIndex,
    file: &Path,
    offset: usize,
) -> Option<&'a [String]> {
    let symbol = index.definition(file, offset)?;
    if symbol.kind == SymbolKind::Function
        && !source::is_function_hover_target(snapshot, index, file, offset, &symbol)
    {
        return None;
    }
    symbol_docs_lines(snapshot, index, file, offset)
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

fn checked_function_for_parsed<'a>(
    snapshot: &'a AnalysisSnapshot,
    symbol: &SymbolRef,
    source_file: &marrow_syntax::SourceFile,
    parsed_function: &FunctionDecl,
) -> Option<&'a CheckedFunction> {
    let function_index = source_file
        .declarations
        .iter()
        .filter_map(|declaration| match declaration {
            marrow_syntax::Declaration::Function(function) => Some(function),
            _ => None,
        })
        .position(|function| function.span == parsed_function.span)?;
    snapshot
        .program
        .modules
        .iter()
        .find(|module| module.source_file == symbol.file)?
        .functions
        .get(function_index)
}

fn checked_function_for_fact<'a>(
    snapshot: &'a AnalysisSnapshot,
    fact: &FunctionFact,
) -> Option<&'a CheckedFunction> {
    let module_fact = snapshot
        .program
        .facts
        .modules()
        .iter()
        .find(|module| module.id == fact.module)?;
    snapshot
        .program
        .modules
        .iter()
        .find(|module| module.source_file == module_fact.source_file)?
        .functions
        .get(fact.source_index as usize)
}

fn checked_const_at<'a>(
    snapshot: &'a AnalysisSnapshot,
    symbol: &SymbolRef,
) -> Option<&'a CheckedConst> {
    snapshot
        .program
        .modules
        .iter()
        .find(|module| module.source_file == symbol.file)?
        .constants
        .iter()
        .find(|constant| source::span_contains_span(constant.span, symbol.span))
}

fn function_fact_for_symbol<'a>(
    snapshot: &'a AnalysisSnapshot,
    symbol: &SymbolRef,
    checked_function: &CheckedFunction,
) -> Option<&'a FunctionFact> {
    let module = snapshot
        .program
        .modules
        .iter()
        .find(|module| module.source_file == symbol.file)?;
    let module_fact = snapshot
        .program
        .facts
        .modules()
        .iter()
        .find(|fact| fact.name == module.name && fact.source_file == module.source_file)?;
    snapshot.program.facts.functions().iter().find(|fact| {
        fact.module == module_fact.id
            && fact.name == checked_function.name
            && fact.span == checked_function.span
    })
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
