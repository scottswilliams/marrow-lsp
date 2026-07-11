use lsp_types::{
    Documentation, MarkupContent, MarkupKind, ParameterInformation, ParameterLabel, SignatureHelp,
    SignatureInformation,
};
use marrow_check::CheckedProgram;
use marrow_check::tooling::{
    CallableArgumentStyle, CallableValueShape, SourceSignatureHelpCallable,
    SourceSignatureHelpFact, SourceSignatureHelpParameter,
};

use crate::callables::render_callable_shape;
use crate::types::render_type;

struct Signature {
    label: String,
    documentation: Option<String>,
    params: Vec<Parameter>,
}

struct Parameter {
    name: Option<String>,
    label: String,
    documentation: Option<String>,
}

pub(super) fn help(program: &CheckedProgram, fact: SourceSignatureHelpFact) -> SignatureHelp {
    let signature = render_signature(program, fact.callable);
    let active = active_parameter(
        &signature.params,
        fact.active_argument,
        fact.named_argument.as_deref(),
    );
    SignatureHelp {
        signatures: vec![SignatureInformation {
            label: signature.label,
            documentation: markdown_documentation(signature.documentation),
            parameters: Some(
                signature
                    .params
                    .into_iter()
                    .map(|param| ParameterInformation {
                        label: ParameterLabel::Simple(param.label),
                        documentation: markdown_documentation(param.documentation),
                    })
                    .collect(),
            ),
            active_parameter: None,
        }],
        active_signature: Some(0),
        active_parameter: active,
    }
}

fn render_signature(program: &CheckedProgram, callable: SourceSignatureHelpCallable) -> Signature {
    match callable {
        SourceSignatureHelpCallable::Intrinsic {
            path,
            argument_style,
            docs,
            params,
            return_shape,
        } => render_intrinsic(program, path, argument_style, docs, params, return_shape),
        SourceSignatureHelpCallable::ResourceConstructor {
            name,
            docs,
            params,
            return_type,
        } => {
            let params = params
                .into_iter()
                .map(|param| render_typed_parameter(program, param))
                .collect::<Vec<_>>();
            Signature {
                label: format!(
                    "{}({}): {}",
                    name,
                    joined_param_labels(&params),
                    render_type(program, &return_type)
                ),
                documentation: join_docs(&docs),
                params,
            }
        }
        SourceSignatureHelpCallable::Function {
            name,
            docs,
            params,
            return_type,
        } => {
            let params = params
                .into_iter()
                .map(|param| render_typed_parameter(program, param))
                .collect::<Vec<_>>();
            let params_label = joined_param_labels(&params);
            let label = match return_type {
                Some(ty) => format!("{}({}): {}", name, params_label, render_type(program, &ty)),
                None => format!("{}({})", name, params_label),
            };
            Signature {
                label,
                documentation: join_docs(&docs),
                params,
            }
        }
    }
}

fn render_intrinsic(
    program: &CheckedProgram,
    path: Vec<String>,
    argument_style: CallableArgumentStyle,
    docs: Vec<String>,
    params: Vec<SourceSignatureHelpParameter>,
    return_shape: Option<CallableValueShape>,
) -> Signature {
    let params = params
        .into_iter()
        .map(|param| render_intrinsic_parameter(program, param, argument_style))
        .collect::<Vec<_>>();
    let path = path.join("::");
    let label = match return_shape
        .as_ref()
        .map(|shape| render_callable_shape(program, shape))
    {
        Some(return_shape) => format!(
            "{}({}): {}",
            path,
            joined_param_labels(&params),
            return_shape
        ),
        None => format!("{}({})", path, joined_param_labels(&params)),
    };
    Signature {
        label,
        documentation: join_docs(&docs),
        params,
    }
}

fn render_intrinsic_parameter(
    program: &CheckedProgram,
    param: SourceSignatureHelpParameter,
    style: CallableArgumentStyle,
) -> Parameter {
    let label = match style {
        CallableArgumentStyle::Positional => {
            let label = match param.shape {
                Some(CallableValueShape::SavedRoot) if param.label == "root" => "^root".to_string(),
                _ => param.label,
            };
            if param.repeat {
                format!("{label}...")
            } else {
                label
            }
        }
        CallableArgumentStyle::NamedFields => {
            let shape = param
                .shape
                .as_ref()
                .map(|shape| render_callable_shape(program, shape))
                .unwrap_or_else(|| "unknown".to_string());
            format!("{}: {}", param.label, shape)
        }
    };
    Parameter {
        name: param.name,
        label,
        documentation: join_docs(&param.docs),
    }
}

fn render_typed_parameter(
    program: &CheckedProgram,
    param: SourceSignatureHelpParameter,
) -> Parameter {
    let label = match param.ty {
        Some(ty) => format!("{}: {}", param.label, render_type(program, &ty)),
        None => param.label,
    };
    Parameter {
        name: param.name,
        label,
        documentation: join_docs(&param.docs),
    }
}

fn joined_param_labels(params: &[Parameter]) -> String {
    params
        .iter()
        .map(|param| param.label.as_str())
        .collect::<Vec<_>>()
        .join(", ")
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

fn markdown_documentation(value: Option<String>) -> Option<Documentation> {
    value.map(|value| {
        Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        })
    })
}

fn active_parameter(
    params: &[Parameter],
    comma_index: usize,
    named_argument: Option<&str>,
) -> Option<u32> {
    if params.is_empty() {
        return None;
    }
    if let Some(name) = named_argument
        && let Some(index) = params
            .iter()
            .position(|param| param.name.as_deref() == Some(name))
    {
        return Some(index as u32);
    }
    Some(comma_index.min(params.len() - 1) as u32)
}
