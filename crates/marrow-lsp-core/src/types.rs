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
/// same-module resource or enum, `unknown`) as they are written in source.
pub(crate) fn render_type(ty: &MarrowType) -> String {
    match ty {
        MarrowType::Primitive(scalar) => scalar.name().to_string(),
        MarrowType::Error => "Error".to_string(),
        MarrowType::Resource(name) => name.clone(),
        MarrowType::GroupEntry { resource, .. } => resource.clone(),
        MarrowType::Identity(resource) => format!("{resource}::Id"),
        MarrowType::Enum { module, name } if module.is_empty() => name.clone(),
        MarrowType::Enum { module, name } => format!("{module}::{name}"),
        MarrowType::Sequence(element) => format!("sequence[{}]", render_type(element)),
        MarrowType::LocalTree { value, .. } => format!("tree[{}]", render_type(value)),
        MarrowType::Invalid => "unknown".to_string(),
        MarrowType::Unknown => "unknown".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enum_types_render_with_their_owner_when_qualified() {
        let ty = MarrowType::Enum {
            module: "shelf::state".to_string(),
            name: "Status".to_string(),
        };

        assert_eq!(render_type(&ty), "shelf::state::Status");
    }

    #[test]
    fn module_less_enum_types_render_as_the_bare_name() {
        let ty = MarrowType::Enum {
            module: String::new(),
            name: "Status".to_string(),
        };

        assert_eq!(render_type(&ty), "Status");
    }

    #[test]
    fn group_entry_types_render_as_the_owning_resource() {
        let ty = MarrowType::GroupEntry {
            resource: "Order".to_string(),
            layers: vec!["lines".to_string(), "discounts".to_string()],
        };

        assert_eq!(render_type(&ty), "Order");
    }

    #[test]
    fn local_tree_types_render_as_tree_of_value() {
        let ty = MarrowType::LocalTree {
            keys: vec![MarrowType::Primitive(marrow_schema::ScalarType::Str)],
            value: Box::new(MarrowType::Primitive(marrow_schema::ScalarType::Int)),
        };

        assert_eq!(render_type(&ty), "tree[int]");
    }

    #[test]
    fn invalid_types_render_as_unknown() {
        assert_eq!(render_type(&MarrowType::Invalid), "unknown");
    }
}
