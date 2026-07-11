use lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind};
use marrow_check::tooling::{
    CallableSignature, CallableSignatureKind, SavedPlaceHoverFact, SavedPlaceHoverKeyParam,
    SourceCallableFunctionFact, SourceCallableHoverFact, SourceCallableParamFact,
    SourceEnumHoverFact, SourceEnumMemberHoverFact, SourceEnumMemberStatus, SourceHoverFact,
    SourceModulePathHoverFact, SourceOperatorHoverFact, SourceResourceHoverFact,
    SourceResourceHoverMember, SourceResourceHoverMemberKind, SourceResourceHoverPathSegment,
    SourceSchemaHoverFact, SourceSchemaHoverKeyParam, SourceStandardLibraryCapability,
    SourceTypeHoverFact, StoreRootHoverFact, StoreRootHoverMember, StoreRootHoverPathSegment,
};
use marrow_check::{
    CheckedFacts, CheckedProgram, DirectEffectFacts, HostEffect, MarrowType, SavedPlaceEffect,
};
use marrow_schema::stdlib;

use crate::callables::render_callable_signature;
use crate::types::render_type;

pub(super) fn hover(program: &CheckedProgram, fact: SourceHoverFact) -> Hover {
    markdown_hover(match fact {
        SourceHoverFact::Callable(fact) => source_callable_hover(program, &fact),
        SourceHoverFact::ModulePath(fact) => source_module_path_hover(&fact),
        SourceHoverFact::Operator(fact) => source_operator_hover(&fact),
        SourceHoverFact::StoreRoot(fact) => store_hover(&fact),
        SourceHoverFact::Schema(fact) => source_schema_hover(&fact),
        SourceHoverFact::SavedPlace(fact) => saved_place_markdown(&fact),
        SourceHoverFact::Type(fact) => type_hover(program, &fact),
    })
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

fn source_callable_hover(program: &CheckedProgram, fact: &SourceCallableHoverFact) -> String {
    match fact {
        SourceCallableHoverFact::Intrinsic(signature) => {
            intrinsic_callable_hover(program, signature)
        }
        SourceCallableHoverFact::Function(function) => function_hover(program, function),
        SourceCallableHoverFact::Parameter(param) => parameter_hover(program, param),
        SourceCallableHoverFact::ModuleConst { name, ty, docs } => {
            module_const_hover(program, name, ty.as_ref(), docs)
        }
    }
}

fn intrinsic_callable_hover(program: &CheckedProgram, signature: &CallableSignature) -> String {
    let mut value = marrow_code_block(&render_callable_signature(program, signature));
    append_docs(
        &mut value,
        Some(callable_context(signature.kind).to_string()),
    );
    append_docs(&mut value, join_docs(&signature.docs));
    value
}

fn callable_context(kind: CallableSignatureKind) -> &'static str {
    match kind {
        CallableSignatureKind::Builtin => "default library builtin.",
        CallableSignatureKind::ScalarConversion => "default library scalar conversion.",
        CallableSignatureKind::ErrorConstructor => "default library Error constructor.",
        CallableSignatureKind::IdentityConstructor => "default library Id constructor.",
        CallableSignatureKind::StandardLibrary => "default library std operation.",
    }
}

fn function_hover(program: &CheckedProgram, function: &SourceCallableFunctionFact) -> String {
    let mut value = marrow_code_block(&function_signature(program, function));
    append_docs(&mut value, join_docs(&function.docs));
    append_docs(&mut value, parameter_docs(&function.params));
    if let Some(effects) = &function.direct_effects
        && let Some(markdown) = direct_effects_markdown(&program.facts, effects)
    {
        append_docs(&mut value, Some(markdown));
    }
    value
}

fn parameter_hover(program: &CheckedProgram, param: &SourceCallableParamFact) -> String {
    let mut value = marrow_code_block(&parameter_signature(program, param));
    if let Some(docs) = join_docs(&param.docs) {
        value.push_str("\n\n");
        value.push_str("**Parameter**\n");
        value.push_str(&docs);
    }
    value
}

fn module_const_hover(
    program: &CheckedProgram,
    name: &str,
    ty: Option<&MarrowType>,
    docs: &[String],
) -> String {
    let mut value = marrow_code_block(&module_const_signature(program, name, ty));
    append_docs(&mut value, join_docs(docs));
    value
}

fn type_hover(program: &CheckedProgram, fact: &SourceTypeHoverFact) -> String {
    let mut value = marrow_code_block(&render_type(program, &fact.ty));
    append_docs(&mut value, join_docs(&fact.docs));
    value
}

fn source_operator_hover(fact: &SourceOperatorHoverFact) -> String {
    let mut value = marrow_code_block(&format!("operator {}", fact.spelling));
    append_docs(&mut value, Some("language operator.".to_string()));
    append_docs(&mut value, Some(fact.description.clone()));
    value
}

fn source_module_path_hover(fact: &SourceModulePathHoverFact) -> String {
    match fact {
        SourceModulePathHoverFact::StandardLibraryNamespace(namespace) => {
            let mut value = marrow_code_block("std");
            append_section(
                &mut value,
                Some(format!(
                    "default library namespace.\n\nModules: {}",
                    namespace.modules.join(", ")
                )),
            );
            value
        }
        SourceModulePathHoverFact::StandardLibraryModule(module) => {
            let mut value = marrow_code_block(&format!("std::{}", module.module));
            append_section(
                &mut value,
                Some(format!(
                    "default library module.\n\nOperations: {}",
                    module
                        .operations
                        .iter()
                        .map(|op| format!(
                            "{} ({})",
                            op.name,
                            op.required_capability
                                .map(source_std_capability_label)
                                .unwrap_or("pure")
                        ))
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            );
            value
        }
        SourceModulePathHoverFact::ProjectModule(module) => {
            let mut value = marrow_code_block(&format!("module {}", module.module));
            append_section(&mut value, Some("project source module.".to_string()));
            value
        }
    }
}

fn store_hover(fact: &StoreRootHoverFact) -> String {
    let mut value = marrow_code_block(&store_signature(fact));
    append_docs(&mut value, join_docs(&fact.store_docs));
    append_section(&mut value, identity_summary(fact));
    append_section(&mut value, store_member_summary(fact));
    value
}

fn saved_place_markdown(fact: &SavedPlaceHoverFact) -> String {
    let (signature, docs) = saved_place_signature_and_docs(fact);
    let mut value = marrow_code_block(&signature);
    append_docs(&mut value, join_docs(docs));
    value
}

fn source_schema_hover(fact: &SourceSchemaHoverFact) -> String {
    match fact {
        SourceSchemaHoverFact::Resource(resource) => resource_hover(resource),
        SourceSchemaHoverFact::Enum(enum_fact) => enum_hover(enum_fact),
        SourceSchemaHoverFact::EnumMember(member) => enum_member_hover(member),
    }
}

fn resource_hover(fact: &SourceResourceHoverFact) -> String {
    let mut value = marrow_code_block(&resource_signature(&fact.name));
    append_docs(&mut value, join_docs(&fact.docs));
    append_section(&mut value, resource_member_summary(&fact.members));
    value
}

fn enum_hover(fact: &SourceEnumHoverFact) -> String {
    let mut value = marrow_code_block(&format!("enum {}", fact.name));
    append_docs(&mut value, join_docs(&fact.docs));
    append_section(&mut value, enum_member_summary(fact));
    value
}

fn enum_member_hover(fact: &SourceEnumMemberHoverFact) -> String {
    let mut value = marrow_code_block(&format!("{}::{}", fact.enum_name, fact.path.join("::")));
    append_docs(&mut value, join_docs(&fact.docs));
    value.push_str("\n\n");
    value.push_str("**Facts**\n");
    value.push_str(&format!("- enum {}\n", fact.enum_name));
    value.push_str(&format!("- {}", enum_member_status(fact.status)));
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

fn source_std_capability_label(capability: SourceStandardLibraryCapability) -> &'static str {
    match capability {
        SourceStandardLibraryCapability::Clock => "clock",
        SourceStandardLibraryCapability::Context => "context",
        SourceStandardLibraryCapability::Environment => "environment",
        SourceStandardLibraryCapability::Log => "log",
        SourceStandardLibraryCapability::Filesystem => "filesystem",
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

fn function_signature(program: &CheckedProgram, function: &SourceCallableFunctionFact) -> String {
    let params = function
        .params
        .iter()
        .map(|param| format!("{}: {}", param.name, render_type(program, &param.ty)))
        .collect::<Vec<_>>()
        .join(", ");
    match &function.return_type {
        Some(ty) => format!(
            "fn {}({params}): {}",
            function.name,
            render_type(program, ty)
        ),
        None => format!("fn {}({params})", function.name),
    }
}

fn parameter_signature(program: &CheckedProgram, param: &SourceCallableParamFact) -> String {
    format!("{}: {}", param.name, render_type(program, &param.ty))
}

fn module_const_signature(program: &CheckedProgram, name: &str, ty: Option<&MarrowType>) -> String {
    match ty {
        Some(ty) => format!("const {}: {}", name, render_type(program, ty)),
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

fn resource_signature(name: &str) -> String {
    format!("resource {name}")
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

fn resource_member_summary(members: &[SourceResourceHoverMember]) -> Option<String> {
    bounded_summary(
        "Members",
        members.iter().map(resource_member_line).collect(),
    )
}

fn resource_member_line(member: &SourceResourceHoverMember) -> String {
    let path = source_resource_member_path(&member.path);
    match &member.kind {
        SourceResourceHoverMemberKind::Field { required, ty } => {
            let required = if *required { "required " } else { "" };
            format!("{required}{path}: {ty}")
        }
        SourceResourceHoverMemberKind::Layer => path,
    }
}

fn source_resource_member_path(path: &[SourceResourceHoverPathSegment]) -> String {
    path.iter()
        .map(|segment| {
            format!(
                "{}{}",
                segment.name,
                source_hover_key_params(&segment.key_params)
            )
        })
        .collect::<Vec<_>>()
        .join(".")
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

fn enum_member_summary(fact: &SourceEnumHoverFact) -> Option<String> {
    bounded_summary(
        "Members",
        fact.members
            .iter()
            .map(|member| {
                format!(
                    "{} {}",
                    member.path.join("::"),
                    enum_member_status(member.status)
                )
            })
            .collect(),
    )
}

fn enum_member_status(status: SourceEnumMemberStatus) -> &'static str {
    match status {
        SourceEnumMemberStatus::Selectable => "selectable",
        SourceEnumMemberStatus::Category => "category",
        SourceEnumMemberStatus::Group => "group",
    }
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

fn source_hover_key_params(keys: &[SourceSchemaHoverKeyParam]) -> String {
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
