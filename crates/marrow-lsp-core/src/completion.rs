//! Context-aware completion driven by the error-recovering lexer.
//!
//! The cursor often sits in source that does not parse — right after a `^`, a
//! `.`, or a `::` the user just typed. So this never relies on a clean AST at the
//! cursor: it classifies the context from the cached [`LexedSource`] tokens to the
//! left of the offset, then draws candidates from the last clean snapshot
//! ([`CheckedProgram`]) and the lexical scope ([`marrow_check::scope_at`]). Both
//! the lex and the snapshot are cached, so completion re-lexes nothing and
//! triggers no project recompute, and it still answers when the file has parse
//! errors elsewhere.

use std::path::Path;

use lsp_types::{CompletionItem, CompletionItemKind, Documentation, MarkupContent, MarkupKind};
use marrow_check::{
    CheckedModule, CheckedProgram, StoreLeafKind, scope_at,
    tooling::{
        DeclaredDataChild, DeclaredDataChildKind, SourceDataPathSegment,
        declared_source_data_children,
    },
};
use marrow_schema::{EnumSchema, ResourceSchema, StoreSchema, stdlib};
use marrow_syntax::{LexedSource, ParsedSource, Token, TokenKind};

use crate::{language_facts, types::render_type};

/// The Marrow keywords offered in statement/expression position. The type
/// keywords are offered separately in type position, so they are left out here.
const KEYWORDS: &[&str] = &[
    "const",
    "var",
    "if",
    "else",
    "while",
    "for",
    "in",
    "break",
    "continue",
    "return",
    "delete",
    "transaction",
    "try",
    "catch",
    "throw",
    "match",
    "true",
    "false",
    "not",
    "and",
    "or",
];

/// The built-in type names offered in type position.
const TYPE_NAMES: &[&str] = &[
    "int", "decimal", "bool", "string", "bytes", "date", "instant", "duration", "sequence",
    "unknown",
];

/// The completions for byte `offset` in `file`, classified from the cached lex
/// and drawn from the cached snapshot program.
///
/// `source` is the document's text (the lex's spans index into it); `parsed` is
/// the document's cached parse (used to type the lexical scope); `lexed` is the
/// cached lex the context is read from; `program` is the last clean snapshot's
/// checked program. The list is empty rather than absent when nothing fits, so
/// the editor shows no stale suggestions.
pub fn completion(
    program: &CheckedProgram,
    file: &Path,
    source: &str,
    parsed: &ParsedSource,
    lexed: &LexedSource,
    offset: usize,
) -> Vec<CompletionItem> {
    match classify(source, lexed, offset) {
        Context::Root => root_completions(program),
        Context::SavedPath { segments } => saved_path_completions(program, &segments),
        Context::InvalidSavedPath => Vec::new(),
        Context::Namespace { qualifier } => namespace_completions(program, file, &qualifier),
        Context::Type => type_completions(program),
        Context::Bare => bare_completions(program, file, parsed, offset),
    }
}

/// What the tokens left of the cursor say the cursor is completing.
enum Context {
    /// Just after a `^`: the durable saved roots.
    Root,
    /// A saved-path dot position recovered as source-shaped path segments.
    SavedPath {
        segments: Vec<SourceDataPathSegment>,
    },
    /// A saved-path dot position whose key-list source shape is malformed.
    InvalidSavedPath,
    /// After `qualifier::`: what the qualifier (an enum, a used module, or a
    /// `std` module path) exposes.
    Namespace { qualifier: Vec<String> },
    /// A type annotation position (after a `:` introducing a type): type names and
    /// store identity types.
    Type,
    /// A bare identifier in expression position: in-scope names, keywords, builtins.
    Bare,
}

/// Classify the completion context from the tokens at or left of `offset`.
///
/// The driving token is the last non-trivia token whose span starts before the
/// offset; the classifier reads it and, where a path needs reconstructing, the
/// run of tokens before it. This stays entirely on the lexer because the cursor
/// commonly sits in source no parser would accept (a dangling `^`, `.`, or `::`).
fn classify(source: &str, lexed: &LexedSource, offset: usize) -> Context {
    let tokens = significant_tokens(lexed);
    // The index of the last token that begins strictly before the cursor. A token
    // the cursor sits inside (an identifier prefix being typed) counts as the one
    // before its delimiter, so look one further left when the cursor is mid-word.
    let Some(driver) = tokens
        .iter()
        .rposition(|token| token.span.start_byte < offset)
    else {
        return Context::Bare;
    };

    // If the cursor is inside an identifier (typing a prefix), the meaningful
    // delimiter is the token before that identifier.
    let anchor =
        if tokens[driver].kind == TokenKind::Identifier && tokens[driver].span.end_byte >= offset {
            driver.checked_sub(1)
        } else {
            Some(driver)
        };

    let Some(anchor) = anchor else {
        return Context::Bare;
    };

    match tokens[anchor].kind {
        TokenKind::Caret => Context::Root,
        TokenKind::DoubleColon => match qualifier_before(source, &tokens, anchor) {
            Some(qualifier) => Context::Namespace { qualifier },
            None => Context::Bare,
        },
        TokenKind::Dot | TokenKind::QuestionDot => {
            match saved_path_before_dot(source, &tokens, anchor) {
                SavedPathRecovery::Complete(segments) => Context::SavedPath { segments },
                SavedPathRecovery::Invalid => Context::InvalidSavedPath,
                SavedPathRecovery::NotSavedPath => Context::Bare,
            }
        }
        TokenKind::DotDot => {
            if saved_path_attempt_before_dot(&tokens, anchor) {
                Context::InvalidSavedPath
            } else {
                Context::Bare
            }
        }
        // A `:` that introduces a type annotation (`name: <here>`, `): <here>`).
        TokenKind::Colon => {
            if introduces_type(&tokens, anchor) {
                Context::Type
            } else {
                Context::Bare
            }
        }
        _ => Context::Bare,
    }
}

/// Reconstruct the `::`-separated name path immediately before the `::` at
/// `colon_index`, e.g. the `std`, `clock` of `std::clock::`. `None` when the path
/// does not start at an identifier.
fn qualifier_before(source: &str, tokens: &[Token], colon_index: usize) -> Option<Vec<String>> {
    let mut segments = Vec::new();
    let mut i = colon_index;
    loop {
        let ident = i.checked_sub(1)?;
        if tokens[ident].kind != TokenKind::Identifier {
            return None;
        }
        segments.push(tokens[ident].text(source).to_string());
        match ident.checked_sub(1) {
            Some(prev) if tokens[prev].kind == TokenKind::DoubleColon => i = prev,
            _ => break,
        }
    }
    segments.reverse();
    Some(segments)
}

/// Reconstruct the source-shaped saved path before the `.` at `dot_index`.
fn saved_path_before_dot(source: &str, tokens: &[Token], dot_index: usize) -> SavedPathRecovery {
    let mut cursor = dot_index;
    let mut pieces = Vec::new();
    let mut invalid_key_list = false;

    loop {
        let Some(mut segment) = cursor.checked_sub(1) else {
            return unrecovered_saved_path(tokens, dot_index);
        };
        let key_slots = if tokens[segment].kind == TokenKind::RightParen {
            let close_paren = segment;
            let Some(open_paren) = matching_open_paren(tokens, close_paren) else {
                return unrecovered_saved_path(tokens, dot_index);
            };
            let Some(name) = open_paren.checked_sub(1) else {
                return unrecovered_saved_path(tokens, dot_index);
            };
            segment = name;
            match count_top_level_arguments(tokens, open_paren, close_paren) {
                Some(count) => count,
                None => {
                    invalid_key_list = true;
                    0
                }
            }
        } else {
            0
        };
        if tokens[segment].kind != TokenKind::Identifier {
            return unrecovered_saved_path(tokens, dot_index);
        }
        let Some(before) = segment.checked_sub(1) else {
            return unrecovered_saved_path(tokens, dot_index);
        };
        let name = tokens[segment].text(source).to_string();
        match tokens[before].kind {
            TokenKind::Caret => {
                if invalid_key_list {
                    return SavedPathRecovery::Invalid;
                }
                pieces.push(SavedPathPiece::Root { name, key_slots });
                return SavedPathRecovery::Complete(source_data_path_segments(pieces));
            }
            TokenKind::Dot | TokenKind::QuestionDot => {
                pieces.push(SavedPathPiece::Member { name, key_slots });
                cursor = before;
            }
            _ => return unrecovered_saved_path(tokens, dot_index),
        }
    }
}

fn unrecovered_saved_path(tokens: &[Token], dot_index: usize) -> SavedPathRecovery {
    if saved_path_attempt_before_dot(tokens, dot_index) {
        SavedPathRecovery::Invalid
    } else {
        SavedPathRecovery::NotSavedPath
    }
}

fn saved_path_attempt_before_dot(tokens: &[Token], dot_index: usize) -> bool {
    let Some(mut i) = dot_index.checked_sub(1) else {
        return false;
    };
    let mut depth = 0usize;

    loop {
        match tokens[i].kind {
            TokenKind::Caret => return true,
            TokenKind::RightParen | TokenKind::RightBracket => depth += 1,
            TokenKind::LeftParen | TokenKind::LeftBracket => {
                depth = depth.saturating_sub(1);
            }
            kind if depth == 0 && is_saved_path_attempt_boundary(kind) => return false,
            _ => {}
        }

        let Some(prev) = i.checked_sub(1) else {
            return false;
        };
        i = prev;
    }
}

fn is_saved_path_attempt_boundary(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Keyword(_)
            | TokenKind::Colon
            | TokenKind::DoubleColon
            | TokenKind::Comma
            | TokenKind::DotDot
            | TokenKind::DotDotEqual
            | TokenKind::Equal
            | TokenKind::EqualEqual
            | TokenKind::BangEqual
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
            | TokenKind::At
    )
}

enum SavedPathRecovery {
    Complete(Vec<SourceDataPathSegment>),
    Invalid,
    NotSavedPath,
}

enum SavedPathPiece {
    Root { name: String, key_slots: usize },
    Member { name: String, key_slots: usize },
}

fn source_data_path_segments(pieces: Vec<SavedPathPiece>) -> Vec<SourceDataPathSegment> {
    let mut segments = Vec::new();
    for piece in pieces.into_iter().rev() {
        match piece {
            SavedPathPiece::Root { name, key_slots } => {
                segments.push(SourceDataPathSegment::Root(name));
                append_key_slots(&mut segments, key_slots);
            }
            SavedPathPiece::Member { name, key_slots } => {
                segments.push(SourceDataPathSegment::Member(name));
                append_key_slots(&mut segments, key_slots);
            }
        }
    }
    segments
}

fn append_key_slots(segments: &mut Vec<SourceDataPathSegment>, count: usize) {
    segments.extend((0..count).map(|_| SourceDataPathSegment::KeySlot));
}

/// The index of the `(` matching the `)` at `close_index`, accounting for nested
/// parentheses. `None` if unbalanced.
fn matching_open_paren(tokens: &[Token], close_index: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = close_index;
    loop {
        match tokens[i].kind {
            TokenKind::RightParen => depth += 1,
            TokenKind::LeftParen => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i = i.checked_sub(1)?;
    }
}

fn count_top_level_arguments(
    tokens: &[Token],
    open_index: usize,
    close_index: usize,
) -> Option<usize> {
    let mut depth = 0usize;
    let mut complete_slots = 0usize;
    let mut slot_has_token = false;

    for token in &tokens[open_index + 1..close_index] {
        match token.kind {
            TokenKind::LeftParen | TokenKind::LeftBracket => {
                depth += 1;
                slot_has_token = true;
            }
            TokenKind::RightParen | TokenKind::RightBracket => {
                depth = depth.checked_sub(1)?;
                slot_has_token = true;
            }
            TokenKind::Comma if depth == 0 => {
                if !slot_has_token {
                    return None;
                }
                complete_slots += 1;
                slot_has_token = false;
            }
            _ => slot_has_token = true,
        }
    }

    if depth != 0 {
        return None;
    }
    if complete_slots == 0 && !slot_has_token {
        return Some(0);
    }
    if !slot_has_token {
        return None;
    }

    Some(complete_slots + 1)
}

/// Whether the `:` at `colon_index` introduces a type annotation rather than a
/// labeled argument or a key. A type-introducing `:` follows a parameter or
/// binding name and is itself preceded by an identifier or a `)` (a function
/// signature's `): ` return type). A labeled call argument `name:` sits inside a
/// `(...)`, which we cannot tell apart purely lexically; the common binding and
/// signature cases are what type completion targets.
fn introduces_type(tokens: &[Token], colon_index: usize) -> bool {
    let Some(before) = colon_index.checked_sub(1) else {
        return false;
    };
    matches!(
        tokens[before].kind,
        TokenKind::Identifier | TokenKind::RightParen
    )
}

/// The durable saved roots declared across every module: one item per store.
fn root_completions(program: &CheckedProgram) -> Vec<CompletionItem> {
    let mut items = Vec::new();
    for store in stores(program) {
        items.push(
            item(&store.root, CompletionItemKind::STRUCT)
                .detail(format!("saved root of {}", store.resource))
                .docs_from(&store.docs),
        );
    }
    dedup(items)
}

fn saved_path_completions(
    program: &CheckedProgram,
    segments: &[SourceDataPathSegment],
) -> Vec<CompletionItem> {
    declared_source_data_children(program, segments)
        .unwrap_or_default()
        .iter()
        .map(declared_data_child_completion)
        .collect()
}

fn declared_data_child_completion(child: &DeclaredDataChild) -> CompletionItem {
    let kind = match child.kind {
        DeclaredDataChildKind::Field { .. } => CompletionItemKind::FIELD,
        DeclaredDataChildKind::Layer => CompletionItemKind::STRUCT,
    };
    item(&child.name, kind).detail(declared_data_child_detail(child))
}

fn declared_data_child_detail(child: &DeclaredDataChild) -> String {
    let mut detail = match child.kind {
        DeclaredDataChildKind::Field { required: true } => "required field".to_string(),
        DeclaredDataChildKind::Field { required: false } => "field".to_string(),
        DeclaredDataChildKind::Layer => "layer".to_string(),
    };
    if !child.key_params.is_empty() {
        detail.push('(');
        detail.push_str(&declared_key_params_detail(child));
        detail.push(')');
    }
    if let Some(leaf) = child.leaf.as_ref().and_then(store_leaf_detail) {
        detail.push_str(": ");
        detail.push_str(&leaf);
    }
    detail
}

fn declared_key_params_detail(child: &DeclaredDataChild) -> String {
    child
        .key_params
        .iter()
        .map(|param| match param.scalar {
            Some(scalar) => format!("{}: {}", param.name, scalar.name()),
            None => param.name.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn store_leaf_detail(leaf: &StoreLeafKind) -> Option<String> {
    match leaf {
        StoreLeafKind::Scalar(scalar) => Some(scalar.name().to_string()),
        StoreLeafKind::Identity { store_root, .. } => Some(format!("Id(^{store_root})")),
        StoreLeafKind::Enum { .. } => None,
    }
}

/// What `qualifier::` exposes: an enum's members, a used module's public
/// functions and constants, or a `std` module's ops.
fn namespace_completions(
    program: &CheckedProgram,
    file: &Path,
    qualifier: &[String],
) -> Vec<CompletionItem> {
    // `std::` exposes modules; `std::<module>::` exposes that module's ops.
    if qualifier.first().map(String::as_str) == Some("std") {
        return match qualifier {
            [_] => std_root_module_completions(),
            [_, module] => std_module_completions(module),
            _ => Vec::new(),
        };
    }

    if let Some(enum_schema) = enum_for_namespace(program, file, qualifier) {
        return enum_member_completions(enum_schema);
    }

    Vec::new()
}

/// The modules exposed at `std::`, in stdlib table order.
fn std_root_module_completions() -> Vec<CompletionItem> {
    dedup(
        stdlib::all()
            .iter()
            .map(|op| item(op.module, CompletionItemKind::MODULE).detail("std module".to_string()))
            .collect(),
    )
}

/// The ops of `std::<module>::`, each with its rendered signature as detail.
fn std_module_completions(module: &str) -> Vec<CompletionItem> {
    stdlib::all()
        .iter()
        .filter(|op| op.module == module)
        .map(|op| item(op.op, CompletionItemKind::FUNCTION).detail(std_signature(op)))
        .collect()
}

/// The members of an enum, in declaration order.
fn enum_member_completions(enum_schema: &EnumSchema) -> Vec<CompletionItem> {
    enum_schema
        .members
        .iter()
        .map(|member| {
            item(&member.name, CompletionItemKind::ENUM_MEMBER)
                .detail(enum_schema.name.clone())
                .docs_from(&member.docs)
        })
        .collect()
}

/// Type-position completions: the built-in type names plus every resource name,
/// keyed store identity, and enum name.
fn type_completions(program: &CheckedProgram) -> Vec<CompletionItem> {
    let mut items: Vec<CompletionItem> = TYPE_NAMES
        .iter()
        .map(|name| item(name, CompletionItemKind::KEYWORD).detail("type".to_string()))
        .collect();
    for resource in resources(program) {
        items.push(item(&resource.name, CompletionItemKind::STRUCT).detail("resource".to_string()));
    }
    for store in stores(program).filter(|store| !store.identity_keys.is_empty()) {
        items.push(
            item(&format!("Id(^{})", store.root), CompletionItemKind::CLASS)
                .detail(format!("identity of ^{}", store.root)),
        );
    }
    for enum_schema in enums(program) {
        items.push(
            item(&enum_schema.name, CompletionItemKind::ENUM)
                .detail("enum".to_string())
                .docs_from(&enum_schema.docs),
        );
    }
    dedup(items)
}

/// Bare-identifier completions: the in-scope bindings (typed by the checker),
/// then the keywords and builtins.
fn bare_completions(
    program: &CheckedProgram,
    file: &Path,
    parsed: &ParsedSource,
    offset: usize,
) -> Vec<CompletionItem> {
    let mut items = Vec::new();
    for (name, ty) in scope_at(program, file, parsed, offset) {
        items.push(item(&name, CompletionItemKind::VARIABLE).detail(render_type(&ty)));
    }
    for keyword in KEYWORDS {
        items.push(item(keyword, CompletionItemKind::KEYWORD));
    }
    for builtin in language_facts::bare_function_builtins() {
        items.push(
            item(builtin.name, CompletionItemKind::FUNCTION).detail(builtin.detail.to_string()),
        );
    }
    for name in language_facts::scalar_conversion_names() {
        if let Some(detail) = language_facts::scalar_conversion_detail(name) {
            items.push(item(name, CompletionItemKind::FUNCTION).detail(detail));
        }
    }
    items
}

/// Every resource schema declared across all modules of the program.
fn resources(program: &CheckedProgram) -> impl Iterator<Item = &ResourceSchema> {
    program
        .modules
        .iter()
        .flat_map(|module| module.resources.iter())
}

/// Every store schema declared across all modules of the program.
fn stores(program: &CheckedProgram) -> impl Iterator<Item = &StoreSchema> {
    program
        .modules
        .iter()
        .flat_map(|module| module.stores.iter())
}

/// Every enum schema declared across all modules of the program.
fn enums(program: &CheckedProgram) -> impl Iterator<Item = &EnumSchema> {
    program
        .modules
        .iter()
        .flat_map(|module| module.enums.iter())
}

fn enum_for_namespace<'a>(
    program: &'a CheckedProgram,
    file: &Path,
    qualifier: &[String],
) -> Option<&'a EnumSchema> {
    let (name, module_prefix) = qualifier.split_last()?;
    if module_prefix.is_empty() {
        return enum_visible_by_bare_name(program, file, name);
    }

    None
}

fn enum_visible_by_bare_name<'a>(
    program: &'a CheckedProgram,
    file: &Path,
    name: &str,
) -> Option<&'a EnumSchema> {
    current_module(program, file).and_then(|module| enum_in_module(module, name))
}

fn enum_in_module<'a>(module: &'a CheckedModule, name: &str) -> Option<&'a EnumSchema> {
    module
        .enums
        .iter()
        .find(|enum_schema| enum_schema.name == name)
}

fn current_module<'a>(program: &'a CheckedProgram, file: &Path) -> Option<&'a CheckedModule> {
    program
        .modules
        .iter()
        .find(|module| module.source_file == file)
}

/// A one-line rendering of a std op's signature, e.g. `(string, string): bool`.
fn std_signature(op: &stdlib::StdOp) -> String {
    let params = op
        .params
        .iter()
        .map(param_type_name)
        .collect::<Vec<_>>()
        .join(", ");
    match return_type_name(&op.ret) {
        Some(ret) => format!("({params}): {ret}"),
        None => format!("({params})"),
    }
}

fn param_type_name(param: &stdlib::ParamType) -> String {
    match param {
        stdlib::ParamType::Scalar(scalar) => scalar.name().to_string(),
        stdlib::ParamType::ScalarAny => "scalar".to_string(),
        stdlib::ParamType::Sequence(scalar) => format!("sequence[{}]", scalar.name()),
        stdlib::ParamType::Error => "Error".to_string(),
        stdlib::ParamType::Path => "path".to_string(),
    }
}

fn return_type_name(ret: &stdlib::ReturnType) -> Option<String> {
    match ret {
        stdlib::ReturnType::Scalar(scalar) => Some(scalar.name().to_string()),
        stdlib::ReturnType::Sequence(scalar) => Some(format!("sequence[{}]", scalar.name())),
        stdlib::ReturnType::Void => None,
    }
}

/// The significant tokens of a lex: everything but layout trivia and the EOF, in
/// source order. Comments are dropped too; they never bound a completion context.
fn significant_tokens(lexed: &LexedSource) -> Vec<Token> {
    lexed
        .tokens
        .iter()
        .filter(|token| {
            !matches!(
                token.kind,
                TokenKind::Indent
                    | TokenKind::Dedent
                    | TokenKind::Newline
                    | TokenKind::Eof
                    | TokenKind::Comment
                    | TokenKind::DocComment
            )
        })
        .copied()
        .collect()
}

/// A small builder over [`CompletionItem`] so each site reads as one expression.
trait ItemExt {
    fn detail(self, detail: String) -> Self;
    fn docs_from(self, docs: &[String]) -> Self;
}

impl ItemExt for CompletionItem {
    fn detail(mut self, detail: String) -> Self {
        self.detail = Some(detail);
        self
    }
    fn docs_from(mut self, docs: &[String]) -> Self {
        if !docs.is_empty() {
            self.documentation = Some(Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value: docs.join("\n"),
            }));
        }
        self
    }
}

/// A base completion item with a label and kind.
fn item(label: &str, kind: CompletionItemKind) -> CompletionItem {
    CompletionItem {
        label: label.to_string(),
        kind: Some(kind),
        ..CompletionItem::default()
    }
}

/// Drop items with a duplicate label, keeping the first. Saved roots and type
/// names can repeat across modules; the editor wants each once.
fn dedup(items: Vec<CompletionItem>) -> Vec<CompletionItem> {
    let mut seen = std::collections::HashSet::new();
    items
        .into_iter()
        .filter(|item| seen.insert(item.label.clone()))
        .collect()
}

#[cfg(test)]
mod tests;
