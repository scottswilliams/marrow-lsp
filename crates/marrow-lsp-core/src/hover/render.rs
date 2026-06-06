use lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind};
use marrow_schema::{EnumSchema, IndexSchema, NodeKind, ResourceSchema, StoreSchema};
use marrow_syntax::{IndexDecl, KeyParam, ResourceMember};

pub(super) fn markdown_hover(value: String) -> Hover {
    Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range: None,
    }
}

pub(super) fn resource_hover(schema: &ResourceSchema) -> String {
    let mut value = marrow_code_block(&resource_signature(schema));
    append_docs(&mut value, join_docs(&schema.docs));
    append_section(&mut value, resource_member_summary(schema, &[]));
    value
}

pub(super) fn store_hover(store: &StoreSchema, resource: &ResourceSchema) -> String {
    let mut value = marrow_code_block(&store_signature(store));
    append_docs(&mut value, join_docs(&store.docs));
    append_section(&mut value, identity_summary(store));
    append_section(
        &mut value,
        resource_member_summary(resource, &store.indexes),
    );
    value
}

pub(super) fn enum_hover(schema: &EnumSchema) -> String {
    let mut value = marrow_code_block(&format!("enum {}", schema.name));
    append_docs(&mut value, join_docs(&schema.docs));
    append_section(&mut value, enum_member_summary(schema));
    value
}

pub(super) fn enum_member_hover(schema: &EnumSchema, ordinal: usize) -> String {
    let path = schema.member_path(ordinal);
    let mut value = marrow_code_block(&format!("{}::{}", schema.name, path.join("::")));
    if let Some(member) = schema.members.get(ordinal) {
        append_docs(&mut value, join_docs(&member.docs));
    }
    let status = if schema.is_selectable_leaf(ordinal) {
        "selectable"
    } else if schema.is_category(ordinal) {
        "category"
    } else {
        "group"
    };
    value.push_str("\n\n");
    value.push_str("**Facts**\n");
    value.push_str(&format!("- enum {}\n", schema.name));
    value.push_str(&format!("- {status}"));
    value
}

pub(super) fn resource_member_markdown(member: &ResourceMember) -> String {
    let mut value = marrow_code_block(&resource_member_signature(member));
    append_docs(&mut value, join_docs(resource_member_docs(member)));
    value
}

pub(super) fn index_markdown(index: &IndexDecl) -> String {
    let mut value = marrow_code_block(&index_signature(index));
    append_docs(&mut value, join_docs(&index.docs));
    value
}

pub(super) fn append_docs(value: &mut String, docs: Option<String>) {
    if let Some(docs) = docs {
        value.push_str("\n\n");
        value.push_str(&docs);
    }
}

/// Join `;;` doc lines into one Markdown paragraph, or `None` when there is no doc
/// text (no lines, or only blank ones).
pub(super) fn join_docs(lines: &[String]) -> Option<String> {
    let joined = lines
        .iter()
        .map(|line| line.trim())
        .collect::<Vec<_>>()
        .join(" ");
    let joined = joined.trim();
    if joined.is_empty() {
        None
    } else {
        Some(joined.to_string())
    }
}

/// Wrap `code` in a fenced `marrow` block so the client renders it as `.mw`.
pub(super) fn marrow_code_block(code: &str) -> String {
    format!("```marrow\n{code}\n```")
}

fn resource_signature(schema: &ResourceSchema) -> String {
    format!("resource {}", schema.name)
}

fn store_signature(store: &StoreSchema) -> String {
    let mut signature = format!("store ^{}", store.root);
    if !store.identity_keys.is_empty() {
        signature.push('(');
        signature.push_str(
            &store
                .identity_keys
                .iter()
                .map(|key| format!("{}: {}", key.name, key.ty))
                .collect::<Vec<_>>()
                .join(", "),
        );
        signature.push(')');
    }
    signature.push_str(": ");
    signature.push_str(&store.resource);
    signature
}

fn identity_summary(store: &StoreSchema) -> Option<String> {
    if store.identity_keys.is_empty() {
        return Some("**Identity**\n- keyless singleton; no generated identity type".to_string());
    }
    let mut lines = vec![format!("- generated identity type: `Id(^{})`", store.root)];
    lines.extend(
        store
            .identity_keys
            .iter()
            .map(|key| format!("- `{}: {}`", key.name, key.ty)),
    );
    Some(format!("**Identity**\n{}", lines.join("\n")))
}

fn resource_member_summary(schema: &ResourceSchema, indexes: &[IndexSchema]) -> Option<String> {
    let mut lines = Vec::new();
    for member in &schema.members {
        resource_member_lines(member, "", &mut lines);
    }
    lines.extend(indexes.iter().map(|index| {
        let unique = if index.unique { " unique" } else { "" };
        format!("index {}({}){unique}", index.name, index.args.join(", "))
    }));
    bounded_summary("Members", lines)
}

fn resource_member_lines(member: &marrow_schema::Node, prefix: &str, lines: &mut Vec<String>) {
    let name = resource_member_path_segment(member);
    let path = if prefix.is_empty() {
        name
    } else {
        format!("{prefix}.{name}")
    };
    match &member.kind {
        NodeKind::Slot { ty, required } => {
            let required = if *required { "required " } else { "" };
            lines.push(format!("{required}{path}: {ty}"));
        }
        NodeKind::Group => {
            lines.push(path.clone());
            for child in &member.members {
                resource_member_lines(child, &path, lines);
            }
        }
    }
}

fn resource_member_path_segment(member: &marrow_schema::Node) -> String {
    format!("{}{}", member.name, key_params(&member.key_params))
}

fn key_params(keys: &[marrow_schema::KeyDef]) -> String {
    if keys.is_empty() {
        String::new()
    } else {
        format!(
            "({})",
            keys.iter()
                .map(|key| format!("{}: {}", key.name, key.ty))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

fn enum_member_summary(schema: &EnumSchema) -> Option<String> {
    bounded_summary(
        "Members",
        schema
            .members
            .iter()
            .enumerate()
            .map(|(ordinal, _)| {
                let status = if schema.is_selectable_leaf(ordinal) {
                    "selectable"
                } else if schema.is_category(ordinal) {
                    "category"
                } else {
                    "group"
                };
                format!("{} {status}", schema.member_path(ordinal).join("::"))
            })
            .collect(),
    )
}

fn bounded_summary(label: &str, lines: Vec<String>) -> Option<String> {
    const LIMIT: usize = 5;
    if lines.is_empty() {
        return None;
    }
    let total = lines.len();
    let mut shown = lines
        .into_iter()
        .take(LIMIT)
        .map(|line| format!("- `{line}`"))
        .collect::<Vec<_>>();
    if total > LIMIT {
        shown.push(format!("- ... {} more", total - LIMIT));
    }
    Some(format!("**{label}**\n{}", shown.join("\n")))
}

fn append_section(value: &mut String, section: Option<String>) {
    if let Some(section) = section {
        value.push_str("\n\n");
        value.push_str(&section);
    }
}

fn resource_member_signature(member: &ResourceMember) -> String {
    match member {
        ResourceMember::Field(field) => {
            let required = if field.required { "required " } else { "" };
            format!(
                "{required}{}{}: {}",
                field.name,
                syntax_key_params(&field.keys),
                field.ty
            )
        }
        ResourceMember::Group(group) => {
            format!("{}{}", group.name, syntax_key_params(&group.keys))
        }
    }
}

fn index_signature(index: &IndexDecl) -> String {
    let unique = if index.unique { " unique" } else { "" };
    format!("index {}({}){unique}", index.name, index.args.join(", "))
}

fn resource_member_docs(member: &ResourceMember) -> &[String] {
    match member {
        ResourceMember::Field(field) => &field.docs,
        ResourceMember::Group(group) => &group.docs,
    }
}

fn syntax_key_params(keys: &[KeyParam]) -> String {
    if keys.is_empty() {
        String::new()
    } else {
        format!(
            "({})",
            keys.iter()
                .map(|key| format!("{}: {}", key.name, key.ty))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}
