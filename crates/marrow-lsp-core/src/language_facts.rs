use marrow_schema::{ScalarType, stdlib};
use marrow_syntax::Keyword;

pub(crate) struct BareBuiltin {
    pub(crate) name: &'static str,
    pub(crate) detail: &'static str,
    pub(crate) description: &'static str,
}

pub(crate) struct OperatorFact {
    pub(crate) spelling: &'static str,
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

const OPERATOR_FACTS: &[OperatorFact] = &[
    OperatorFact {
        spelling: "not",
        description: "logical negation.",
    },
    OperatorFact {
        spelling: "and",
        description: "logical conjunction.",
    },
    OperatorFact {
        spelling: "or",
        description: "logical disjunction.",
    },
    OperatorFact {
        spelling: "is",
        description: "type test.",
    },
    OperatorFact {
        spelling: "==",
        description: "equality comparison.",
    },
    OperatorFact {
        spelling: "!=",
        description: "inequality comparison.",
    },
    OperatorFact {
        spelling: "<",
        description: "less-than comparison.",
    },
    OperatorFact {
        spelling: "<=",
        description: "less-than-or-equal comparison.",
    },
    OperatorFact {
        spelling: ">",
        description: "greater-than comparison.",
    },
    OperatorFact {
        spelling: ">=",
        description: "greater-than-or-equal comparison.",
    },
    OperatorFact {
        spelling: "+",
        description: "addition.",
    },
    OperatorFact {
        spelling: "-",
        description: "subtraction or numeric negation.",
    },
    OperatorFact {
        spelling: "*",
        description: "multiplication.",
    },
    OperatorFact {
        spelling: "/",
        description: "division.",
    },
    OperatorFact {
        spelling: "%",
        description: "remainder.",
    },
    OperatorFact {
        spelling: "_",
        description: "string concatenation.",
    },
    OperatorFact {
        spelling: "??",
        description: "fallback value selection.",
    },
    OperatorFact {
        spelling: "?.",
        description: "optional member access.",
    },
    OperatorFact {
        spelling: "..",
        description: "exclusive range.",
    },
    OperatorFact {
        spelling: "..=",
        description: "inclusive range.",
    },
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
    if name == "ErrorCode" {
        return Some("ErrorCode(value): ErrorCode".to_string());
    }
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
    scalar_conversion_detail(name)
        .map(|detail| format!("{detail}\n\ndefault library scalar conversion."))
}

pub(crate) fn std_namespace_hover() -> String {
    let modules = std_modules().join(", ");
    format!("std\n\ndefault library namespace.\n\nModules: {modules}")
}

pub(crate) fn std_module_hover(module: &str) -> Option<String> {
    let ops = stdlib::all()
        .iter()
        .filter(|op| op.module == module)
        .map(|op| format!("{} ({})", op.op, capability_label(op.capability)))
        .collect::<Vec<_>>();
    if ops.is_empty() {
        return None;
    }
    Some(format!(
        "std::{module}\n\ndefault library module.\n\nOperations: {}",
        ops.join(", ")
    ))
}

pub(crate) fn std_operation_hover(module: &str, op: &str) -> Option<String> {
    let op = stdlib::lookup(module, op)?;
    Some(format!(
        "{}\n\ndefault library std operation.\n\nCapability: {}",
        std_signature(op),
        capability_label(op.capability)
    ))
}

pub(crate) fn operator_hover(spelling: &str) -> Option<String> {
    OPERATOR_FACTS
        .iter()
        .find(|fact| fact.spelling == spelling)
        .map(|fact| {
            format!(
                "operator {}\n\nlanguage operator.\n\n{}",
                fact.spelling, fact.description
            )
        })
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

pub(crate) fn keyword_is_callable_path_segment(keyword: Keyword) -> bool {
    keyword_callable_spelling(keyword)
        .and_then(bare_builtin_kind)
        .is_some()
}

fn keyword_callable_spelling(keyword: Keyword) -> Option<&'static str> {
    match keyword {
        Keyword::Int => Some("int"),
        Keyword::Decimal => Some("decimal"),
        Keyword::Bool => Some("bool"),
        Keyword::String => Some("string"),
        Keyword::Bytes => Some("bytes"),
        Keyword::Date => Some("date"),
        Keyword::Instant => Some("instant"),
        Keyword::Duration => Some("duration"),
        Keyword::ErrorCode => Some("ErrorCode"),
        Keyword::Error => Some("Error"),
        _ => None,
    }
}

fn std_modules() -> Vec<&'static str> {
    let mut modules = Vec::new();
    for op in stdlib::all() {
        if !modules.contains(&op.module) {
            modules.push(op.module);
        }
    }
    modules
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
