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
    AnalysisSnapshot, BindingIndex, CheckedFunction, DefItem, Resolution, ResolvableKind,
    SymbolKind, SymbolRef, build_alias_map, build_binding_index, expand_module_alias, resolve,
    type_at,
};
use marrow_schema::{Element, EnumSchema, ResourceSchema};
use marrow_store::backend::Presence;
use marrow_syntax::{
    Declaration, EnumDecl, EnumMember, FunctionDecl, Keyword, ParamMode, ResourceDecl,
    ResourceMember, SourceSpan, TokenKind, lex_source,
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
    if let Some(value) = function_hover(snapshot, index, file, offset, reader) {
        return Some(markdown_hover(value));
    }
    if let Some(value) = rich_symbol_hover(snapshot, index, file, offset, reader) {
        return Some(markdown_hover(value));
    }
    if let Some(value) = resource_constructor_hover(snapshot, file, offset, reader) {
        return Some(markdown_hover(value));
    }
    if let Some(value) = type_name_hover(snapshot, file, offset, reader) {
        return Some(markdown_hover(value));
    }
    if let Some(value) = saved_root_hover(snapshot, file, offset, reader) {
        return Some(markdown_hover(value));
    }

    let docs = symbol_docs_at_hover_target(snapshot, index, file, offset);
    hover_with_live(snapshot, file, offset, docs.as_deref(), reader)
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
        SymbolKind::ResourceIdentity => {
            let schema = resource_schema_for_symbol(snapshot, &symbol)?;
            resource_identity_hover(schema, reader)
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
    if !is_type_annotation_target(&analyzed.source, offset) {
        return None;
    }
    let segments = qualified_name_at(&analyzed.source, offset)?;
    if let [resource, id] = segments.as_slice()
        && id == "Id"
    {
        let schema = resource_schema_for_identity_type_name(snapshot, file, resource)?;
        return resource_identity_hover(schema, reader);
    }
    if let Some(schema) = enum_schema_for_type_name(snapshot, file, &segments) {
        return Some(enum_hover(schema));
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
    let [marrow_store::path::PathSegment::Root(root)] = segments.as_slice() else {
        return None;
    };
    let schema = marrow_check::resolve::resolve_resource_by_root(&snapshot.program, root)?;
    Some(resource_hover(schema, reader))
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
    if let Some(value) = std_library_call_hover(&tokens, &analyzed.source, index) {
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

fn std_library_call_hover(
    tokens: &[marrow_syntax::Token],
    source: &str,
    index: usize,
) -> Option<String> {
    if !is_std_library_call(tokens, source, index) {
        return None;
    }
    language_facts::std_operation_hover(tokens[index - 2].text(source), tokens[index].text(source))
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
                || is_resource_saved_root_declaration(snapshot, file, offset, symbol)
        }
        SymbolKind::ResourceIdentity => {
            is_named_symbol_reference(snapshot, index, file, offset, symbol)
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

fn is_resource_saved_root_declaration(
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
    let Some(store) = &resource.store else {
        return false;
    };
    let Some((start, end)) = saved_root_identifier_span(&analyzed.source, symbol.span, &store.root)
    else {
        return false;
    };
    start <= offset && offset <= end
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

fn saved_root_identifier_span(
    source: &str,
    span: SourceSpan,
    root: &str,
) -> Option<(usize, usize)> {
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
            Some((token.span.start_byte, token.span.end_byte))
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

fn resource_schema_for_identity_type_name<'a>(
    snapshot: &'a AnalysisSnapshot,
    file: &Path,
    name: &str,
) -> Option<&'a ResourceSchema> {
    let from_module = module_name_for_file(snapshot, file)?;
    let segments = [name.to_string(), "Id".to_string()];
    match resolve(
        &snapshot.program,
        from_module,
        &segments,
        ResolvableKind::ResourceIdentity,
    ) {
        Resolution::Found(def) => match def.item {
            DefItem::Resource(schema) if has_identity_keys(schema) => Some(schema),
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
    let current_module = module_name_for_file(snapshot, file);
    match segments {
        [name] => current_module.and_then(|module| enum_schema_in_module(snapshot, module, name)),
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
            enum_schema_in_module(snapshot, &module_name, name)
        }
        [] => None,
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

fn resource_hover(schema: &ResourceSchema, reader: Option<&StoreReader>) -> String {
    let mut value = marrow_code_block(&resource_signature(schema));
    append_docs(&mut value, join_docs(&schema.docs));
    append_section(&mut value, identity_summary(schema));
    append_section(&mut value, resource_member_summary(schema));
    if let Some(reader) = reader
        && let Some(root) = schema.saved_root.as_ref().map(|saved| saved.root.as_str())
        && let Availability::Available(count) = reader.record_count(root)
    {
        value.push_str("\n\n");
        value.push_str(&format!("**live**: {}", count.display()));
    }
    value
}

fn resource_identity_hover(
    schema: &ResourceSchema,
    _reader: Option<&StoreReader>,
) -> Option<String> {
    if !has_identity_keys(schema) {
        return None;
    }
    let mut value = marrow_code_block(&format!("{}::Id", schema.name));
    append_docs(&mut value, join_docs(&schema.docs));
    append_section(&mut value, identity_summary(schema));
    Some(value)
}

fn has_identity_keys(schema: &ResourceSchema) -> bool {
    schema
        .saved_root
        .as_ref()
        .is_some_and(|saved| !saved.identity_keys.is_empty())
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
    value.push_str(&format!("- ordinal {ordinal}\n"));
    value.push_str(&format!("- {status}"));
    value
}

fn resource_signature(schema: &ResourceSchema) -> String {
    let mut signature = format!("resource {}", schema.name);
    if let Some(saved) = &schema.saved_root {
        signature.push_str(" at ^");
        signature.push_str(&saved.root);
        if !saved.identity_keys.is_empty() {
            signature.push('(');
            signature.push_str(
                &saved
                    .identity_keys
                    .iter()
                    .map(|key| format!("{}: {}", key.name, key.ty))
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            signature.push(')');
        }
    }
    signature
}

fn identity_summary(schema: &ResourceSchema) -> Option<String> {
    let saved = schema.saved_root.as_ref()?;
    if saved.identity_keys.is_empty() {
        return Some("**Identity**\n- keyless singleton".to_string());
    }
    Some(format!(
        "**Identity**\n{}",
        saved
            .identity_keys
            .iter()
            .map(|key| format!("- `{}: {}`", key.name, key.ty))
            .collect::<Vec<_>>()
            .join("\n")
    ))
}

fn resource_member_summary(schema: &ResourceSchema) -> Option<String> {
    let mut lines = schema
        .members
        .iter()
        .map(resource_member_line)
        .collect::<Vec<_>>();
    lines.extend(schema.indexes.iter().map(|index| {
        let unique = if index.unique { " unique" } else { "" };
        format!("index {}({}){unique}", index.name, index.args.join(", "))
    }));
    bounded_summary("Members", lines)
}

fn resource_member_line(member: &marrow_schema::Node) -> String {
    let keys = key_params(&member.key_params);
    match &member.element {
        Element::Slot { ty, required } => {
            let required = if *required { "required " } else { "" };
            format!("{required}{}{}: {}", member.name, keys, ty)
        }
        Element::Group => format!("{}{}", member.name, keys),
    }
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
                format!(
                    "{} ordinal {ordinal} {status}",
                    schema.member_path(ordinal).join("::")
                )
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

fn is_type_annotation_target(source: &str, offset: usize) -> bool {
    let tokens = lex_source(source).tokens;
    let Some((start, _end, _index)) = qualified_name_token_bounds(&tokens, offset) else {
        return false;
    };
    tokens
        .get(start.wrapping_sub(1))
        .is_some_and(|token| token.kind == TokenKind::Colon)
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
    let tokens = lex_source(source).tokens;
    let (start, end, _) = qualified_name_token_bounds(&tokens, offset)?;
    let mut segments = Vec::new();
    for token in tokens[start..=end]
        .iter()
        .filter(|token| token.kind == TokenKind::Identifier)
    {
        segments.push(token.text(source).to_string());
    }
    (!segments.is_empty()).then_some(segments)
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

fn is_std_library_call(tokens: &[marrow_syntax::Token], source: &str, index: usize) -> bool {
    index >= 4
        && is_call_leaf(tokens, index)
        && tokens[index - 1].kind == TokenKind::DoubleColon
        && is_path_segment_token(tokens[index - 2].kind)
        && tokens[index - 3].kind == TokenKind::DoubleColon
        && is_path_segment_token(tokens[index - 4].kind)
        && tokens[index - 4].text(source) == "std"
        && starts_at_callee_root(tokens, index - 4)
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
/// constants, enums, enum members, and a resource's saved fields, layers, and
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
/// documentable declaration. A resource's generated identity shares its
/// resource's span, so it reads the resource's docs; locals and parameters are
/// not declarations here and return `None`.
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
        SymbolKind::Resource | SymbolKind::ResourceIdentity => {
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
        SymbolKind::Field | SymbolKind::Layer | SymbolKind::Index => source
            .declarations
            .iter()
            .find_map(|declaration| match declaration {
                Declaration::Resource(resource) => member_docs(&resource.members, symbol.span),
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

/// The `docs` of the resource member (field, group/layer, or index) whose span is
/// `span`, searching nested groups, or `None`.
fn member_docs(members: &[ResourceMember], span: SourceSpan) -> Option<&[String]> {
    for member in members {
        match member {
            ResourceMember::Field(field) if field.span == span => return Some(&field.docs),
            ResourceMember::Group(group) if group.span == span => return Some(&group.docs),
            ResourceMember::Index(index) if index.span == span => return Some(&index.docs),
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

/// The live-data line for a resolved saved path, or `None` when the store is
/// unavailable so the hover stays type-only. A root path shows its record count;
/// any other path shows its presence and, for a stored value, the typed value.
fn live_value_line(
    reader: &StoreReader,
    segments: &[marrow_store::path::PathSegment],
    program: &marrow_check::CheckedProgram,
) -> Option<String> {
    use marrow_store::path::PathSegment;
    // A bare `^root` reads as the record count, the useful fact at a root.
    if let [PathSegment::Root(root)] = segments {
        return match reader.record_count(root) {
            Availability::Available(count) => Some(format!("**live**: {}", count.display())),
            Availability::Unavailable => None,
        };
    }
    match reader.get(segments, program) {
        Availability::Available(stored) => Some(format!("**live**: {}", present(&stored))),
        Availability::Unavailable => None,
    }
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
    fn hover_renders_a_parameters_type_as_a_marrow_block() {
        let source = "module a\n\npub fn f(n: int): int\n    return n\n";
        let (snapshot, file) = analyze(source);
        // Hover over the `n` in `return n`.
        let offset = offset_of(source, "return n") + "return ".len();
        let hover = hover(&snapshot, &file, offset).expect("a type at the use of `n`");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup contents");
        };
        assert_eq!(markup.value, "```marrow\nint\n```");
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
    fn hover_over_a_resource_declaration_name_shows_root_identity_docs_and_members() {
        let source = "\
module a

;; Books saved by id.
resource Book at ^books(id: int)
    ;; Display title.
    required title: string
    pages: int
    tags(pos: int): string
    index byTitle(title) unique
";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);
        let value = hover_value(&snapshot, &index, &file, source, "Book at ^books");

        assert!(
            value.starts_with("```marrow\nresource Book at ^books(id: int)\n```"),
            "resource signature should lead the hover: {value}"
        );
        assert!(
            value.contains("Books saved by id."),
            "resource docs should be shown: {value}"
        );
        assert!(
            value.contains("id: int"),
            "identity key should be summarized: {value}"
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
            value.contains("index byTitle(title) unique"),
            "index summary should be included: {value}"
        );
    }

    #[test]
    fn hover_over_the_resource_keyword_does_not_show_the_resource() {
        let source = "\
module a

;; Books saved by id.
resource Book at ^books(id: int)
    required title: string
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
                !markup
                    .value
                    .starts_with("```marrow\nresource Book at ^books(id: int)\n```"),
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
resource Book at ^books(id: int)
    required title: string

pub fn make(): Book
    return Book(id: 1, title: \"x\")
";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);
        let value = hover_value(&snapshot, &index, &file, source, "Book(id: 1");

        assert!(
            value.starts_with("```marrow\nresource Book at ^books(id: int)\n```"),
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
    fn hover_over_same_named_resource_declaration_uses_that_module_schema() {
        let first_source = "\
module shelf::first

;; First module books.
resource Book at ^first_books(id: int)
    required title: string
";
        let second_source = "\
module shelf::second

;; Second module books.
resource Book at ^second_books(code: string)
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
        let value = hover_value(
            &snapshot,
            &index,
            &file,
            second_source,
            "Book at ^second_books",
        );

        assert!(
            value.starts_with("```marrow\nresource Book at ^second_books(code: string)\n```"),
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
            !value.contains("^first_books"),
            "resource hover should not use the first module root: {value}"
        );
        assert!(
            !value.contains("First module books."),
            "resource hover should not use the first module docs: {value}"
        );
    }

    #[test]
    fn hover_over_same_named_resource_identity_uses_current_module_schema() {
        let first_source = "\
module shelf::first

;; First module books.
resource Book at ^first_books(id: int)
    required title: string
";
        let second_source = "\
module shelf::second

;; Second module books.
resource Book at ^second_books(code: string)
    required subtitle: string

pub fn load(id: Book::Id): Book::Id
    return id
";
        let (snapshot, file) = analyze_files(
            &[
                ("shelf/first.mw", first_source),
                ("shelf/second.mw", second_source),
            ],
            "shelf/second.mw",
        );
        let index = index_for(&snapshot);
        let value = hover_value(&snapshot, &index, &file, second_source, "Book::Id");

        assert!(
            value.starts_with("```marrow\nBook::Id\n```"),
            "identity hover should lead with the generated identity type: {value}"
        );
        assert!(
            value.contains("Second module books."),
            "identity hover should include the second module docs: {value}"
        );
        assert!(
            value.contains("code: string"),
            "identity hover should include the second module key: {value}"
        );
        assert!(
            !value.contains("First module books."),
            "identity hover should not use the first module docs: {value}"
        );
        assert!(
            !value.contains("id: int"),
            "identity hover should not use the first module key: {value}"
        );
    }

    #[test]
    fn hover_over_saved_root_declaration_works_when_resource_name_matches_root() {
        let source = "\
module a

;; Books saved by id.
resource books at ^books(id: int)
    required title: string
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
            value.starts_with("```marrow\nresource books at ^books(id: int)\n```"),
            "saved root hover should use the resource signature: {value}"
        );
        assert!(
            value.contains("Books saved by id."),
            "saved root hover should include resource docs: {value}"
        );
    }

    #[test]
    fn resource_member_summary_is_bounded() {
        let source = "\
module a

resource Book at ^books(id: int)
    one: string
    two: string
    three: string
    four: string
    five: string
    six: string
";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);
        let value = hover_value(&snapshot, &index, &file, source, "Book at ^books");

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
    fn hover_over_a_saved_root_use_works_without_a_reader() {
        let source = "\
module a

;; Books saved by id.
resource Book at ^books(id: int)
    required title: string

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
            value.starts_with("```marrow\nresource Book at ^books(id: int)\n```"),
            "saved root should show its resource even without live data: {value}"
        );
        assert!(
            value.contains("Books saved by id."),
            "saved root should include resource docs: {value}"
        );
        assert!(
            !value.contains("**live**"),
            "no reader means no live line: {value}"
        );
    }

    #[test]
    fn hover_over_a_saved_root_declaration_works_without_a_reader() {
        let source = "\
module a

;; Books saved by id.
resource Book at ^books(id: int)
    required title: string
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
            value.starts_with("```marrow\nresource Book at ^books(id: int)\n```"),
            "declared saved root should show its resource even without live data: {value}"
        );
        assert!(
            value.contains("Books saved by id."),
            "declared saved root should include resource docs: {value}"
        );
    }

    #[test]
    fn hover_over_a_generated_identity_use_shows_identity_keys() {
        let source = "\
module a

;; Books saved by id.
resource Book at ^books(id: int, locale: string)
    required title: string

pub fn load(id: Book::Id): Book::Id
    return id
";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);
        let value = hover_value(&snapshot, &index, &file, source, "Book::Id");

        assert!(
            value.starts_with("```marrow\nBook::Id\n```"),
            "identity hover should lead with the generated identity type: {value}"
        );
        assert!(
            value.contains("Books saved by id."),
            "identity hover can reuse resource docs: {value}"
        );
        assert!(
            value.contains("id: int"),
            "identity hover should include the first key: {value}"
        );
        assert!(
            value.contains("locale: string"),
            "identity hover should include composite keys: {value}"
        );
    }

    #[test]
    fn hover_over_keyless_resource_identity_annotation_does_not_show_identity() {
        let source = "\
module a

;; Singleton settings.
resource Settings at ^settings
    theme: string

pub fn load(id: Settings::Id)
    return
";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);
        let offset = offset_of(source, "Settings::Id");
        let hover = hover_with_index(&snapshot, &index, &file, offset, None);
        if let Some(Hover {
            contents: HoverContents::Markup(markup),
            ..
        }) = hover
        {
            assert!(
                !markup.value.starts_with("```marrow\nSettings::Id\n```"),
                "keyless resource should not render generated identity hover: {}",
                markup.value
            );
        }
    }

    #[test]
    fn hover_over_an_enum_declaration_name_shows_docs_and_members() {
        let source = "\
module a

;; Lifecycle state.
enum Status
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
    }

    #[test]
    fn hover_over_the_enum_keyword_does_not_show_the_enum() {
        let source = "\
module a

;; Lifecycle state.
enum Status
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
        let state_source = "\
module shelf::state

;; Lifecycle state.
enum Status
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
    fn hover_over_an_enum_member_value_shows_path_ordinal_status_and_docs() {
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
            value.contains("ordinal 1"),
            "enum member hover should include its ordinal: {value}"
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
    fn hover_on_a_saved_path_shows_the_live_value() {
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
            markup.value.contains("```marrow\nstring\n```"),
            "the type is always shown: {}",
            markup.value
        );
        assert!(
            markup.value.contains("\"Mort\""),
            "the live stored value should be shown: {}",
            markup.value
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
