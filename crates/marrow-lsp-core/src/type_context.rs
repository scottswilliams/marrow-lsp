use marrow_syntax::{Declaration, EvolveStep, ResourceMember, SourceFile, Statement, TypeRef};

pub(crate) fn type_annotation_at(file: &SourceFile, offset: usize) -> bool {
    file.declarations
        .iter()
        .any(|declaration| declaration_type_annotation_at(declaration, offset))
}

fn declaration_type_annotation_at(declaration: &Declaration, offset: usize) -> bool {
    match declaration {
        Declaration::Const(declaration) => declaration
            .ty
            .as_ref()
            .is_some_and(|ty| type_ref_covers(ty, offset)),
        Declaration::Resource(resource) => resource
            .members
            .iter()
            .any(|member| resource_member_type_annotation_at(member, offset)),
        Declaration::Store(store) => store
            .root
            .keys
            .iter()
            .any(|key| type_ref_covers(&key.ty, offset)),
        Declaration::Function(function) => {
            function
                .params
                .iter()
                .any(|param| type_ref_covers(&param.ty, offset))
                || function
                    .return_type
                    .as_ref()
                    .is_some_and(|ty| type_ref_covers(ty, offset))
                || block_type_annotation_at(&function.body, offset)
        }
        Declaration::Evolve(evolve) => evolve
            .steps
            .iter()
            .any(|step| evolve_step_type_annotation_at(step, offset)),
        Declaration::Enum(_) => false,
    }
}

fn evolve_step_type_annotation_at(step: &EvolveStep, offset: usize) -> bool {
    match step {
        EvolveStep::Transform { body, .. } => block_type_annotation_at(body, offset),
        EvolveStep::Rename { .. } | EvolveStep::Default { .. } | EvolveStep::Retire { .. } => false,
    }
}

fn resource_member_type_annotation_at(member: &ResourceMember, offset: usize) -> bool {
    match member {
        ResourceMember::Field(field) => {
            type_ref_covers(&field.ty, offset)
                || field
                    .keys
                    .iter()
                    .any(|key| type_ref_covers(&key.ty, offset))
        }
        ResourceMember::Group(group) => {
            group
                .keys
                .iter()
                .any(|key| type_ref_covers(&key.ty, offset))
                || group
                    .members
                    .iter()
                    .any(|member| resource_member_type_annotation_at(member, offset))
        }
    }
}

fn block_type_annotation_at(block: &marrow_syntax::Block, offset: usize) -> bool {
    block
        .statements
        .iter()
        .any(|statement| statement_type_annotation_at(statement, offset))
}

fn statement_type_annotation_at(statement: &Statement, offset: usize) -> bool {
    match statement {
        Statement::Const { ty, .. } => ty.as_ref().is_some_and(|ty| type_ref_covers(ty, offset)),
        Statement::Var { keys, ty, .. } => {
            keys.iter().any(|key| type_ref_covers(&key.ty, offset))
                || ty.as_ref().is_some_and(|ty| type_ref_covers(ty, offset))
        }
        Statement::If {
            then_block,
            else_ifs,
            else_block,
            ..
        } => {
            block_type_annotation_at(then_block, offset)
                || else_ifs
                    .iter()
                    .any(|else_if| block_type_annotation_at(&else_if.block, offset))
                || else_block
                    .as_ref()
                    .is_some_and(|block| block_type_annotation_at(block, offset))
        }
        Statement::While { body, .. }
        | Statement::For { body, .. }
        | Statement::Transaction { body, .. } => block_type_annotation_at(body, offset),
        Statement::Try {
            body,
            catch,
            finally,
            ..
        } => {
            block_type_annotation_at(body, offset)
                || catch.as_ref().is_some_and(|catch| {
                    catch
                        .ty
                        .as_ref()
                        .is_some_and(|ty| type_ref_covers(ty, offset))
                        || block_type_annotation_at(&catch.block, offset)
                })
                || finally
                    .as_ref()
                    .is_some_and(|block| block_type_annotation_at(block, offset))
        }
        Statement::Match { arms, .. } => arms
            .iter()
            .any(|arm| block_type_annotation_at(&arm.block, offset)),
        Statement::Assign { .. }
        | Statement::Delete { .. }
        | Statement::Return { .. }
        | Statement::Break { .. }
        | Statement::Continue { .. }
        | Statement::Throw { .. }
        | Statement::Expr { .. } => false,
    }
}

fn type_ref_covers(ty: &TypeRef, offset: usize) -> bool {
    ty.span.start_byte <= offset && offset <= ty.span.end_byte
}
