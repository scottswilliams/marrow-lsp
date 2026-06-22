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
    CheckedProgram, StoreLeafKind, scope_at,
    tooling::{
        DeclaredDataChild, DeclaredDataChildKind, SourceEnumNamespaceCompletionFact,
        SourceModuleNamespaceCompletionFact, SourceNamespaceCompletionFact,
        SourceNamespaceFunctionCompletion, SourceSavedRootCompletionCandidate,
        SourceTypeCompletionCandidate, declared_source_receiver_data_children,
        intrinsic_completion_callables, source_namespace_completion_fact,
        source_saved_root_completion_fact, source_type_completion_fact,
    },
};
use marrow_schema::stdlib;
use marrow_syntax::{LexedSource, ParsedSource, SourceFile, SourceSpan, Token, TokenKind};

use crate::{callables::render_callable_signature, types::render_type};

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
        Context::SavedPath { receiver, span } => {
            saved_path_completions(program, file, parsed, &receiver, span)
        }
        Context::InvalidSavedPath => Vec::new(),
        Context::Namespace { qualifier } => {
            namespace_completions(program, file, &parsed.file, &qualifier)
        }
        Context::Type => type_completions(program, file, &parsed.file),
        Context::Bare => bare_completions(program, file, parsed, offset),
    }
}

/// What the tokens left of the cursor say the cursor is completing.
enum Context {
    /// Just after a `^`: the durable saved roots.
    Root,
    /// A saved-path dot position recovered as the receiver source before the dot.
    SavedPath { receiver: String, span: SourceSpan },
    /// A saved-path dot position whose source receiver shape is malformed.
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
                SavedPathRecovery::Complete { receiver, span } => {
                    Context::SavedPath { receiver, span }
                }
                SavedPathRecovery::Invalid => Context::InvalidSavedPath,
                SavedPathRecovery::NotSavedPath => Context::Bare,
            }
        }
        TokenKind::DotDot | TokenKind::DotDotEqual => {
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
        _ if malformed_saved_member_receiver(&tokens, anchor) => Context::InvalidSavedPath,
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

/// Recover the immediate saved-data receiver before the `.` at `dot_index`.
fn saved_path_before_dot(source: &str, tokens: &[Token], dot_index: usize) -> SavedPathRecovery {
    let Some(end) = dot_index.checked_sub(1) else {
        return unrecovered_saved_path(tokens, dot_index);
    };
    if matches!(tokens[end].kind, TokenKind::DotDot | TokenKind::DotDotEqual) {
        return unrecovered_saved_path(tokens, dot_index);
    }
    let Some(start) = saved_receiver_start(tokens, end) else {
        return unrecovered_saved_path(tokens, dot_index);
    };
    let span = recovered_receiver_span(tokens, start, end);
    let receiver = source[span.start_byte..span.end_byte].to_string();
    SavedPathRecovery::Complete { receiver, span }
}

fn saved_receiver_start(tokens: &[Token], end: usize) -> Option<usize> {
    match tokens[end].kind {
        TokenKind::Identifier => saved_receiver_identifier_start(tokens, end),
        TokenKind::RightParen => {
            let open = matching_open_paren(tokens, end)?;
            if let Some(callee) = open.checked_sub(1)
                && tokens[callee].kind == TokenKind::Identifier
            {
                return saved_receiver_identifier_start(tokens, callee);
            }
            saved_receiver_group_start(tokens, open, end)
        }
        _ => None,
    }
}

fn saved_receiver_group_start(tokens: &[Token], open: usize, close: usize) -> Option<usize> {
    let inner_end = close.checked_sub(1)?;
    if inner_end <= open {
        return None;
    }
    (saved_receiver_start(tokens, inner_end)? == open + 1).then_some(open)
}

fn saved_receiver_identifier_start(tokens: &[Token], ident: usize) -> Option<usize> {
    let before = ident.checked_sub(1)?;
    match tokens[before].kind {
        TokenKind::Caret => Some(before),
        TokenKind::Dot | TokenKind::QuestionDot => {
            saved_receiver_start(tokens, before.checked_sub(1)?)
        }
        _ => None,
    }
}

fn recovered_receiver_span(tokens: &[Token], start: usize, end: usize) -> SourceSpan {
    let start_span = tokens[start].span;
    SourceSpan {
        start_byte: start_span.start_byte,
        end_byte: tokens[end].span.end_byte,
        line: start_span.line,
        column: start_span.column,
    }
}

fn unrecovered_saved_path(tokens: &[Token], dot_index: usize) -> SavedPathRecovery {
    if malformed_saved_member_receiver_before_dot(tokens, dot_index)
        || saved_path_attempt_before_dot(tokens, dot_index)
        || open_saved_postfix_before_dot(tokens, dot_index)
    {
        SavedPathRecovery::Invalid
    } else {
        SavedPathRecovery::NotSavedPath
    }
}

fn malformed_saved_member_receiver_before_dot(tokens: &[Token], dot_index: usize) -> bool {
    dot_index
        .checked_sub(1)
        .is_some_and(|receiver| malformed_saved_member_receiver(tokens, receiver))
}

fn malformed_saved_member_receiver(tokens: &[Token], receiver: usize) -> bool {
    let Some(delimiter) = receiver.checked_sub(1) else {
        return false;
    };
    if !matches!(
        tokens[delimiter].kind,
        TokenKind::Dot | TokenKind::QuestionDot | TokenKind::DotDot | TokenKind::DotDotEqual
    ) {
        return false;
    }
    delimiter
        .checked_sub(1)
        .is_some_and(|end| saved_path_attempt_ending_at(tokens, end))
}

fn saved_path_attempt_before_dot(tokens: &[Token], dot_index: usize) -> bool {
    let Some(mut end) = dot_index.checked_sub(1) else {
        return false;
    };
    if matches!(tokens[end].kind, TokenKind::DotDot | TokenKind::DotDotEqual) {
        let Some(prev) = end.checked_sub(1) else {
            return false;
        };
        end = prev;
    }

    saved_path_attempt_ending_at(tokens, end)
}

fn saved_path_attempt_ending_at(tokens: &[Token], mut i: usize) -> bool {
    loop {
        match tokens[i].kind {
            TokenKind::Caret => return true,
            TokenKind::Identifier => return saved_path_callee_prefix_before(tokens, i),
            TokenKind::RightParen => match matching_open_paren(tokens, i) {
                Some(open) => {
                    if let Some(callee) = open.checked_sub(1)
                        && tokens[callee].kind == TokenKind::Identifier
                    {
                        return saved_path_callee_prefix_before(tokens, callee);
                    }
                    if saved_receiver_group_start(tokens, open, i).is_some() {
                        return true;
                    }
                    let Some(prev) = open.checked_sub(1) else {
                        return false;
                    };
                    i = prev;
                }
                None => {
                    let Some(prev) = i.checked_sub(1) else {
                        return false;
                    };
                    i = prev;
                }
            },
            TokenKind::RightBracket => match matching_open_bracket(tokens, i) {
                Some(open) => {
                    let Some(prev) = open.checked_sub(1) else {
                        return false;
                    };
                    return saved_path_attempt_ending_at(tokens, prev);
                }
                None => {
                    let Some(prev) = i.checked_sub(1) else {
                        return false;
                    };
                    i = prev;
                }
            },
            TokenKind::Dot | TokenKind::QuestionDot => {
                let Some(prev) = i.checked_sub(1) else {
                    return false;
                };
                i = prev;
            }
            _ => return false,
        }
    }
}

fn open_saved_postfix_before_dot(tokens: &[Token], dot_index: usize) -> bool {
    let Some(mut i) = dot_index.checked_sub(1) else {
        return false;
    };
    let dot_line = tokens[dot_index].span.line;
    let mut paren_depth = 0usize;
    let mut bracket_depth = 0usize;
    loop {
        if tokens[i].span.line < dot_line {
            return false;
        }
        match tokens[i].kind {
            TokenKind::RightParen => paren_depth += 1,
            TokenKind::LeftParen if paren_depth > 0 => paren_depth -= 1,
            TokenKind::LeftParen
                if bracket_depth == 0 && open_call_has_completed_saved_receiver(tokens, i) =>
            {
                return true;
            }
            TokenKind::RightBracket => bracket_depth += 1,
            TokenKind::LeftBracket if bracket_depth > 0 => bracket_depth -= 1,
            TokenKind::LeftBracket
                if paren_depth == 0 && open_bracket_has_saved_receiver(tokens, i) =>
            {
                return true;
            }
            _ => {}
        }
        let Some(prev) = i.checked_sub(1) else {
            return false;
        };
        i = prev;
    }
}

fn open_call_has_completed_saved_receiver(tokens: &[Token], open: usize) -> bool {
    let Some(receiver) = open.checked_sub(1) else {
        return false;
    };
    matches!(
        tokens[receiver].kind,
        TokenKind::RightParen | TokenKind::RightBracket
    ) && saved_path_attempt_ending_at(tokens, receiver)
}

fn open_bracket_has_saved_receiver(tokens: &[Token], open: usize) -> bool {
    open.checked_sub(1)
        .is_some_and(|receiver| saved_path_attempt_ending_at(tokens, receiver))
}

fn saved_path_callee_prefix_before(tokens: &[Token], callee: usize) -> bool {
    let Some(before) = callee.checked_sub(1) else {
        return false;
    };
    match tokens[before].kind {
        TokenKind::Caret => true,
        TokenKind::Dot | TokenKind::QuestionDot => before
            .checked_sub(1)
            .is_some_and(|end| saved_path_attempt_ending_at(tokens, end)),
        _ => false,
    }
}

enum SavedPathRecovery {
    Complete { receiver: String, span: SourceSpan },
    Invalid,
    NotSavedPath,
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

fn matching_open_bracket(tokens: &[Token], close_index: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = close_index;
    loop {
        match tokens[i].kind {
            TokenKind::RightBracket => depth += 1,
            TokenKind::LeftBracket => {
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
    source_saved_root_completion_fact(program)
        .candidates
        .iter()
        .map(saved_root_completion_item)
        .collect()
}

fn saved_root_completion_item(candidate: &SourceSavedRootCompletionCandidate) -> CompletionItem {
    item(&candidate.root, CompletionItemKind::STRUCT)
        .detail(format!("saved root of {}", candidate.resource_name))
        .docs_from(&candidate.docs)
}

fn saved_path_completions(
    program: &CheckedProgram,
    file: &Path,
    parsed: &ParsedSource,
    receiver: &str,
    span: SourceSpan,
) -> Vec<CompletionItem> {
    declared_source_receiver_data_children(program, file, parsed, receiver, span)
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

/// What `qualifier::` exposes: an enum's members, a project module's resources,
/// visible enums and functions, or a `std` module's ops.
fn namespace_completions(
    program: &CheckedProgram,
    file: &Path,
    source_file: &marrow_syntax::SourceFile,
    qualifier: &[String],
) -> Vec<CompletionItem> {
    // `std::` exposes modules; known `std::<module>::` paths expose std ops.
    // Unknown `std::<name>::` paths may still be project modules.
    if qualifier.first().map(String::as_str) == Some("std") {
        match qualifier {
            [_] => return std_root_module_completions(),
            [_, module] if std_module_exists(module) => return std_module_completions(module),
            _ => {}
        }
    }

    match source_namespace_completion_fact(program, file, source_file, qualifier) {
        Some(SourceNamespaceCompletionFact::Module(fact)) => module_namespace_completions(&fact),
        Some(SourceNamespaceCompletionFact::Enum(fact)) => enum_member_completions(&fact),
        None => Vec::new(),
    }
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

fn std_module_exists(module: &str) -> bool {
    stdlib::all().iter().any(|op| op.module == module)
}

fn module_namespace_completions(fact: &SourceModuleNamespaceCompletionFact) -> Vec<CompletionItem> {
    let mut items = Vec::new();
    items.extend(fact.resources.iter().map(|resource| {
        item(&resource.name, CompletionItemKind::STRUCT)
            .detail("resource".to_string())
            .docs_from(&resource.docs)
    }));
    items.extend(fact.enums.iter().map(|enum_schema| {
        item(&enum_schema.name, CompletionItemKind::ENUM)
            .detail("enum".to_string())
            .docs_from(&enum_schema.docs)
    }));
    items.extend(fact.functions.iter().map(|function| {
        item(&function.name, CompletionItemKind::FUNCTION)
            .detail(source_function_signature(function))
            .docs_from(&function.docs)
    }));
    dedup(items)
}

/// The members of an enum, in declaration order.
fn enum_member_completions(fact: &SourceEnumNamespaceCompletionFact) -> Vec<CompletionItem> {
    fact.members
        .iter()
        .map(|member| {
            item(&member.name, CompletionItemKind::ENUM_MEMBER)
                .detail(fact.enum_name.clone())
                .docs_from(&member.docs)
        })
        .collect()
}

fn source_function_signature(function: &SourceNamespaceFunctionCompletion) -> String {
    let params = function
        .params
        .iter()
        .map(|param| format!("{}: {}", param.name, render_type(&param.ty)))
        .collect::<Vec<_>>()
        .join(", ");
    match &function.return_type {
        Some(return_type) => format!(
            "fn {}({params}): {}",
            function.name,
            render_type(return_type)
        ),
        None => format!("fn {}({params})", function.name),
    }
}

/// Type-position completions: the built-in type names plus every resource name,
/// keyed store identity, and enum name.
fn type_completions(
    program: &CheckedProgram,
    file: &Path,
    source_file: &SourceFile,
) -> Vec<CompletionItem> {
    source_type_completion_fact(program, file, source_file)
        .candidates
        .iter()
        .map(type_completion_item)
        .collect()
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
    for callable in intrinsic_completion_callables() {
        let label = callable.path.join("::");
        items.push(
            item(&label, CompletionItemKind::FUNCTION)
                .detail(render_callable_signature(&callable))
                .docs_from(&callable.docs),
        );
    }
    items
}

fn type_completion_item(candidate: &SourceTypeCompletionCandidate) -> CompletionItem {
    match candidate {
        SourceTypeCompletionCandidate::Builtin { spelling } => {
            item(spelling.spelling(), CompletionItemKind::KEYWORD).detail("type".to_string())
        }
        SourceTypeCompletionCandidate::Resource { path, docs, .. } => {
            item(&source_type_path_label(path), CompletionItemKind::STRUCT)
                .detail("resource".to_string())
                .docs_from(docs)
        }
        SourceTypeCompletionCandidate::StoreIdentity { root, docs } => {
            item(&format!("Id(^{root})"), CompletionItemKind::CLASS)
                .detail(format!("identity of ^{root}"))
                .docs_from(docs)
        }
        SourceTypeCompletionCandidate::Enum { path, docs, .. } => {
            item(&source_type_path_label(path), CompletionItemKind::ENUM)
                .detail("enum".to_string())
                .docs_from(docs)
        }
    }
}

fn source_type_path_label(path: &[String]) -> String {
    path.join("::")
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

fn dedup(items: Vec<CompletionItem>) -> Vec<CompletionItem> {
    let mut seen = std::collections::HashSet::new();
    items
        .into_iter()
        .filter(|item| seen.insert(item.label.clone()))
        .collect()
}

#[cfg(test)]
mod tests;
