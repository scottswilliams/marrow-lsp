use std::path::Path;

use marrow_check::{
    AnalysisSnapshot, CheckedFunction, CheckedProgram, DefItem, Resolution, ResolvableKind,
    resolve,
    tooling::{self, CallableArgumentStyle, CallableSignature, ResourceConstructorSignature},
};
use marrow_syntax::{Declaration, FunctionDecl, SourceSpan};

use crate::callables::{render_callable_parameter_label, render_callable_signature};
use crate::types::render_type;

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
    snapshot: Option<&AnalysisSnapshot>,
    file: &Path,
    segments: &[String],
) -> Option<Signature> {
    let from_module = module_of_file(program, file)?;
    if let Some(signature) = intrinsic_signature(snapshot, file, segments) {
        return Some(signature);
    }
    if let Some(signature) = resource_signature(program, file, segments) {
        return Some(signature);
    }
    function_signature(program, snapshot, from_module, segments)
}

fn intrinsic_signature(
    snapshot: Option<&AnalysisSnapshot>,
    file: &Path,
    segments: &[String],
) -> Option<Signature> {
    match snapshot {
        Some(snapshot) => tooling::intrinsic_callable_signature_for_file(snapshot, file, segments),
        None => tooling::intrinsic_callable_signature(segments),
    }
    .map(render_intrinsic_signature)
}

fn render_intrinsic_signature(callable: CallableSignature) -> Signature {
    let params = callable
        .params
        .iter()
        .map(|param| render_intrinsic_parameter(param, callable.argument_style))
        .collect::<Vec<_>>();
    Signature {
        label: render_callable_signature(&callable),
        documentation: join_docs(&callable.docs),
        params,
    }
}

fn render_intrinsic_parameter(
    param: &tooling::CallableParameter,
    style: CallableArgumentStyle,
) -> Parameter {
    match style {
        CallableArgumentStyle::Positional => Parameter {
            name: None,
            label: render_callable_parameter_label(param, style),
            documentation: join_docs(&param.docs),
        },
        CallableArgumentStyle::NamedFields => Parameter {
            name: Some(param.label.clone()),
            label: render_callable_parameter_label(param, style),
            documentation: join_docs(&param.docs),
        },
    }
}

fn resource_signature(
    program: &CheckedProgram,
    file: &Path,
    segments: &[String],
) -> Option<Signature> {
    tooling::resource_constructor_signature(program, file, segments)
        .map(render_resource_constructor_signature)
}

fn render_resource_constructor_signature(resource: ResourceConstructorSignature) -> Signature {
    let params = resource
        .fields
        .iter()
        .map(|field| {
            named_param_with_docs(&field.name, &render_type(&field.ty), join_docs(&field.docs))
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
    snapshot: Option<&AnalysisSnapshot>,
    from_module: &str,
    segments: &[String],
) -> Option<Signature> {
    match resolve(program, from_module, segments, ResolvableKind::Function) {
        Resolution::Found(def) => match def.item {
            DefItem::Function(function) => {
                let docs = snapshot.and_then(|snapshot| {
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
            let documentation = docs
                .and_then(|function| function.params.get(index))
                .filter(|decl| decl.name == param.name)
                .and_then(|decl| join_docs(&decl.docs));
            Parameter {
                name: Some(param.name.clone()),
                label: format!("{}: {}", param.name, render_type(&param.ty)),
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
