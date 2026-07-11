//! LSP-local wrapper around Marrow's source-facing type spelling.

use marrow_check::{CheckedProgram, MarrowType, tooling::render_marrow_type};

pub(crate) fn render_type(program: &CheckedProgram, ty: &MarrowType) -> String {
    render_marrow_type(program, ty)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_tree_types_render_as_tree_of_value() {
        let program = CheckedProgram::default();
        let ty = MarrowType::LocalTree {
            keys: vec![MarrowType::Primitive(marrow_schema::ScalarType::Str)],
            value: Box::new(MarrowType::Primitive(marrow_schema::ScalarType::Int)),
        };

        assert_eq!(render_type(&program, &ty), "tree[int]");
    }

    #[test]
    fn explicit_dynamic_types_render_as_unknown() {
        assert_eq!(
            render_type(&CheckedProgram::default(), &MarrowType::Dynamic),
            "unknown"
        );
    }
}
