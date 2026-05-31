use marrow_schema::ScalarType;

pub(crate) struct BareBuiltin {
    pub(crate) name: &'static str,
    pub(crate) detail: &'static str,
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

pub(crate) fn bare_builtin_kind(name: &str) -> Option<BareBuiltinKind> {
    if ScalarType::from_scalar_name(name).is_some() {
        return Some(BareBuiltinKind::ScalarConversion);
    }
    bare_function_builtins()
        .iter()
        .any(|builtin| builtin.name == name)
        .then_some(BareBuiltinKind::Function)
}
