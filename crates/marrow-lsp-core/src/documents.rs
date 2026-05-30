//! The set of buffers an editor has open, keyed by URL.
//!
//! Sync is full-text: open and change replace the whole buffer and rebuild its
//! [`LineIndex`]; close drops it. The store holds the unsaved text the user is
//! editing, which the workspace overlays onto disk so analysis matches the buffer.

use std::collections::HashMap;

use lsp_types::Url;
use marrow_syntax::{LexedSource, ParsedSource, lex_source, parse_source};

use crate::positions::LineIndex;

/// One open buffer: its current text, the editor's version counter, a position
/// index, and the parse/lex of the text — all rebuilt once per edit.
///
/// The cached `lexed` and `parsed` are the performance foundation for the
/// per-document requests (completion, semantic tokens, formatting): each reads
/// this snapshot of the buffer instead of re-lexing or re-parsing on every
/// request, and none of them triggers a project recompute. Both the lexer and
/// the parser are error-recovering and always succeed, so the cache is always
/// present even while the buffer has errors.
pub struct Document {
    pub text: String,
    pub version: i32,
    pub index: LineIndex,
    pub lexed: LexedSource,
    pub parsed: ParsedSource,
}

impl Document {
    /// Build a document from its text, computing the index, lex, and parse once.
    fn new(version: i32, text: String) -> Self {
        let index = LineIndex::new(text.clone());
        let lexed = lex_source(&text);
        let parsed = parse_source(&text);
        Self {
            text,
            version,
            index,
            lexed,
            parsed,
        }
    }
}

/// The open buffers, keyed by URL.
#[derive(Default)]
pub struct Documents {
    documents: HashMap<Url, Document>,
}

impl Documents {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a newly opened buffer, replacing any buffer already at `url`.
    pub fn open(&mut self, url: Url, version: i32, text: String) {
        self.documents.insert(url, Document::new(version, text));
    }

    /// Replace an open buffer's whole text (full-text sync), rebuilding its index,
    /// lex, and parse. A change for a URL that is not open is ignored.
    pub fn change(&mut self, url: &Url, version: i32, text: String) {
        if let Some(document) = self.documents.get_mut(url) {
            *document = Document::new(version, text);
        }
    }

    /// Drop a closed buffer.
    pub fn close(&mut self, url: &Url) {
        self.documents.remove(url);
    }

    pub fn get(&self, url: &Url) -> Option<&Document> {
        self.documents.get(url)
    }

    /// The URLs of every open buffer.
    pub fn urls(&self) -> impl Iterator<Item = &Url> {
        self.documents.keys()
    }

    /// Every open buffer with its URL.
    pub fn iter(&self) -> impl Iterator<Item = (&Url, &Document)> {
        self.documents.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url() -> Url {
        Url::parse("file:///project/src/a.mw").unwrap()
    }

    #[test]
    fn open_then_change_replaces_text_and_version() {
        let mut documents = Documents::new();
        documents.open(url(), 1, "old".to_string());
        documents.change(&url(), 2, "new text".to_string());

        let document = documents.get(&url()).unwrap();
        assert_eq!(document.text, "new text");
        assert_eq!(document.version, 2);
        // The index reflects the new text, not the old.
        assert_eq!(document.index.position(8).character, 8);
    }

    #[test]
    fn close_removes_the_buffer() {
        let mut documents = Documents::new();
        documents.open(url(), 1, "x".to_string());
        documents.close(&url());
        assert!(documents.get(&url()).is_none());
    }

    #[test]
    fn change_to_an_unopened_url_is_ignored() {
        let mut documents = Documents::new();
        documents.change(&url(), 1, "x".to_string());
        assert!(documents.get(&url()).is_none());
    }
}
