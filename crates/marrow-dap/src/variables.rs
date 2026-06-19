//! DAP Variables-view adaptation for Marrow runtime debug facts.

use marrow_run::{
    DEBUG_VALUE_MAX_PAGE_LIMIT, DebugLocal, DebugValue, DebugValueChildCounts, DebugValueFilter,
    DebugValuePage,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildPage {
    All { start: usize },
    Range { start: usize, count: usize },
}

impl ChildPage {
    pub fn new(start: usize, count: usize) -> Self {
        Self::Range { start, count }
    }

    pub fn requested(start: usize, count: Option<usize>) -> Self {
        match count {
            Some(0) | None => Self::All { start },
            Some(count) => Self::new(start, count),
        }
    }

    pub fn to_debug_value_page(self) -> DebugValuePage {
        match self {
            Self::All { start } => DebugValuePage::new(start, DEBUG_VALUE_MAX_PAGE_LIMIT),
            Self::Range { start, count } => DebugValuePage::new(start, count),
        }
    }
}

impl Default for ChildPage {
    fn default() -> Self {
        Self::All { start: 0 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChildCounts {
    pub named: Option<usize>,
    pub indexed: Option<usize>,
}

impl From<DebugValueChildCounts> for ChildCounts {
    fn from(counts: DebugValueChildCounts) -> Self {
        Self {
            named: counts.named,
            indexed: counts.indexed,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VariablesFilter {
    All,
    Named,
    Indexed,
}

impl VariablesFilter {
    pub fn to_debug_value_filter(self) -> DebugValueFilter {
        match self {
            Self::All => DebugValueFilter::All,
            Self::Named => DebugValueFilter::Named,
            Self::Indexed => DebugValueFilter::Indexed,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Child {
    pub name: String,
    pub value: String,
    pub children_truncated: Option<bool>,
    pub expand: Option<DebugValue>,
}

pub fn is_expandable(value: &DebugValue) -> bool {
    value.child_counts().is_some()
}

pub fn child_counts(value: &DebugValue) -> Option<ChildCounts> {
    value.child_counts().map(ChildCounts::from)
}

#[cfg(test)]
pub fn value_preview(value: &DebugValue) -> String {
    value.preview()
}

pub fn children(value: &DebugValue, page: ChildPage, filter: VariablesFilter) -> Vec<Child> {
    value
        .children(page.to_debug_value_page(), filter.to_debug_value_filter())
        .into_iter()
        .map(|child| child_from_debug(child.name, child.value))
        .collect()
}

pub fn local_children(locals: Vec<DebugLocal>) -> Vec<Child> {
    locals
        .into_iter()
        .map(|local| child_from_debug(local.name, local.value))
        .collect()
}

fn child_from_debug(name: String, value: DebugValue) -> Child {
    let preview = value.preview();
    let expandable = is_expandable(&value);
    let children_truncated = expandable.then(|| value.children_truncated());
    Child {
        name,
        value: preview,
        children_truncated,
        expand: expandable.then_some(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use marrow_run::Value;

    fn debug_value(value: Value) -> DebugValue {
        DebugValue::from_value(value)
    }

    #[test]
    fn a_scalar_is_a_leaf() {
        let value = debug_value(Value::Int(3));

        assert!(!is_expandable(&value));
        assert!(children(&value, ChildPage::default(), VariablesFilter::All).is_empty());
    }

    #[test]
    fn a_sequence_expands_to_indexed_elements() {
        let seq = debug_value(Value::Sequence(vec![
            Value::Int(10),
            Value::Str("hi".into()),
        ]));
        assert!(is_expandable(&seq));
        let kids = children(&seq, ChildPage::default(), VariablesFilter::All);
        assert_eq!(kids.len(), 2);
        assert_eq!(kids[0].name, "[1]");
        assert_eq!(kids[0].value, "10");
        assert_eq!(kids[1].name, "[2]");
        assert_eq!(kids[1].value, "hi");
    }

    #[test]
    fn a_resource_expands_to_named_fields_and_nested_ones_re_expand() {
        let resource = debug_value(Value::Resource(vec![
            ("title".into(), Value::Str("Dune".into())),
            ("tags".into(), Value::Sequence(vec![Value::Int(1)])),
        ]));
        let kids = children(&resource, ChildPage::default(), VariablesFilter::All);
        assert_eq!(kids[0].name, "title");
        assert_eq!(kids[0].value, "Dune");
        assert_eq!(kids[0].expand, None);
        assert_eq!(kids[0].children_truncated, None);
        assert_eq!(kids[1].name, "tags");
        assert!(kids[1].expand.is_some());
        assert_eq!(kids[1].children_truncated, Some(false));
    }

    #[test]
    fn a_child_page_bounds_sequence_expansion() {
        let seq = debug_value(Value::Sequence((0..5).map(Value::Int).collect()));
        let kids = children(&seq, ChildPage::new(1, 2), VariablesFilter::All);
        let names: Vec<&str> = kids.iter().map(|kid| kid.name.as_str()).collect();

        assert_eq!(names, ["[2]", "[3]"]);
    }

    #[test]
    fn an_omitted_count_returns_all_captured_children_from_start() {
        let seq = debug_value(Value::Sequence((0..5).map(Value::Int).collect()));
        let kids = children(&seq, ChildPage::requested(1, None), VariablesFilter::All);
        let names: Vec<&str> = kids.iter().map(|kid| kid.name.as_str()).collect();

        assert_eq!(names, ["[2]", "[3]", "[4]", "[5]"]);
    }

    #[test]
    fn a_zero_count_returns_all_captured_children_from_start() {
        let seq = debug_value(Value::Sequence((0..5).map(Value::Int).collect()));
        let kids = children(&seq, ChildPage::requested(1, Some(0)), VariablesFilter::All);
        let names: Vec<&str> = kids.iter().map(|kid| kid.name.as_str()).collect();

        assert_eq!(names, ["[2]", "[3]", "[4]", "[5]"]);
    }

    #[test]
    fn filters_keep_named_and_indexed_children_separate() {
        let seq = debug_value(Value::Sequence(vec![Value::Int(1)]));
        assert!(children(&seq, ChildPage::default(), VariablesFilter::Named).is_empty());
        assert_eq!(
            children(&seq, ChildPage::default(), VariablesFilter::Indexed)[0].name,
            "[1]"
        );

        let resource = debug_value(Value::Resource(vec![(
            "title".into(),
            Value::Str("Dune".into()),
        )]));
        assert!(children(&resource, ChildPage::default(), VariablesFilter::Indexed).is_empty());
        assert_eq!(
            children(&resource, ChildPage::default(), VariablesFilter::Named)[0].name,
            "title"
        );
    }

    #[test]
    fn a_string_preview_is_bounded() {
        let preview = value_preview(&debug_value(Value::Str("x".repeat(400))));

        assert!(preview.len() < 400, "{preview}");
        assert!(preview.ends_with("..."), "{preview}");
    }

    #[test]
    fn a_resource_preview_does_not_render_every_field() {
        let resource = debug_value(Value::Resource(
            (0..50)
                .map(|index| (format!("field{index}"), Value::Int(index)))
                .collect(),
        ));
        let preview = value_preview(&resource);

        assert!(preview.contains("field0"), "{preview}");
        assert!(!preview.contains("field49"), "{preview}");
        assert!(preview.contains("more"), "{preview}");
    }
}
