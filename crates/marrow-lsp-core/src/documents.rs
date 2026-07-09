//! The set of buffers an editor has open, keyed by URL.
//!
//! Sync is full-text: open and change replace the whole buffer and rebuild its
//! [`LineIndex`]; close drops it. The store holds the unsaved text the user is
//! editing, which the workspace overlays onto disk so analysis matches the buffer.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lsp_types::Url;
use marrow_syntax::{LexedSource, ParsedSource, lex_source, parse_source};

use crate::positions::LineIndex;

/// One open buffer: its current text, a position index, and the parse/lex of the
/// text — all rebuilt once per edit.
///
/// Every derived artifact is held behind an [`Arc`] so a [`DocumentSnapshot`] can
/// share them rather than deep-copy them: taking a snapshot for one request is a
/// handful of refcount bumps, not an O(file-size) clone of the text, index, lex,
/// and parse. The `index` shares the very same `Arc<str>` as `text`, so a buffer's
/// source lives in exactly one allocation instead of being stored twice.
///
/// The cached `lexed` and `parsed` are the performance foundation for the
/// per-document requests (completion, semantic tokens, formatting): each reads
/// this snapshot of the buffer instead of re-lexing or re-parsing on every
/// request, and none of them triggers a project recompute. Both the lexer and
/// the parser are error-recovering and always succeed, so the cache is always
/// present even while the buffer has errors.
pub struct Document {
    pub text: Arc<str>,
    pub version: Option<i32>,
    pub index: Arc<LineIndex>,
    pub lexed: Arc<LexedSource>,
    pub parsed: Arc<ParsedSource>,
}

/// A shared, immutable view of one [`Document`] for a single request. Cloning it
/// only bumps refcounts on the shared text, index, lex, and parse; it never copies
/// the buffer.
#[derive(Clone)]
pub struct DocumentSnapshot {
    pub text: Arc<str>,
    pub version: Option<i32>,
    pub index: Arc<LineIndex>,
    pub lexed: Arc<LexedSource>,
    pub parsed: Arc<ParsedSource>,
}

pub struct DocumentAnalysisSnapshot {
    generation: u64,
    open_documents: Vec<OpenDocumentForAnalysis>,
}

pub struct OpenDocumentForAnalysis {
    pub url: Url,
    pub path: PathBuf,
    pub text: Arc<str>,
    pub version: Option<i32>,
    pub index: Arc<LineIndex>,
}

impl Document {
    /// Build a document from its text, computing the index, lex, and parse once.
    /// The text is moved into a single `Arc<str>` that the index shares, so the
    /// buffer's source is never stored twice.
    fn new(text: String, version: Option<i32>) -> Self {
        let text: Arc<str> = Arc::from(text);
        let index = Arc::new(LineIndex::new(Arc::clone(&text)));
        let lexed = Arc::new(lex_source(&text));
        let parsed = Arc::new(parse_source(&text));
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
    generation: u64,
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
        self.generation += 1;
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
            self.generation += 1;
        }
    }

    /// Drop a closed buffer.
    pub fn close(&mut self, url: &Url) {
        if self.documents.remove(url).is_some() {
            self.generation += 1;
        }
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

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// A shared view of one open buffer for a single request. Cloning the buffer's
    /// `Arc`s is a refcount bump, so this stays O(1) in the file size no matter how
    /// many buffers are open.
    pub fn snapshot(&self, url: &Url) -> Option<DocumentSnapshot> {
        self.documents.get(url).map(|document| DocumentSnapshot {
            text: Arc::clone(&document.text),
            version: document.version,
            index: Arc::clone(&document.index),
            lexed: Arc::clone(&document.lexed),
            parsed: Arc::clone(&document.parsed),
        })
    }

    pub fn analysis_snapshot(&self) -> DocumentAnalysisSnapshot {
        let mut open_documents: Vec<OpenDocumentForAnalysis> = self
            .documents
            .iter()
            .filter_map(|(url, document)| {
                url.to_file_path().ok().map(|path| OpenDocumentForAnalysis {
                    url: url.clone(),
                    path,
                    text: Arc::clone(&document.text),
                    version: document.version,
                    index: Arc::clone(&document.index),
                })
            })
            .collect();
        open_documents.sort_by(|left, right| left.url.as_str().cmp(right.url.as_str()));
        DocumentAnalysisSnapshot {
            generation: self.generation,
            open_documents,
        }
    }

    pub fn matches_generation(&self, snapshot: &DocumentAnalysisSnapshot) -> bool {
        self.generation == snapshot.generation
    }
}

impl DocumentAnalysisSnapshot {
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn open_documents(&self) -> &[OpenDocumentForAnalysis] {
        &self.open_documents
    }

    pub fn get(&self, url: &Url) -> Option<&OpenDocumentForAnalysis> {
        self.open_documents
            .iter()
            .find(|document| document.url == *url)
    }

    pub fn line_index_for_path(&self, path: &Path) -> Option<&LineIndex> {
        self.open_documents
            .iter()
            .find(|document| document.path == path)
            .map(|document| document.index.as_ref())
    }

    pub fn text_for_path(&self, path: &Path) -> Option<&str> {
        self.open_documents
            .iter()
            .find(|document| document.path == path)
            .map(|document| document.text.as_ref())
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
        assert_eq!(document.text.as_ref(), "new text");
        assert_eq!(document.version, None);
        assert_eq!(documents.generation(), 2);
        // The index reflects the new text, not the old.
        assert_eq!(document.index.position(8).character, 8);
    }

    #[test]
    fn versioned_open_then_change_replaces_version() {
        let mut documents = Documents::new();
        documents.open_with_version(url(), "old".to_string(), Some(1));
        documents.change_with_version(&url(), "new text".to_string(), Some(2));

        let document = documents.get(&url()).unwrap();
        assert_eq!(document.text.as_ref(), "new text");
        assert_eq!(document.version, Some(2));
        assert_eq!(documents.generation(), 2);
        // The index reflects the new text, not the old.
        assert_eq!(document.index.position(8).character, 8);
    }

    #[test]
    fn opening_without_a_version_keeps_none() {
        let mut documents = Documents::new();
        documents.open(url(), "text".to_string());

        assert_eq!(documents.get(&url()).unwrap().version, None);
        assert_eq!(documents.generation(), 1);
    }

    #[test]
    fn close_removes_the_buffer() {
        let mut documents = Documents::new();
        documents.open(url(), "x".to_string());
        documents.close(&url());
        assert!(documents.get(&url()).is_none());
        assert_eq!(documents.generation(), 2);
    }

    #[test]
    fn change_to_an_unopened_url_is_ignored() {
        let mut documents = Documents::new();
        documents.change(&url(), "x".to_string());
        assert!(documents.get(&url()).is_none());
        assert_eq!(documents.generation(), 0);
    }

    #[test]
    fn analysis_snapshot_captures_open_file_documents() {
        let mut documents = Documents::new();
        documents.open_with_version(url(), "module a\n".to_string(), Some(7));
        let snapshot = documents.analysis_snapshot();

        assert_eq!(snapshot.generation(), 1);
        assert_eq!(snapshot.open_documents.len(), 1);
        assert_eq!(
            snapshot.open_documents[0].path,
            url().to_file_path().unwrap()
        );
        assert_eq!(snapshot.open_documents[0].version, Some(7));
        assert!(documents.matches_generation(&snapshot));
        assert_eq!(
            snapshot.text_for_path(&url().to_file_path().unwrap()),
            Some("module a\n")
        );
    }

    /// A snapshot of a large open buffer must be a set of refcount bumps, never a
    /// deep copy of the text, index, lex, and parse. Pointer identity between the
    /// document's `Arc`s and the snapshot's proves nothing was reallocated, so
    /// snapshot cost is O(1) in the file size rather than O(file-size × requests).
    #[test]
    fn snapshot_shares_storage_without_deep_cloning() {
        let mut documents = Documents::new();
        // A buffer large enough that a deep clone would be an obvious cost.
        let big = "pub fn f(): int\n    return 1\n".repeat(50_000);
        documents.open(url(), big);

        let document = documents.get(&url()).unwrap();
        let text_ptr = Arc::as_ptr(&document.text);
        let index_ptr = Arc::as_ptr(&document.index);
        let lexed_ptr = Arc::as_ptr(&document.lexed);
        let parsed_ptr = Arc::as_ptr(&document.parsed);

        let snapshot = documents.snapshot(&url()).unwrap();

        assert!(
            std::ptr::eq(Arc::as_ptr(&snapshot.text), text_ptr),
            "snapshot text must share the buffer allocation, not copy it"
        );
        assert!(std::ptr::eq(Arc::as_ptr(&snapshot.index), index_ptr));
        assert!(std::ptr::eq(Arc::as_ptr(&snapshot.lexed), lexed_ptr));
        assert!(std::ptr::eq(Arc::as_ptr(&snapshot.parsed), parsed_ptr));
    }

    /// The buffer's source lives in one allocation: the document text `Arc` and the
    /// index's internal text must name the same bytes, so text is not stored twice.
    #[test]
    fn document_and_its_index_share_one_text_allocation() {
        let mut documents = Documents::new();
        documents.open(
            url(),
            "module a\n\npub fn f(): int\n    return 1\n".to_string(),
        );

        let document = documents.get(&url()).unwrap();
        assert!(
            std::ptr::eq(document.text.as_ptr(), document.index.text().as_ptr()),
            "the document text and its line index must share one allocation"
        );
    }
}
