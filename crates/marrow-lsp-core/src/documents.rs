//! The set of buffers an editor has open, keyed by URL.
//!
//! Sync is full-text: open and change replace the whole buffer and rebuild its
//! [`LineIndex`]; close drops it. The store holds the unsaved text the user is
//! editing, which the workspace overlays onto disk so analysis matches the buffer.

use std::collections::HashMap;

use lsp_types::Url;

use crate::positions::LineIndex;

/// One open buffer: its current text, the editor's version counter, and a
/// position index rebuilt on every edit.
pub struct Document {
    pub text: String,
    pub version: i32,
    pub index: LineIndex,
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
        let index = LineIndex::new(text.clone());
        self.documents.insert(
            url,
            Document {
                text,
                version,
                index,
            },
        );
    }

    /// Replace an open buffer's whole text (full-text sync) and rebuild its index.
    /// A change for a URL that is not open is ignored.
    pub fn change(&mut self, url: &Url, version: i32, text: String) {
        if let Some(document) = self.documents.get_mut(url) {
            document.index = LineIndex::new(text.clone());
            document.text = text;
            document.version = version;
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
