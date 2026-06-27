use std::path::Path;

use marrow_check::{
    AnalysisSnapshot, BindingIndex, CheckedFacts, MarrowType,
    tooling::{
        SavedPlaceHoverFact, SourceCallableHoverFact, SourceModulePathHoverFact,
        SourceOperatorHoverFact, SourceSchemaHoverFact, StoreRootHoverFact,
        saved_place_hover_fact_at, source_callable_hover_fact_at, source_module_path_hover_fact_at,
        source_operator_hover_fact_at, source_schema_hover_fact_at, source_symbol_docs_at,
        store_root_hover_fact_at,
    },
    type_at,
};

pub(super) enum HoverFact<'a> {
    SourceCallable {
        fact: SourceCallableHoverFact,
        checked_facts: &'a CheckedFacts,
    },
    SourceModulePath(SourceModulePathHoverFact),
    SourceOperator(SourceOperatorHoverFact),
    StoreRoot(StoreRootHoverFact),
    SourceSchema(SourceSchemaHoverFact),
    SavedPlace(SavedPlaceHoverFact),
    Type {
        ty: MarrowType,
        docs: Option<Vec<String>>,
    },
}

pub(super) fn collect<'a>(
    snapshot: &'a AnalysisSnapshot,
    index: &'a BindingIndex,
    file: &Path,
    offset: usize,
) -> Option<HoverFact<'a>> {
    if let Some(fact) = source_callable_hover_fact_at(snapshot, index, file, offset) {
        return Some(HoverFact::SourceCallable {
            fact,
            checked_facts: &snapshot.program.facts,
        });
    }
    if let Some(fact) = source_module_path_hover_fact_at(snapshot, index, file, offset) {
        return Some(HoverFact::SourceModulePath(fact));
    }
    if let Some(fact) = store_root_hover_fact_at(snapshot, file, offset) {
        return Some(HoverFact::StoreRoot(fact));
    }
    if let Some(fact) = source_schema_hover_fact_at(snapshot, index, file, offset) {
        return Some(HoverFact::SourceSchema(fact));
    }
    if let Some(fact) = saved_place_hover_fact_at(snapshot, index, file, offset) {
        return Some(HoverFact::SavedPlace(fact));
    }
    if let Some(fact) = source_operator_hover_fact_at(snapshot, file, offset) {
        return Some(HoverFact::SourceOperator(fact));
    }

    let docs = source_symbol_docs_at(snapshot, index, file, offset).map(|docs| docs.lines);
    checked_type_at(snapshot, file, offset).map(|ty| HoverFact::Type { ty, docs })
}

pub(super) fn checked_type_at(
    snapshot: &AnalysisSnapshot,
    file: &Path,
    offset: usize,
) -> Option<MarrowType> {
    let analyzed = snapshot.files.iter().find(|f| f.path == file)?;
    type_at(&snapshot.program, file, &analyzed.parsed, offset)
}
