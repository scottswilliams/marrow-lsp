use std::path::Path;

use lsp_types::{CompletionItem, CompletionItemKind, Documentation, MarkupContent, MarkupKind};
use marrow_check::{
    CheckedProgram,
    tooling::{SourceCompletionItem, SourceCompletionItemKind, source_completion_fact},
};
use marrow_syntax::{LexedSource, ParsedSource};

pub fn completion(
    program: &CheckedProgram,
    file: &Path,
    source: &str,
    parsed: &ParsedSource,
    lexed: &LexedSource,
    offset: usize,
) -> Vec<CompletionItem> {
    source_completion_fact(program, file, source, parsed, lexed, offset)
        .items
        .into_iter()
        .map(completion_item)
        .collect()
}

fn completion_item(item: SourceCompletionItem) -> CompletionItem {
    CompletionItem {
        label: item.label,
        kind: Some(completion_item_kind(item.kind)),
        detail: item.detail,
        documentation: completion_documentation(item.docs),
        ..CompletionItem::default()
    }
}

fn completion_documentation(docs: Vec<String>) -> Option<Documentation> {
    (!docs.is_empty()).then(|| {
        Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: docs.join("\n"),
        })
    })
}

fn completion_item_kind(kind: SourceCompletionItemKind) -> CompletionItemKind {
    match kind {
        SourceCompletionItemKind::Keyword => CompletionItemKind::KEYWORD,
        SourceCompletionItemKind::Local => CompletionItemKind::VARIABLE,
        SourceCompletionItemKind::Function => CompletionItemKind::FUNCTION,
        SourceCompletionItemKind::Resource => CompletionItemKind::STRUCT,
        SourceCompletionItemKind::SavedRoot => CompletionItemKind::STRUCT,
        SourceCompletionItemKind::Field => CompletionItemKind::FIELD,
        SourceCompletionItemKind::Layer => CompletionItemKind::STRUCT,
        SourceCompletionItemKind::Enum => CompletionItemKind::ENUM,
        SourceCompletionItemKind::EnumMember => CompletionItemKind::ENUM_MEMBER,
        SourceCompletionItemKind::Module => CompletionItemKind::MODULE,
        SourceCompletionItemKind::StoreIdentity => CompletionItemKind::CLASS,
    }
}

#[cfg(test)]
mod tests;
