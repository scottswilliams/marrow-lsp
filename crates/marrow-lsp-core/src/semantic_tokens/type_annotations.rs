use std::collections::HashMap;
use std::path::Path;

use marrow_check::AnalysisSnapshot;

use super::{ByteSpan, TYPE_STRUCT, TokenStyle};

pub(super) fn type_annotation_overrides(
    analysis: Option<(&AnalysisSnapshot, &Path)>,
) -> HashMap<ByteSpan, TokenStyle> {
    analysis
        .map(|(snapshot, path)| {
            marrow_check::tooling::identity_type_annotations(snapshot, path)
                .into_iter()
                .map(|fact| {
                    (
                        (
                            fact.constructor_span.start_byte,
                            fact.constructor_span.end_byte,
                        ),
                        TokenStyle::plain(TYPE_STRUCT),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}
