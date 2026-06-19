use std::path::Path;

use lsp_types::{Location, Url};
use marrow_check::{AnalysisSnapshot, CatalogDeclaration, CatalogEntryKind, UseSite, UseSiteKind};
use marrow_syntax::SourceSpan;

use super::{indices::FileIndex, source_names::span_covers};

pub(super) fn definition(
    snapshot: &AnalysisSnapshot,
    indices: &impl FileIndex,
    file: &Path,
    offset: usize,
) -> Option<Location> {
    let target = catalog_target_at(snapshot, file, offset)?;
    let declaration = supported_declaration(snapshot.catalog_declaration(target.catalog_id)?)?;
    location(&declaration.file, declaration.span, indices)
}

pub(super) fn references(
    snapshot: &AnalysisSnapshot,
    indices: &impl FileIndex,
    file: &Path,
    offset: usize,
    include_declaration: bool,
) -> Option<Vec<Location>> {
    let target = catalog_target_at(snapshot, file, offset)?;
    let declaration = supported_declaration(snapshot.catalog_declaration(target.catalog_id)?)?;

    let mut locations = Vec::new();
    if include_declaration
        && let Some(location) = location(&declaration.file, declaration.span, indices)
    {
        push_unique_location(&mut locations, location);
    }
    for location in snapshot
        .use_sites()
        .iter()
        .filter(|site| site.catalog_id == target.catalog_id)
        .filter(|site| supported_use_site(site).is_some())
        .filter_map(|site| location(&site.file, site.span, indices))
    {
        push_unique_location(&mut locations, location);
    }
    Some(locations)
}

fn catalog_target_at<'a>(
    snapshot: &'a AnalysisSnapshot,
    file: &Path,
    offset: usize,
) -> Option<CatalogTarget<'a>> {
    use_site_at(snapshot, file, offset).or_else(|| declaration_at(snapshot, file, offset))
}

fn use_site_at<'a>(
    snapshot: &'a AnalysisSnapshot,
    file: &Path,
    offset: usize,
) -> Option<CatalogTarget<'a>> {
    snapshot
        .use_sites()
        .iter()
        .filter_map(supported_use_site)
        .filter(|site| site.file == file && span_covers(site.span, offset))
        .map(|site| CatalogTarget {
            catalog_id: site.catalog_id.as_str(),
            span: site.span,
        })
        .min_by_key(|target| span_len(target.span))
}

fn declaration_at<'a>(
    snapshot: &'a AnalysisSnapshot,
    file: &Path,
    offset: usize,
) -> Option<CatalogTarget<'a>> {
    snapshot
        .catalog_declarations()
        .iter()
        .filter_map(supported_declaration)
        .filter(|declaration| declaration.file == file && span_covers(declaration.span, offset))
        .map(|declaration| CatalogTarget {
            catalog_id: declaration.catalog_id.as_str(),
            span: declaration.span,
        })
        .min_by_key(|target| span_len(target.span))
}

fn supported_use_site(site: &UseSite) -> Option<&UseSite> {
    matches!(
        site.kind,
        UseSiteKind::SavedRoot | UseSiteKind::ResourceMember | UseSiteKind::StoreIndex
    )
    .then_some(site)
}

fn supported_declaration(declaration: &CatalogDeclaration) -> Option<&CatalogDeclaration> {
    matches!(
        declaration.kind,
        CatalogEntryKind::Store | CatalogEntryKind::ResourceMember | CatalogEntryKind::StoreIndex
    )
    .then_some(declaration)
}

fn location(file: &Path, span: SourceSpan, indices: &impl FileIndex) -> Option<Location> {
    let line_index = indices.index_for(file)?;
    let url = Url::from_file_path(file).ok()?;
    Some(Location {
        uri: url,
        range: line_index.range(span.start_byte, span.end_byte),
    })
}

fn span_len(span: SourceSpan) -> usize {
    span.end_byte.saturating_sub(span.start_byte)
}

fn push_unique_location(locations: &mut Vec<Location>, location: Location) {
    if !locations.iter().any(|existing| existing == &location) {
        locations.push(location);
    }
}

struct CatalogTarget<'a> {
    catalog_id: &'a str,
    span: SourceSpan,
}
