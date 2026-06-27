use std::path::Path;

use lsp_types::{Location, Url};
use marrow_check::AnalysisSnapshot;
use marrow_check::tooling::{
    SourceCatalogLocationFact, source_catalog_definition_fact_at, source_catalog_reference_facts_at,
};

use super::indices::FileIndex;

pub(super) fn definition(
    snapshot: &AnalysisSnapshot,
    indices: &impl FileIndex,
    file: &Path,
    offset: usize,
) -> Option<Location> {
    let fact = source_catalog_definition_fact_at(snapshot, file, offset)?;
    location(&fact, indices)
}

pub(super) fn references(
    snapshot: &AnalysisSnapshot,
    indices: &impl FileIndex,
    file: &Path,
    offset: usize,
    include_declaration: bool,
) -> Option<Vec<Location>> {
    let facts = source_catalog_reference_facts_at(snapshot, file, offset, include_declaration)?;
    Some(
        facts
            .iter()
            .filter_map(|fact| location(fact, indices))
            .collect(),
    )
}

fn location(fact: &SourceCatalogLocationFact, indices: &impl FileIndex) -> Option<Location> {
    let line_index = indices.index_for(&fact.file)?;
    let url = Url::from_file_path(&fact.file).ok()?;
    Some(Location {
        uri: url,
        range: line_index.range(fact.span.start_byte, fact.span.end_byte),
    })
}
