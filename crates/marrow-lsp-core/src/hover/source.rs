use std::path::Path;

use marrow_check::{AnalysisSnapshot, BindingIndex, SymbolKind, SymbolRef};
use marrow_syntax::{
    Declaration, EnumDecl, EnumMember, FunctionDecl, IndexDecl, Keyword, ResourceDecl,
    ResourceMember, SourceSpan, StoreDecl, TokenKind, lex_source,
};

pub(super) fn is_named_symbol_reference(
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

pub(super) fn is_symbol_declaration_name(
    snapshot: &AnalysisSnapshot,
    file: &Path,
    offset: usize,
    symbol: &SymbolRef,
) -> bool {
    match symbol.kind {
        SymbolKind::Resource => is_resource_declaration_name(snapshot, file, offset, symbol),
        SymbolKind::Enum => is_enum_declaration_name(snapshot, file, offset, symbol),
        SymbolKind::EnumMember => is_enum_member_declaration_name(snapshot, file, offset, symbol),
        _ => false,
    }
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

pub(super) fn is_resource_member_hover_target(
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

pub(super) fn offset_is_on_declaration_name(
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

pub(super) fn is_function_hover_target(
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

pub(super) fn parsed_function_at(
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

pub(super) fn parsed_const_at(
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

pub(super) fn span_covers(span: SourceSpan, offset: usize) -> bool {
    span.start_byte <= offset && offset <= span.end_byte
}

pub(super) fn span_contains_span(outer: SourceSpan, inner: SourceSpan) -> bool {
    span_covers(outer, inner.start_byte) && span_covers(outer, inner.end_byte)
}

pub(super) fn declaration_docs<'a>(
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
        SymbolKind::Local | SymbolKind::Param | SymbolKind::ModuleRef => None,
    }
}

pub(super) fn resource_at(
    source: &marrow_syntax::SourceFile,
    span: SourceSpan,
) -> Option<&ResourceDecl> {
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

pub(super) fn enum_at(source: &marrow_syntax::SourceFile, span: SourceSpan) -> Option<&EnumDecl> {
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

pub(super) fn resource_member_at(
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

pub(super) fn store_root_at<'a>(
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

pub(super) fn store_index_at(
    source: &marrow_syntax::SourceFile,
    span: SourceSpan,
) -> Option<&IndexDecl> {
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

fn member_docs(members: &[ResourceMember], span: SourceSpan) -> Option<&[String]> {
    for member in members {
        match member {
            ResourceMember::Field(field) if span_contains_span(field.span, span) => {
                return Some(&field.docs);
            }
            ResourceMember::Group(group) if span_contains_span(group.span, span) => {
                return Some(&group.docs);
            }
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

pub(super) fn enum_member_path_at(
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
