use marrow_check::tooling::{
    CallableArgumentStyle, CallableParameter, CallableSignature, CallableValueShape,
};

use crate::types::render_type;

pub(crate) fn render_callable_signature(callable: &CallableSignature) -> String {
    let params = callable
        .params
        .iter()
        .map(|param| render_callable_parameter_label(param, callable.argument_style))
        .collect::<Vec<_>>()
        .join(", ");
    let path = callable.path.join("::");
    match callable.return_shape.as_ref().map(render_callable_shape) {
        Some(return_shape) => format!("{path}({params}): {return_shape}"),
        None => format!("{path}({params})"),
    }
}

pub(crate) fn render_callable_parameter_label(
    param: &CallableParameter,
    style: CallableArgumentStyle,
) -> String {
    match style {
        CallableArgumentStyle::Positional => {
            let label = match param.shape {
                CallableValueShape::SavedRoot if param.label == "root" => "^root".to_string(),
                _ => param.label.clone(),
            };
            if param.repeat {
                format!("{label}...")
            } else {
                label
            }
        }
        CallableArgumentStyle::NamedFields => {
            format!("{}: {}", param.label, render_callable_shape(&param.shape))
        }
    }
}

pub(crate) fn render_callable_shape(shape: &CallableValueShape) -> String {
    match shape {
        CallableValueShape::Type(ty) => render_type(ty),
        CallableValueShape::Scalar => "scalar".to_string(),
        CallableValueShape::Value => "value".to_string(),
        CallableValueShape::Sequence => "sequence".to_string(),
        CallableValueShape::Collection => "collection".to_string(),
        CallableValueShape::SavedPath => "path".to_string(),
        CallableValueShape::SavedLayer => "layer".to_string(),
        CallableValueShape::SavedRoot => "^root".to_string(),
        CallableValueShape::Identity => "Id".to_string(),
        CallableValueShape::ErrorCode => "ErrorCode".to_string(),
    }
}
