use lsp_types::{
    Documentation, MarkupContent, MarkupKind, ParameterInformation, ParameterLabel, SignatureHelp,
    SignatureInformation,
};

use super::signatures::{Parameter, Signature};

pub(super) fn help(
    signature: Signature,
    active: usize,
    named_argument: Option<String>,
) -> SignatureHelp {
    let active = active_parameter(&signature.params, active, named_argument.as_deref());
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
