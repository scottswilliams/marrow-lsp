//! The schema-change-impact advisory: a capped, on-demand scan of a project's
//! saved data that flags every stored record the *current* schema can no longer
//! account for.
//!
//! The scan is bounded and on demand only. It delegates integrity semantics to
//! Marrow's canonical tooling sample.

pub use marrow_json::saved_data::DataIntegrityResultJson as DataIntegrityResult;

use crate::store::SavedDataSession;

/// The most stored entries one scan reads before reporting the rest as
/// `truncated`. The advisory is a fast, on-demand health check, not a full audit,
/// so it walks a bounded prefix of the tree rather than an unbounded store.
const INTEGRITY_SCAN_LIMIT: usize = 5000;

/// Run the schema-change-impact advisory for the LSP custom request,
/// soft-degrading a missing session or unreadable store to an `available: false`
/// envelope. The transport passes `None` when live data is off or the
/// editor-visible source is stale, so the store is never opened in those cases.
pub fn data_integrity(session: Option<&SavedDataSession>) -> DataIntegrityResult {
    let Some(session) = session else {
        return DataIntegrityResult::unavailable();
    };
    match session
        .surface_read()
        .saved_data_integrity_sample(INTEGRITY_SCAN_LIMIT)
    {
        Ok(stamped) => DataIntegrityResult::from(stamped)
            .with_data_view_boundary(session.surface_read().data_view_boundary()),
        Err(_) => DataIntegrityResult::unavailable(),
    }
}

#[cfg(test)]
mod tests {
    use marrow_json::saved_data::DataIntegrityResultJson;
    use serde_json::json;

    use super::*;

    #[test]
    fn data_integrity_unavailable_uses_shared_result_shape() {
        fn assert_marrow_json_result(_: &DataIntegrityResultJson) {}

        let result = data_integrity(None);
        assert_marrow_json_result(&result);
        assert_eq!(
            serde_json::to_value(result).unwrap(),
            json!({
                "available": false,
                "findings": [],
                "scanned": 0,
                "truncated": false,
                "store_snapshot": null,
            })
        );
    }
}
