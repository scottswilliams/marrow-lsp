use std::path::Path;

use marrow_check::{
    AnalysisSnapshot, BindingIndex, CheckedFacts,
    tooling::{
        SavedPlaceHoverFact, SourceCallableHoverFact, SourceModulePathHoverFact,
        SourceOperatorHoverFact, SourceSchemaHoverFact, SourceTypeHoverFact, StoreRootHoverFact,
        saved_place_hover_fact_at, source_callable_hover_fact_at, source_module_path_hover_fact_at,
        source_operator_hover_fact_at, source_schema_hover_fact_at, source_type_hover_fact_at,
        store_root_hover_fact_at,
    },
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
    Type(SourceTypeHoverFact),
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

    source_type_hover_fact_at(snapshot, index, file, offset).map(HoverFact::Type)
}
