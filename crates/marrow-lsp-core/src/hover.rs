//! Hover: presentation over checked facts, as a `marrow` code block.
//!
//! An editor sends a byte offset (already converted from an LSP position by the
//! document's [`LineIndex`](crate::positions::LineIndex)). Positive semantic hovers
//! render checker facts such as binding-index symbols and expression types. Local
//! token and parse inspection only decides presentation targets or suppresses
//! surfaces whose canonical Marrow facts are not exposed yet.

use std::path::Path;

use lsp_types::Hover;
use marrow_check::{
    AnalysisSnapshot, BindingIndex, CheckedConst, CheckedFunction, CheckedParam, DefItem,
    FunctionFact, Resolution, ResolvableKind, SymbolKind, SymbolRef, build_binding_index, resolve,
    type_at,
};
use marrow_schema::{EnumSchema, ResourceSchema};
use marrow_syntax::{
    Declaration, EnumDecl, EnumMember, FunctionDecl, IndexDecl, Keyword, ResourceDecl,
    ResourceMember, SourceSpan, StoreDecl, TokenKind, lex_source,
};

use crate::language_facts;
use crate::types::render_type;

mod render;

use self::render::{
    append_docs, direct_effects_markdown, enum_hover, enum_member_hover, index_markdown, join_docs,
    markdown_hover, marrow_code_block, resource_hover, resource_member_markdown, store_hover,
};

/// The hover for byte `offset` in `file`, or `None` when no known symbol or type
/// covers it.
///
/// The file must be one the snapshot analyzed; a path it does not know (or an
/// offset no expression covers) yields `None`, so the editor shows nothing.
pub fn hover(snapshot: &AnalysisSnapshot, file: &Path, offset: usize) -> Option<Hover> {
    let index = build_binding_index(snapshot);
    hover_with_index(snapshot, &index, file, offset)
}

/// The hover for byte `offset`, using `index` for resolved symbol facts.
pub fn hover_with_index(
    snapshot: &AnalysisSnapshot,
    index: &BindingIndex,
    file: &Path,
    offset: usize,
) -> Option<Hover> {
    if let Some(value) = default_library_call_hover(snapshot, file, offset) {
        return Some(markdown_hover(default_library_hover_markdown(&value)));
    }
    if let Some(value) = operator_hover(snapshot, file, offset) {
        return Some(markdown_hover(default_library_hover_markdown(&value)));
    }
    if let Some(value) = function_hover(snapshot, index, file, offset) {
        return Some(markdown_hover(value));
    }
    if let Some(value) = parameter_hover(snapshot, index, file, offset) {
        return Some(markdown_hover(value));
    }
    if let Some(value) = module_const_hover(snapshot, index, file, offset) {
        return Some(markdown_hover(value));
    }
    if blocked_module_prefix_hover(snapshot, index, file, offset) {
        return None;
    }
    if let Some(value) = store_root_declaration_hover(snapshot, file, offset) {
        return Some(markdown_hover(value));
    }
    if let Some(value) = rich_symbol_hover(snapshot, index, file, offset) {
        return Some(markdown_hover(value));
    }
    if let Some(value) = resource_member_hover(snapshot, index, file, offset) {
        return Some(markdown_hover(value));
    }
    if let Some(value) = resource_constructor_hover(snapshot, file, offset) {
        return Some(markdown_hover(value));
    }
    if let Some(value) = type_name_hover(snapshot, file, offset) {
        return Some(markdown_hover(value));
    }
    let docs = symbol_docs_at_hover_target(snapshot, index, file, offset);
    hover_with_docs(snapshot, file, offset, docs.as_deref())
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
        module_path_at_with_position(&analyzed.source, offset)
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

fn rich_symbol_hover(
    snapshot: &AnalysisSnapshot,
    index: &BindingIndex,
    file: &Path,
    offset: usize,
) -> Option<String> {
    let symbol = index.definition(file, offset)?;
    if !is_rich_symbol_hover_target(snapshot, index, file, offset, &symbol) {
        return None;
    }
    match symbol.kind {
        SymbolKind::Resource => {
            let schema = resource_schema_for_symbol(snapshot, &symbol)?;
            Some(resource_hover(schema))
        }
        SymbolKind::Enum => {
            let schema = enum_schema_for_symbol(snapshot, &symbol)?;
            Some(enum_hover(schema))
        }
        SymbolKind::EnumMember => {
            let (schema, ordinal) = enum_member_schema_for_symbol(snapshot, &symbol)?;
            Some(enum_member_hover(schema, ordinal))
        }
        _ => None,
    }
}

fn type_name_hover(snapshot: &AnalysisSnapshot, file: &Path, offset: usize) -> Option<String> {
    let analyzed = snapshot.files.iter().find(|f| f.path == file)?;
    if type_at(&snapshot.program, file, &analyzed.parsed, offset).is_some() {
        return None;
    }
    if !crate::type_context::type_annotation_at(&analyzed.parsed.file, offset) {
        return None;
    }
    let (segments, segment_index) = qualified_name_at_with_position(&analyzed.source, offset)?;
    if segment_index + 1 == segments.len() {
        if let Some(schema) = resource_schema_for_type_name(snapshot, file, &segments) {
            return Some(resource_hover(schema));
        }
        if let Some(schema) = enum_schema_for_type_name(snapshot, file, &segments) {
            return Some(enum_hover(schema));
        }
    }
    None
}

fn store_root_declaration_hover(
    snapshot: &AnalysisSnapshot,
    file: &Path,
    offset: usize,
) -> Option<String> {
    let analyzed = snapshot.files.iter().find(|f| f.path == file)?;
    let store = store_root_at(&analyzed.parsed.file, &analyzed.source, offset)?;
    let store_resource =
        marrow_check::resolve::resolve_store_by_root(&snapshot.program, &store.root.root)?;
    Some(store_hover(store_resource.store, store_resource.resource))
}

fn resource_member_hover(
    snapshot: &AnalysisSnapshot,
    index: &BindingIndex,
    file: &Path,
    offset: usize,
) -> Option<String> {
    let symbol = index.definition(file, offset)?;
    if !matches!(
        symbol.kind,
        SymbolKind::Field | SymbolKind::Layer | SymbolKind::Index
    ) || !is_resource_member_hover_target(snapshot, index, file, offset, &symbol)
    {
        return None;
    }

    let parsed_file = snapshot.files.iter().find(|f| f.path == symbol.file)?;
    let value = if symbol.kind == SymbolKind::Index {
        let index = store_index_at(&parsed_file.parsed.file, symbol.span)?;
        index_markdown(index)
    } else {
        let member = resource_member_at(&parsed_file.parsed.file, symbol.span)?;
        resource_member_markdown(member)
    };

    Some(value)
}

fn resource_constructor_hover(
    snapshot: &AnalysisSnapshot,
    file: &Path,
    offset: usize,
) -> Option<String> {
    let analyzed = snapshot.files.iter().find(|f| f.path == file)?;
    if !is_resource_constructor_leaf(&analyzed.source, offset) {
        return None;
    }
    let segments = qualified_name_at(&analyzed.source, offset)?;
    let from_module = module_name_for_file(snapshot, file)?;
    match resolve(
        &snapshot.program,
        from_module,
        &segments,
        ResolvableKind::Resource,
    ) {
        Resolution::Found(def) => match def.item {
            DefItem::Resource(schema) => Some(resource_hover(schema)),
            _ => None,
        },
        _ => None,
    }
}

fn symbol_docs_at_hover_target(
    snapshot: &AnalysisSnapshot,
    index: &BindingIndex,
    file: &Path,
    offset: usize,
) -> Option<String> {
    let symbol = index.definition(file, offset)?;
    if symbol.kind == SymbolKind::Function
        && !is_function_hover_target(snapshot, index, file, offset, &symbol)
    {
        return None;
    }
    symbol_docs(snapshot, index, file, offset)
}

/// The hover for byte `offset`, augmented with the symbol's documentation when
/// available.
///
/// The type always comes from the checker and is shown first. `docs`, when
/// present, is the symbol's `;;` description rendered as a paragraph below the
/// type.
fn hover_with_docs(
    snapshot: &AnalysisSnapshot,
    file: &Path,
    offset: usize,
    docs: Option<&str>,
) -> Option<Hover> {
    let analyzed = snapshot.files.iter().find(|f| f.path == file)?;
    let ty = type_at(&snapshot.program, file, &analyzed.parsed, offset)?;
    let mut value = marrow_code_block(&render_type(&ty));

    if let Some(docs) = docs {
        value.push_str("\n\n");
        value.push_str(docs);
    }

    Some(markdown_hover(value))
}

fn function_hover(
    snapshot: &AnalysisSnapshot,
    index: &BindingIndex,
    file: &Path,
    offset: usize,
) -> Option<String> {
    let symbol = index.definition(file, offset)?;
    if symbol.kind != SymbolKind::Function
        || !is_function_hover_target(snapshot, index, file, offset, &symbol)
    {
        return None;
    }

    let parsed_file = snapshot.files.iter().find(|f| f.path == symbol.file)?;
    let parsed_function = parsed_function_at(&parsed_file.parsed.file, symbol.span)?;
    let checked_function =
        checked_function_for_parsed(snapshot, &symbol, &parsed_file.parsed.file, parsed_function)?;
    let mut value = marrow_code_block(&function_signature(checked_function));

    if let Some(docs) = join_docs(&parsed_function.docs) {
        value.push_str("\n\n");
        value.push_str(&docs);
    }

    if let Some(docs) = parameter_docs(parsed_function) {
        value.push_str("\n\n");
        value.push_str(&docs);
    }

    if let Some(effects) = direct_effects_hover(snapshot, &symbol, checked_function) {
        value.push_str("\n\n");
        value.push_str(&effects);
    }

    Some(value)
}

fn direct_effects_hover(
    snapshot: &AnalysisSnapshot,
    symbol: &SymbolRef,
    checked_function: &CheckedFunction,
) -> Option<String> {
    let fact = function_fact_for_symbol(snapshot, symbol, checked_function)?;
    direct_effects_markdown(&snapshot.program.facts, &fact.direct_effects)
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

fn parameter_hover(
    snapshot: &AnalysisSnapshot,
    index: &BindingIndex,
    file: &Path,
    offset: usize,
) -> Option<String> {
    let symbol = index.definition(file, offset)?;
    if symbol.kind != SymbolKind::Param {
        return None;
    }

    let parameter = index.parameter_definition(&symbol)?;
    let function_fact = snapshot.program.facts.function(parameter.function);
    let parsed_file = snapshot.files.iter().find(|f| f.path == symbol.file)?;
    let parsed_function = parsed_function_at(&parsed_file.parsed.file, function_fact.span)?;
    let name = parameter_use_name(snapshot, file, offset, parsed_function)?;
    let checked_function = checked_function_for_fact(snapshot, function_fact)?;
    let checked_param = checked_function.params.get(parameter.index)?;
    if checked_param.name != name {
        return None;
    }
    let parsed_param = parsed_function.params.get(parameter.index)?;

    let mut value = marrow_code_block(&parameter_signature(checked_param));
    if let Some(docs) = join_docs(&parsed_param.docs) {
        value.push_str("\n\n");
        value.push_str("**Parameter**\n");
        value.push_str(&docs);
    }
    Some(value)
}

fn parameter_use_name<'a>(
    snapshot: &'a AnalysisSnapshot,
    file: &Path,
    offset: usize,
    function: &FunctionDecl,
) -> Option<&'a str> {
    if !span_covers(function.body.span, offset) {
        return None;
    }
    let analyzed = snapshot.files.iter().find(|f| f.path == file)?;
    let tokens = lex_source(&analyzed.source).tokens;
    let (index, token) = tokens.iter().enumerate().find(|(_, token)| {
        token.kind == TokenKind::Identifier && span_covers(token.span, offset)
    })?;
    if previous_significant_kind(&tokens, index).is_some_and(|kind| {
        matches!(
            kind,
            TokenKind::DoubleColon | TokenKind::Dot | TokenKind::QuestionDot
        )
    }) || next_significant_kind(&tokens, index).is_some_and(|kind| kind == TokenKind::Colon)
    {
        return None;
    }
    Some(token.text(&analyzed.source))
}

fn module_const_hover(
    snapshot: &AnalysisSnapshot,
    index: &BindingIndex,
    file: &Path,
    offset: usize,
) -> Option<String> {
    let symbol = index.definition(file, offset)?;
    if symbol.kind != SymbolKind::ModuleConst || symbol.file != file {
        return None;
    }

    let parsed_file = snapshot.files.iter().find(|f| f.path == symbol.file)?;
    let parsed_const = parsed_const_at(&parsed_file.parsed.file, symbol.span)?;
    if !offset_is_on_declaration_name(&parsed_file.source, symbol.span, &parsed_const.name, offset)
    {
        return None;
    }
    let checked_const = checked_const_at(snapshot, &symbol)?;

    let mut value = marrow_code_block(&module_const_signature(checked_const));
    append_docs(&mut value, join_docs(&parsed_const.docs));
    Some(value)
}

fn next_significant_kind(tokens: &[marrow_syntax::Token], index: usize) -> Option<TokenKind> {
    tokens
        .get(index + 1..)?
        .iter()
        .find(|token| !is_trivia(token.kind))
        .map(|token| token.kind)
}

fn default_library_call_hover(
    snapshot: &AnalysisSnapshot,
    file: &Path,
    offset: usize,
) -> Option<String> {
    let analyzed = snapshot.files.iter().find(|f| f.path == file)?;
    let tokens = lex_source(&analyzed.source).tokens;
    let index = tokens.iter().position(|token| {
        is_path_segment_token(token.kind)
            && token.span.start_byte <= offset
            && offset <= token.span.end_byte
    })?;
    if let Some(value) = std_library_path_hover(&tokens, &analyzed.source, index) {
        return Some(value);
    }
    if is_bare_call_leaf(&tokens, index) {
        let name = tokens[index].text(&analyzed.source);
        if let Some(value) = language_facts::bare_builtin_hover(name) {
            return Some(value);
        }
        if let Some(value) = language_facts::scalar_conversion_hover(name) {
            return Some(value);
        }
    }
    None
}

fn std_library_path_hover(
    tokens: &[marrow_syntax::Token],
    source: &str,
    index: usize,
) -> Option<String> {
    let (module_index, op_index) = std_operation_call_for_segment(tokens, source, index)?;
    let module = tokens[module_index].text(source);
    let op = tokens[op_index].text(source);
    if index == op_index {
        return language_facts::std_operation_hover(module, op);
    }
    language_facts::std_operation_hover(module, op)?;
    if index + 2 == module_index {
        return Some(language_facts::std_namespace_hover());
    }
    if index == module_index {
        return language_facts::std_module_hover(module);
    }
    None
}

fn operator_hover(snapshot: &AnalysisSnapshot, file: &Path, offset: usize) -> Option<String> {
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
    ) && (previous_significant_kind(tokens, index)
        .is_some_and(|kind| kind == TokenKind::DoubleColon)
        || next_significant_kind(tokens, index).is_some_and(|kind| kind == TokenKind::DoubleColon)
        || previous_significant_kind(tokens, index)
            .is_some_and(|kind| matches!(kind, TokenKind::Keyword(Keyword::Module | Keyword::Use))))
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

fn default_library_hover_markdown(fact: &str) -> String {
    let Some((signature, context)) = fact.split_once("\n\n") else {
        return marrow_code_block(fact);
    };
    let mut value = marrow_code_block(signature);
    value.push_str("\n\n");
    value.push_str(context);
    value
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
            is_named_symbol_reference(snapshot, index, file, offset, symbol)
                || is_resource_declaration_name(snapshot, file, offset, symbol)
        }
        SymbolKind::Enum => {
            !blocked_enum_type_symbol_hover(snapshot, file, offset, symbol)
                && (is_named_symbol_reference(snapshot, index, file, offset, symbol)
                    || is_enum_declaration_name(snapshot, file, offset, symbol))
        }
        SymbolKind::EnumMember => {
            is_named_symbol_reference(snapshot, index, file, offset, symbol)
                || is_enum_member_declaration_name(snapshot, file, offset, symbol)
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
    let Some((segments, _)) = qualified_name_at_with_position(&analyzed.source, offset) else {
        return false;
    };
    segments.len() > 1 || symbol.file != file
}

fn is_named_symbol_reference(
    snapshot: &AnalysisSnapshot,
    index: &BindingIndex,
    file: &Path,
    offset: usize,
    symbol: &SymbolRef,
) -> bool {
    index.references(symbol).iter().any(|reference| {
        reference.kind == symbol.kind
            && reference.file == file
            && reference.span != symbol.span
            && span_covers(reference.span, offset)
            && offset_is_on_qualified_name(snapshot, file, reference.span, offset)
    })
}

fn is_resource_declaration_name(
    snapshot: &AnalysisSnapshot,
    file: &Path,
    offset: usize,
    symbol: &SymbolRef,
) -> bool {
    if symbol.file != file {
        return false;
    }
    let Some(analyzed) = snapshot.files.iter().find(|f| f.path == symbol.file) else {
        return false;
    };
    let Some(resource) = resource_at(&analyzed.parsed.file, symbol.span) else {
        return false;
    };
    offset_is_on_declaration_name(&analyzed.source, symbol.span, &resource.name, offset)
}

fn is_enum_declaration_name(
    snapshot: &AnalysisSnapshot,
    file: &Path,
    offset: usize,
    symbol: &SymbolRef,
) -> bool {
    if symbol.file != file {
        return false;
    }
    let Some(analyzed) = snapshot.files.iter().find(|f| f.path == symbol.file) else {
        return false;
    };
    let Some(enum_decl) = enum_at(&analyzed.parsed.file, symbol.span) else {
        return false;
    };
    offset_is_on_declaration_name(&analyzed.source, symbol.span, &enum_decl.name, offset)
}

fn is_enum_member_declaration_name(
    snapshot: &AnalysisSnapshot,
    file: &Path,
    offset: usize,
    symbol: &SymbolRef,
) -> bool {
    if symbol.file != file {
        return false;
    }
    let Some(analyzed) = snapshot.files.iter().find(|f| f.path == symbol.file) else {
        return false;
    };
    let Some(member) = enum_member_at(&analyzed.parsed.file, symbol.span) else {
        return false;
    };
    offset_is_on_declaration_name(&analyzed.source, symbol.span, &member.name, offset)
}

fn is_resource_member_hover_target(
    snapshot: &AnalysisSnapshot,
    index: &BindingIndex,
    file: &Path,
    offset: usize,
    symbol: &SymbolRef,
) -> bool {
    is_saved_member_declaration_name(snapshot, file, offset, symbol)
        || index.references(symbol).iter().any(|reference| {
            reference.file == file
                && reference.span != symbol.span
                && span_covers(reference.span, offset)
                && offset_is_on_last_identifier(snapshot, file, reference.span, offset)
        })
}

fn is_saved_member_declaration_name(
    snapshot: &AnalysisSnapshot,
    file: &Path,
    offset: usize,
    symbol: &SymbolRef,
) -> bool {
    match symbol.kind {
        SymbolKind::Field | SymbolKind::Layer => {
            is_resource_member_declaration_name(snapshot, file, offset, symbol)
        }
        SymbolKind::Index => is_index_declaration_name(snapshot, file, offset, symbol),
        _ => false,
    }
}

fn is_resource_member_declaration_name(
    snapshot: &AnalysisSnapshot,
    file: &Path,
    offset: usize,
    symbol: &SymbolRef,
) -> bool {
    if symbol.file != file {
        return false;
    }
    let Some(analyzed) = snapshot.files.iter().find(|f| f.path == symbol.file) else {
        return false;
    };
    let Some(member) = resource_member_at(&analyzed.parsed.file, symbol.span) else {
        return false;
    };
    offset_is_on_declaration_name(
        &analyzed.source,
        symbol.span,
        resource_member_name(member),
        offset,
    )
}

fn is_index_declaration_name(
    snapshot: &AnalysisSnapshot,
    file: &Path,
    offset: usize,
    symbol: &SymbolRef,
) -> bool {
    if symbol.file != file {
        return false;
    }
    let Some(analyzed) = snapshot.files.iter().find(|f| f.path == symbol.file) else {
        return false;
    };
    let Some(index) = store_index_at(&analyzed.parsed.file, symbol.span) else {
        return false;
    };
    offset_is_on_declaration_name(&analyzed.source, symbol.span, &index.name, offset)
}

fn offset_is_on_declaration_name(
    source: &str,
    span: SourceSpan,
    name: &str,
    offset: usize,
) -> bool {
    let Some((start, end)) = declaration_identifier_span(source, span, name) else {
        return false;
    };
    start <= offset && offset <= end
}

fn declaration_identifier_span(
    source: &str,
    span: SourceSpan,
    name: &str,
) -> Option<(usize, usize)> {
    lex_source(source)
        .tokens
        .into_iter()
        .find(|token| {
            token.kind == TokenKind::Identifier
                && span_covers(span, token.span.start_byte)
                && span_covers(span, token.span.end_byte)
                && token.text(source) == name
        })
        .map(|token| (token.span.start_byte, token.span.end_byte))
}

fn saved_root_span(source: &str, span: SourceSpan, root: &str) -> Option<(usize, usize)> {
    let tokens = lex_source(source).tokens;
    tokens.windows(2).find_map(|pair| {
        let [previous, token] = pair else {
            return None;
        };
        if previous.kind == TokenKind::Caret
            && token.kind == TokenKind::Identifier
            && span_covers(span, previous.span.start_byte)
            && span_covers(span, token.span.end_byte)
            && token.text(source) == root
        {
            Some((previous.span.start_byte, token.span.end_byte))
        } else {
            None
        }
    })
}

fn is_function_hover_target(
    snapshot: &AnalysisSnapshot,
    index: &BindingIndex,
    file: &Path,
    offset: usize,
    symbol: &SymbolRef,
) -> bool {
    index.references(symbol).iter().any(|reference| {
        reference.kind == SymbolKind::Function
            && reference.file == file
            && reference.span != symbol.span
            && span_covers(reference.span, offset)
            && offset_is_on_last_identifier(snapshot, file, reference.span, offset)
    }) || is_function_declaration_name(snapshot, file, offset, symbol)
}

fn offset_is_on_last_identifier(
    snapshot: &AnalysisSnapshot,
    file: &Path,
    span: SourceSpan,
    offset: usize,
) -> bool {
    let Some(analyzed) = snapshot.files.iter().find(|f| f.path == file) else {
        return false;
    };
    let Some((start, end)) = last_identifier_span(span, &analyzed.source) else {
        return false;
    };
    start <= offset && offset <= end
}

fn offset_is_on_qualified_name(
    snapshot: &AnalysisSnapshot,
    file: &Path,
    span: SourceSpan,
    offset: usize,
) -> bool {
    let Some(analyzed) = snapshot.files.iter().find(|f| f.path == file) else {
        return false;
    };
    let Some((start, end)) = qualified_identifier_span(span, &analyzed.source) else {
        return false;
    };
    start <= offset && offset <= end
}

fn is_function_declaration_name(
    snapshot: &AnalysisSnapshot,
    file: &Path,
    offset: usize,
    symbol: &SymbolRef,
) -> bool {
    if symbol.file != file {
        return false;
    }
    let Some(analyzed) = snapshot.files.iter().find(|f| f.path == symbol.file) else {
        return false;
    };
    let Some(function) = parsed_function_at(&analyzed.parsed.file, symbol.span) else {
        return false;
    };
    let Some((start, end)) = name_span(&function.name, function.span, &analyzed.source) else {
        return false;
    };
    start <= offset && offset <= end
}

fn parsed_function_at(
    source: &marrow_syntax::SourceFile,
    span: SourceSpan,
) -> Option<&FunctionDecl> {
    source
        .declarations
        .iter()
        .find_map(|declaration| match declaration {
            Declaration::Function(function) if span_contains_span(function.span, span) => {
                Some(function)
            }
            _ => None,
        })
}

fn checked_function_for_parsed<'a>(
    snapshot: &'a AnalysisSnapshot,
    symbol: &SymbolRef,
    source: &marrow_syntax::SourceFile,
    parsed_function: &FunctionDecl,
) -> Option<&'a CheckedFunction> {
    let function_index = source
        .declarations
        .iter()
        .filter_map(|declaration| match declaration {
            Declaration::Function(function) => Some(function),
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

fn parsed_const_at(
    source: &marrow_syntax::SourceFile,
    span: SourceSpan,
) -> Option<&marrow_syntax::ConstDecl> {
    source
        .declarations
        .iter()
        .find_map(|declaration| match declaration {
            Declaration::Const(constant) if span_contains_span(constant.span, span) => {
                Some(constant)
            }
            _ => None,
        })
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
        .find(|constant| span_contains_span(constant.span, symbol.span))
}

fn name_span(name: &str, span: SourceSpan, source: &str) -> Option<(usize, usize)> {
    let lexed = lex_source(source);
    let mut after_fn = false;
    for token in lexed.tokens {
        if !span_covers(span, token.span.start_byte) || !span_covers(span, token.span.end_byte) {
            continue;
        }
        match token.kind {
            TokenKind::Keyword(Keyword::Fn) => after_fn = true,
            TokenKind::Identifier if after_fn && token.text(source) == name => {
                return Some((token.span.start_byte, token.span.end_byte));
            }
            TokenKind::Newline if after_fn => return None,
            _ => {}
        }
    }
    None
}

fn qualified_identifier_span(span: SourceSpan, source: &str) -> Option<(usize, usize)> {
    let lexed = lex_source(source);
    let mut start = None;
    let mut end = None;
    for token in lexed.tokens {
        if token.kind == TokenKind::Identifier
            && span_covers(span, token.span.start_byte)
            && span_covers(span, token.span.end_byte)
        {
            start.get_or_insert(token.span.start_byte);
            end = Some(token.span.end_byte);
        }
    }
    Some((start?, end?))
}

fn last_identifier_span(span: SourceSpan, source: &str) -> Option<(usize, usize)> {
    let lexed = lex_source(source);
    let mut found = None;
    for token in lexed.tokens {
        if token.kind == TokenKind::Identifier
            && span_covers(span, token.span.start_byte)
            && span_covers(span, token.span.end_byte)
        {
            found = Some((token.span.start_byte, token.span.end_byte));
        }
    }
    found
}

fn span_covers(span: SourceSpan, offset: usize) -> bool {
    span.start_byte <= offset && offset <= span.end_byte
}

fn span_contains_span(outer: SourceSpan, inner: SourceSpan) -> bool {
    span_covers(outer, inner.start_byte) && span_covers(outer, inner.end_byte)
}

fn function_signature(function: &CheckedFunction) -> String {
    let params = function
        .params
        .iter()
        .map(|param| format!("{}: {}", param.name, render_type(&param.ty)))
        .collect::<Vec<_>>()
        .join(", ");
    match &function.return_type {
        Some(ty) => format!("fn {}({params}): {}", function.name, render_type(ty)),
        None => format!("fn {}({params})", function.name),
    }
}

fn parameter_signature(param: &CheckedParam) -> String {
    format!("{}: {}", param.name, render_type(&param.ty))
}

fn module_const_signature(constant: &CheckedConst) -> String {
    match &constant.ty {
        Some(ty) => format!("const {}: {}", constant.name, render_type(ty)),
        None => format!("const {}", constant.name),
    }
}

fn parameter_docs(function: &FunctionDecl) -> Option<String> {
    let docs = function
        .params
        .iter()
        .filter_map(|param| {
            let docs = join_docs(&param.docs)?;
            Some(format!("- `{}`: {docs}", param.name))
        })
        .collect::<Vec<_>>();
    if docs.is_empty() {
        None
    } else {
        Some(format!("**Parameters**\n{}", docs.join("\n")))
    }
}

fn resource_schema_for_symbol<'a>(
    snapshot: &'a AnalysisSnapshot,
    symbol: &SymbolRef,
) -> Option<&'a ResourceSchema> {
    let analyzed = snapshot.files.iter().find(|f| f.path == symbol.file)?;
    let resource = resource_at(&analyzed.parsed.file, symbol.span)?;
    snapshot
        .program
        .modules
        .iter()
        .find(|module| module.source_file == symbol.file)?
        .resources
        .iter()
        .find(|schema| schema.name == resource.name)
}

fn resource_schema_for_type_name<'a>(
    snapshot: &'a AnalysisSnapshot,
    file: &Path,
    segments: &[String],
) -> Option<&'a ResourceSchema> {
    let from_module = module_name_for_file(snapshot, file)?;
    match resolve(
        &snapshot.program,
        from_module,
        segments,
        ResolvableKind::Resource,
    ) {
        Resolution::Found(def) => match def.item {
            DefItem::Resource(schema) => Some(schema),
            _ => None,
        },
        _ => None,
    }
}

fn enum_schema_for_symbol<'a>(
    snapshot: &'a AnalysisSnapshot,
    symbol: &SymbolRef,
) -> Option<&'a EnumSchema> {
    let analyzed = snapshot.files.iter().find(|f| f.path == symbol.file)?;
    let enum_decl = enum_at(&analyzed.parsed.file, symbol.span)?;
    enum_schema_in_file(snapshot, &symbol.file, &enum_decl.name)
}

fn enum_member_schema_for_symbol<'a>(
    snapshot: &'a AnalysisSnapshot,
    symbol: &SymbolRef,
) -> Option<(&'a EnumSchema, usize)> {
    let analyzed = snapshot.files.iter().find(|f| f.path == symbol.file)?;
    let (enum_name, path) = enum_member_path_at(&analyzed.parsed.file, symbol.span)?;
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

fn enum_schema_for_type_name<'a>(
    snapshot: &'a AnalysisSnapshot,
    file: &Path,
    segments: &[String],
) -> Option<&'a EnumSchema> {
    match segments {
        [name] => enum_schema_in_file(snapshot, file, name),
        [] => None,
        [_, ..] => None,
    }
}

fn module_name_for_file<'a>(snapshot: &'a AnalysisSnapshot, file: &Path) -> Option<&'a str> {
    snapshot
        .program
        .modules
        .iter()
        .find(|module| module.source_file == file)
        .map(|module| module.name.as_str())
}

fn enum_member_path_at(
    source: &marrow_syntax::SourceFile,
    span: SourceSpan,
) -> Option<(String, Vec<String>)> {
    for declaration in &source.declarations {
        let Declaration::Enum(enum_decl) = declaration else {
            continue;
        };
        let mut path = Vec::new();
        if enum_member_path_in(&enum_decl.members, span, &mut path) {
            return Some((enum_decl.name.clone(), path));
        }
    }
    None
}

fn enum_member_path_in(members: &[EnumMember], span: SourceSpan, path: &mut Vec<String>) -> bool {
    for member in members {
        path.push(member.name.clone());
        if span_contains_span(member.span, span) || enum_member_path_in(&member.members, span, path)
        {
            return true;
        }
        path.pop();
    }
    false
}

fn is_resource_constructor_leaf(source: &str, offset: usize) -> bool {
    let tokens = lex_source(source).tokens;
    let Some((_start, end, index)) = qualified_name_token_bounds(&tokens, offset) else {
        return false;
    };
    index == end
        && tokens
            .get(end + 1)
            .is_some_and(|token| token.kind == TokenKind::LeftParen)
}

fn qualified_name_at(source: &str, offset: usize) -> Option<Vec<String>> {
    qualified_name_at_with_position(source, offset).map(|(segments, _)| segments)
}

fn qualified_name_at_with_position(source: &str, offset: usize) -> Option<(Vec<String>, usize)> {
    let tokens = lex_source(source).tokens;
    let (start, end, index) = qualified_name_token_bounds(&tokens, offset)?;
    let mut segments = Vec::new();
    let mut current_segment = None;
    for (token_index, token) in tokens[start..=end].iter().enumerate() {
        if token.kind == TokenKind::Identifier {
            if start + token_index == index {
                current_segment = Some(segments.len());
            }
            segments.push(token.text(source).to_string());
        }
    }
    (!segments.is_empty()).then_some((segments, current_segment?))
}

fn module_path_at_with_position(source: &str, offset: usize) -> Option<(Vec<String>, usize, bool)> {
    let tokens = lex_source(source).tokens;
    let index = tokens.iter().position(|token| {
        is_path_segment_token(token.kind)
            && token.span.start_byte <= offset
            && offset <= token.span.end_byte
    })?;
    let mut start = index;
    while start >= 2
        && tokens[start - 1].kind == TokenKind::DoubleColon
        && is_path_segment_token(tokens[start - 2].kind)
    {
        start -= 2;
    }
    let mut end = index;
    while end + 2 < tokens.len()
        && tokens[end + 1].kind == TokenKind::DoubleColon
        && is_path_segment_token(tokens[end + 2].kind)
    {
        end += 2;
    }

    let mut segments = Vec::new();
    let mut cursor_segment = None;
    let mut token_index = start;
    loop {
        if !is_path_segment_token(tokens[token_index].kind) {
            return None;
        }
        if token_index == index {
            cursor_segment = Some(segments.len());
        }
        segments.push(tokens[token_index].text(source).to_string());
        if token_index == end {
            break;
        }
        if token_index + 2 > end || tokens[token_index + 1].kind != TokenKind::DoubleColon {
            return None;
        }
        token_index += 2;
    }

    let declaration = previous_significant_kind(&tokens, start)
        .is_some_and(|kind| matches!(kind, TokenKind::Keyword(Keyword::Module | Keyword::Use)));
    Some((segments, cursor_segment?, declaration))
}

fn qualified_name_token_bounds(
    tokens: &[marrow_syntax::Token],
    offset: usize,
) -> Option<(usize, usize, usize)> {
    let index = tokens.iter().position(|token| {
        token.kind == TokenKind::Identifier
            && token.span.start_byte <= offset
            && offset <= token.span.end_byte
    })?;
    let mut start = index;
    while start >= 2
        && tokens[start - 1].kind == TokenKind::DoubleColon
        && tokens[start - 2].kind == TokenKind::Identifier
    {
        start -= 2;
    }
    let mut end = index;
    while end + 2 < tokens.len()
        && tokens[end + 1].kind == TokenKind::DoubleColon
        && tokens[end + 2].kind == TokenKind::Identifier
    {
        end += 2;
    }
    Some((start, end, index))
}

fn std_operation_call_for_segment(
    tokens: &[marrow_syntax::Token],
    source: &str,
    index: usize,
) -> Option<(usize, usize)> {
    let candidates = [
        index
            .checked_add(2)
            .zip(index.checked_add(4))
            .map(|(module_index, op_index)| (index, module_index, op_index)),
        index
            .checked_sub(2)
            .zip(index.checked_add(2))
            .map(|(std_index, op_index)| (std_index, index, op_index)),
        index
            .checked_sub(4)
            .zip(index.checked_sub(2))
            .map(|(std_index, module_index)| (std_index, module_index, index)),
    ];
    candidates
        .into_iter()
        .flatten()
        .find(|(std_index, module_index, op_index)| {
            is_std_operation_call(tokens, source, *std_index, *module_index, *op_index)
        })
        .map(|(_, module_index, op_index)| (module_index, op_index))
}

fn is_std_operation_call(
    tokens: &[marrow_syntax::Token],
    source: &str,
    std_index: usize,
    module_index: usize,
    op_index: usize,
) -> bool {
    tokens
        .get(std_index)
        .is_some_and(|token| is_path_segment_token(token.kind) && token.text(source) == "std")
        && tokens
            .get(std_index + 1)
            .is_some_and(|token| token.kind == TokenKind::DoubleColon)
        && tokens
            .get(module_index)
            .is_some_and(|token| is_path_segment_token(token.kind))
        && tokens
            .get(module_index + 1)
            .is_some_and(|token| token.kind == TokenKind::DoubleColon)
        && tokens
            .get(op_index)
            .is_some_and(|token| is_path_segment_token(token.kind))
        && starts_at_callee_root(tokens, std_index)
        && is_call_leaf(tokens, op_index)
}

fn is_bare_call_leaf(tokens: &[marrow_syntax::Token], index: usize) -> bool {
    is_call_leaf(tokens, index) && starts_at_callee_root(tokens, index)
}

fn is_call_leaf(tokens: &[marrow_syntax::Token], index: usize) -> bool {
    is_path_segment_token(tokens[index].kind)
        && tokens
            .get(index + 1)
            .is_some_and(|token| token.kind == TokenKind::LeftParen)
}

fn starts_at_callee_root(tokens: &[marrow_syntax::Token], index: usize) -> bool {
    !previous_significant_kind(tokens, index).is_some_and(|kind| {
        matches!(
            kind,
            TokenKind::DoubleColon
                | TokenKind::Dot
                | TokenKind::QuestionDot
                | TokenKind::Keyword(Keyword::Fn)
        )
    })
}

fn previous_significant_kind(tokens: &[marrow_syntax::Token], index: usize) -> Option<TokenKind> {
    tokens[..index]
        .iter()
        .rev()
        .find(|token| !is_trivia(token.kind))
        .map(|token| token.kind)
}

fn is_path_segment_token(kind: TokenKind) -> bool {
    matches!(kind, TokenKind::Identifier | TokenKind::Keyword(_))
}

fn is_trivia(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Indent
            | TokenKind::Dedent
            | TokenKind::Newline
            | TokenKind::Eof
            | TokenKind::Comment
            | TokenKind::DocComment
    )
}

/// The `;;` description of the symbol at byte `offset` in `file`, as a Markdown
/// paragraph, or `None` when none applies.
///
/// The symbol is resolved through the cached [`BindingIndex`] (the same one
/// go-to-definition and references read), so this never rebuilds resolution per
/// request. Only documentable declarations — functions, resources, module
/// constants, enums, enum members, resource fields and layers, and store
/// indexes — carry docs; a local or parameter has none and yields `None`. The
/// declaration is located in
/// the snapshot's parsed file by matching the definition span, and its `docs`
/// lines (each a `;;` line) are joined into one paragraph. An undocumented
/// declaration yields `None`.
pub fn symbol_docs(
    snapshot: &AnalysisSnapshot,
    index: &BindingIndex,
    file: &Path,
    offset: usize,
) -> Option<String> {
    let symbol = index.definition(file, offset)?;
    let analyzed = snapshot.files.iter().find(|f| f.path == symbol.file)?;
    let docs = declaration_docs(&analyzed.parsed.file, &symbol)?;
    join_docs(docs)
}

/// The `docs` lines of the declaration the definition `symbol` names, found in
/// `source` by matching the definition span, or `None` when the symbol is not a
/// documentable declaration. Locals and parameters are not declarations here and
/// return `None`.
fn declaration_docs<'a>(
    source: &'a marrow_syntax::SourceFile,
    symbol: &SymbolRef,
) -> Option<&'a [String]> {
    match symbol.kind {
        SymbolKind::Function => {
            source
                .declarations
                .iter()
                .find_map(|declaration| match declaration {
                    Declaration::Function(function)
                        if span_contains_span(function.span, symbol.span) =>
                    {
                        Some(function.docs.as_slice())
                    }
                    _ => None,
                })
        }
        SymbolKind::ModuleConst => {
            source
                .declarations
                .iter()
                .find_map(|declaration| match declaration {
                    Declaration::Const(constant)
                        if span_contains_span(constant.span, symbol.span) =>
                    {
                        Some(constant.docs.as_slice())
                    }
                    _ => None,
                })
        }
        SymbolKind::Resource => {
            resource_at(source, symbol.span).map(|resource| resource.docs.as_slice())
        }
        SymbolKind::Enum => enum_at(source, symbol.span).map(|enum_decl| enum_decl.docs.as_slice()),
        SymbolKind::EnumMember => {
            source
                .declarations
                .iter()
                .find_map(|declaration| match declaration {
                    Declaration::Enum(enum_decl) => {
                        enum_member_docs(&enum_decl.members, symbol.span)
                    }
                    _ => None,
                })
        }
        SymbolKind::Field | SymbolKind::Layer => {
            source
                .declarations
                .iter()
                .find_map(|declaration| match declaration {
                    Declaration::Resource(resource) => member_docs(&resource.members, symbol.span),
                    _ => None,
                })
        }
        SymbolKind::Index => source
            .declarations
            .iter()
            .find_map(|declaration| match declaration {
                Declaration::Store(store) => store
                    .indexes
                    .iter()
                    .find(|index| span_contains_span(index.span, symbol.span))
                    .map(|index| index.docs.as_slice()),
                _ => None,
            }),
        // Locals, parameters, and module aliases carry no `;;` documentation.
        SymbolKind::Local | SymbolKind::Param | SymbolKind::ModuleRef => None,
    }
}

/// The resource declaration whose span is `span`, or `None`.
fn resource_at(source: &marrow_syntax::SourceFile, span: SourceSpan) -> Option<&ResourceDecl> {
    source
        .declarations
        .iter()
        .find_map(|declaration| match declaration {
            Declaration::Resource(resource) if span_contains_span(resource.span, span) => {
                Some(resource)
            }
            _ => None,
        })
}

/// The enum declaration whose span is `span`, or `None`.
fn enum_at(source: &marrow_syntax::SourceFile, span: SourceSpan) -> Option<&EnumDecl> {
    source
        .declarations
        .iter()
        .find_map(|declaration| match declaration {
            Declaration::Enum(enum_decl) if span_contains_span(enum_decl.span, span) => {
                Some(enum_decl)
            }
            _ => None,
        })
}

/// The `docs` of the enum member whose span is `span`, searching nested members,
/// or `None`.
fn enum_member_docs(members: &[EnumMember], span: SourceSpan) -> Option<&[String]> {
    for member in members {
        if span_contains_span(member.span, span) {
            return Some(&member.docs);
        }
        if let Some(docs) = enum_member_docs(&member.members, span) {
            return Some(docs);
        }
    }
    None
}

/// The enum member whose span is `span`, searching nested members, or `None`.
fn enum_member_at(source: &marrow_syntax::SourceFile, span: SourceSpan) -> Option<&EnumMember> {
    for declaration in &source.declarations {
        let Declaration::Enum(enum_decl) = declaration else {
            continue;
        };
        if let Some(member) = enum_member_in(&enum_decl.members, span) {
            return Some(member);
        }
    }
    None
}

fn enum_member_in(members: &[EnumMember], span: SourceSpan) -> Option<&EnumMember> {
    for member in members {
        if span_contains_span(member.span, span) {
            return Some(member);
        }
        if let Some(member) = enum_member_in(&member.members, span) {
            return Some(member);
        }
    }
    None
}

fn resource_member_at(
    source: &marrow_syntax::SourceFile,
    span: SourceSpan,
) -> Option<&ResourceMember> {
    for declaration in &source.declarations {
        let Declaration::Resource(resource) = declaration else {
            continue;
        };
        if let Some(member) = resource_member_in(&resource.members, span) {
            return Some(member);
        }
    }
    None
}

fn resource_member_in(members: &[ResourceMember], span: SourceSpan) -> Option<&ResourceMember> {
    for member in members {
        match member {
            ResourceMember::Field(field) if span_contains_span(field.span, span) => {
                return Some(member);
            }
            ResourceMember::Group(group) if span_contains_span(group.span, span) => {
                return Some(member);
            }
            ResourceMember::Group(group) => {
                if let Some(member) = resource_member_in(&group.members, span) {
                    return Some(member);
                }
            }
            _ => {}
        }
    }
    None
}

fn store_root_at<'a>(
    source: &'a marrow_syntax::SourceFile,
    text: &str,
    offset: usize,
) -> Option<&'a StoreDecl> {
    for declaration in &source.declarations {
        let Declaration::Store(store) = declaration else {
            continue;
        };
        let Some((start, end)) = saved_root_span(text, store.span, &store.root.root) else {
            continue;
        };
        if start <= offset && offset <= end {
            return Some(store);
        }
    }
    None
}

fn store_index_at(source: &marrow_syntax::SourceFile, span: SourceSpan) -> Option<&IndexDecl> {
    source
        .declarations
        .iter()
        .find_map(|declaration| match declaration {
            Declaration::Store(store) => store
                .indexes
                .iter()
                .find(|index| span_contains_span(index.span, span)),
            _ => None,
        })
}

fn resource_member_name(member: &ResourceMember) -> &str {
    match member {
        ResourceMember::Field(field) => &field.name,
        ResourceMember::Group(group) => &group.name,
    }
}

/// The `docs` of the resource member (field, group/layer, or index) whose span is
/// `span`, searching nested groups, or `None`.
fn member_docs(members: &[ResourceMember], span: SourceSpan) -> Option<&[String]> {
    for member in members {
        match member {
            ResourceMember::Field(field) if span_contains_span(field.span, span) => {
                return Some(&field.docs);
            }
            ResourceMember::Group(group) if span_contains_span(group.span, span) => {
                return Some(&group.docs);
            }
            // A nested group can hold the named member; descend before moving on.
            ResourceMember::Group(group) => {
                if let Some(docs) = member_docs(&group.members, span) {
                    return Some(docs);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests;
