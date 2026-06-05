//! Rendering and expansion of in-memory locals for the Variables view.
//!
//! A scalar local renders to one line via [`Value::display_debug`]. The compound
//! shapes — `sequence`, `resource`, `identity` — render to a structural preview
//! and *expand*: the client follows a `variablesReference` to list their elements
//! or fields. Expansion walks an owned [`Value`] (cloned out of the frame on the
//! run-thread, since the frame borrow never leaves it), so this module is pure.

use marrow_run::Value;

/// One child variable produced by expanding a compound [`Value`]: its name (the
/// element index or field name), its one-line rendering, and the value to expand
/// further when the client drills in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Child {
    pub name: String,
    pub value: String,
    /// The nested value, present only when it can itself be expanded.
    pub expand: Option<Value>,
}

/// Whether a value has children to expand: only the compound in-memory shapes do.
/// A scalar is a leaf.
pub fn is_expandable(value: &Value) -> bool {
    matches!(
        value,
        Value::Sequence(_) | Value::Resource(_) | Value::Identity(_)
    )
}

/// The children of a compound value, for a `variables` request that followed its
/// reference. A `sequence` yields its elements by index, a `resource` its present
/// fields by name, an identity its lowered key segments. A scalar yields none.
pub fn children(value: &Value) -> Vec<Child> {
    match value {
        Value::Sequence(items) => items
            .iter()
            .enumerate()
            .map(|(index, item)| Child {
                name: format!("[{index}]"),
                value: item.display_debug(),
                expand: is_expandable(item).then(|| item.clone()),
            })
            .collect(),
        Value::Resource(fields) => fields
            .iter()
            .map(|(name, field)| Child {
                name: name.clone(),
                value: field.display_debug(),
                expand: is_expandable(field).then(|| field.clone()),
            })
            .collect(),
        Value::Identity(identity) => identity
            .keys()
            .iter()
            .enumerate()
            .map(|(index, key)| Child {
                name: format!("[{index}]"),
                value: format!("{key:?}"),
                expand: None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_scalar_is_a_leaf() {
        assert!(!is_expandable(&Value::Int(3)));
        assert!(children(&Value::Int(3)).is_empty());
    }

    #[test]
    fn a_sequence_expands_to_indexed_elements() {
        let seq = Value::Sequence(vec![Value::Int(10), Value::Str("hi".into())]);
        assert!(is_expandable(&seq));
        let kids = children(&seq);
        assert_eq!(kids.len(), 2);
        assert_eq!(kids[0].name, "[0]");
        assert_eq!(kids[0].value, "10");
        assert_eq!(kids[1].name, "[1]");
        assert_eq!(kids[1].value, "hi");
    }

    #[test]
    fn a_resource_expands_to_named_fields_and_nested_ones_re_expand() {
        let nested = Value::Sequence(vec![Value::Int(1)]);
        let resource = Value::Resource(vec![
            ("title".into(), Value::Str("Dune".into())),
            ("tags".into(), nested.clone()),
        ]);
        let kids = children(&resource);
        assert_eq!(kids[0].name, "title");
        assert_eq!(kids[0].value, "Dune");
        assert_eq!(kids[0].expand, None);
        assert_eq!(kids[1].name, "tags");
        // The nested sequence carries an expandable value forward.
        assert_eq!(kids[1].expand, Some(nested));
    }
}
