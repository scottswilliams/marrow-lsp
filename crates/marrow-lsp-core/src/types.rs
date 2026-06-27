//! LSP-local wrapper around Marrow's source-facing type spelling.

use marrow_check::{MarrowType, tooling::render_marrow_type};

pub(crate) fn render_type(ty: &MarrowType) -> String {
    render_marrow_type(ty)
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
    fn identity_types_render_as_store_root_id() {
        assert_eq!(
            render_type(&MarrowType::Identity("books".to_string())),
            "Id(^books)"
        );
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
