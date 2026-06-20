use marrow_schema::{scalar_type_from_name, stdlib};

pub(crate) struct BareBuiltin {
    pub(crate) name: &'static str,
    pub(crate) detail: &'static str,
}

pub(crate) struct OperatorFact {
    pub(crate) spelling: &'static str,
    pub(crate) description: &'static str,
}

const BARE_FUNCTION_BUILTINS: &[BareBuiltin] = &[
    BareBuiltin {
        name: "exists",
        detail: "exists(path): bool",
    },
    BareBuiltin {
        name: "keys",
        detail: "keys(layer): sequence",
    },
    BareBuiltin {
        name: "values",
        detail: "values(layer): sequence",
    },
    BareBuiltin {
        name: "entries",
        detail: "entries(layer): sequence",
    },
    BareBuiltin {
        name: "count",
        detail: "count(layer): int",
    },
    BareBuiltin {
        name: "reversed",
        detail: "reversed(sequence): sequence",
    },
    BareBuiltin {
        name: "next",
        detail: "next(path): value",
    },
    BareBuiltin {
        name: "prev",
        detail: "prev(path): value",
    },
    BareBuiltin {
        name: "append",
        detail: "append(layer, value): int",
    },
    BareBuiltin {
        name: "nextId",
        detail: "nextId(^root): Id",
    },
    BareBuiltin {
        name: "write",
        detail: "write(value)",
    },
    BareBuiltin {
        name: "print",
        detail: "print(value)",
    },
    BareBuiltin {
        name: "Error",
        detail: "Error(code: ErrorCode, message: string): Error",
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
    scalar_type_from_name(name).map(|scalar| format!("{name}(value): {}", scalar.name()))
}

pub(crate) fn std_namespace_hover() -> String {
    let modules = std_modules().join(", ");
    format!("std\n\ndefault library namespace.\n\nModules: {modules}")
}

pub(crate) fn std_module_hover(module: &str) -> Option<String> {
    let ops = stdlib::all()
        .iter()
        .filter(|op| op.module == module)
        .map(|op| format!("{} ({})", op.op, capability_label(op.requires_capability)))
        .collect::<Vec<_>>();
    if ops.is_empty() {
        return None;
    }
    Some(format!(
        "std::{module}\n\ndefault library module.\n\nOperations: {}",
        ops.join(", ")
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

fn std_modules() -> Vec<&'static str> {
    let mut modules = Vec::new();
    for op in stdlib::all() {
        if !modules.contains(&op.module) {
            modules.push(op.module);
        }
    }
    modules
}

fn capability_label(capability: Option<stdlib::Capability>) -> &'static str {
    match capability {
        None => "pure",
        Some(stdlib::Capability::Clock) => "clock",
        Some(stdlib::Capability::Context) => "context",
        Some(stdlib::Capability::Environment) => "environment",
        Some(stdlib::Capability::Log) => "log",
        Some(stdlib::Capability::Filesystem) => "filesystem",
    }
}
