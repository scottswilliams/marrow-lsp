//! Context-aware completion driven by the error-recovering lexer.
//!
//! The cursor often sits in source that does not parse — right after a `^`, a
//! `.`, or a `::` the user just typed. So this never relies on a clean AST at the
//! cursor: it asks Marrow's tooling API for the context recovered from the cached
//! [`LexedSource`] tokens to the left of the offset, then draws candidates from
//! the last clean snapshot ([`CheckedProgram`]) and the lexical scope
//! ([`marrow_check::scope_at`]). Both the lex and the snapshot are cached, so
//! completion re-lexes nothing and triggers no project recompute, and it still
//! answers when the file has parse errors elsewhere.

use std::path::Path;

use lsp_types::{CompletionItem, CompletionItemKind, Documentation, MarkupContent, MarkupKind};
use marrow_check::{
    CheckedProgram, StoreLeafKind, scope_at,
    tooling::{
        DeclaredDataChild, DeclaredDataChildKind, SourceCompletionContext,
        SourceEnumNamespaceCompletionFact, SourceModuleNamespaceCompletionFact,
        SourceNamespaceCompletionFact, SourceNamespaceFunctionCompletion,
        SourceSavedRootCompletionCandidate, SourceStandardLibraryModuleNamespaceCompletionFact,
        SourceStandardLibraryRootNamespaceCompletionFact, SourceTypeCompletionCandidate,
        declared_source_receiver_data_children, intrinsic_completion_callables,
        source_completion_context, source_namespace_completion_fact,
        source_saved_root_completion_fact, source_type_completion_fact,
    },
};
use marrow_syntax::{LexedSource, ParsedSource, SourceFile, SourceSpan};

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
    match source_completion_context(source, lexed, offset) {
        SourceCompletionContext::Root => root_completions(program),
        SourceCompletionContext::SavedPath { receiver, span } => {
            saved_path_completions(program, file, parsed, &receiver, span)
        }
        SourceCompletionContext::InvalidSavedPath => Vec::new(),
        SourceCompletionContext::Namespace { qualifier } => {
            namespace_completions(program, file, &parsed.file, &qualifier)
        }
        SourceCompletionContext::Type => type_completions(program, file, &parsed.file),
        SourceCompletionContext::Bare => bare_completions(program, file, parsed, offset),
    }
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
    match source_namespace_completion_fact(program, file, source_file, qualifier) {
        Some(SourceNamespaceCompletionFact::Module(fact)) => module_namespace_completions(&fact),
        Some(SourceNamespaceCompletionFact::Enum(fact)) => enum_member_completions(&fact),
        Some(SourceNamespaceCompletionFact::StandardLibraryRoot(fact)) => {
            standard_library_root_completions(&fact)
        }
        Some(SourceNamespaceCompletionFact::StandardLibraryModule(fact)) => {
            standard_library_module_completions(&fact)
        }
        None => Vec::new(),
    }
}

fn standard_library_root_completions(
    fact: &SourceStandardLibraryRootNamespaceCompletionFact,
) -> Vec<CompletionItem> {
    fact.modules
        .iter()
        .map(|module| {
            item(&module.name, CompletionItemKind::MODULE).detail("std module".to_string())
        })
        .collect()
}

fn standard_library_module_completions(
    fact: &SourceStandardLibraryModuleNamespaceCompletionFact,
) -> Vec<CompletionItem> {
    fact.operations
        .iter()
        .map(|operation| {
            item(&operation.name, CompletionItemKind::FUNCTION)
                .detail(render_callable_signature(&operation.signature))
                .docs_from(&operation.signature.docs)
        })
        .collect()
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
