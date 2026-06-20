use lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind};
use marrow_check::{
    CheckedConst, CheckedFacts, CheckedFunction, CheckedParam, DirectEffectFacts, HostEffect,
    MarrowType, SavedPlaceEffect,
};
use marrow_schema::{EnumSchema, IndexSchema, NodeKind, ResourceSchema, StoreSchema, stdlib};
use marrow_syntax::{FunctionDecl, IndexDecl, KeyParam, ResourceMember};

use crate::types::{render_schema_leaf_type, render_type};

use super::facts::HoverFact;

pub(super) fn hover(fact: HoverFact<'_>) -> Hover {
    markdown_hover(match fact {
        HoverFact::CanonicalLibraryText(text) => default_library_hover_markdown(&text),
        HoverFact::Function {
            checked,
            parsed,
            effects,
        } => function_hover(checked, parsed, effects),
        HoverFact::Parameter { checked, docs } => parameter_hover(checked, docs),
        HoverFact::ModuleConst { checked, docs } => module_const_hover(checked, docs),
        HoverFact::StoreRoot { store, resource } => store_hover(store, resource),
        HoverFact::Resource { schema } => resource_hover(schema),
        HoverFact::Enum { schema } => enum_hover(schema),
        HoverFact::EnumMember { schema, ordinal } => enum_member_hover(schema, ordinal),
        HoverFact::ResourceMember { member } => resource_member_markdown(member),
        HoverFact::Index { index } => index_markdown(index),
        HoverFact::Type { ty, docs } => type_hover(&ty, docs),
    })
}

#[cfg(test)]
pub(super) fn type_hover_with_docs_text(ty: &MarrowType, docs: Option<&str>) -> Hover {
    let mut value = marrow_code_block(&render_type(ty));
    if let Some(docs) = docs {
        value.push_str("\n\n");
        value.push_str(docs);
    }
    markdown_hover(value)
}

fn markdown_hover(value: String) -> Hover {
    Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range: None,
    }
}

fn function_hover(
    checked: &CheckedFunction,
    parsed: &FunctionDecl,
    effects: Option<super::facts::DirectEffects<'_>>,
) -> String {
    let mut value = marrow_code_block(&function_signature(checked));
    append_docs(&mut value, join_docs(&parsed.docs));
    append_docs(&mut value, parameter_docs(parsed));
    if let Some(effects) = effects
        && let Some(markdown) = direct_effects_markdown(effects.facts, effects.effects)
    {
        append_docs(&mut value, Some(markdown));
    }
    value
}

fn parameter_hover(checked: &CheckedParam, docs: &[String]) -> String {
    let mut value = marrow_code_block(&parameter_signature(checked));
    if let Some(docs) = join_docs(docs) {
        value.push_str("\n\n");
        value.push_str("**Parameter**\n");
        value.push_str(&docs);
    }
    value
}

fn module_const_hover(checked: &CheckedConst, docs: &[String]) -> String {
    let mut value = marrow_code_block(&module_const_signature(checked));
    append_docs(&mut value, join_docs(docs));
    value
}

fn type_hover(ty: &MarrowType, docs: Option<&[String]>) -> String {
    let mut value = marrow_code_block(&render_type(ty));
    if let Some(docs) = docs {
        append_docs(&mut value, join_docs(docs));
    }
    value
}

fn default_library_hover_markdown(fact: &str) -> String {
    let Some((signature, context)) = fact.split_once("\n\n") else {
        return marrow_code_block(fact);
    };
    let mut value = marrow_code_block(signature);
    value.push_str("\n\n");
    value.push_str(context);
    value
}

fn resource_hover(schema: &ResourceSchema) -> String {
    let mut value = marrow_code_block(&resource_signature(schema));
    append_docs(&mut value, join_docs(&schema.docs));
    append_section(&mut value, resource_member_summary(schema, &[]));
    value
}

fn store_hover(store: &StoreSchema, resource: &ResourceSchema) -> String {
    let mut value = marrow_code_block(&store_signature(store));
    append_docs(&mut value, join_docs(&store.docs));
    append_section(&mut value, identity_summary(store));
    append_section(
        &mut value,
        resource_member_summary(resource, &store.indexes),
    );
    value
}

fn enum_hover(schema: &EnumSchema) -> String {
    let mut value = marrow_code_block(&format!("enum {}", schema.name));
    append_docs(&mut value, join_docs(&schema.docs));
    append_section(&mut value, enum_member_summary(schema));
    value
}

fn enum_member_hover(schema: &EnumSchema, ordinal: usize) -> String {
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

fn resource_member_markdown(member: &ResourceMember) -> String {
    let mut value = marrow_code_block(&resource_member_signature(member));
    append_docs(&mut value, join_docs(resource_member_docs(member)));
    value
}

fn index_markdown(index: &IndexDecl) -> String {
    let mut value = marrow_code_block(&index_signature(index));
    append_docs(&mut value, join_docs(&index.docs));
    value
}

fn direct_effects_markdown(facts: &CheckedFacts, effects: &DirectEffectFacts) -> Option<String> {
    let mut lines = Vec::new();
    if let Some(line) = saved_effects_line(facts, "saved reads", &effects.saved_reads) {
        lines.push(line);
    }
    if let Some(line) = saved_effects_line(facts, "saved writes", &effects.saved_writes) {
        lines.push(line);
    }
    if effects.transactions {
        lines.push("- transaction".to_string());
    }
    if let Some(line) = host_effects_line(&effects.host_calls) {
        lines.push(line);
    }
    if effects.throws {
        lines.push("- throws".to_string());
    }

    if lines.is_empty() {
        None
    } else {
        Some(format!("**Direct effects**\n{}", lines.join("\n")))
    }
}

fn saved_effects_line(
    facts: &CheckedFacts,
    label: &str,
    places: &[SavedPlaceEffect],
) -> Option<String> {
    let items = effect_items(
        places
            .iter()
            .filter_map(|place| saved_place_display(facts, place)),
    );
    if items.is_empty() {
        None
    } else {
        Some(format!("- {label}: {}", items.join(", ")))
    }
}

fn host_effects_line(effects: &[HostEffect]) -> Option<String> {
    let items = effect_items(effects.iter().map(|effect| match effect {
        HostEffect::Output => "output".to_string(),
        HostEffect::Capability(capability) => capability_label(*capability).to_string(),
    }));
    if items.is_empty() {
        None
    } else {
        Some(format!("- host: {}", items.join(", ")))
    }
}

fn effect_items(items: impl IntoIterator<Item = String>) -> Vec<String> {
    const MAX_EFFECT_ITEMS: usize = 8;

    let mut items = items.into_iter().collect::<Vec<_>>();
    items.sort();
    items.dedup();

    let extra = items.len().saturating_sub(MAX_EFFECT_ITEMS);
    items.truncate(MAX_EFFECT_ITEMS);
    if extra > 0 {
        items.push(format!("+{extra} more"));
    }
    items
}

fn saved_place_display(facts: &CheckedFacts, place: &SavedPlaceEffect) -> Option<String> {
    let resource = facts
        .resources()
        .iter()
        .find(|resource| resource.id == place.resource)?;
    let mut path = vec![resource.name.clone()];
    for member_id in &place.members {
        let member = facts
            .resource_members()
            .iter()
            .find(|member| member.id == *member_id)?;
        path.push(member.name.clone());
    }
    Some(path.join("."))
}

fn capability_label(capability: stdlib::Capability) -> &'static str {
    match capability {
        stdlib::Capability::Clock => "clock",
        stdlib::Capability::Context => "context",
        stdlib::Capability::Environment => "environment",
        stdlib::Capability::Log => "log",
        stdlib::Capability::Filesystem => "filesystem",
    }
}

fn append_docs(value: &mut String, docs: Option<String>) {
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
fn marrow_code_block(code: &str) -> String {
    format!("```marrow\n{code}\n```")
}

fn function_signature(function: &CheckedFunction) -> String {
    let params = function
        .params
        .iter()
        .map(|param| format!("{}: {}", param.name, render_type(&param.ty)))
        .collect::<Vec<_>>()
        .join(", ");
    match &function.return_type {
        Some(ty) => format!("fn {}({params}): {}", function.name, render_type(ty)),
        None => format!("fn {}({params})", function.name),
    }
}

fn parameter_signature(param: &CheckedParam) -> String {
    format!("{}: {}", param.name, render_type(&param.ty))
}

fn module_const_signature(constant: &CheckedConst) -> String {
    match &constant.ty {
        Some(ty) => format!("const {}: {}", constant.name, render_type(ty)),
        None => format!("const {}", constant.name),
    }
}

fn parameter_docs(function: &FunctionDecl) -> Option<String> {
    let docs = function
        .params
        .iter()
        .filter_map(|param| {
            let docs = join_docs(&param.docs)?;
            Some(format!("- `{}`: {docs}", param.name))
        })
        .collect::<Vec<_>>();
    if docs.is_empty() {
        None
    } else {
        Some(format!("**Parameters**\n{}", docs.join("\n")))
    }
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
        NodeKind::Slot { ty, required, .. } => {
            let required = if *required { "required " } else { "" };
            lines.push(format!(
                "{required}{path}: {}",
                render_schema_leaf_type(member, ty)
            ));
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
