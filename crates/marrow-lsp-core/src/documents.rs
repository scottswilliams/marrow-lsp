//! The set of buffers an editor has open, keyed by URL.
//!
//! Sync is full-text: open and change replace the whole buffer and rebuild its
//! [`LineIndex`]; close drops it. The store holds the unsaved text the user is
//! editing, which the workspace overlays onto disk so analysis matches the buffer.

use std::collections::HashMap;

use lsp_types::Url;
use marrow_syntax::{LexedSource, ParsedSource, lex_source, parse_source};

use crate::positions::LineIndex;

/// One open buffer: its current text, a position index, and the parse/lex of the
/// text — all rebuilt once per edit.
///
/// The cached `lexed` and `parsed` are the performance foundation for the
/// per-document requests (completion, semantic tokens, formatting): each reads
/// this snapshot of the buffer instead of re-lexing or re-parsing on every
/// request, and none of them triggers a project recompute. Both the lexer and
/// the parser are error-recovering and always succeed, so the cache is always
/// present even while the buffer has errors.
pub struct Document {
    pub text: String,
    pub version: Option<i32>,
    pub index: LineIndex,
    pub lexed: LexedSource,
    pub parsed: ParsedSource,
}

impl Document {
    /// Build a document from its text, computing the index, lex, and parse once.
    fn new(text: String, version: Option<i32>) -> Self {
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
    pub fn open(&mut self, url: Url, text: String) {
        self.open_with_version(url, text, None);
    }

    /// Record an opened buffer with the editor's document version, when the
    /// transport has one to echo back on diagnostics.
    pub fn open_with_version(&mut self, url: Url, text: String, version: Option<i32>) {
        self.documents.insert(url, Document::new(text, version));
    }

    /// Replace an open buffer's whole text (full-text sync), rebuilding its index,
    /// lex, and parse. A change for a URL that is not open is ignored.
    pub fn change(&mut self, url: &Url, text: String) {
        self.change_with_version(url, text, None);
    }

    /// Replace an open buffer and preserve the editor's current document version
    /// for the next diagnostics publish.
    pub fn change_with_version(&mut self, url: &Url, text: String, version: Option<i32>) {
        if let Some(document) = self.documents.get_mut(url) {
            *document = Document::new(text, version);
        }
    }

    /// Drop a closed buffer.
    pub fn close(&mut self, url: &Url) {
        self.documents.remove(url);
    }

    pub fn get(&self, url: &Url) -> Option<&Document> {
        self.documents.get(url)
    }

    /// The editor version for an open buffer, if the transport supplied one.
    pub fn version(&self, url: &Url) -> Option<i32> {
        self.get(url).and_then(|document| document.version)
    }

    /// The URLs of every open buffer.
    pub fn urls(&self) -> impl Iterator<Item = &Url> {
        self.documents.keys()
    }

    /// Every open buffer with its URL. Used by the workspace to overlay unsaved
    /// text onto disk; no transport reads it directly.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&Url, &Document)> {
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
    fn open_then_change_replaces_text() {
        let mut documents = Documents::new();
        documents.open(url(), "old".to_string());
        documents.change(&url(), "new text".to_string());

        let document = documents.get(&url()).unwrap();
        assert_eq!(document.text, "new text");
        // The index reflects the new text, not the old.
        assert_eq!(document.index.position(8).character, 8);
    }

    #[test]
    fn close_removes_the_buffer() {
        let mut documents = Documents::new();
        documents.open(url(), "x".to_string());
        documents.close(&url());
        assert!(documents.get(&url()).is_none());
    }

    #[test]
    fn change_to_an_unopened_url_is_ignored() {
        let mut documents = Documents::new();
        documents.change(&url(), "x".to_string());
        assert!(documents.get(&url()).is_none());
    }
}
