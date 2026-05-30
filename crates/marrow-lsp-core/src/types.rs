//! The `.mw` spelling of a checker type, shared by every feature that shows a
//! type to a human.
//!
//! Hover, completion detail, and the MCP `mw_type_at` answer all render the same
//! checker [`MarrowType`] as the source text a developer would write. They must
//! agree — a hover and an agent query for the same expression spell the type
//! identically — so the one renderer lives here rather than copied per feature.

use marrow_check::MarrowType;

/// The canonical `.mw` spelling of a checker type. Mirrors `marrow_schema::Type`'s
/// `Display` for the storable types and names the checker-only forms (`Error`, a
/// same-module resource, `unknown`) as they are written in source.
pub(crate) fn render_type(ty: &MarrowType) -> String {
    match ty {
        MarrowType::Primitive(scalar) => scalar.name().to_string(),
        MarrowType::Error => "Error".to_string(),
        MarrowType::Resource(name) => name.clone(),
        MarrowType::Identity(resource) => format!("{resource}::Id"),
        MarrowType::Sequence(element) => format!("sequence[{}]", render_type(element)),
        MarrowType::Unknown => "unknown".to_string(),
    }
}
