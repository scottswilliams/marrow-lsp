pub(crate) struct OperatorFact {
    pub(crate) spelling: &'static str,
    pub(crate) description: &'static str,
}

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
