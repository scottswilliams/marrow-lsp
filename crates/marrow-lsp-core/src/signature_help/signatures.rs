use std::path::Path;

use marrow_check::{
    AnalysisSnapshot, CheckedFunction, CheckedParamMode, CheckedProgram, Def, DefItem, Resolution,
    ResolvableKind, resolve,
};
use marrow_schema::{NodeKind, ResourceSchema, stdlib};
use marrow_syntax::{Declaration, FunctionDecl, SourceSpan};

use crate::{language_facts, types::render_type};

pub(super) struct Signature {
    pub(super) label: String,
    pub(super) documentation: Option<String>,
    pub(super) params: Vec<Parameter>,
}

pub(super) struct Parameter {
    pub(super) name: Option<String>,
    pub(super) label: String,
    pub(super) documentation: Option<String>,
}

pub(super) fn signature_for(
    program: &CheckedProgram,
    docs: Option<&AnalysisSnapshot>,
    file: &Path,
    segments: &[String],
) -> Option<Signature> {
    let from_module = module_of_file(program, file)?;
    if let Some(signature) = builtin_signature(segments) {
        return Some(signature);
    }
    if let Some(signature) = resource_signature(program, from_module, segments) {
        return Some(signature);
    }
    function_signature(program, docs, from_module, segments)
}

fn builtin_signature(segments: &[String]) -> Option<Signature> {
    if let [first, module, op] = segments
        && first == "std"
    {
        return stdlib::lookup(module, op).map(std_signature);
    }

    let [name] = segments else {
        return None;
    };
    if let Some(builtin) = language_facts::bare_function_builtins()
        .iter()
        .find(|builtin| builtin.name == name)
    {
        let mut signature = signature_from_label(builtin.detail);
        signature.documentation = Some(builtin.description.to_string());
        return Some(signature);
    }
    language_facts::scalar_conversion_detail(name).map(|detail| signature_from_label(&detail))
}

fn resource_signature(
    program: &CheckedProgram,
    from_module: &str,
    segments: &[String],
) -> Option<Signature> {
    match resolve(program, from_module, segments, ResolvableKind::Resource) {
        Resolution::Found(Def {
            item: DefItem::Resource(resource),
            ..
        }) => Some(resource_constructor_signature(resource)),
        _ => None,
    }
}

fn resource_constructor_signature(resource: &ResourceSchema) -> Signature {
    let params =
        resource
            .members
            .iter()
            .filter_map(|node| match &node.kind {
                NodeKind::Slot { ty, .. } if node.key_params.is_empty() => Some(
                    named_param_with_docs(&node.name, &ty.to_string(), join_docs(&node.docs)),
                ),
                _ => None,
            })
            .collect::<Vec<_>>();
    Signature {
        label: format!(
            "{}({}): {}",
            resource.name,
            joined_param_labels(&params),
            resource.name
        ),
        documentation: join_docs(&resource.docs),
        params,
    }
}

fn function_signature(
    program: &CheckedProgram,
    docs: Option<&AnalysisSnapshot>,
    from_module: &str,
    segments: &[String],
) -> Option<Signature> {
    match resolve(program, from_module, segments, ResolvableKind::Function) {
        Resolution::Found(def) => match def.item {
            DefItem::Function(function) => {
                let docs = docs.and_then(|snapshot| {
                    function_decl(snapshot, &def.module.source_file, function.span)
                });
                Some(checked_function_signature(function, docs))
            }
            _ => None,
        },
        _ => None,
    }
}

fn checked_function_signature(
    function: &CheckedFunction,
    docs: Option<&FunctionDecl>,
) -> Signature {
    let params = function
        .params
        .iter()
        .enumerate()
        .map(|(index, param)| {
            let mode = match param.mode {
                Some(CheckedParamMode::InOut) => "inout ",
                None => "",
            };
            let documentation = docs
                .and_then(|function| function.params.get(index))
                .filter(|decl| decl.name == param.name)
                .and_then(|decl| join_docs(&decl.docs));
            Parameter {
                name: Some(param.name.clone()),
                label: format!("{mode}{}: {}", param.name, render_type(&param.ty)),
                documentation,
            }
        })
        .collect::<Vec<_>>();
    let params_label = joined_param_labels(&params);
    let label = match &function.return_type {
        Some(ty) => format!("{}({params_label}): {}", function.name, render_type(ty)),
        None => format!("{}({params_label})", function.name),
    };
    Signature {
        label,
        documentation: docs.and_then(|function| join_docs(&function.docs)),
        params,
    }
}

fn std_signature(op: &stdlib::StdOp) -> Signature {
    let params = op
        .params
        .iter()
        .map(|param| Parameter {
            name: None,
            label: std_param_label(param),
            documentation: None,
        })
        .collect::<Vec<_>>();
    let params_label = joined_param_labels(&params);
    let label = match std_return_label(&op.ret) {
        Some(ret) => format!("std::{}::{}({params_label}): {ret}", op.module, op.op),
        None => format!("std::{}::{}({params_label})", op.module, op.op),
    };
    Signature {
        label,
        documentation: None,
        params,
    }
}

fn signature_from_label(label: &str) -> Signature {
    let params = label
        .split_once('(')
        .and_then(|(_, rest)| rest.split_once(')'))
        .map(|(params, _)| {
            if params.trim().is_empty() {
                Vec::new()
            } else {
                params
                    .split(',')
                    .map(|param| {
                        let label = param.trim().to_string();
                        let name = label
                            .split_once(':')
                            .map(|(name, _)| name.trim().to_string())
                            .or_else(|| Some(label.clone()));
                        Parameter {
                            name,
                            label,
                            documentation: None,
                        }
                    })
                    .collect()
            }
        })
        .unwrap_or_default();
    Signature {
        label: label.to_string(),
        documentation: None,
        params,
    }
}

fn named_param_with_docs(name: &str, ty: &str, documentation: Option<String>) -> Parameter {
    Parameter {
        name: Some(name.to_string()),
        label: format!("{name}: {ty}"),
        documentation,
    }
}

fn joined_param_labels(params: &[Parameter]) -> String {
    params
        .iter()
        .map(|param| param.label.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn std_param_label(param: &stdlib::ParamType) -> String {
    match param {
        stdlib::ParamType::Scalar(scalar) => scalar.name().to_string(),
        stdlib::ParamType::Error => "Error".to_string(),
        stdlib::ParamType::Path => "path".to_string(),
    }
}

fn std_return_label(ret: &stdlib::ReturnType) -> Option<String> {
    match ret {
        stdlib::ReturnType::Scalar(scalar) => Some(scalar.name().to_string()),
        stdlib::ReturnType::Sequence(scalar) => Some(format!("sequence[{}]", scalar.name())),
        stdlib::ReturnType::Void => None,
    }
}

fn function_decl<'a>(
    snapshot: &'a AnalysisSnapshot,
    file: &Path,
    span: SourceSpan,
) -> Option<&'a FunctionDecl> {
    let analyzed = snapshot
        .files
        .iter()
        .find(|file_info| file_info.path == file)?;
    analyzed
        .parsed
        .file
        .declarations
        .iter()
        .find_map(|declaration| match declaration {
            Declaration::Function(function) if function.span == span => Some(function),
            _ => None,
        })
}

fn join_docs(lines: &[String]) -> Option<String> {
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

fn module_of_file<'p>(program: &'p CheckedProgram, file: &Path) -> Option<&'p str> {
    program
        .modules
        .iter()
        .find(|module| module.source_file == file)
        .map(|module| module.name.as_str())
}
