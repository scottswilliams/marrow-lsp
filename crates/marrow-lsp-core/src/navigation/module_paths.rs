use std::path::Path;

use lsp_types::{Location, Url};
use marrow_check::{BindingIndex, DefItem, Resolution, ResolvableKind, SymbolKind, resolve};
use marrow_syntax::{Keyword, SourceSpan, Token, TokenKind, lex_source};

use super::{
    indices::FileIndex,
    source_names::{name_in_span, span_covers},
};
use crate::{language_facts, type_context::type_annotation_at};

pub(super) enum ModulePathDefinition {
    Location(Location),
    NoDefinition,
}

pub(super) fn definition(
    snapshot: &marrow_check::AnalysisSnapshot,
    index: &BindingIndex,
    indices: &impl FileIndex,
    file: &Path,
    offset: usize,
) -> Option<ModulePathDefinition> {
    let analyzed = snapshot
        .files
        .iter()
        .find(|analyzed| analyzed.path == file)?;
    let in_type_annotation = type_annotation_at(&analyzed.parsed.file, offset);
    let path = qualified_path_at(&analyzed.source, offset, in_type_annotation)?;
    let function_location = if path.declaration.is_none() && !in_type_annotation {
        function_path_definition(snapshot, indices, file, &path)
    } else {
        None
    };
    if path.declaration.is_none()
        && let Some(symbol) = index.definition(file, offset)
        && path.binding_index_should_win(symbol.kind)
    {
        return None;
    }
    match path.declaration {
        Some(ModulePathDeclaration::Module | ModulePathDeclaration::Use) => {
            Some(ModulePathDefinition::NoDefinition)
        }
        None => {
            if path.cursor_segment + 1 == path.segments.len() {
                if let Some(location) = function_location {
                    return Some(ModulePathDefinition::Location(location));
                }
                return None;
            }
            Some(ModulePathDefinition::NoDefinition)
        }
    }
}

pub(super) fn saved_root_syntax_at(
    snapshot: &marrow_check::AnalysisSnapshot,
    file: &Path,
    offset: usize,
) -> bool {
    let Some(analyzed) = snapshot.files.iter().find(|analyzed| analyzed.path == file) else {
        return false;
    };
    let tokens = lex_source(&analyzed.source).tokens;
    tokens.windows(2).any(|pair| {
        let [previous, token] = pair else {
            return false;
        };
        previous.kind == TokenKind::Caret
            && token.kind == TokenKind::Identifier
            && previous.span.start_byte <= offset
            && offset <= token.span.end_byte
    })
}

fn function_path_definition(
    snapshot: &marrow_check::AnalysisSnapshot,
    indices: &impl FileIndex,
    file: &Path,
    path: &QualifiedPath,
) -> Option<Location> {
    let current = snapshot
        .program
        .modules
        .iter()
        .find(|module| module.source_file == file)?;
    let Resolution::Found(definition) = resolve(
        &snapshot.program,
        &current.name,
        &path.segments,
        ResolvableKind::Function,
    ) else {
        return None;
    };
    let DefItem::Function(function) = definition.item else {
        return None;
    };
    function_location(
        snapshot,
        indices,
        &definition.module.source_file,
        function.span,
        &function.name,
    )
}

struct QualifiedPath {
    segments: Vec<String>,
    cursor_segment: usize,
    declaration: Option<ModulePathDeclaration>,
}

impl QualifiedPath {
    fn binding_index_should_win(&self, kind: SymbolKind) -> bool {
        matches!(kind, SymbolKind::Enum | SymbolKind::EnumMember)
    }
}

#[derive(Clone, Copy)]
enum ModulePathDeclaration {
    Module,
    Use,
}

fn qualified_path_at(
    source: &str,
    offset: usize,
    in_type_annotation: bool,
) -> Option<QualifiedPath> {
    let tokens = lex_source(source).tokens;
    let cursor = tokens
        .iter()
        .position(|token| is_possible_path_segment(token) && span_covers(token.span, offset))?;

    let mut first = cursor;
    while first >= 2
        && tokens[first - 1].kind == TokenKind::DoubleColon
        && is_possible_path_segment(&tokens[first - 2])
    {
        first -= 2;
    }

    let mut last = cursor;
    while last + 2 < tokens.len()
        && tokens[last + 1].kind == TokenKind::DoubleColon
        && is_possible_path_segment(&tokens[last + 2])
    {
        last += 2;
    }

    let mut segment_tokens = Vec::new();
    let mut index = first;
    loop {
        if !is_possible_path_segment(&tokens[index]) {
            return None;
        }
        segment_tokens.push(index);
        if index == last {
            break;
        }
        if index + 2 > last || tokens[index + 1].kind != TokenKind::DoubleColon {
            return None;
        }
        index += 2;
    }

    let cursor_segment = segment_tokens
        .iter()
        .position(|segment| *segment == cursor)?;
    let declaration = path_declaration(&tokens, first);
    if segment_tokens
        .iter()
        .any(|segment| !is_path_segment(&tokens[*segment], declaration, in_type_annotation))
    {
        return None;
    }
    let segments = segment_tokens
        .iter()
        .map(|segment| tokens[*segment].text(source).to_string())
        .collect();
    Some(QualifiedPath {
        segments,
        cursor_segment,
        declaration,
    })
}

fn is_possible_path_segment(token: &Token) -> bool {
    matches!(token.kind, TokenKind::Identifier | TokenKind::Keyword(_))
}

fn is_path_segment(
    token: &Token,
    declaration: Option<ModulePathDeclaration>,
    in_type_annotation: bool,
) -> bool {
    match token.kind {
        TokenKind::Identifier => true,
        TokenKind::Keyword(keyword) => {
            declaration.is_some()
                || in_type_annotation
                || language_facts::keyword_is_callable_path_segment(keyword)
        }
        _ => false,
    }
}

fn path_declaration(tokens: &[Token], first_segment: usize) -> Option<ModulePathDeclaration> {
    match tokens.get(first_segment.checked_sub(1)?)?.kind {
        TokenKind::Keyword(Keyword::Module) => Some(ModulePathDeclaration::Module),
        TokenKind::Keyword(Keyword::Use) => Some(ModulePathDeclaration::Use),
        _ => None,
    }
}

fn function_location(
    snapshot: &marrow_check::AnalysisSnapshot,
    indices: &impl FileIndex,
    file: &Path,
    span: SourceSpan,
    function_name: &str,
) -> Option<Location> {
    let analyzed = snapshot
        .files
        .iter()
        .find(|analyzed| analyzed.path == file)?;
    let (start, end) = name_in_span(
        &analyzed.source,
        span.start_byte,
        span.end_byte,
        function_name,
    )?;
    let line_index = indices.index_for(file)?;
    let url = Url::from_file_path(file).ok()?;
    Some(Location {
        uri: url,
        range: line_index.range(start, end),
    })
}
