use marrow_schema::{ScalarType, stdlib};

pub(crate) struct BareBuiltin {
    pub(crate) name: &'static str,
    pub(crate) detail: &'static str,
    pub(crate) description: &'static str,
}

const BARE_FUNCTION_BUILTINS: &[BareBuiltin] = &[
    BareBuiltin {
        name: "exists",
        detail: "exists(path): bool",
        description: "Returns true when the saved path exists.",
    },
    BareBuiltin {
        name: "keys",
        detail: "keys(layer): sequence",
        description: "Returns the keys in a layer.",
    },
    BareBuiltin {
        name: "values",
        detail: "values(layer): sequence",
        description: "Returns the values in a layer.",
    },
    BareBuiltin {
        name: "entries",
        detail: "entries(layer): sequence",
        description: "Returns the entries in a layer.",
    },
    BareBuiltin {
        name: "count",
        detail: "count(layer): int",
        description: "Returns child count for a saved path, 1 for a scalar, or 0 when absent.",
    },
    BareBuiltin {
        name: "reversed",
        detail: "reversed(sequence): sequence",
        description: "Returns the sequence in reverse order.",
    },
    BareBuiltin {
        name: "next",
        detail: "next(path): value",
        description: "Returns the next key after a saved path.",
    },
    BareBuiltin {
        name: "prev",
        detail: "prev(path): value",
        description: "Returns the previous key before a saved path.",
    },
    BareBuiltin {
        name: "append",
        detail: "append(layer, value): int",
        description: "Appends a value to a layer and returns its key.",
    },
    BareBuiltin {
        name: "nextId",
        detail: "nextId(^root): Id",
        description: "Returns the next id for a saved root.",
    },
    BareBuiltin {
        name: "write",
        detail: "write(value)",
        description: "Writes rendered text to output without a newline.",
    },
    BareBuiltin {
        name: "print",
        detail: "print(value)",
        description: "Writes rendered text to output with a newline.",
    },
    BareBuiltin {
        name: "Error",
        detail: "Error(code: ErrorCode, message: string): Error",
        description: "Constructs an Error value.",
    },
];

const SCALAR_CONVERSION_NAMES: &[&str] = &[
    "bool",
    "int",
    "string",
    "bytes",
    "ErrorCode",
    "date",
    "instant",
    "duration",
    "decimal",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BareBuiltinKind {
    Function,
    ScalarConversion,
}

pub(crate) fn bare_function_builtins() -> &'static [BareBuiltin] {
    BARE_FUNCTION_BUILTINS
}

pub(crate) fn scalar_conversion_names() -> &'static [&'static str] {
    SCALAR_CONVERSION_NAMES
}

pub(crate) fn scalar_conversion_detail(name: &str) -> Option<String> {
    ScalarType::from_scalar_name(name).map(|scalar| format!("{name}(value): {}", scalar.name()))
}

pub(crate) fn bare_builtin_hover(name: &str) -> Option<String> {
    bare_function_builtins()
        .iter()
        .find(|builtin| builtin.name == name)
        .map(|builtin| {
            format!(
                "{}\n\ndefault library builtin.\n\n{}",
                builtin.detail, builtin.description
            )
        })
}

pub(crate) fn scalar_conversion_hover(name: &str) -> Option<String> {
    if name == "ErrorCode" {
        return None;
    }
    scalar_conversion_detail(name)
        .map(|detail| format!("{detail}\n\ndefault library scalar conversion."))
}

pub(crate) fn std_operation_hover(module: &str, op: &str) -> Option<String> {
    let op = stdlib::lookup(module, op)?;
    Some(format!(
        "{}\n\ndefault library std operation.\n\nCapability: {}",
        std_signature(op),
        capability_label(op.capability)
    ))
}

pub(crate) fn bare_builtin_kind(name: &str) -> Option<BareBuiltinKind> {
    if ScalarType::from_scalar_name(name).is_some() {
        return Some(BareBuiltinKind::ScalarConversion);
    }
    bare_function_builtins()
        .iter()
        .any(|builtin| builtin.name == name)
        .then_some(BareBuiltinKind::Function)
}

fn std_signature(op: &stdlib::StdOp) -> String {
    let params = op
        .params
        .iter()
        .map(param_type_name)
        .collect::<Vec<_>>()
        .join(", ");
    match return_type_name(&op.ret) {
        Some(ret) => format!("std::{}::{}({params}): {ret}", op.module, op.op),
        None => format!("std::{}::{}({params})", op.module, op.op),
    }
}

fn param_type_name(param: &stdlib::ParamType) -> String {
    match param {
        stdlib::ParamType::Scalar(scalar) => scalar.name().to_string(),
        stdlib::ParamType::Error => "Error".to_string(),
        stdlib::ParamType::Path => "path".to_string(),
    }
}

fn return_type_name(ret: &stdlib::ReturnType) -> Option<String> {
    match ret {
        stdlib::ReturnType::Scalar(scalar) => Some(scalar.name().to_string()),
        stdlib::ReturnType::Sequence(scalar) => Some(format!("sequence[{}]", scalar.name())),
        stdlib::ReturnType::Void => None,
    }
}

fn capability_label(capability: stdlib::Capability) -> &'static str {
    match capability {
        stdlib::Capability::Pure => "pure",
        stdlib::Capability::Clock => "clock",
        stdlib::Capability::Env => "env",
        stdlib::Capability::Log => "log",
        stdlib::Capability::Io => "io",
        stdlib::Capability::Assert => "assert",
    }
}
