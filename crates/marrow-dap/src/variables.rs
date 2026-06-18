//! Rendering and expansion of in-memory locals for the Variables view.
//!
//! Values render to bounded one-line previews. Compound shapes — `sequence`,
//! `resource`, `identity` — can also expand: the client follows a
//! `variablesReference` to list their elements or fields. Expansion walks an
//! owned [`Value`] cloned out of the frame, so this module is pure.

use marrow_run::Value;
use marrow_store::key::SavedKey;

const VALUE_PREVIEW_CHARS: usize = 256;
const RESOURCE_FIELD_PREVIEW_COUNT: usize = 12;
const IDENTITY_KEY_PREVIEW_COUNT: usize = 8;

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

    pub fn start(self) -> usize {
        match self {
            Self::All { start } | Self::Range { start, .. } => start,
        }
    }

    pub fn count(self) -> Option<usize> {
        match self {
            Self::All { .. } => None,
            Self::Range { count, .. } => Some(count),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VariablesFilter {
    All,
    Named,
    Indexed,
}

impl VariablesFilter {
    pub fn allows_named(self) -> bool {
        matches!(self, Self::All | Self::Named)
    }

    pub fn allows_indexed(self) -> bool {
        matches!(self, Self::All | Self::Indexed)
    }
}

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

pub fn child_counts(value: &Value) -> Option<ChildCounts> {
    match value {
        Value::Sequence(items) => Some(ChildCounts {
            named: None,
            indexed: Some(items.len()),
        }),
        Value::Resource(fields) => Some(ChildCounts {
            named: Some(fields.len()),
            indexed: None,
        }),
        Value::Identity(identity) => Some(ChildCounts {
            named: None,
            indexed: Some(identity.keys().len()),
        }),
        _ => None,
    }
}

pub fn value_preview(value: &Value) -> String {
    match value {
        Value::Str(text) => truncate_chars(text, VALUE_PREVIEW_CHARS),
        Value::Resource(fields) => resource_preview(fields),
        Value::Identity(identity) => identity_preview(identity),
        _ => truncate_chars(&value.display_debug(), VALUE_PREVIEW_CHARS),
    }
}

/// The children of a compound value, for a `variables` request that followed its
/// reference. A `sequence` yields its elements by index, a `resource` its present
/// fields by name, an identity its lowered key segments. A scalar yields none.
pub fn children(value: &Value, page: ChildPage, filter: VariablesFilter) -> Vec<Child> {
    match value {
        Value::Sequence(items) if filter.allows_indexed() => {
            collect_page(items.iter().enumerate(), page, |(index, item)| Child {
                name: format!("[{index}]"),
                value: value_preview(item),
                expand: is_expandable(item).then(|| item.clone()),
            })
        }
        Value::Resource(fields) if filter.allows_named() => {
            collect_page(fields.iter(), page, |(name, field)| Child {
                name: name.clone(),
                value: value_preview(field),
                expand: is_expandable(field).then(|| field.clone()),
            })
        }
        Value::Identity(identity) if filter.allows_indexed() => {
            collect_page(identity.keys().iter().enumerate(), page, |(index, key)| {
                Child {
                    name: format!("[{index}]"),
                    value: saved_key_preview(key),
                    expand: None,
                }
            })
        }
        _ => Vec::new(),
    }
}

pub fn collect_page<T>(
    iter: impl Iterator<Item = T>,
    page: ChildPage,
    mut render: impl FnMut(T) -> Child,
) -> Vec<Child> {
    let iter = iter.skip(page.start());
    match page.count() {
        Some(count) => iter.take(count).map(render).collect(),
        None => iter.map(&mut render).collect(),
    }
}

pub fn local_children_from_refs(
    names: Vec<&str>,
    latest: &std::collections::HashMap<&str, &Value>,
    page: ChildPage,
    filter: VariablesFilter,
) -> Vec<Child> {
    if !filter.allows_named() {
        return Vec::new();
    }
    collect_page(names.into_iter(), page, |name| {
        let value = latest.get(name).expect("name was inserted");
        Child {
            name: name.to_string(),
            value: value_preview(value),
            expand: is_expandable(value).then(|| (*value).clone()),
        }
    })
}

fn resource_preview(fields: &[(String, Value)]) -> String {
    let mut names: Vec<String> = fields
        .iter()
        .take(RESOURCE_FIELD_PREVIEW_COUNT)
        .map(|(name, _)| truncate_chars(name, VALUE_PREVIEW_CHARS))
        .collect();
    if fields.len() > RESOURCE_FIELD_PREVIEW_COUNT {
        names.push(format!(
            "... {} more",
            fields.len() - RESOURCE_FIELD_PREVIEW_COUNT
        ));
    }
    format!("resource{{{}}}", names.join(", "))
}

fn identity_preview(identity: &marrow_run::IdentityValue) -> String {
    let root = truncate_chars(identity.root(), VALUE_PREVIEW_CHARS);
    let mut keys: Vec<String> = identity
        .keys()
        .iter()
        .take(IDENTITY_KEY_PREVIEW_COUNT)
        .map(saved_key_preview)
        .collect();
    if identity.keys().len() > IDENTITY_KEY_PREVIEW_COUNT {
        keys.push(format!(
            "... {} more",
            identity.keys().len() - IDENTITY_KEY_PREVIEW_COUNT
        ));
    }
    format!("^{root}({})", keys.join(", "))
}

fn saved_key_preview(key: &SavedKey) -> String {
    match key {
        SavedKey::Int(value) => value.to_string(),
        SavedKey::Bool(value) => value.to_string(),
        SavedKey::Str(value) => truncate_chars(value, VALUE_PREVIEW_CHARS),
        SavedKey::Date(value) => format!("date({value})"),
        SavedKey::Duration(value) => format!("duration({value})"),
        SavedKey::Instant(value) => format!("instant({value})"),
        SavedKey::Bytes(bytes) => format!("bytes[{}]", bytes.len()),
    }
}

fn truncate_chars(text: &str, limit: usize) -> String {
    let mut end = 0;
    for (count, (index, ch)) in text.char_indices().enumerate() {
        if count == limit {
            let mut truncated = String::from(&text[..end]);
            truncated.push('…');
            return truncated;
        }
        end = index + ch.len_utf8();
    }
    text.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_scalar_is_a_leaf() {
        assert!(!is_expandable(&Value::Int(3)));
        assert!(children(&Value::Int(3), ChildPage::default(), VariablesFilter::All).is_empty());
    }

    #[test]
    fn a_sequence_expands_to_indexed_elements() {
        let seq = Value::Sequence(vec![Value::Int(10), Value::Str("hi".into())]);
        assert!(is_expandable(&seq));
        let kids = children(&seq, ChildPage::default(), VariablesFilter::All);
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
        let kids = children(&resource, ChildPage::default(), VariablesFilter::All);
        assert_eq!(kids[0].name, "title");
        assert_eq!(kids[0].value, "Dune");
        assert_eq!(kids[0].expand, None);
        assert_eq!(kids[1].name, "tags");
        // The nested sequence carries an expandable value forward.
        assert_eq!(kids[1].expand, Some(nested));
    }

    #[test]
    fn a_child_page_bounds_sequence_expansion() {
        let seq = Value::Sequence((0..5).map(Value::Int).collect());
        let kids = children(&seq, ChildPage::new(1, 2), VariablesFilter::All);
        let names: Vec<&str> = kids.iter().map(|kid| kid.name.as_str()).collect();

        assert_eq!(names, ["[1]", "[2]"]);
    }

    #[test]
    fn an_omitted_count_returns_all_children_from_start() {
        let seq = Value::Sequence((0..5).map(Value::Int).collect());
        let kids = children(&seq, ChildPage::requested(1, None), VariablesFilter::All);
        let names: Vec<&str> = kids.iter().map(|kid| kid.name.as_str()).collect();

        assert_eq!(names, ["[1]", "[2]", "[3]", "[4]"]);
    }

    #[test]
    fn a_zero_count_returns_all_children_from_start() {
        let seq = Value::Sequence((0..5).map(Value::Int).collect());
        let kids = children(&seq, ChildPage::requested(1, Some(0)), VariablesFilter::All);
        let names: Vec<&str> = kids.iter().map(|kid| kid.name.as_str()).collect();

        assert_eq!(names, ["[1]", "[2]", "[3]", "[4]"]);
    }

    #[test]
    fn filters_keep_named_and_indexed_children_separate() {
        let seq = Value::Sequence(vec![Value::Int(1)]);
        assert!(children(&seq, ChildPage::default(), VariablesFilter::Named).is_empty());
        assert_eq!(
            children(&seq, ChildPage::default(), VariablesFilter::Indexed)[0].name,
            "[0]"
        );

        let resource = Value::Resource(vec![("title".into(), Value::Str("Dune".into()))]);
        assert!(children(&resource, ChildPage::default(), VariablesFilter::Indexed).is_empty());
        assert_eq!(
            children(&resource, ChildPage::default(), VariablesFilter::Named)[0].name,
            "title"
        );
    }

    #[test]
    fn a_string_preview_is_bounded() {
        let preview = value_preview(&Value::Str("x".repeat(400)));

        assert!(preview.len() < 400, "{preview}");
        assert!(preview.ends_with('…'), "{preview}");
    }

    #[test]
    fn a_resource_preview_does_not_render_every_field() {
        let resource = Value::Resource(
            (0..50)
                .map(|index| (format!("field{index}"), Value::Int(index)))
                .collect(),
        );
        let preview = value_preview(&resource);

        assert!(preview.contains("field0"), "{preview}");
        assert!(!preview.contains("field49"), "{preview}");
        assert!(preview.contains("more"), "{preview}");
    }
}
