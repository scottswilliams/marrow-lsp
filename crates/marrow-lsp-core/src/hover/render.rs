use lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind};
use marrow_check::tooling::{
    SavedPlaceHoverFact, SavedPlaceHoverKeyParam, SourceCallableFunctionFact,
    SourceCallableHoverFact, SourceCallableParamFact, StoreRootHoverFact, StoreRootHoverMember,
    StoreRootHoverPathSegment,
};
use marrow_check::{CheckedFacts, DirectEffectFacts, HostEffect, MarrowType, SavedPlaceEffect};
use marrow_schema::{EnumSchema, IndexSchema, NodeKind, ResourceSchema, stdlib};

use crate::types::{render_schema_leaf_type, render_type};

use super::facts::HoverFact;

pub(super) fn hover(fact: HoverFact<'_>) -> Hover {
    markdown_hover(match fact {
        HoverFact::CanonicalLibraryText(text) => default_library_hover_markdown(&text),
        HoverFact::SourceCallable {
            fact,
            checked_facts,
        } => source_callable_hover(&fact, checked_facts),
        HoverFact::StoreRoot(fact) => store_hover(&fact),
        HoverFact::Resource { schema } => resource_hover(schema),
        HoverFact::Enum { schema } => enum_hover(schema),
        HoverFact::EnumMember { schema, ordinal } => enum_member_hover(schema, ordinal),
        HoverFact::SavedPlace(fact) => saved_place_markdown(&fact),
        HoverFact::Type { ty, docs } => type_hover(&ty, docs.as_deref()),
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

fn source_callable_hover(fact: &SourceCallableHoverFact, checked_facts: &CheckedFacts) -> String {
    match fact {
        SourceCallableHoverFact::Function(function) => function_hover(function, checked_facts),
        SourceCallableHoverFact::Parameter(param) => parameter_hover(param),
        SourceCallableHoverFact::ModuleConst { name, ty, docs } => {
            module_const_hover(name, ty.as_ref(), docs)
        }
    }
}

fn function_hover(function: &SourceCallableFunctionFact, checked_facts: &CheckedFacts) -> String {
    let mut value = marrow_code_block(&function_signature(function));
    append_docs(&mut value, join_docs(&function.docs));
    append_docs(&mut value, parameter_docs(&function.params));
    if let Some(effects) = &function.direct_effects
        && let Some(markdown) = direct_effects_markdown(checked_facts, effects)
    {
        append_docs(&mut value, Some(markdown));
    }
    value
}

fn parameter_hover(param: &SourceCallableParamFact) -> String {
    let mut value = marrow_code_block(&parameter_signature(param));
    if let Some(docs) = join_docs(&param.docs) {
        value.push_str("\n\n");
        value.push_str("**Parameter**\n");
        value.push_str(&docs);
    }
    value
}

fn module_const_hover(name: &str, ty: Option<&MarrowType>, docs: &[String]) -> String {
    let mut value = marrow_code_block(&module_const_signature(name, ty));
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

fn store_hover(fact: &StoreRootHoverFact) -> String {
    let mut value = marrow_code_block(&store_signature(fact));
    append_docs(&mut value, join_docs(&fact.store_docs));
    append_section(&mut value, identity_summary(fact));
    append_section(&mut value, store_member_summary(fact));
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

fn saved_place_markdown(fact: &SavedPlaceHoverFact) -> String {
    let (signature, docs) = saved_place_signature_and_docs(fact);
    let mut value = marrow_code_block(&signature);
    append_docs(&mut value, join_docs(docs));
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

fn function_signature(function: &SourceCallableFunctionFact) -> String {
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

fn parameter_signature(param: &SourceCallableParamFact) -> String {
    format!("{}: {}", param.name, render_type(&param.ty))
}

fn module_const_signature(name: &str, ty: Option<&MarrowType>) -> String {
    match ty {
        Some(ty) => format!("const {}: {}", name, render_type(ty)),
        None => format!("const {}", name),
    }
}

fn parameter_docs(params: &[SourceCallableParamFact]) -> Option<String> {
    let docs = params
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

fn store_signature(fact: &StoreRootHoverFact) -> String {
    let mut signature = format!("store ^{}", fact.root);
    if !fact.identity_keys.is_empty() {
        signature.push_str(&hover_key_params(&fact.identity_keys));
    }
    signature.push_str(": ");
    signature.push_str(&fact.resource);
    signature
}

fn identity_summary(fact: &StoreRootHoverFact) -> Option<String> {
    if fact.identity_keys.is_empty() {
        return Some("**Identity**\n- keyless singleton; no generated identity type".to_string());
    }
    let mut lines = vec![format!("- generated identity type: `Id(^{})`", fact.root)];
    lines.extend(
        fact.identity_keys
            .iter()
            .map(|key| format!("- `{}: {}`", key.name, key.ty)),
    );
    Some(format!("**Identity**\n{}", lines.join("\n")))
}

fn store_member_summary(fact: &StoreRootHoverFact) -> Option<String> {
    bounded_summary(
        "Members",
        fact.members.iter().map(store_member_line).collect(),
    )
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
    format!("{}{}", member.name, schema_key_params(&member.key_params))
}

fn schema_key_params(keys: &[marrow_schema::KeyDef]) -> String {
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

fn store_member_line(member: &StoreRootHoverMember) -> String {
    match member {
        StoreRootHoverMember::Field { path, required, ty } => {
            let required = if *required { "required " } else { "" };
            format!("{required}{}: {ty}", store_member_path(path))
        }
        StoreRootHoverMember::Layer { path } => store_member_path(path),
        StoreRootHoverMember::Index { name, args, unique } => {
            let unique = if *unique { " unique" } else { "" };
            format!("index {name}({}){unique}", args.join(", "))
        }
    }
}

fn store_member_path(path: &[StoreRootHoverPathSegment]) -> String {
    path.iter()
        .map(|segment| format!("{}{}", segment.name, hover_key_params(&segment.key_params)))
        .collect::<Vec<_>>()
        .join(".")
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

fn saved_place_signature_and_docs(fact: &SavedPlaceHoverFact) -> (String, &[String]) {
    match fact {
        SavedPlaceHoverFact::Field {
            name,
            key_params,
            ty,
            required,
            docs,
        } => {
            let required = if *required { "required " } else { "" };
            (
                format!("{required}{name}{}: {ty}", hover_key_params(key_params)),
                docs,
            )
        }
        SavedPlaceHoverFact::Layer {
            name,
            key_params,
            docs,
        } => (format!("{name}{}", hover_key_params(key_params)), docs),
        SavedPlaceHoverFact::Index {
            name,
            args,
            unique,
            docs,
        } => {
            let unique = if *unique { " unique" } else { "" };
            (format!("index {name}({}){unique}", args.join(", ")), docs)
        }
    }
}

fn hover_key_params(keys: &[SavedPlaceHoverKeyParam]) -> String {
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
