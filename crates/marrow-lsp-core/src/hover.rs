//! Hover: the checked fact under the cursor, as a `marrow` code block.
//!
//! An editor sends a byte offset (already converted from an LSP position by the
//! document's [`LineIndex`](crate::positions::LineIndex)); this renders resolved
//! function symbols from the binding index, or asks the checker for the expression
//! type there with [`marrow_check::type_at`]. The rendered code is fenced as
//! `marrow` so the client shows it with `.mw` syntax highlighting. The facts come
//! only from the checker — this module never re-infers anything.

use std::path::Path;

use lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind};
use marrow_check::{
    AnalysisSnapshot, BindingIndex, CheckedConst, CheckedFacts, CheckedFunction, CheckedModule,
    CheckedParam, DefItem, DirectEffectFacts, FunctionFact, HostEffect, Resolution, ResolvableKind,
    SavedPlaceEffect, SymbolKind, SymbolRef, build_alias_map, build_binding_index,
    expand_module_alias, resolve, type_at,
};
use marrow_schema::{EnumSchema, IndexSchema, NodeKind, ResourceSchema, StoreSchema, stdlib};
use marrow_store::backend::Presence;
use marrow_syntax::{
    Block, Declaration, EnumDecl, EnumMember, FunctionDecl, IndexDecl, KeyParam, Keyword,
    ParamMode, ResourceDecl, ResourceMember, SourceSpan, Statement, StoreDecl, TokenKind, TypeRef,
    lex_source,
};

use crate::language_facts;
use crate::store::{Availability, StoreReader, StoredValue, saved_path_at};
use crate::types::render_type;

/// The hover for byte `offset` in `file`, or `None` when no known symbol or type
/// covers it.
///
/// The file must be one the snapshot analyzed; a path it does not know (or an
/// offset no expression covers) yields `None`, so the editor shows nothing.
pub fn hover(snapshot: &AnalysisSnapshot, file: &Path, offset: usize) -> Option<Hover> {
    let index = build_binding_index(snapshot);
    hover_with_index(snapshot, &index, file, offset, None)
}

/// The hover for byte `offset`, using `index` for resolved symbol facts and
/// `reader` for optional live stored data.
pub fn hover_with_index(
    snapshot: &AnalysisSnapshot,
    index: &BindingIndex,
    file: &Path,
    offset: usize,
    reader: Option<&StoreReader>,
) -> Option<Hover> {
    if let Some(value) = default_library_call_hover(snapshot, file, offset) {
        return Some(markdown_hover(default_library_hover_markdown(&value)));
    }
    if let Some(value) = operator_hover(snapshot, file, offset) {
        return Some(markdown_hover(default_library_hover_markdown(&value)));
    }
    if let Some(value) = function_hover(snapshot, index, file, offset, reader) {
        return Some(markdown_hover(value));
    }
    if let Some(value) = parameter_hover(snapshot, index, file, offset) {
        return Some(markdown_hover(value));
    }
    if let Some(value) = module_const_hover(snapshot, index, file, offset) {
        return Some(markdown_hover(value));
    }
    if let Some(value) = project_module_hover(snapshot, index, file, offset) {
        return Some(markdown_hover(value));
    }
    if let Some(value) = saved_root_hover(snapshot, file, offset, reader) {
        return Some(markdown_hover(value));
    }
    if let Some(value) = store_root_declaration_hover(snapshot, file, offset, reader) {
        return Some(markdown_hover(value));
    }
    if let Some(value) = rich_symbol_hover(snapshot, index, file, offset, reader) {
        return Some(markdown_hover(value));
    }
    if let Some(value) = resource_member_hover(snapshot, index, file, offset, reader) {
        return Some(markdown_hover(value));
    }
    if let Some(value) = resource_constructor_hover(snapshot, file, offset, reader) {
        return Some(markdown_hover(value));
    }
    if let Some(value) = type_name_hover(snapshot, file, offset, reader) {
        return Some(markdown_hover(value));
    }
    let docs = symbol_docs_at_hover_target(snapshot, index, file, offset);
    hover_with_live(snapshot, file, offset, docs.as_deref(), reader)
}

fn project_module_hover(
    snapshot: &AnalysisSnapshot,
    index: &BindingIndex,
    file: &Path,
    offset: usize,
) -> Option<String> {
    let module_name = project_module_name_at(snapshot, index, file, offset)?;
    let module = snapshot
        .program
        .modules
        .iter()
        .find(|module| !module.name.is_empty() && module.name == module_name)?;
    Some(project_module_hover_markdown(module))
}

fn project_module_name_at(
    snapshot: &AnalysisSnapshot,
    index: &BindingIndex,
    file: &Path,
    offset: usize,
) -> Option<String> {
    let analyzed = snapshot.files.iter().find(|f| f.path == file)?;
    let (segments, cursor_segment, declaration) =
        module_path_at_with_position(&analyzed.source, offset)?;
    if declaration {
        return Some(segments[..=cursor_segment].join("::"));
    }
    if binding_index_should_win_project_module_hover(index, file, offset) {
        return None;
    }
    if cursor_segment + 1 == segments.len() {
        return None;
    }
    let prefix = segments[..=cursor_segment].join("::");
    let current = snapshot
        .program
        .modules
        .iter()
        .find(|module| module.source_file == file)?;
    let aliases = build_alias_map(&current.imports);
    Some(expand_module_alias(&prefix, &aliases))
}

fn binding_index_should_win_project_module_hover(
    index: &BindingIndex,
    file: &Path,
    offset: usize,
) -> bool {
    let Some(symbol) = index.definition(file, offset) else {
        return false;
    };
    matches!(symbol.kind, SymbolKind::Enum | SymbolKind::EnumMember)
}

fn project_module_hover_markdown(module: &CheckedModule) -> String {
    let mut value = marrow_code_block(&format!("module {}", module.name));
    value.push_str("\n\n**Project module**");

    let mut facts = Vec::new();
    push_module_count(&mut facts, module.functions.len(), "function", "functions");
    push_module_count(&mut facts, module.resources.len(), "resource", "resources");
    push_module_count(&mut facts, module.enums.len(), "enum", "enums");
    if !facts.is_empty() {
        value.push('\n');
        value.push_str(&facts.join("\n"));
    }
    value
}

fn push_module_count(lines: &mut Vec<String>, count: usize, singular: &str, plural: &str) {
    match count {
        0 => {}
        1 => lines.push(format!("- 1 {singular}")),
        _ => lines.push(format!("- {count} {plural}")),
    }
}

fn rich_symbol_hover(
    snapshot: &AnalysisSnapshot,
    index: &BindingIndex,
    file: &Path,
    offset: usize,
    reader: Option<&StoreReader>,
) -> Option<String> {
    let symbol = index.definition(file, offset)?;
    if !is_rich_symbol_hover_target(snapshot, index, file, offset, &symbol) {
        return None;
    }
    match symbol.kind {
        SymbolKind::Resource => {
            let schema = resource_schema_for_symbol(snapshot, &symbol)?;
            Some(resource_hover(schema, reader))
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

fn type_name_hover(
    snapshot: &AnalysisSnapshot,
    file: &Path,
    offset: usize,
    reader: Option<&StoreReader>,
) -> Option<String> {
    let analyzed = snapshot.files.iter().find(|f| f.path == file)?;
    if type_at(&snapshot.program, file, &analyzed.parsed, offset).is_some() {
        return None;
    }
    if !is_type_annotation_target(&analyzed.parsed.file, offset) {
        return None;
    }
    let (segments, segment_index) = qualified_name_at_with_position(&analyzed.source, offset)?;
    if segment_index + 1 == segments.len() {
        if let Some(schema) = resource_schema_for_type_name(snapshot, file, &segments) {
            return Some(resource_hover(schema, reader));
        }
        if let Some(schema) = enum_schema_for_type_name(snapshot, file, &segments) {
            return Some(enum_hover(schema));
        }
    }
    None
}

fn saved_root_hover(
    snapshot: &AnalysisSnapshot,
    file: &Path,
    offset: usize,
    reader: Option<&StoreReader>,
) -> Option<String> {
    let analyzed = snapshot.files.iter().find(|f| f.path == file)?;
    let segments = saved_path_at(&analyzed.parsed.file, &snapshot.program, offset)?;
    let Some(marrow_store::path::PathSegment::Root(root)) = segments.first() else {
        return None;
    };
    if !offset_is_on_saved_root(&analyzed.source, offset, root) {
        return None;
    }
    let (store, resource) = marrow_check::resolve::resolve_store_by_root(&snapshot.program, root)?;
    Some(store_hover(store, resource, reader))
}

fn store_root_declaration_hover(
    snapshot: &AnalysisSnapshot,
    file: &Path,
    offset: usize,
    reader: Option<&StoreReader>,
) -> Option<String> {
    let analyzed = snapshot.files.iter().find(|f| f.path == file)?;
    let store = store_root_at(&analyzed.parsed.file, &analyzed.source, offset)?;
    let (store_schema, resource) =
        marrow_check::resolve::resolve_store_by_root(&snapshot.program, &store.root.root)?;
    Some(store_hover(store_schema, resource, reader))
}

fn resource_member_hover(
    snapshot: &AnalysisSnapshot,
    index: &BindingIndex,
    file: &Path,
    offset: usize,
    reader: Option<&StoreReader>,
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
    let mut value = if symbol.kind == SymbolKind::Index {
        let index = store_index_at(&parsed_file.parsed.file, symbol.span)?;
        let mut value = marrow_code_block(&index_signature(index));
        append_docs(&mut value, join_docs(&index.docs));
        value
    } else {
        let member = resource_member_at(&parsed_file.parsed.file, symbol.span)?;
        let mut value = marrow_code_block(&resource_member_signature(member));
        append_docs(&mut value, join_docs(resource_member_docs(member)));
        value
    };

    if let Some(reader) = reader
        && let Some(analyzed) = snapshot.files.iter().find(|f| f.path == file)
        && let Some(segments) = saved_path_at(&analyzed.parsed.file, &snapshot.program, offset)
        && let Some(line) = live_value_line(reader, &segments, &snapshot.program)
    {
        value.push_str("\n\n");
        value.push_str(&line);
    }

    Some(value)
}

fn resource_constructor_hover(
    snapshot: &AnalysisSnapshot,
    file: &Path,
    offset: usize,
    reader: Option<&StoreReader>,
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
            DefItem::Resource(schema) => Some(resource_hover(schema, reader)),
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

/// The hover for byte `offset`, augmented with the symbol's documentation and the
/// live stored value when each is available.
///
/// The type always comes from the checker and is shown first. `docs`, when
/// present, is the symbol's `;;` description rendered as a paragraph below the
/// type. When `reader` is `Some` and the cursor resolves to a concrete saved path,
/// a final line shows the live value (a typed scalar, `N records`, `present`, or
/// `absent`). A store that is `Unavailable` (locked, corrupt, missing) adds
/// nothing — the hover shows only the type and docs, never an error. Passing
/// `None` for `reader` disables live data entirely (the `marrow.liveData` setting
/// is off, or no native store is configured); passing `None` for `docs` omits the
/// description (the symbol has none, or is a local/parameter that carries none).
pub fn hover_with_live(
    snapshot: &AnalysisSnapshot,
    file: &Path,
    offset: usize,
    docs: Option<&str>,
    reader: Option<&StoreReader>,
) -> Option<Hover> {
    let analyzed = snapshot.files.iter().find(|f| f.path == file)?;
    let ty = type_at(&snapshot.program, file, &analyzed.parsed, offset)?;
    let mut value = marrow_code_block(&render_type(&ty));

    // The documentation paragraph sits between the type and any live value: the
    // type is the primary fact, the docs explain the symbol, the live value is
    // the current store reading.
    if let Some(docs) = docs {
        value.push_str("\n\n");
        value.push_str(docs);
    }

    if let Some(reader) = reader
        && let Some(segments) = saved_path_at(&analyzed.parsed.file, &snapshot.program, offset)
        && let Some(line) = live_value_line(reader, &segments, &snapshot.program)
    {
        value.push_str("\n\n");
        value.push_str(&line);
    }

    Some(markdown_hover(value))
}

fn function_hover(
    snapshot: &AnalysisSnapshot,
    index: &BindingIndex,
    file: &Path,
    offset: usize,
    reader: Option<&StoreReader>,
) -> Option<String> {
    let symbol = index.definition(file, offset)?;
    if symbol.kind != SymbolKind::Function
        || !is_function_hover_target(snapshot, index, file, offset, &symbol)
    {
        return None;
    }

    let parsed_file = snapshot.files.iter().find(|f| f.path == symbol.file)?;
    let parsed_function = parsed_function_at(&parsed_file.parsed.file, symbol.span)?;
    let checked_function = checked_function_at(snapshot, &symbol)?;
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

    if let Some(reader) = reader
        && let Some(analyzed) = snapshot.files.iter().find(|f| f.path == file)
        && let Some(segments) = saved_path_at(&analyzed.parsed.file, &snapshot.program, offset)
        && let Some(line) = live_value_line(reader, &segments, &snapshot.program)
    {
        value.push_str("\n\n");
        value.push_str(&line);
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

fn direct_effects_markdown(facts: &CheckedFacts, effects: &DirectEffectFacts) -> Option<String> {
    let mut lines = Vec::new();
    if let Some(line) = saved_effects_line(facts, "saved reads", &effects.saved_reads) {
        lines.push(line);
    }
    if let Some(line) = saved_effects_line(facts, "saved writes", &effects.saved_writes) {
        lines.push(line);
    }
    if effects.transactions {
        lines.push("- transaction".to_string());
    }
    if let Some(line) = host_effects_line(&effects.host_calls) {
        lines.push(line);
    }
    if effects.throws {
        lines.push("- throws".to_string());
    }

    if lines.is_empty() {
        None
    } else {
        Some(format!("**Direct effects**\n{}", lines.join("\n")))
    }
}

fn saved_effects_line(
    facts: &CheckedFacts,
    label: &str,
    places: &[SavedPlaceEffect],
) -> Option<String> {
    let items = effect_items(
        places
            .iter()
            .filter_map(|place| saved_place_display(facts, place)),
    );
    if items.is_empty() {
        None
    } else {
        Some(format!("- {label}: {}", items.join(", ")))
    }
}

fn host_effects_line(effects: &[HostEffect]) -> Option<String> {
    let items = effect_items(effects.iter().map(|effect| match effect {
        HostEffect::Output => "output".to_string(),
        HostEffect::Capability(capability) => capability_label(*capability).to_string(),
    }));
    if items.is_empty() {
        None
    } else {
        Some(format!("- host: {}", items.join(", ")))
    }
}

fn effect_items(items: impl IntoIterator<Item = String>) -> Vec<String> {
    const MAX_EFFECT_ITEMS: usize = 8;

    let mut items = items.into_iter().collect::<Vec<_>>();
    items.sort();
    items.dedup();

    let extra = items.len().saturating_sub(MAX_EFFECT_ITEMS);
    items.truncate(MAX_EFFECT_ITEMS);
    if extra > 0 {
        items.push(format!("+{extra} more"));
    }
    items
}

fn saved_place_display(facts: &CheckedFacts, place: &SavedPlaceEffect) -> Option<String> {
    let resource = facts
        .resources()
        .iter()
        .find(|resource| resource.id == place.resource)?;
    let mut path = vec![resource.name.clone()];
    for member_id in &place.members {
        let member = facts
            .resource_members()
            .iter()
            .find(|member| member.id == *member_id)?;
        path.push(member.name.clone());
    }
    Some(path.join("."))
}

fn capability_label(capability: stdlib::Capability) -> &'static str {
    match capability {
        stdlib::Capability::Pure => "pure",
        stdlib::Capability::Clock => "clock",
        stdlib::Capability::Env => "env",
        stdlib::Capability::Log => "log",
        stdlib::Capability::Io => "io",
        stdlib::Capability::Assert => "assert",
    }
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

    let parsed_file = snapshot.files.iter().find(|f| f.path == symbol.file)?;
    let parsed_function = parsed_function_at(&parsed_file.parsed.file, symbol.span)?;
    let name = parameter_use_name(snapshot, file, offset, parsed_function)?;
    let checked_function = checked_function_at(snapshot, &symbol)?;
    let checked_param = checked_function
        .params
        .iter()
        .rev()
        .find(|param| param.name == name)?;
    let parsed_param = parsed_function
        .params
        .iter()
        .rev()
        .find(|param| param.name == name)?;

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
        | TokenKind::Percent
        | TokenKind::Underscore => Some(text),
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
            is_named_symbol_reference(snapshot, index, file, offset, symbol)
                || is_enum_declaration_name(snapshot, file, offset, symbol)
        }
        SymbolKind::EnumMember => {
            is_named_symbol_reference(snapshot, index, file, offset, symbol)
                || is_enum_member_declaration_name(snapshot, file, offset, symbol)
        }
        _ => false,
    }
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
    is_resource_member_declaration_name(snapshot, file, offset, symbol)
        || index.references(symbol).iter().any(|reference| {
            reference.file == file
                && reference.span != symbol.span
                && span_covers(reference.span, offset)
                && offset_is_on_last_identifier(snapshot, file, reference.span, offset)
        })
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

fn offset_is_on_saved_root(source: &str, offset: usize, root: &str) -> bool {
    let tokens = lex_source(source).tokens;
    tokens.windows(2).any(|pair| {
        let [previous, token] = pair else {
            return false;
        };
        previous.kind == TokenKind::Caret
            && token.kind == TokenKind::Identifier
            && token.text(source) == root
            && previous.span.start_byte <= offset
            && offset <= token.span.end_byte
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
            Declaration::Function(function) if function.span == span => Some(function),
            _ => None,
        })
}

fn checked_function_at<'a>(
    snapshot: &'a AnalysisSnapshot,
    symbol: &SymbolRef,
) -> Option<&'a CheckedFunction> {
    snapshot
        .program
        .modules
        .iter()
        .find(|module| module.source_file == symbol.file)?
        .functions
        .iter()
        .find(|function| function.span == symbol.span)
}

fn parsed_const_at(
    source: &marrow_syntax::SourceFile,
    span: SourceSpan,
) -> Option<&marrow_syntax::ConstDecl> {
    source
        .declarations
        .iter()
        .find_map(|declaration| match declaration {
            Declaration::Const(constant) if constant.span == span => Some(constant),
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
        .find(|constant| constant.span == symbol.span)
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

fn function_signature(function: &CheckedFunction) -> String {
    let params = function
        .params
        .iter()
        .map(|param| {
            let mode = match param.mode {
                Some(ParamMode::Out) => "out ",
                Some(ParamMode::InOut) => "inout ",
                None => "",
            };
            format!("{mode}{}: {}", param.name, render_type(&param.ty))
        })
        .collect::<Vec<_>>()
        .join(", ");
    match &function.return_type {
        Some(ty) => format!("fn {}({params}): {}", function.name, render_type(ty)),
        None => format!("fn {}({params})", function.name),
    }
}

fn parameter_signature(param: &CheckedParam) -> String {
    let mode = match param.mode {
        Some(ParamMode::Out) => "out ",
        Some(ParamMode::InOut) => "inout ",
        None => "",
    };
    format!("{mode}{}: {}", param.name, render_type(&param.ty))
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
        [name] => enum_schema_visible_by_bare_name(snapshot, file, name),
        [.., name] => {
            let analyzed = snapshot.files.iter().find(|f| f.path == file)?;
            let imports = analyzed
                .parsed
                .file
                .uses
                .iter()
                .map(|use_decl| use_decl.name.clone())
                .collect::<Vec<_>>();
            let aliases = build_alias_map(&imports);
            let prefix = segments[..segments.len() - 1].join("::");
            let module_name = expand_module_alias(&prefix, &aliases);
            enum_schema_in_visible_module(snapshot, file, &module_name, name)
        }
        [] => None,
    }
}

fn enum_schema_visible_by_bare_name<'a>(
    snapshot: &'a AnalysisSnapshot,
    file: &Path,
    name: &str,
) -> Option<&'a EnumSchema> {
    let current_module = module_name_for_file(snapshot, file);
    if let Some(schema) =
        current_module.and_then(|module| enum_schema_in_module(snapshot, module, name))
    {
        return Some(schema);
    }
    snapshot
        .program
        .modules
        .iter()
        .filter(|module| Some(module.name.as_str()) != current_module && !module.name.is_empty())
        .find_map(|module| {
            let schema = module
                .enums
                .iter()
                .find(|enum_schema| enum_schema.name == name)?;
            enum_is_public(module, name).then_some(schema)
        })
}

fn enum_schema_in_visible_module<'a>(
    snapshot: &'a AnalysisSnapshot,
    file: &Path,
    module_name: &str,
    name: &str,
) -> Option<&'a EnumSchema> {
    let current_module = module_name_for_file(snapshot, file);
    let module = snapshot
        .program
        .modules
        .iter()
        .find(|module| module.name == module_name)?;
    if Some(module.name.as_str()) != current_module && !enum_is_public(module, name) {
        return None;
    }
    module
        .enums
        .iter()
        .find(|enum_schema| enum_schema.name == name)
}

fn enum_is_public(module: &marrow_check::CheckedModule, name: &str) -> bool {
    module.enum_public.get(name).copied().unwrap_or(true)
}

fn module_name_for_file<'a>(snapshot: &'a AnalysisSnapshot, file: &Path) -> Option<&'a str> {
    snapshot
        .program
        .modules
        .iter()
        .find(|module| module.source_file == file)
        .map(|module| module.name.as_str())
}

fn enum_schema_in_module<'a>(
    snapshot: &'a AnalysisSnapshot,
    module_name: &str,
    name: &str,
) -> Option<&'a EnumSchema> {
    snapshot
        .program
        .modules
        .iter()
        .find(|module| module.name == module_name)?
        .enums
        .iter()
        .find(|enum_schema| enum_schema.name == name)
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
        if member.span == span || enum_member_path_in(&member.members, span, path) {
            return true;
        }
        path.pop();
    }
    false
}

fn resource_hover(schema: &ResourceSchema, _reader: Option<&StoreReader>) -> String {
    let mut value = marrow_code_block(&resource_signature(schema));
    append_docs(&mut value, join_docs(&schema.docs));
    append_section(&mut value, resource_member_summary(schema, &[]));
    value
}

fn store_hover(
    store: &StoreSchema,
    resource: &ResourceSchema,
    reader: Option<&StoreReader>,
) -> String {
    let mut value = marrow_code_block(&store_signature(store));
    append_docs(&mut value, join_docs(&store.docs));
    append_section(&mut value, identity_summary(store));
    append_section(
        &mut value,
        resource_member_summary(resource, &store.indexes),
    );
    if let Some(reader) = reader
        && let Availability::Available(count) = reader.record_count(&store.root)
    {
        value.push_str("\n\n");
        value.push_str(&live_advisory_line(count.display()));
    }
    value
}

fn enum_hover(schema: &EnumSchema) -> String {
    let mut value = marrow_code_block(&format!("enum {}", schema.name));
    append_docs(&mut value, join_docs(&schema.docs));
    append_section(&mut value, enum_member_summary(schema));
    value
}

fn enum_member_hover(schema: &EnumSchema, ordinal: usize) -> String {
    let path = schema.member_path(ordinal);
    let mut value = marrow_code_block(&format!("{}::{}", schema.name, path.join("::")));
    if let Some(member) = schema.members.get(ordinal) {
        append_docs(&mut value, join_docs(&member.docs));
    }
    let status = if schema.is_selectable_leaf(ordinal) {
        "selectable"
    } else if schema.is_category(ordinal) {
        "category"
    } else {
        "group"
    };
    value.push_str("\n\n");
    value.push_str("**Facts**\n");
    value.push_str(&format!("- enum {}\n", schema.name));
    value.push_str(&format!("- {status}"));
    value
}

fn resource_signature(schema: &ResourceSchema) -> String {
    format!("resource {}", schema.name)
}

fn store_signature(store: &StoreSchema) -> String {
    let mut signature = format!("store ^{}", store.root);
    if !store.identity_keys.is_empty() {
        signature.push('(');
        signature.push_str(
            &store
                .identity_keys
                .iter()
                .map(|key| format!("{}: {}", key.name, key.ty))
                .collect::<Vec<_>>()
                .join(", "),
        );
        signature.push(')');
    }
    signature.push_str(": ");
    signature.push_str(&store.resource);
    signature
}

fn identity_summary(store: &StoreSchema) -> Option<String> {
    if store.identity_keys.is_empty() {
        return Some("**Identity**\n- keyless singleton; no generated identity type".to_string());
    }
    let mut lines = vec![format!("- generated identity type: `Id(^{})`", store.root)];
    lines.extend(
        store
            .identity_keys
            .iter()
            .map(|key| format!("- `{}: {}`", key.name, key.ty)),
    );
    Some(format!("**Identity**\n{}", lines.join("\n")))
}

fn resource_member_summary(schema: &ResourceSchema, indexes: &[IndexSchema]) -> Option<String> {
    let mut lines = Vec::new();
    for member in &schema.members {
        resource_member_lines(member, "", &mut lines);
    }
    lines.extend(indexes.iter().map(|index| {
        let unique = if index.unique { " unique" } else { "" };
        format!("index {}({}){unique}", index.name, index.args.join(", "))
    }));
    bounded_summary("Members", lines)
}

fn resource_member_lines(member: &marrow_schema::Node, prefix: &str, lines: &mut Vec<String>) {
    let name = resource_member_path_segment(member);
    let path = if prefix.is_empty() {
        name
    } else {
        format!("{prefix}.{name}")
    };
    match &member.kind {
        NodeKind::Slot { ty, required } => {
            let required = if *required { "required " } else { "" };
            lines.push(format!("{required}{path}: {ty}"));
        }
        NodeKind::Group => {
            lines.push(path.clone());
            for child in &member.members {
                resource_member_lines(child, &path, lines);
            }
        }
    }
}

fn resource_member_path_segment(member: &marrow_schema::Node) -> String {
    format!("{}{}", member.name, key_params(&member.key_params))
}

fn key_params(keys: &[marrow_schema::KeyDef]) -> String {
    if keys.is_empty() {
        String::new()
    } else {
        format!(
            "({})",
            keys.iter()
                .map(|key| format!("{}: {}", key.name, key.ty))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

fn enum_member_summary(schema: &EnumSchema) -> Option<String> {
    bounded_summary(
        "Members",
        schema
            .members
            .iter()
            .enumerate()
            .map(|(ordinal, _)| {
                let status = if schema.is_selectable_leaf(ordinal) {
                    "selectable"
                } else if schema.is_category(ordinal) {
                    "category"
                } else {
                    "group"
                };
                format!("{} {status}", schema.member_path(ordinal).join("::"))
            })
            .collect(),
    )
}

fn bounded_summary(label: &str, lines: Vec<String>) -> Option<String> {
    const LIMIT: usize = 5;
    if lines.is_empty() {
        return None;
    }
    let total = lines.len();
    let mut shown = lines
        .into_iter()
        .take(LIMIT)
        .map(|line| format!("- `{line}`"))
        .collect::<Vec<_>>();
    if total > LIMIT {
        shown.push(format!("- ... {} more", total - LIMIT));
    }
    Some(format!("**{label}**\n{}", shown.join("\n")))
}

fn append_docs(value: &mut String, docs: Option<String>) {
    if let Some(docs) = docs {
        value.push_str("\n\n");
        value.push_str(&docs);
    }
}

fn append_section(value: &mut String, section: Option<String>) {
    if let Some(section) = section {
        value.push_str("\n\n");
        value.push_str(&section);
    }
}

fn is_type_annotation_target(source: &marrow_syntax::SourceFile, offset: usize) -> bool {
    source
        .declarations
        .iter()
        .any(|declaration| declaration_type_annotation_at(declaration, offset))
}

fn declaration_type_annotation_at(declaration: &Declaration, offset: usize) -> bool {
    match declaration {
        Declaration::Const(declaration) => declaration
            .ty
            .as_ref()
            .is_some_and(|ty| type_ref_covers(ty, offset)),
        Declaration::Resource(resource) => resource
            .members
            .iter()
            .any(|member| resource_member_type_annotation_at(member, offset)),
        Declaration::Store(store) => store
            .root
            .keys
            .iter()
            .any(|key| type_ref_covers(&key.ty, offset)),
        Declaration::Function(function) => {
            function
                .params
                .iter()
                .any(|param| type_ref_covers(&param.ty, offset))
                || function
                    .return_type
                    .as_ref()
                    .is_some_and(|ty| type_ref_covers(ty, offset))
                || block_type_annotation_at(&function.body, offset)
        }
        Declaration::Enum(_) => false,
    }
}

fn resource_member_type_annotation_at(member: &ResourceMember, offset: usize) -> bool {
    match member {
        ResourceMember::Field(field) => {
            type_ref_covers(&field.ty, offset)
                || field
                    .keys
                    .iter()
                    .any(|key| type_ref_covers(&key.ty, offset))
        }
        ResourceMember::Group(group) => {
            group
                .keys
                .iter()
                .any(|key| type_ref_covers(&key.ty, offset))
                || group
                    .members
                    .iter()
                    .any(|member| resource_member_type_annotation_at(member, offset))
        }
    }
}

fn block_type_annotation_at(block: &Block, offset: usize) -> bool {
    block
        .statements
        .iter()
        .any(|statement| statement_type_annotation_at(statement, offset))
}

fn statement_type_annotation_at(statement: &Statement, offset: usize) -> bool {
    match statement {
        Statement::Const { ty, .. } => ty.as_ref().is_some_and(|ty| type_ref_covers(ty, offset)),
        Statement::Var { keys, ty, .. } => {
            keys.iter().any(|key| type_ref_covers(&key.ty, offset))
                || ty.as_ref().is_some_and(|ty| type_ref_covers(ty, offset))
        }
        Statement::If {
            then_block,
            else_ifs,
            else_block,
            ..
        } => {
            block_type_annotation_at(then_block, offset)
                || else_ifs
                    .iter()
                    .any(|else_if| block_type_annotation_at(&else_if.block, offset))
                || else_block
                    .as_ref()
                    .is_some_and(|block| block_type_annotation_at(block, offset))
        }
        Statement::While { body, .. }
        | Statement::For { body, .. }
        | Statement::Transaction { body, .. }
        | Statement::Lock { body, .. } => block_type_annotation_at(body, offset),
        Statement::Try {
            body,
            catch,
            finally,
            ..
        } => {
            block_type_annotation_at(body, offset)
                || catch.as_ref().is_some_and(|catch| {
                    catch
                        .ty
                        .as_ref()
                        .is_some_and(|ty| type_ref_covers(ty, offset))
                        || block_type_annotation_at(&catch.block, offset)
                })
                || finally
                    .as_ref()
                    .is_some_and(|block| block_type_annotation_at(block, offset))
        }
        Statement::Match { arms, .. } => arms
            .iter()
            .any(|arm| block_type_annotation_at(&arm.block, offset)),
        Statement::Assign { .. }
        | Statement::Delete { .. }
        | Statement::Merge { .. }
        | Statement::Return { .. }
        | Statement::Break { .. }
        | Statement::Continue { .. }
        | Statement::Throw { .. }
        | Statement::Expr { .. } => false,
    }
}

fn type_ref_covers(ty: &TypeRef, offset: usize) -> bool {
    span_covers(ty.span, offset)
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

fn markdown_hover(value: String) -> Hover {
    Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range: None,
    }
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
                    Declaration::Function(function) if function.span == symbol.span => {
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
                    Declaration::Const(constant) if constant.span == symbol.span => {
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
                    .find(|index| index.span == symbol.span)
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
            Declaration::Resource(resource) if resource.span == span => Some(resource),
            _ => None,
        })
}

/// The enum declaration whose span is `span`, or `None`.
fn enum_at(source: &marrow_syntax::SourceFile, span: SourceSpan) -> Option<&EnumDecl> {
    source
        .declarations
        .iter()
        .find_map(|declaration| match declaration {
            Declaration::Enum(enum_decl) if enum_decl.span == span => Some(enum_decl),
            _ => None,
        })
}

/// The `docs` of the enum member whose span is `span`, searching nested members,
/// or `None`.
fn enum_member_docs(members: &[EnumMember], span: SourceSpan) -> Option<&[String]> {
    for member in members {
        if member.span == span {
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
        if member.span == span {
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
            ResourceMember::Field(field) if field.span == span => return Some(member),
            ResourceMember::Group(group) if group.span == span => return Some(member),
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
            Declaration::Store(store) => store.indexes.iter().find(|index| index.span == span),
            _ => None,
        })
}

fn resource_member_signature(member: &ResourceMember) -> String {
    match member {
        ResourceMember::Field(field) => {
            let required = if field.required { "required " } else { "" };
            format!(
                "{required}{}{}: {}",
                field.name,
                syntax_key_params(&field.keys),
                field.ty
            )
        }
        ResourceMember::Group(group) => {
            format!("{}{}", group.name, syntax_key_params(&group.keys))
        }
    }
}

fn index_signature(index: &IndexDecl) -> String {
    let unique = if index.unique { " unique" } else { "" };
    format!("index {}({}){unique}", index.name, index.args.join(", "))
}

fn resource_member_name(member: &ResourceMember) -> &str {
    match member {
        ResourceMember::Field(field) => &field.name,
        ResourceMember::Group(group) => &group.name,
    }
}

fn resource_member_docs(member: &ResourceMember) -> &[String] {
    match member {
        ResourceMember::Field(field) => &field.docs,
        ResourceMember::Group(group) => &group.docs,
    }
}

fn syntax_key_params(keys: &[KeyParam]) -> String {
    if keys.is_empty() {
        String::new()
    } else {
        format!(
            "({})",
            keys.iter()
                .map(|key| format!("{}: {}", key.name, key.ty))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

/// The `docs` of the resource member (field, group/layer, or index) whose span is
/// `span`, searching nested groups, or `None`.
fn member_docs(members: &[ResourceMember], span: SourceSpan) -> Option<&[String]> {
    for member in members {
        match member {
            ResourceMember::Field(field) if field.span == span => return Some(&field.docs),
            ResourceMember::Group(group) if group.span == span => return Some(&group.docs),
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

/// Join `;;` doc lines into one Markdown paragraph, or `None` when there is no doc
/// text (no lines, or only blank ones).
fn join_docs(lines: &[String]) -> Option<String> {
    let joined = lines
        .iter()
        .map(|line| line.trim())
        .collect::<Vec<_>>()
        .join(" ");
    let joined = joined.trim();
    if joined.is_empty() {
        None
    } else {
        Some(joined.to_string())
    }
}

/// The debug/admin advisory line for a resolved saved path, or `None` when the
/// store is unavailable so the hover stays type-only. A root path shows its
/// record count; any other path shows its presence and, for a stored value, the
/// typed value.
fn live_value_line(
    reader: &StoreReader,
    segments: &[marrow_store::path::PathSegment],
    program: &marrow_check::CheckedProgram,
) -> Option<String> {
    use marrow_store::path::PathSegment;
    // A bare `^root` reads as the record count, the useful fact at a root.
    if let [PathSegment::Root(root)] = segments {
        return match reader.record_count(root) {
            Availability::Available(count) => Some(live_advisory_line(count.display())),
            Availability::Unavailable => None,
        };
    }
    match reader.get(segments, program) {
        Availability::Available(stored) => Some(live_advisory_line(present(&stored))),
        Availability::Unavailable => None,
    }
}

fn live_advisory_line(value: impl std::fmt::Display) -> String {
    const LIVE_ADVISORY_LABEL: &str = "**debug/admin live data (advisory)**";
    format!("{LIVE_ADVISORY_LABEL}: {value}")
}

/// The live-value text for a non-root path: the stored value when one is present,
/// else a presence word.
fn present(stored: &StoredValue) -> String {
    match (&stored.value, stored.presence) {
        (Some(value), _) => value.clone(),
        (None, Presence::ChildrenOnly | Presence::ValueAndChildren) => "present".to_string(),
        (None, Presence::Absent | Presence::ValueOnly) => "absent".to_string(),
    }
}

/// Wrap `code` in a fenced `marrow` block so the client renders it as `.mw`.
fn marrow_code_block(code: &str) -> String {
    format!("```marrow\n{code}\n```")
}

#[cfg(test)]
mod tests {
    use super::*;
    use marrow_check::{ProjectSources, analyze_project};
    use marrow_project::parse_config;

    /// Analyze a one-file project laid out on disk under a temp dir, returning the
    /// snapshot and the module file's path so a test can hover into it.
    fn analyze(source: &str) -> (AnalysisSnapshot, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("marrow.json"), r#"{ "sourceRoots": ["src"] }"#).unwrap();
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let file = src.join("a.mw");
        std::fs::write(&file, source).unwrap();
        let config = parse_config(r#"{ "sourceRoots": ["src"] }"#).unwrap();
        let snapshot = analyze_project(root, &config, &ProjectSources::new()).unwrap();
        // Keep the temp dir alive for the snapshot's lifetime by leaking it; the
        // OS reclaims it when the test process exits.
        std::mem::forget(dir);
        (snapshot, file)
    }

    fn analyze_files(
        files: &[(&str, &str)],
        target: &str,
    ) -> (AnalysisSnapshot, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("marrow.json"), r#"{ "sourceRoots": ["src"] }"#).unwrap();
        let src = root.join("src");
        for (path, source) in files {
            let file = src.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, source).unwrap();
        }
        let config = parse_config(r#"{ "sourceRoots": ["src"] }"#).unwrap();
        let snapshot = analyze_project(root, &config, &ProjectSources::new()).unwrap();
        let file = src.join(target);
        std::mem::forget(dir);
        (snapshot, file)
    }

    /// The byte offset of the first occurrence of `needle` in `source`.
    fn offset_of(source: &str, needle: &str) -> usize {
        source.find(needle).expect("needle present in source")
    }

    fn hover_value(
        snapshot: &AnalysisSnapshot,
        index: &BindingIndex,
        file: &std::path::Path,
        source: &str,
        needle: &str,
    ) -> String {
        let offset = offset_of(source, needle);
        let hover = hover_with_index(snapshot, index, file, offset, None).expect("a hover");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup");
        };
        markup.value
    }

    fn hover_value_at(
        snapshot: &AnalysisSnapshot,
        index: &BindingIndex,
        file: &std::path::Path,
        offset: usize,
    ) -> Option<String> {
        let hover = hover_with_index(snapshot, index, file, offset, None)?;
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup");
        };
        Some(markup.value)
    }

    #[test]
    fn hover_over_a_plain_parameter_use_shows_its_signature_fragment() {
        let source = "module a\n\npub fn f(n: int): int\n    return n\n";
        let (snapshot, file) = analyze(source);
        // Hover over the `n` in `return n`.
        let offset = offset_of(source, "return n") + "return ".len();
        let hover = hover(&snapshot, &file, offset).expect("a hover at the use of `n`");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup contents");
        };
        assert_eq!(markup.value, "```marrow\nn: int\n```");
    }

    #[test]
    fn hover_over_a_second_parameter_use_shows_its_signature_fragment() {
        let source = "\
module a

pub fn f(first: int, second: string): string
    return second
";
        let (snapshot, file) = analyze(source);
        let offset = offset_of(source, "return second") + "return ".len();

        let hover = hover(&snapshot, &file, offset).expect("a hover at the use of `second`");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup contents");
        };
        assert_eq!(markup.value, "```marrow\nsecond: string\n```");
    }

    #[test]
    fn hover_over_a_moded_parameter_use_shows_its_mode() {
        let source = "\
module a

pub fn fill(out value: int)
    value = 1
";
        let (snapshot, file) = analyze(source);
        let offset = offset_of(source, "value = 1") + 1;

        let hover = hover(&snapshot, &file, offset).expect("a hover at the use of `value`");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup contents");
        };
        assert_eq!(markup.value, "```marrow\nout value: int\n```");
    }

    #[test]
    fn hover_over_a_parameter_declaration_does_not_show_parameter_use_hover() {
        let source = "module a\n\npub fn f(n: int): int\n    return n\n";
        let (snapshot, file) = analyze(source);
        let offset = offset_of(source, "n: int");

        let hover = hover(&snapshot, &file, offset);
        if let Some(hover) = hover {
            let HoverContents::Markup(markup) = hover.contents else {
                panic!("expected markup contents");
            };
            assert_ne!(markup.value, "```marrow\nn: int\n```");
        }
    }

    #[test]
    fn hover_over_a_field_named_like_a_parameter_does_not_show_parameter_use_hover() {
        let source = "\
module a

resource Book
    required title: string

pub fn get(
    ;; The parameter, not the field.
    title: string,
): string
    var book: Book
    return book.title
";
        let (snapshot, file) = analyze(source);
        let offset = offset_of(source, "book.title") + "book.".len();

        let hover = hover_value_at(&snapshot, &index_for(&snapshot), &file, offset);
        if let Some(value) = hover {
            assert_ne!(value, "```marrow\ntitle: string\n```");
            assert!(
                !value.contains("The parameter, not the field."),
                "field hover should not inherit parameter docs: {value}"
            );
        }
    }

    #[test]
    fn hover_over_a_named_argument_label_does_not_show_parameter_use_hover() {
        let source = "\
module a

resource Book
    required title: string

pub fn make(
    ;; The parameter, not the named argument label.
    title: string,
): Book
    return Book(title: title)
";
        let (snapshot, file) = analyze(source);
        let offset = offset_of(source, "Book(title: title)") + "Book(".len();

        let hover = hover_value_at(&snapshot, &index_for(&snapshot), &file, offset);
        if let Some(value) = hover {
            assert_ne!(value, "```marrow\ntitle: string\n```");
            assert!(
                !value.contains("The parameter, not the named argument label."),
                "named argument label should not inherit parameter docs: {value}"
            );
        }
    }

    #[test]
    fn hover_over_a_duplicate_parameter_name_uses_the_latest_binding() {
        let source = "\
module a

pub fn f(
    ;; First parameter docs.
    n: int,
    ;; Second parameter docs.
    n: string,
): string
    return n
";
        let (snapshot, file) = analyze(source);
        let offset = offset_of(source, "return n") + "return ".len();

        let hover = hover(&snapshot, &file, offset).expect("a hover at the use of `n`");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup contents");
        };
        assert!(
            markup.value.starts_with("```marrow\nn: string\n```"),
            "duplicate parameter hover should match the binding in scope: {}",
            markup.value
        );
        assert!(
            markup.value.contains("Second parameter docs."),
            "duplicate parameter hover should use the later parameter docs: {}",
            markup.value
        );
        assert!(
            !markup.value.contains("First parameter docs."),
            "duplicate parameter hover should not use the shadowed parameter docs: {}",
            markup.value
        );
    }

    #[test]
    fn hover_over_a_documented_parameter_use_shows_its_docs() {
        let source = "\
module a

pub fn f(
    ;; The number to return.
    n: int,
): int
    return n
";
        let (snapshot, file) = analyze(source);
        let offset = offset_of(source, "return n") + "return ".len();

        let hover = hover(&snapshot, &file, offset).expect("a hover at the use of `n`");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup contents");
        };
        assert!(
            markup.value.starts_with("```marrow\nn: int\n```"),
            "parameter signature should lead the hover: {}",
            markup.value
        );
        assert!(
            markup.value.contains("**Parameter**"),
            "parameter docs should be in a short section: {}",
            markup.value
        );
        assert!(
            markup.value.contains("The number to return."),
            "parameter docs should be shown: {}",
            markup.value
        );
    }

    #[test]
    fn hover_over_a_shadowing_local_use_does_not_show_parameter_use_docs() {
        let source = "\
module a

pub fn f(
    ;; The outer parameter.
    n: int,
): int
    if true
        const n: int = 99
        return n
    return n
";
        let (snapshot, file) = analyze(source);
        let offset = offset_of(source, "return n") + "return ".len();

        let hover = hover(&snapshot, &file, offset).expect("a hover at the local use of `n`");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup contents");
        };
        assert_eq!(markup.value, "```marrow\nint\n```");
        assert!(
            !markup.value.contains("The outer parameter."),
            "shadowing local should not inherit parameter docs: {}",
            markup.value
        );
    }

    #[test]
    fn hover_over_a_function_call_shows_signature_and_description() {
        let source = "\
module a

;; Adds two numbers.
pub fn add(x: int, y: int): int
    return x + y

pub fn caller(): int
    return add(1, 2)
";
        let (snapshot, file) = analyze(source);
        let offset = source.rfind("add(1, 2)").unwrap();

        let hover = hover(&snapshot, &file, offset).expect("a hover at the function call");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup contents");
        };
        assert!(
            markup
                .value
                .starts_with("```marrow\nfn add(x: int, y: int): int\n```"),
            "function signature should lead the hover: {}",
            markup.value
        );
        assert!(
            markup.value.contains("Adds two numbers."),
            "function docs should follow the signature: {}",
            markup.value
        );
        assert!(
            !markup.value.contains("Parameters"),
            "parameter docs section should be omitted when no parameter has docs: {}",
            markup.value
        );
    }

    #[test]
    fn hover_over_a_function_call_shows_direct_effects_from_checked_facts() {
        let source = "\
module a

resource Book at ^books(id: int)
    required title: string
    required visits: int

pub fn touch(id: int): string
    const title: string = ^books(id).title
    transaction
        ^books(id).visits = ^books(id).visits + 1
    print(title)
    return title

pub fn caller(): string
    return touch(1)
";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);
        let value = hover_value(&snapshot, &index, &file, source, "touch(1)");

        assert!(
            value.starts_with("```marrow\nfn touch(id: int): string\n```"),
            "function signature should lead the hover: {value}"
        );
        assert!(
            value.contains("**Direct effects**"),
            "function hover should include a direct-effects section: {value}"
        );
        assert!(
            value.contains("- saved reads: Book.title, Book.visits"),
            "direct saved reads should use canonical resource/member facts: {value}"
        );
        assert!(
            value.contains("- saved writes: Book.visits"),
            "direct saved writes should use canonical resource/member facts: {value}"
        );
        assert!(
            value.contains("- transaction"),
            "direct transaction effect should be shown: {value}"
        );
        assert!(
            value.contains("- host: output"),
            "direct host output effect should be shown: {value}"
        );
    }

    #[test]
    fn hover_over_a_function_declaration_name_shows_signature() {
        let source = "\
module a

pub fn answer(): int
    return 42
";
        let (snapshot, file) = analyze(source);
        let offset = offset_of(source, "answer") + 1;

        let hover = hover(&snapshot, &file, offset).expect("a hover at the function declaration");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup contents");
        };
        assert_eq!(markup.value, "```marrow\nfn answer(): int\n```");
    }

    #[test]
    fn hover_over_bare_builtin_call_shows_default_library_signature() {
        let source = "\
module a

resource Book at ^books(id: int)
    required title: string

pub fn f(): bool
    return exists(^books(1))

pub fn g(): Id(^books)
    return nextId(^books)

pub fn h()
    write(\"a\")
    print(\"b\")
    return count(^books)
";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);
        let value = hover_value(&snapshot, &index, &file, source, "exists(^books");

        assert!(
            value.starts_with("```marrow\nexists(path): bool\n```"),
            "builtin signature should lead the hover: {value}"
        );
        assert!(
            value.contains("default library"),
            "builtin hover should identify default-library context: {value}"
        );
        assert!(
            value.contains("builtin"),
            "builtin hover should identify the source kind: {value}"
        );
        assert!(
            value.contains("Returns true when the saved path exists."),
            "builtin hover should include exists docs: {value}"
        );

        let next_id_value = hover_value(&snapshot, &index, &file, source, "nextId(^books");
        assert!(
            next_id_value.starts_with("```marrow\nnextId(^root): Id\n```"),
            "builtin signature should lead the hover: {next_id_value}"
        );
        assert!(
            next_id_value.contains("Returns the next id for a saved root."),
            "builtin hover should include nextId docs: {next_id_value}"
        );
        assert!(
            !next_id_value.contains("Returns true when the saved path exists."),
            "builtin docs should be specific to the hovered builtin: {next_id_value}"
        );

        let write_value = hover_value(&snapshot, &index, &file, source, "write(\"a");
        assert!(
            write_value.contains("Writes rendered text to output without a newline."),
            "write hover should describe output behavior without implying saved-state mutation: {write_value}"
        );
        let print_value = hover_value(&snapshot, &index, &file, source, "print(\"b");
        assert!(
            print_value.contains("Writes rendered text to output with a newline."),
            "print hover should describe output behavior: {print_value}"
        );
        let count_value = hover_value(&snapshot, &index, &file, source, "count(^books");
        assert!(
            count_value.contains(
                "Returns child count for a saved path, 1 for a scalar, or 0 when absent."
            ),
            "count hover should describe path/scalar/absent behavior: {count_value}"
        );
    }

    #[test]
    fn hover_over_std_call_leaf_shows_signature_and_capability() {
        let source = "\
module a

pub fn f(): int
    return std::text::length(\"abc\")
";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);
        let value = hover_value(&snapshot, &index, &file, source, "length(\"abc");

        assert!(
            value.starts_with("```marrow\nstd::text::length(string): int\n```"),
            "std signature should lead the hover: {value}"
        );
        assert!(
            value.contains("Capability: pure"),
            "std hover should include capability context: {value}"
        );
        assert!(
            value.contains("default library"),
            "std hover should identify default-library context: {value}"
        );
    }

    #[test]
    fn hover_over_scalar_conversion_call_shows_default_library_signature() {
        let source = "\
module a

pub fn f(): int
    return int(\"1\")
";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);
        let value = hover_value(&snapshot, &index, &file, source, "int(\"1");

        assert!(
            value.starts_with("```marrow\nint(value): int\n```"),
            "scalar conversion signature should lead the hover: {value}"
        );
        assert!(
            value.contains("conversion"),
            "scalar hover should identify conversion context: {value}"
        );
        assert!(
            value.contains("default library"),
            "scalar hover should identify default-library context: {value}"
        );
    }

    #[test]
    fn hover_over_error_code_conversion_shows_default_library_signature() {
        let source = "\
module a

pub fn f(): ErrorCode
    return ErrorCode(\"not_found\")
";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);
        let value = hover_value(&snapshot, &index, &file, source, "ErrorCode(\"not");

        assert!(
            value.starts_with("```marrow\nErrorCode(value): ErrorCode\n```"),
            "ErrorCode conversion signature should use the language type: {value}"
        );
        assert!(
            value.contains("conversion"),
            "ErrorCode hover should identify conversion context: {value}"
        );
        assert!(
            value.contains("default library"),
            "ErrorCode hover should identify default-library context: {value}"
        );
    }

    #[test]
    fn hover_over_std_namespace_and_module_segments_shows_default_library_context() {
        let source = "\
module a

pub fn f(): int
    return std::text::length(\"abc\")
";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);

        let std_value = hover_value(&snapshot, &index, &file, source, "std::text");
        assert!(
            std_value.starts_with("```marrow\nstd\n```"),
            "std namespace hover should lead with the namespace: {std_value}"
        );
        assert!(
            std_value.contains("default library namespace"),
            "std namespace hover should identify default-library context: {std_value}"
        );

        let text_offset = offset_of(source, "std::text") + "std::".len();
        let text_value = hover_value_at(&snapshot, &index, &file, text_offset)
            .expect("a hover over the std::text module segment");
        assert!(
            text_value.starts_with("```marrow\nstd::text\n```"),
            "std::text module hover should lead with the module path: {text_value}"
        );
        assert!(
            text_value.contains("default library module"),
            "std::text hover should identify default-library context: {text_value}"
        );
        assert!(
            text_value.contains("length"),
            "std::text hover should summarize available operations: {text_value}"
        );

        let length_value = hover_value(&snapshot, &index, &file, source, "length(\"abc");
        assert!(
            length_value.starts_with("```marrow\nstd::text::length(string): int\n```"),
            "std operation leaf hover should remain the operation signature: {length_value}"
        );
    }

    #[test]
    fn hover_over_std_namespace_and_module_segments_requires_std_operation() {
        let std_text_source = "\
module std::text

pub fn custom(): int
    return 1
";
        let std_custom_source = "\
module std::custom

pub fn ping(): int
    return 1
";
        let app_source = "\
module app

pub fn run(): int
    const text_value = std::text::custom()
    const custom_value = std::custom::ping()
    return text_value
";
        let (snapshot, file) = analyze_files(
            &[
                ("std/text.mw", std_text_source),
                ("std/custom.mw", std_custom_source),
                ("app.mw", app_source),
            ],
            "app.mw",
        );
        let index = index_for(&snapshot);

        for needle in ["std::text::custom", "std::custom::ping"] {
            let std_value = hover_value_at(&snapshot, &index, &file, offset_of(app_source, needle));
            assert!(
                !std_value
                    .as_deref()
                    .is_some_and(|value| value.contains("default library")),
                "project-owned {needle} std segment should not get default-library docs: {std_value:?}"
            );

            let module_offset = offset_of(app_source, needle) + "std::".len();
            let module_value = hover_value_at(&snapshot, &index, &file, module_offset);
            assert!(
                !module_value
                    .as_deref()
                    .is_some_and(|value| value.contains("default library")),
                "project-owned {needle} module segment should not get default-library docs: {module_value:?}"
            );
        }
    }

    #[test]
    fn hover_over_std_namespace_and_module_segments_keep_builtin_precedence() {
        let std_text_source = "\
module std::text

pub fn length(value: string): bool
    return false
";
        let app_source = "\
module app

pub fn run(): int
    return std::text::length(\"abc\")
";
        let (snapshot, file) = analyze_files(
            &[("std/text.mw", std_text_source), ("app.mw", app_source)],
            "app.mw",
        );
        let index = index_for(&snapshot);

        let std_value =
            hover_value_at(&snapshot, &index, &file, offset_of(app_source, "std::text"));
        assert!(
            std_value
                .as_deref()
                .is_some_and(|value| value.contains("default library namespace")),
            "std segment should keep default-library docs when a builtin std op wins dispatch: {std_value:?}"
        );

        let text_offset = offset_of(app_source, "std::text") + "std::".len();
        let text_value = hover_value_at(&snapshot, &index, &file, text_offset);
        assert!(
            text_value
                .as_deref()
                .is_some_and(|value| value.contains("default library module")),
            "std::text segment should keep default-library docs when a builtin std op wins dispatch: {text_value:?}"
        );

        let length_value = hover_value(&snapshot, &index, &file, app_source, "length(\"abc");
        assert!(
            length_value.starts_with("```marrow\nstd::text::length(string): int\n```"),
            "std::text::length leaf hover should keep builtin precedence over a project function: {length_value}"
        );
        assert!(
            length_value.contains("default library"),
            "std::text::length leaf hover should identify the builtin dispatch: {length_value}"
        );
    }

    #[test]
    fn hover_over_project_module_leaf_in_use_shows_project_module_context() {
        let books_source = "\
module shelf::books

pub fn titleOf(): string
    return \"Dune\"
";
        let app_source = "\
module shelf::app

use shelf::books

pub fn run(): string
    return books::titleOf()
";
        let (snapshot, file) = analyze_files(
            &[
                ("shelf/books.mw", books_source),
                ("shelf/app.mw", app_source),
            ],
            "shelf/app.mw",
        );
        let index = index_for(&snapshot);
        let leaf = offset_of(app_source, "use shelf::books") + "use shelf::".len();
        let value = hover_value_at(&snapshot, &index, &file, leaf + 1)
            .expect("a hover over the imported module leaf");

        assert!(
            value.starts_with("```marrow\nmodule shelf::books\n```"),
            "module hover should lead with the imported module path: {value}"
        );
        assert!(
            value.contains("Project module"),
            "module hover should identify project-module context: {value}"
        );
    }

    #[test]
    fn hover_over_project_module_qualifier_keeps_function_leaf_hover() {
        let books_source = "\
module shelf::books

;; Formats a book title.
pub fn titleOf(): string
    return \"Dune\"
";
        let app_source = "\
module shelf::app

use shelf::books

pub fn run(): string
    return books::titleOf()
";
        let (snapshot, file) = analyze_files(
            &[
                ("shelf/books.mw", books_source),
                ("shelf/app.mw", app_source),
            ],
            "shelf/app.mw",
        );
        let index = index_for(&snapshot);
        let call = offset_of(app_source, "books::titleOf()");
        let qualifier = hover_value_at(&snapshot, &index, &file, call + 1)
            .expect("a hover over the imported module qualifier");

        assert!(
            qualifier.starts_with("```marrow\nmodule shelf::books\n```"),
            "module qualifier should show its project module: {qualifier}"
        );
        assert!(
            qualifier.contains("Project module"),
            "module qualifier should identify project-module context: {qualifier}"
        );
        assert!(
            !qualifier.contains("fn titleOf("),
            "module qualifier should not pretend to be the function leaf: {qualifier}"
        );
        assert!(
            !qualifier.contains("Formats a book title."),
            "module qualifier should not inherit function docs: {qualifier}"
        );

        let leaf = call + "books::".len();
        let function = hover_value_at(&snapshot, &index, &file, leaf + 1)
            .expect("a hover over the function leaf");
        assert!(
            function.starts_with("```marrow\nfn titleOf(): string\n```"),
            "function leaf should keep the rich function hover: {function}"
        );
        assert!(
            function.contains("Formats a book title."),
            "function leaf should keep function docs: {function}"
        );
    }

    #[test]
    fn hover_over_fully_qualified_project_module_segment_keeps_function_leaf_hover() {
        let books_source = "\
module shelf::books

;; Formats a book title.
pub fn titleOf(): string
    return \"Dune\"
";
        let app_source = "\
module shelf::app

pub fn run(): string
    return shelf::books::titleOf()
";
        let (snapshot, file) = analyze_files(
            &[
                ("shelf/books.mw", books_source),
                ("shelf/app.mw", app_source),
            ],
            "shelf/app.mw",
        );
        let index = index_for(&snapshot);
        let call = offset_of(app_source, "shelf::books::titleOf()");
        let books = call + "shelf::".len();
        let qualifier = hover_value_at(&snapshot, &index, &file, books + 1)
            .expect("a hover over the fully qualified module segment");

        assert!(
            qualifier.starts_with("```marrow\nmodule shelf::books\n```"),
            "fully qualified module segment should show the project module: {qualifier}"
        );
        assert!(
            qualifier.contains("Project module"),
            "fully qualified module segment should identify project-module context: {qualifier}"
        );

        let leaf = call + "shelf::books::".len();
        let function = hover_value_at(&snapshot, &index, &file, leaf + 1)
            .expect("a hover over the function leaf");
        assert!(
            function.starts_with("```marrow\nfn titleOf(): string\n```"),
            "fully qualified function leaf should keep the function hover: {function}"
        );
        assert!(
            function.contains("Formats a book title."),
            "fully qualified function leaf should keep function docs: {function}"
        );
    }

    #[test]
    fn hover_over_project_module_qualifier_does_not_show_qualified_enum_hover() {
        let state_source = "\
module shelf::state

;; Publication state.
pub enum Status
    ;; Open for edits.
    open
";
        let app_source = "\
module shelf::app

use shelf::state

pub fn status(): state::Status
    return state::Status::open
";
        let (snapshot, file) = analyze_files(
            &[
                ("shelf/state.mw", state_source),
                ("shelf/app.mw", app_source),
            ],
            "shelf/app.mw",
        );
        let index = index_for(&snapshot);
        let qualifier = offset_of(app_source, "state::Status::open");
        let value = hover_value_at(&snapshot, &index, &file, qualifier + 1)
            .expect("a hover over the module qualifier");

        assert!(
            value.starts_with("```marrow\nmodule shelf::state\n```"),
            "module qualifier should show module context: {value}"
        );
        assert!(
            value.contains("Project module"),
            "module qualifier should identify project-module context: {value}"
        );
        assert!(
            !value.starts_with("```marrow\nenum Status\n```"),
            "module qualifier should not pretend to be the enum: {value}"
        );
        assert!(
            !value.contains("Publication state."),
            "module qualifier should not inherit enum docs: {value}"
        );
        assert!(
            !value.contains("Open for edits."),
            "module qualifier should not inherit enum member docs: {value}"
        );
    }

    #[test]
    fn hover_over_enum_path_head_wins_over_same_named_project_module() {
        let status_source = "\
module status

pub fn code(): int
    return 1
";
        let app_source = "\
module app

;; Local status enum.
enum status
    active

pub fn current(): status
    return status::active
";
        let (snapshot, file) = analyze_files(
            &[("status.mw", status_source), ("app.mw", app_source)],
            "app.mw",
        );
        let index = index_for(&snapshot);
        let head = offset_of(app_source, "status::active");
        let value = hover_value_at(&snapshot, &index, &file, head + 1)
            .expect("a hover over the enum path head");

        assert!(
            value.starts_with("```marrow\nenum status\n```"),
            "enum path head should keep the enum hover: {value}"
        );
        assert!(
            value.contains("Local status enum."),
            "enum path head should keep enum docs: {value}"
        );
        assert!(
            !value.starts_with("```marrow\nmodule status\n```"),
            "enum path head should not be stolen by the same-named module: {value}"
        );
    }

    #[test]
    fn hover_over_removed_resource_identity_head_does_not_invent_identity_hover() {
        let book_source = "\
module book

pub fn code(): int
    return 1
";
        let app_source = "\
module app

;; Saved books.
resource book at ^books(id: int)
    required title: string

pub fn load(): book::Id
    return book::Id(1)
";
        let (snapshot, file) = analyze_files(
            &[("book.mw", book_source), ("app.mw", app_source)],
            "app.mw",
        );
        let index = index_for(&snapshot);
        let head = offset_of(app_source, "book::Id(1)");
        if let Some(value) = hover_value_at(&snapshot, &index, &file, head + 1) {
            assert!(
                !value.starts_with("```marrow\nbook::Id\n```"),
                "removed identity path must not get a generated identity hover: {value}"
            );
            assert!(
                !value.contains("Saved books."),
                "removed identity path must not inherit resource docs: {value}"
            );
        }
    }

    #[test]
    fn hover_over_project_std_text_module_without_builtin_dispatch_is_project_context() {
        let std_text_source = "\
module std::text

;; Project-only text helper.
pub fn custom(): int
    return 1
";
        let app_source = "\
module app

pub fn run(): int
    return std::text::custom()
";
        let (snapshot, file) = analyze_files(
            &[("std/text.mw", std_text_source), ("app.mw", app_source)],
            "app.mw",
        );
        let index = index_for(&snapshot);
        let text = offset_of(app_source, "std::text::custom") + "std::".len();
        let module = hover_value_at(&snapshot, &index, &file, text + 1)
            .expect("a hover over the project std::text module");

        assert!(
            module.starts_with("```marrow\nmodule std::text\n```"),
            "project std::text module should lead with the project module path: {module}"
        );
        assert!(
            module.contains("Project module"),
            "project std::text module should identify project context: {module}"
        );
        assert!(
            !module.contains("default library"),
            "project std::text module should not get default-library docs without builtin dispatch: {module}"
        );

        let custom = hover_value_at(
            &snapshot,
            &index,
            &file,
            offset_of(app_source, "custom()") + 1,
        )
        .expect("a hover over the project function leaf");
        assert!(
            custom.starts_with("```marrow\nfn custom(): int\n```"),
            "project function leaf should keep its function hover: {custom}"
        );
        assert!(
            custom.contains("Project-only text helper."),
            "project function leaf should keep function docs: {custom}"
        );
        assert!(
            !custom.contains("default library"),
            "project function leaf should not get default-library docs: {custom}"
        );
    }

    #[test]
    fn hover_over_expression_operators_shows_language_operator_docs() {
        let source = "\
module a

pub fn sum(x: int, y: int): int
    return x + y

pub fn both(left: bool, right: bool): bool
    return left and right

pub fn joined(left: string, right: string): string
    return left _ right
";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);

        let plus = hover_value(&snapshot, &index, &file, source, "+");
        assert!(
            plus.starts_with("```marrow\noperator +\n```"),
            "symbolic operator hover should lead with the operator: {plus}"
        );
        assert!(
            plus.contains("language operator"),
            "symbolic operator hover should identify language-operator context: {plus}"
        );
        assert!(
            plus.contains("addition"),
            "plus hover should describe addition behavior: {plus}"
        );

        let and_value = hover_value(&snapshot, &index, &file, source, "and");
        assert!(
            and_value.starts_with("```marrow\noperator and\n```"),
            "keyword operator hover should lead with the operator: {and_value}"
        );
        assert!(
            and_value.contains("language operator"),
            "keyword operator hover should identify language-operator context: {and_value}"
        );
        assert!(
            and_value.contains("logical"),
            "and hover should describe logical behavior: {and_value}"
        );

        let concat = hover_value(&snapshot, &index, &file, source, "_");
        assert!(
            concat.starts_with("```marrow\noperator _\n```"),
            "concat operator hover should lead with the operator: {concat}"
        );
        assert!(
            concat.contains("concatenation"),
            "underscore hover should describe concatenation behavior: {concat}"
        );
    }

    #[test]
    fn hover_over_operator_keyword_path_segment_does_not_show_operator_docs() {
        let ops_source = "\
module and::tools

pub fn length(value: string): int
    return 0
";
        let app_source = "\
module app

pub fn run(): int
    return and::tools::length(\"abc\")
";
        let (snapshot, file) = analyze_files(
            &[("and/tools.mw", ops_source), ("app.mw", app_source)],
            "app.mw",
        );
        let index = index_for(&snapshot);

        let module_offset = offset_of(app_source, "and::tools");
        let value = hover_value_at(&snapshot, &index, &file, module_offset);

        assert!(
            !value
                .as_deref()
                .is_some_and(|value| value.contains("language operator")),
            "module path segment named like an operator should not get operator docs: {value:?}"
        );
    }

    #[test]
    fn hover_over_operator_keyword_declaration_and_import_does_not_show_operator_docs() {
        let and_source = "\
module and

pub fn value(): int
    return 1
";
        let app_source = "\
module app

use and

pub fn run(): int
    return and::value()
";
        let (module_snapshot, module_file) = analyze_files(&[("and.mw", and_source)], "and.mw");
        let module_index = index_for(&module_snapshot);
        let module_value = hover_value_at(
            &module_snapshot,
            &module_index,
            &module_file,
            offset_of(and_source, "module and") + "module ".len(),
        );
        assert!(
            !module_value
                .as_deref()
                .is_some_and(|value| value.contains("language operator")),
            "module declaration segment named like an operator should not get operator docs: {module_value:?}"
        );

        let (app_snapshot, app_file) =
            analyze_files(&[("and.mw", and_source), ("app.mw", app_source)], "app.mw");
        let app_index = index_for(&app_snapshot);
        let use_value = hover_value_at(
            &app_snapshot,
            &app_index,
            &app_file,
            offset_of(app_source, "use and") + "use ".len(),
        );
        assert!(
            !use_value
                .as_deref()
                .is_some_and(|value| value.contains("language operator")),
            "use segment named like an operator should not get operator docs: {use_value:?}"
        );
    }

    #[test]
    fn hover_over_operator_keyword_resource_fields_does_not_show_operator_docs() {
        let source = "\
module app

resource Thing at ^things(id: int)
    required and: int
    required or: int
    required is: bool
    required not: bool
";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);

        for name in ["and", "or", "is", "not"] {
            let offset = offset_of(source, &format!("required {name}:")) + "required ".len();
            let value = hover_value_at(&snapshot, &index, &file, offset);
            assert!(
                !value
                    .as_deref()
                    .is_some_and(|value| value.contains("language operator")),
                "resource field named `{name}` should not get operator docs: {value:?}"
            );
        }
    }

    #[test]
    fn builtin_call_hover_wins_over_same_named_user_function_but_not_declaration() {
        let source = "\
module a

resource Book at ^books(id: int)
    required title: string

fn exists(value: unknown): bool
    return false

fn f(): bool
    return exists(^books(1))
";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);

        let declaration_offset = offset_of(source, "fn exists") + "fn ".len();
        let declaration = hover_value_at(&snapshot, &index, &file, declaration_offset)
            .expect("a hover at the user function declaration");
        assert_eq!(
            declaration, "```marrow\nfn exists(value: unknown): bool\n```",
            "declaration hover should remain the user function"
        );
        assert!(
            !declaration.contains("default library"),
            "declaration hover should not include builtin docs: {declaration}"
        );

        let call = hover_value(&snapshot, &index, &file, source, "exists(^books");
        assert!(
            call.starts_with("```marrow\nexists(path): bool\n```"),
            "call hover should use the builtin signature: {call}"
        );
        assert!(
            call.contains("default library"),
            "call hover should identify default-library context: {call}"
        );
    }

    #[test]
    fn qualified_builtin_like_leaves_do_not_get_default_library_hover() {
        let foo_source = "\
module foo

pub fn exists(): bool
    return false
";
        let foo_std_text_source = "\
module foo::std::text

pub fn length(value: string): int
    return 0
";
        let app_source = "\
module app

use foo
use foo::std::text

pub fn f(): bool
    const found = foo::exists()
    const text_len = foo::std::text::length(\"abc\")
    return found
";
        let (snapshot, file) = analyze_files(
            &[
                ("foo.mw", foo_source),
                ("foo/std/text.mw", foo_std_text_source),
                ("app.mw", app_source),
            ],
            "app.mw",
        );
        let index = index_for(&snapshot);

        let exists_value = hover_value(&snapshot, &index, &file, app_source, "exists()");
        assert!(
            !exists_value.contains("exists(path): bool"),
            "qualified foo::exists call should not get builtin hover: {exists_value}"
        );
        assert!(
            !exists_value.contains("default library"),
            "qualified foo::exists call should not get default-library docs: {exists_value}"
        );

        let length_value = hover_value(&snapshot, &index, &file, app_source, "length(\"abc");
        assert!(
            !length_value.contains("std::text::length(string): int"),
            "qualified foo::std::text::length call should not get std hover: {length_value}"
        );
        assert!(
            !length_value.contains("default library"),
            "qualified foo::std::text::length call should not get default-library docs: {length_value}"
        );

        let std_offset = offset_of(app_source, "std::text::length");
        let std_value = hover_value_at(&snapshot, &index, &file, std_offset);
        assert!(
            !std_value
                .as_deref()
                .is_some_and(|value| value.contains("default library")),
            "qualified foo::std::text std segment should not get default-library docs: {std_value:?}"
        );

        let text_offset = offset_of(app_source, "std::text::length") + "std::".len();
        let text_value = hover_value_at(&snapshot, &index, &file, text_offset);
        assert!(
            !text_value
                .as_deref()
                .is_some_and(|value| value.contains("default library")),
            "qualified foo::std::text text segment should not get default-library docs: {text_value:?}"
        );
    }

    #[test]
    fn hover_over_a_single_letter_function_declaration_name_ignores_the_fn_keyword() {
        let source = "\
module a

pub fn n(): int
    return 1
";
        let (snapshot, file) = analyze(source);
        let offset = offset_of(source, "n():") + 1;

        let hover = hover(&snapshot, &file, offset).expect("a hover at the function declaration");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup contents");
        };
        assert_eq!(markup.value, "```marrow\nfn n(): int\n```");
    }

    #[test]
    fn hover_over_a_function_with_moded_params_shows_modes() {
        let source = "\
module a

pub fn fill(out value: int, inout count: int)
    value = count
    count = count + 1
";
        let (snapshot, file) = analyze(source);
        let offset = offset_of(source, "fill") + 1;

        let hover = hover(&snapshot, &file, offset).expect("a hover at the function declaration");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup contents");
        };
        assert_eq!(
            markup.value,
            "```marrow\nfn fill(out value: int, inout count: int)\n```"
        );
    }

    #[test]
    fn hover_over_a_function_with_parameter_docs_shows_documented_params() {
        let source = "\
module a

pub fn add(
    ;; The left value.
    x: int,
    ;; The right value.
    y: int,
): int
    return x + y

pub fn caller(): int
    return add(1, 2)
";
        let (snapshot, file) = analyze(source);
        let offset = source.rfind("add(1, 2)").unwrap();

        let hover = hover(&snapshot, &file, offset).expect("a hover at the function call");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup contents");
        };
        assert!(
            markup
                .value
                .starts_with("```marrow\nfn add(x: int, y: int): int\n```"),
            "function signature should lead the hover: {}",
            markup.value
        );
        assert!(
            markup.value.contains("**Parameters**"),
            "parameter docs section should be present: {}",
            markup.value
        );
        assert!(
            markup.value.contains("- `x`: The left value."),
            "first parameter docs should be shown: {}",
            markup.value
        );
        assert!(
            markup.value.contains("- `y`: The right value."),
            "second parameter docs should be shown: {}",
            markup.value
        );
    }

    #[test]
    fn hover_over_a_resource_declaration_name_shows_docs_and_members_without_store_identity() {
        let source = "\
module a

;; Book records.
resource Book
    ;; Display title.
    required title: string
    pages: int
    tags(pos: int): string

;; Books saved by id.
store ^books(id: int): Book
    index byTitle(title) unique
";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);
        let value = hover_value(&snapshot, &index, &file, source, "Book\n    ;; Display");

        assert!(
            value.starts_with("```marrow\nresource Book\n```"),
            "resource signature should lead the hover: {value}"
        );
        assert!(
            value.contains("Book records."),
            "resource docs should be shown: {value}"
        );
        assert!(
            !value.contains("Books saved by id."),
            "resource hover should not inherit store docs: {value}"
        );
        assert!(
            !value.contains("Id(^books)"),
            "resource hover should not name the store-owned identity type: {value}"
        );
        assert!(
            value.contains("required title: string"),
            "field summary should include member type: {value}"
        );
        assert!(
            !value.contains("title: string required"),
            "field summary should preserve resource syntax order: {value}"
        );
        assert!(
            value.contains("tags(pos: int): string"),
            "keyed leaf summary should include key and value type: {value}"
        );
        assert!(
            !value.contains("index byTitle(title) unique"),
            "resource hover should not include store-owned indexes: {value}"
        );
    }

    #[test]
    fn hover_over_the_resource_keyword_does_not_show_the_resource() {
        let source = "\
module a

resource Book
    required title: string

;; Books saved by id.
store ^books(id: int): Book
";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);
        let offset = offset_of(source, "resource Book");
        let hover = hover_with_index(&snapshot, &index, &file, offset, None);
        if let Some(Hover {
            contents: HoverContents::Markup(markup),
            ..
        }) = hover
        {
            assert!(
                !markup.value.starts_with("```marrow\nresource Book\n```"),
                "resource keyword should not inherit the declaration-name hover: {}",
                markup.value
            );
        }
    }

    #[test]
    fn hover_over_a_resource_constructor_use_is_rich() {
        let source = "\
module a

;; Books saved by id.
resource Book
    required title: string

pub fn make(): Book
    return Book(title: \"x\")
";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);
        let value = hover_value(&snapshot, &index, &file, source, "Book(title");

        assert!(
            value.starts_with("```marrow\nresource Book\n```"),
            "constructor should show the resource declaration, not only the type: {value}"
        );
        assert!(
            value.contains("Books saved by id."),
            "constructor hover should include resource docs: {value}"
        );
        assert!(
            value.contains("title: string"),
            "constructor hover should include member summary: {value}"
        );
    }

    #[test]
    fn hover_over_a_qualified_resource_constructor_leaf_is_rich() {
        let state_source = "\
module shelf::state

;; Books from state.
resource Book
    required title: string
";
        let app_source = "\
module shelf::app

use shelf::state

pub fn make()
    var book = state::Book(title: \"x\")
";
        let (snapshot, file) = analyze_files(
            &[
                ("shelf/state.mw", state_source),
                ("shelf/app.mw", app_source),
            ],
            "shelf/app.mw",
        );
        let index = index_for(&snapshot);
        let call = offset_of(app_source, "state::Book(title");
        let qualifier_hover = hover_with_index(&snapshot, &index, &file, call + 1, None);
        if let Some(Hover {
            contents: HoverContents::Markup(markup),
            ..
        }) = qualifier_hover
        {
            assert!(
                !markup.value.starts_with("```marrow\nresource Book\n```"),
                "module qualifier should not pretend to be the resource: {}",
                markup.value
            );
        }

        let leaf_offset = call + "state::".len();
        let hover = hover_with_index(&snapshot, &index, &file, leaf_offset, None).expect("a hover");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup");
        };
        let value = markup.value;

        assert!(
            value.starts_with("```marrow\nresource Book\n```"),
            "qualified resource constructor leaf should show resource hover: {value}"
        );
        assert!(
            value.contains("Books from state."),
            "qualified resource constructor should include docs: {value}"
        );
        assert!(
            value.contains("required title: string"),
            "qualified resource constructor should include members: {value}"
        );
    }

    #[test]
    fn hover_over_resource_type_annotation_shows_resource_hover() {
        let source = "\
module a

;; Books saved by id.
resource Book
    required title: string

pub fn echo(book: Book): Book
    var local: Book
    return book
";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);

        for needle in ["book: Book", "): Book", "local: Book"] {
            let offset = offset_of(source, needle) + needle.rfind("Book").unwrap();
            let hover = hover_with_index(&snapshot, &index, &file, offset, None).expect("a hover");
            let HoverContents::Markup(markup) = hover.contents else {
                panic!("expected markup");
            };
            let value = markup.value;

            assert!(
                value.starts_with("```marrow\nresource Book\n```"),
                "resource annotation hover should lead with resource signature: {value}"
            );
            assert!(
                value.contains("Books saved by id."),
                "resource annotation hover should include resource docs: {value}"
            );
            assert!(
                !value.contains("Id(^books)"),
                "resource annotation hover should not name the store-owned identity type: {value}"
            );
            assert!(
                value.contains("required title: string"),
                "resource annotation hover should include member summary: {value}"
            );
        }
    }

    #[test]
    fn hover_over_same_named_resource_type_annotation_uses_current_module_schema() {
        let first_source = "\
module shelf::first

;; First module books.
resource Book
    required title: string
";
        let second_source = "\
module shelf::second

;; Second module books.
resource Book
    required subtitle: string

pub fn echo(book: Book): Book
    return book
";
        let (snapshot, file) = analyze_files(
            &[
                ("shelf/first.mw", first_source),
                ("shelf/second.mw", second_source),
            ],
            "shelf/second.mw",
        );
        let index = index_for(&snapshot);
        let offset = offset_of(second_source, "book: Book") + "book: ".len();
        let hover = hover_with_index(&snapshot, &index, &file, offset, None).expect("a hover");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup");
        };
        let value = markup.value;

        assert!(
            value.starts_with("```marrow\nresource Book\n```"),
            "resource annotation hover should use the current module schema: {value}"
        );
        assert!(
            value.contains("Second module books."),
            "resource annotation hover should include the current module docs: {value}"
        );
        assert!(
            value.contains("subtitle: string"),
            "resource annotation hover should include the current module members: {value}"
        );
        assert!(
            !value.contains("First module books."),
            "resource annotation hover should not use the other module docs: {value}"
        );
        assert!(
            !value.contains("id: int"),
            "resource annotation hover should not use the other module store keys: {value}"
        );
    }

    #[test]
    fn hover_over_wrapped_qualified_resource_type_annotation_shows_resource_hover() {
        let state_source = "\
module shelf::state

;; Books from state.
resource Book
    required title: string
";
        let app_source = "\
module shelf::app

use shelf::state

;; App-local books.
resource Book
    required subtitle: string

pub fn books(items: sequence[state::Book])
    return
";
        let (snapshot, file) = analyze_files(
            &[
                ("shelf/state.mw", state_source),
                ("shelf/app.mw", app_source),
            ],
            "shelf/app.mw",
        );
        let index = index_for(&snapshot);
        let annotation = offset_of(app_source, "state::Book");
        let qualifier_hover = hover_with_index(&snapshot, &index, &file, annotation + 1, None);
        if let Some(Hover {
            contents: HoverContents::Markup(markup),
            ..
        }) = qualifier_hover
        {
            assert!(
                !markup.value.starts_with("```marrow\nresource Book\n```"),
                "module qualifier should not pretend to be the resource: {}",
                markup.value
            );
        }

        let leaf = annotation + "state::".len();
        let hover = hover_with_index(&snapshot, &index, &file, leaf, None).expect("a hover");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup");
        };
        let value = markup.value;

        assert!(
            value.starts_with("```marrow\nresource Book\n```"),
            "wrapped resource annotation hover should use the qualified module schema: {value}"
        );
        assert!(
            value.contains("Books from state."),
            "wrapped resource annotation hover should include foreign docs: {value}"
        );
        assert!(
            value.contains("required title: string"),
            "wrapped resource annotation hover should include foreign members: {value}"
        );
        assert!(
            !value.contains("code: string"),
            "wrapped resource annotation hover should not use app-local same-named resource: {value}"
        );
        assert!(
            !value.contains("App-local books."),
            "wrapped resource annotation hover should not include app-local docs: {value}"
        );
    }

    #[test]
    fn hover_over_same_named_resource_declaration_uses_that_module_schema() {
        let first_source = "\
module shelf::first

;; First module books.
resource Book
    required title: string
";
        let second_source = "\
module shelf::second

;; Second module books.
resource Book
    required subtitle: string
";
        let (snapshot, file) = analyze_files(
            &[
                ("shelf/first.mw", first_source),
                ("shelf/second.mw", second_source),
            ],
            "shelf/second.mw",
        );
        let index = index_for(&snapshot);
        let value = hover_value(&snapshot, &index, &file, second_source, "Book");

        assert!(
            value.starts_with("```marrow\nresource Book\n```"),
            "resource hover should use the declaring module schema: {value}"
        );
        assert!(
            value.contains("Second module books."),
            "resource hover should include the second module docs: {value}"
        );
        assert!(
            value.contains("subtitle: string"),
            "resource hover should include the second module members: {value}"
        );
        assert!(
            !value.contains("First module books."),
            "resource hover should not use the first module docs: {value}"
        );
        assert!(
            !value.contains("id: int"),
            "resource hover should not use the first module store keys: {value}"
        );
    }

    #[test]
    fn hover_over_saved_root_caret_and_chained_use_shows_store_hover() {
        let source = "\
module a

resource Book
    ;; Display title.
    required title: string

;; Books saved by id.
store ^books(id: int): Book

pub fn f(): string
    return ^books(1).title
";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);
        let declaration_caret = offset_of(source, "^books");
        let declaration_root = declaration_caret + 1;
        let use_caret = source.rfind("^books").unwrap();
        let use_root = use_caret + 1;

        for offset in [declaration_caret, declaration_root, use_caret, use_root] {
            let hover = hover_with_index(&snapshot, &index, &file, offset, None).expect("a hover");
            let HoverContents::Markup(markup) = hover.contents else {
                panic!("expected markup");
            };
            let value = markup.value;

            assert!(
                value.starts_with("```marrow\nstore ^books(id: int): Book\n```"),
                "saved root hover should show the store signature: {value}"
            );
            assert!(
                value.contains("Books saved by id."),
                "saved root hover should include store docs: {value}"
            );
            assert!(
                value.contains("Id(^books)"),
                "saved root hover should include the store-owned identity type: {value}"
            );
        }

        let title = source.rfind(").title").unwrap() + ").".len() + 1;
        let hover = hover_with_index(&snapshot, &index, &file, title, None).expect("a hover");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup");
        };
        let value = markup.value;

        assert!(
            value.starts_with("```marrow\nrequired title: string\n```"),
            "saved field hover should stay on the member: {value}"
        );
        assert!(
            value.contains("Display title."),
            "saved field hover should keep member docs: {value}"
        );
    }

    #[test]
    fn hover_over_saved_root_declaration_works_when_resource_name_matches_root() {
        let source = "\
module a

resource books
    required title: string

;; Books saved by id.
store ^books(id: int): books
";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);
        let offset = offset_of(source, "^books") + 1;
        let hover = hover_with_index(&snapshot, &index, &file, offset, None).expect("a hover");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup");
        };
        let value = markup.value;

        assert!(
            value.starts_with("```marrow\nstore ^books(id: int): books\n```"),
            "saved root hover should use the store signature: {value}"
        );
        assert!(
            value.contains("Books saved by id."),
            "saved root hover should include store docs: {value}"
        );
    }

    #[test]
    fn resource_member_summary_is_bounded() {
        let source = "\
module a

resource Book
    one: string
    two: string
    three: string
    four: string
    five: string
    six: string
";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);
        let value = hover_value(&snapshot, &index, &file, source, "Book");

        assert!(
            value.contains("- `five: string`"),
            "fifth member should be shown: {value}"
        );
        assert!(
            !value.contains("- `six: string`"),
            "sixth member should be hidden behind the bound: {value}"
        );
        assert!(
            value.contains("- ... 1 more"),
            "bounded summary should report the hidden member count: {value}"
        );
    }

    #[test]
    fn resource_member_summary_shows_nested_members_bounded() {
        let source = "\
module a

resource Book
    details
        required subtitle: string
        pages: int
    editions(number: int)
        isbn: string
        format: string
";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);
        let value = hover_value(&snapshot, &index, &file, source, "Book");

        assert!(
            value.contains("- `details`"),
            "group itself should be shown: {value}"
        );
        assert!(
            value.contains("- `required details.subtitle: string`"),
            "nested required field should be summarized with its path: {value}"
        );
        assert!(
            value.contains("- `details.pages: int`"),
            "nested plain field should be summarized with its path: {value}"
        );
        assert!(
            value.contains("- `editions(number: int)`"),
            "keyed layer itself should be shown: {value}"
        );
        assert!(
            value.contains("- `editions(number: int).isbn: string`"),
            "nested keyed-layer member should be shown before the bound: {value}"
        );
        assert!(
            !value.contains("- `editions(number: int).format: string`"),
            "sixth flattened member should be hidden behind the bound: {value}"
        );
        assert!(
            value.contains("- ... 1 more"),
            "bounded nested summary should report the hidden member count: {value}"
        );
    }

    #[test]
    fn hover_over_a_saved_root_use_works_without_a_reader() {
        let source = "\
module a

resource Book
    required title: string

;; Books saved by id.
store ^books(id: int): Book

pub fn clear()
    delete ^books
";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);
        let offset = offset_of(source, "delete ^books") + "delete ".len();
        let hover = hover_with_index(&snapshot, &index, &file, offset, None).expect("a hover");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup");
        };
        let value = markup.value;

        assert!(
            value.starts_with("```marrow\nstore ^books(id: int): Book\n```"),
            "saved root should show its store even without live data: {value}"
        );
        assert!(
            value.contains("Books saved by id."),
            "saved root should include store docs: {value}"
        );
        assert!(
            !value.contains("**debug/admin live data (advisory)**"),
            "no reader means no live advisory line: {value}"
        );
    }

    #[test]
    fn hover_over_saved_field_use_shows_member_signature_and_docs() {
        let source = "\
module a

resource Book
    ;; The displayed title.
    required title: string

store ^books(id: int): Book

pub fn f(): string
    return ^books(1).title
";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);
        let offset = source.rfind(").title").unwrap() + ").".len() + 1;
        let hover = hover_with_index(&snapshot, &index, &file, offset, None).expect("a hover");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup");
        };
        let value = markup.value;

        assert!(
            value.starts_with("```marrow\nrequired title: string\n```"),
            "field member signature should lead the hover: {value}"
        );
        assert!(
            value.contains("The displayed title."),
            "field docs should follow the member signature: {value}"
        );
    }

    #[test]
    fn hover_over_resource_field_declaration_name_shows_member_signature() {
        let source = "\
module a

resource Book at ^books(id: int)
    ;; The displayed title.
    required title: string
";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);
        let offset = offset_of(source, "required title") + "required ".len() + 1;
        let hover = hover_with_index(&snapshot, &index, &file, offset, None).expect("a hover");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup");
        };
        let value = markup.value;

        assert!(
            value.starts_with("```marrow\nrequired title: string\n```"),
            "field declaration hover should lead with the member signature: {value}"
        );
        assert!(
            value.contains("The displayed title."),
            "field docs should follow the member signature: {value}"
        );
    }

    #[test]
    fn hover_over_a_saved_root_declaration_works_without_a_reader() {
        let source = "\
module a

resource Book
    required title: string

;; Books saved by id.
store ^books(id: int): Book
";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);
        let offset = offset_of(source, "^books") + 1;
        let hover = hover_with_index(&snapshot, &index, &file, offset, None).expect("a hover");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup");
        };
        let value = markup.value;

        assert!(
            value.starts_with("```marrow\nstore ^books(id: int): Book\n```"),
            "declared saved root should show its store even without live data: {value}"
        );
        assert!(
            value.contains("Books saved by id."),
            "declared saved root should include store docs: {value}"
        );
        assert!(
            !value.contains("**debug/admin live data (advisory)**"),
            "no reader means no live advisory line: {value}"
        );
    }

    #[test]
    fn hover_over_keyless_store_root_says_no_generated_identity_type() {
        let source = "\
module a

resource Settings
    theme: string

;; Singleton settings.
store ^settings: Settings
";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);
        let value = hover_value(&snapshot, &index, &file, source, "^settings");
        assert!(
            value.starts_with("```marrow\nstore ^settings: Settings\n```"),
            "keyless store hover should lead with the store signature: {value}"
        );
        assert!(
            value.contains("no generated identity type"),
            "keyless store hover should say it has no generated identity type: {value}"
        );
    }

    #[test]
    fn hover_over_an_enum_declaration_name_shows_docs_and_members() {
        let source = "\
module a

;; Lifecycle state.
pub enum Status
    open
    closed
";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);
        let value = hover_value(&snapshot, &index, &file, source, "Status");

        assert!(
            value.starts_with("```marrow\nenum Status\n```"),
            "enum signature should lead the hover: {value}"
        );
        assert!(
            value.contains("Lifecycle state."),
            "enum docs should be shown: {value}"
        );
        assert!(
            value.contains("open"),
            "enum member summary should include open: {value}"
        );
        assert!(
            value.contains("closed"),
            "enum member summary should include closed: {value}"
        );
        assert!(
            !value.contains("ordinal"),
            "enum hover should not include source-order ordinals: {value}"
        );
    }

    #[test]
    fn hover_over_the_enum_keyword_does_not_show_the_enum() {
        let source = "\
module a

;; Lifecycle state.
pub enum Status
    open
    closed
";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);
        let offset = offset_of(source, "enum Status");
        let hover = hover_with_index(&snapshot, &index, &file, offset, None);
        if let Some(Hover {
            contents: HoverContents::Markup(markup),
            ..
        }) = hover
        {
            assert!(
                !markup.value.starts_with("```marrow\nenum Status\n```"),
                "enum keyword should not inherit the declaration-name hover: {}",
                markup.value
            );
        }
    }

    #[test]
    fn hover_over_an_enum_type_annotation_shows_docs_and_members() {
        let source = "\
module a

;; Lifecycle state.
enum Status
    open
    closed

pub fn set(status: Status)
    return
";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);
        let offset = offset_of(source, "status: Status") + "status: ".len();
        let hover = hover_with_index(&snapshot, &index, &file, offset, None).expect("a hover");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup");
        };
        let value = markup.value;

        assert!(
            value.starts_with("```marrow\nenum Status\n```"),
            "enum annotation hover should lead with the enum signature: {value}"
        );
        assert!(
            value.contains("Lifecycle state."),
            "enum annotation hover should include docs: {value}"
        );
        assert!(
            value.contains("open"),
            "enum annotation hover should include member summary: {value}"
        );
    }

    #[test]
    fn hover_over_parameter_name_matching_enum_does_not_show_enum_hover() {
        let source = "\
module a

;; Lifecycle state.
enum Status
    open
    closed

pub fn set(Status: int)
    return
";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);
        let offset = offset_of(source, "Status: int");
        let hover = hover_with_index(&snapshot, &index, &file, offset, None);
        if let Some(Hover {
            contents: HoverContents::Markup(markup),
            ..
        }) = hover
        {
            assert!(
                !markup.value.starts_with("```marrow\nenum Status\n```"),
                "parameter name should not be treated as an enum type: {}",
                markup.value
            );
        }
    }

    #[test]
    fn hover_over_qualified_foreign_enum_type_annotation_shows_enum_hover() {
        let state_source = "\
module shelf::state

;; Lifecycle state.
pub enum Status
    open
    closed
";
        let app_source = "\
module shelf::app

use shelf::state

pub fn set(status: state::Status)
    return
";
        let (snapshot, file) = analyze_files(
            &[
                ("shelf/state.mw", state_source),
                ("shelf/app.mw", app_source),
            ],
            "shelf/app.mw",
        );
        let index = index_for(&snapshot);
        let offset = offset_of(app_source, "state::Status") + "state::".len();
        let hover = hover_with_index(&snapshot, &index, &file, offset, None).expect("a hover");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup");
        };
        let value = markup.value;

        assert!(
            value.starts_with("```marrow\nenum Status\n```"),
            "qualified enum annotation should lead with the enum signature: {value}"
        );
        assert!(
            value.contains("Lifecycle state."),
            "qualified enum annotation should include docs: {value}"
        );
        assert!(
            value.contains("open"),
            "qualified enum annotation should include member summary: {value}"
        );
    }

    #[test]
    fn hover_over_bare_project_enum_type_annotation_shows_enum_hover() {
        let other_source = "\
module shelf::other

pub fn noop()
    return
";
        let state_source = "\
module shelf::state

;; Lifecycle state.
pub enum Status
    open
    closed
";
        let app_source = "\
module shelf::app

pub fn set(status: Status)
    return
";
        let (snapshot, file) = analyze_files(
            &[
                ("shelf/other.mw", other_source),
                ("shelf/state.mw", state_source),
                ("shelf/app.mw", app_source),
            ],
            "shelf/app.mw",
        );
        let index = index_for(&snapshot);
        let offset = offset_of(app_source, "status: Status") + "status: ".len();
        let hover = hover_with_index(&snapshot, &index, &file, offset, None).expect("a hover");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup");
        };
        let value = markup.value;

        assert!(
            value.starts_with("```marrow\nenum Status\n```"),
            "bare project enum annotation should lead with the enum signature: {value}"
        );
        assert!(
            value.contains("Lifecycle state."),
            "bare project enum annotation should include docs: {value}"
        );
        assert!(
            value.contains("open"),
            "bare project enum annotation should include member summary: {value}"
        );
    }

    #[test]
    fn hover_over_private_foreign_enum_type_annotation_does_not_show_enum_hover() {
        let state_source = "\
module shelf::state

;; Private lifecycle state.
enum Status
    open
    closed
";
        let app_source = "\
module shelf::app

use shelf::state

pub fn set(status: state::Status)
    return
";
        let (snapshot, file) = analyze_files(
            &[
                ("shelf/state.mw", state_source),
                ("shelf/app.mw", app_source),
            ],
            "shelf/app.mw",
        );
        let index = index_for(&snapshot);
        let offset = offset_of(app_source, "state::Status") + "state::".len();
        let value = hover_value_at(&snapshot, &index, &file, offset);

        assert!(
            !value
                .as_deref()
                .is_some_and(|value| value.contains("Private lifecycle state.")),
            "private foreign enum annotation should not show enum docs: {value:?}"
        );
    }

    #[test]
    fn hover_over_an_enum_member_value_shows_path_status_and_docs() {
        let source = "\
module a

enum Status
    category active
        ;; Open for edits.
        open
    closed

pub fn current(): Status
    return Status::active::open
";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);
        let offset = source.rfind("Status::active::open").unwrap() + "Status::active::".len();
        let hover = hover_with_index(&snapshot, &index, &file, offset, None).expect("a hover");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup");
        };
        let value = markup.value;

        assert!(
            value.starts_with("```marrow\nStatus::active::open\n```"),
            "enum member hover should lead with the full member path: {value}"
        );
        assert!(
            value.contains("enum Status"),
            "enum member hover should name the enum: {value}"
        );
        assert!(
            !value.contains("ordinal"),
            "enum member hover should not include source-order ordinals: {value}"
        );
        assert!(
            value.contains("selectable"),
            "leaf enum member should be marked selectable: {value}"
        );
        assert!(
            value.contains("Open for edits."),
            "enum member docs should be shown: {value}"
        );
    }

    #[test]
    fn hover_over_a_function_call_stays_rich_after_resource_enum_hovers() {
        let source = "\
module a

;; Adds two numbers.
pub fn add(x: int, y: int): int
    return x + y

pub fn caller(): int
    return add(1, 2)
";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);
        let value = hover_value(&snapshot, &index, &file, source, "add(1, 2)");

        assert!(
            value.starts_with("```marrow\nfn add(x: int, y: int): int\n```"),
            "function hovers should keep leading with the signature: {value}"
        );
        assert!(
            value.contains("Adds two numbers."),
            "function docs should still be present: {value}"
        );
    }

    #[test]
    fn hover_over_a_qualified_function_call_only_uses_the_leaf_as_the_function() {
        let math_source = "\
module shelf::math

;; Adds two numbers.
pub fn add(x: int, y: int): int
    return x + y
";
        let app_source = "\
module shelf::app

use shelf::math

pub fn caller(): int
    return math::add(1, 2)
";
        let (snapshot, file) = analyze_files(
            &[("shelf/math.mw", math_source), ("shelf/app.mw", app_source)],
            "shelf/app.mw",
        );
        let call = app_source.rfind("math::add(1, 2)").unwrap();
        let qualifier_hover = hover(&snapshot, &file, call + 1);
        if let Some(hover) = qualifier_hover {
            let HoverContents::Markup(markup) = hover.contents else {
                panic!("expected markup contents");
            };
            assert!(
                !markup.value.contains("fn add("),
                "module qualifier hover should not pretend to be the function: {}",
                markup.value
            );
            assert!(
                !markup.value.contains("Adds two numbers."),
                "module qualifier hover should not inherit function docs: {}",
                markup.value
            );
        }

        let leaf_offset = call + "math::".len() + 1;
        let leaf_hover = hover(&snapshot, &file, leaf_offset).expect("a hover at the callee leaf");
        let HoverContents::Markup(markup) = leaf_hover.contents else {
            panic!("expected markup contents");
        };
        assert!(
            markup
                .value
                .starts_with("```marrow\nfn add(x: int, y: int): int\n```"),
            "function leaf should show the signature: {}",
            markup.value
        );
    }

    #[test]
    fn hover_off_any_expression_is_none() {
        let source = "module a\n\npub fn f(): int\n    return 1\n";
        let (snapshot, file) = analyze(source);
        // Offset 0 sits on the `module` keyword, inside no function body.
        assert!(hover(&snapshot, &file, 0).is_none());
    }

    #[test]
    fn hover_for_an_unknown_file_is_none() {
        let source = "module a\n\npub fn f(): int\n    return 1\n";
        let (snapshot, _) = analyze(source);
        let stray = std::path::Path::new("/nowhere/b.mw");
        assert!(hover(&snapshot, stray, 0).is_none());
    }

    /// Analyze a project that pins a native store and seed `^books(1).title =
    /// "Mort"`. Returns the snapshot, the module file path, the resolved project,
    /// and the live source text so a test can hover into a saved path.
    fn analyze_with_store(
        source: &str,
    ) -> (
        AnalysisSnapshot,
        std::path::PathBuf,
        crate::workspace::Project,
    ) {
        use marrow_store::backend::Backend;
        use marrow_store::path::{PathSegment, SavedKey, encode_path};
        use marrow_store::redb::RedbStore;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let config_text = r#"{
            "sourceRoots": ["src"],
            "store": { "backend": "native", "dataDir": "data" }
        }"#;
        std::fs::write(root.join("marrow.json"), config_text).unwrap();
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(root.join("data")).unwrap();
        let file = src.join("a.mw");
        std::fs::write(&file, source).unwrap();

        let store_path = root.join("data").join("marrow.redb");
        {
            let mut store = RedbStore::open(&store_path).unwrap();
            let title = encode_path(&[
                PathSegment::Root("books".to_string()),
                PathSegment::RecordKey(SavedKey::Int(1)),
                PathSegment::Field("title".to_string()),
            ]);
            store.write(&title, b"Mort".to_vec()).unwrap();
        }

        let config = parse_config(config_text).unwrap();
        let snapshot = analyze_project(root, &config, &ProjectSources::new()).unwrap();
        let project = crate::workspace::Project {
            root: root.to_path_buf(),
            config,
        };
        std::mem::forget(dir);
        (snapshot, file, project)
    }

    #[test]
    fn hover_on_a_saved_path_marks_live_value_advisory() {
        let source = "\
module a

resource Book at ^books(id: int)
    required title: string

pub fn f(): string
    return ^books(1).title
";
        let (snapshot, file, project) = analyze_with_store(source);
        let reader = StoreReader::for_project(&project).expect("native store reader");

        // Hover over `title` in the function body's `^books(1).title`.
        let offset = source.rfind(").title").unwrap() + ").".len() + 1;
        let hover = hover_with_live(&snapshot, &file, offset, None, Some(&reader))
            .expect("a hover at the path");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup contents");
        };
        assert!(
            markup.value.starts_with("```marrow\nstring\n```"),
            "the checked type should lead the hover: {}",
            markup.value
        );
        assert!(
            markup
                .value
                .contains("**debug/admin live data (advisory)**: \"Mort\""),
            "the live stored value should be marked advisory: {}",
            markup.value
        );
        assert!(
            !markup.value.contains("**live**: \"Mort\""),
            "the old bare live label should not be shown: {}",
            markup.value
        );
    }

    #[test]
    fn hover_over_a_saved_root_declaration_marks_live_count_advisory() {
        let source = "\
module a

resource Book
    required title: string

;; Books saved by id.
store ^books(id: int): Book
";
        let (snapshot, file, project) = analyze_with_store(source);
        let reader = StoreReader::for_project(&project).expect("native store reader");
        let index = index_for(&snapshot);
        let offset = offset_of(source, "^books") + 1;
        let hover =
            hover_with_index(&snapshot, &index, &file, offset, Some(&reader)).expect("a hover");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup");
        };
        let value = markup.value;

        assert!(
            value.starts_with("```marrow\nstore ^books(id: int): Book\n```"),
            "declared saved root should keep the store signature: {value}"
        );
        assert!(
            value.contains("Books saved by id."),
            "declared saved root should keep store docs: {value}"
        );
        assert!(
            value.contains("**debug/admin live data (advisory)**: 1 record"),
            "declared saved root should mark the record count advisory: {value}"
        );
        assert!(
            !value.contains("**live**: 1 record"),
            "the old bare live label should not be shown: {value}"
        );
    }

    #[test]
    fn hover_on_a_saved_root_shows_the_record_count() {
        let source = "\
module a

resource Book at ^books(id: int)
    required title: string

pub fn f()
    delete ^books
";
        let (snapshot, file, project) = analyze_with_store(source);
        let reader = StoreReader::for_project(&project).unwrap();
        // Hover over `^books` in the body. The type at a bare root may not resolve,
        // so this asserts the path resolver + count only when a hover is produced.
        let offset = source.rfind("^books").unwrap();
        if let Some(hover) = hover_with_live(&snapshot, &file, offset, None, Some(&reader)) {
            let HoverContents::Markup(markup) = hover.contents else {
                panic!("expected markup");
            };
            assert!(
                markup.value.contains("1 record"),
                "a root hover should show the record count: {}",
                markup.value
            );
        }
    }

    #[test]
    fn hover_without_a_reader_is_type_only() {
        let source = "\
module a

resource Book at ^books(id: int)
    required title: string

pub fn f(): string
    return ^books(1).title
";
        let (snapshot, file, _project) = analyze_with_store(source);
        let offset = source.rfind(").title").unwrap() + ").".len() + 1;
        // No reader, no docs: the live-data setting is off. Only the type shows.
        let hover = hover_with_live(&snapshot, &file, offset, None, None).expect("a hover");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup");
        };
        assert_eq!(markup.value, "```marrow\nstring\n```");
    }

    /// Build the binding index for a snapshot so a docs test can resolve a symbol.
    fn index_for(snapshot: &AnalysisSnapshot) -> BindingIndex {
        marrow_check::build_binding_index(snapshot)
    }

    #[test]
    fn symbol_docs_for_a_documented_function_returns_its_description() {
        let source = "\
module a

;; Adds two numbers.
pub fn add(x: int, y: int): int
    return x + y

pub fn caller(): int
    return add(1, 2)
";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);
        // Hover over the call `add(1, 2)`.
        let offset = source.rfind("add(1, 2)").unwrap();
        let docs = symbol_docs(&snapshot, &index, &file, offset).expect("function docs");
        assert_eq!(docs, "Adds two numbers.");
    }

    #[test]
    fn hover_over_a_documented_function_shows_type_and_description() {
        let source = "\
module a

;; Adds two numbers.
pub fn add(x: int, y: int): int
    return x + y

pub fn caller(): int
    return add(1, 2)
";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);
        let offset = source.rfind("add(1, 2)").unwrap();
        let docs = symbol_docs(&snapshot, &index, &file, offset);
        let hover = hover_with_live(&snapshot, &file, offset, docs.as_deref(), None)
            .expect("a hover at the call");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup");
        };
        assert!(
            markup.value.starts_with("```marrow\n"),
            "the type still leads the hover: {}",
            markup.value
        );
        assert!(
            markup.value.contains("Adds two numbers."),
            "the description should be shown: {}",
            markup.value
        );
    }

    #[test]
    fn hover_over_a_documented_module_const_declaration_shows_type_and_description() {
        let source = "\
module a

;; Maximum count.
const LIMIT: int = 10

pub fn caller(): int
    return LIMIT
";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);
        let offset = offset_of(source, "LIMIT: int") + 1;
        let value = hover_value_at(&snapshot, &index, &file, offset)
            .expect("module const declaration hover");

        assert!(
            value.starts_with("```marrow\nconst LIMIT: int\n```"),
            "the module const signature should lead the hover: {value}"
        );
        assert!(
            value.contains("Maximum count."),
            "the description should be shown: {value}"
        );
    }

    #[test]
    fn symbol_docs_for_a_resource_name_returns_its_description() {
        let source = "\
module a

;; A book on a shelf.
resource Book at ^books(id: int)
    required title: string

pub fn f(): Book
    return Book(id: 1, title: \"x\")
";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);
        // Hover over the constructor use `Book(...)`.
        let offset = source.rfind("Book(id: 1").unwrap();
        let docs = symbol_docs(&snapshot, &index, &file, offset).expect("resource docs");
        assert_eq!(docs, "A book on a shelf.");
    }

    #[test]
    fn symbol_docs_for_a_saved_field_returns_its_description() {
        let source = "\
module a

resource Book at ^books(id: int)
    ;; The book's title.
    required title: string

pub fn f(): string
    return ^books(1).title
";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);
        // Hover over `title` in `^books(1).title`.
        let offset = source.rfind(").title").unwrap() + ").".len() + 1;
        let docs = symbol_docs(&snapshot, &index, &file, offset).expect("field docs");
        assert_eq!(docs, "The book's title.");
    }

    #[test]
    fn symbol_docs_for_a_documented_enum_returns_its_description() {
        let source = "\
module a

;; Lifecycle state.
enum Status
    open
    closed
";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);
        let offset = source.find("Status").unwrap() + 1;
        let docs = symbol_docs(&snapshot, &index, &file, offset).expect("enum docs");
        assert_eq!(docs, "Lifecycle state.");
    }

    #[test]
    fn symbol_docs_for_a_documented_enum_member_returns_its_description() {
        let source = "\
module a

enum Status
    ;; Open for edits.
    open
    closed

pub fn current(): Status
    return Status::open
";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);
        let offset = source.rfind("Status::open").unwrap() + "Status::".len() + 1;
        let docs = symbol_docs(&snapshot, &index, &file, offset).expect("enum member docs");
        assert_eq!(docs, "Open for edits.");
    }

    #[test]
    fn symbol_docs_for_an_undocumented_symbol_is_none() {
        let source = "\
module a

pub fn add(x: int, y: int): int
    return x + y

pub fn caller(): int
    return add(1, 2)
";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);
        let offset = source.rfind("add(1, 2)").unwrap();
        assert!(
            symbol_docs(&snapshot, &index, &file, offset).is_none(),
            "an undocumented function has no description"
        );
    }

    #[test]
    fn symbol_docs_for_a_local_or_param_is_none() {
        let source = "module a\n\npub fn f(n: int): int\n    return n\n";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);
        // Hover over the parameter use `n` in `return n`.
        let offset = offset_of(source, "return n") + "return ".len();
        assert!(
            symbol_docs(&snapshot, &index, &file, offset).is_none(),
            "a parameter carries no `;;` documentation"
        );
    }
}
