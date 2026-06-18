use std::collections::HashMap;

use marrow_syntax::{
    Block, ConstDecl, Declaration, EvolveDecl, EvolveStep, FunctionDecl, LexedSource, ResourceDecl,
    ResourceMember, SourceFile, Statement, StoreDecl, SurfaceDecl, TokenKind, TypeRef,
};

use super::{
    ByteSpan, TYPE_STRUCT, TokenStyle,
    syntax::{is_path_segment_token, is_trivia, token_in_span},
};

pub(super) fn type_annotation_overrides(
    lexed: &LexedSource,
    file: &SourceFile,
    source: &str,
) -> HashMap<ByteSpan, TokenStyle> {
    let mut overrides = HashMap::new();
    for declaration in &file.declarations {
        add_declaration_type_annotation_overrides(&mut overrides, lexed, source, declaration);
    }
    overrides
}

fn add_declaration_type_annotation_overrides(
    overrides: &mut HashMap<ByteSpan, TokenStyle>,
    lexed: &LexedSource,
    source: &str,
    declaration: &Declaration,
) {
    match declaration {
        Declaration::Const(const_decl) => {
            add_const_type_annotation_overrides(overrides, lexed, source, const_decl);
        }
        Declaration::Function(function) => {
            add_function_type_annotation_overrides(overrides, lexed, source, function);
        }
        Declaration::Resource(resource) => {
            add_resource_type_annotation_overrides(overrides, lexed, source, resource);
        }
        Declaration::Store(store) => {
            add_store_type_annotation_overrides(overrides, lexed, source, store);
        }
        Declaration::Surface(surface) => {
            add_surface_type_annotation_overrides(overrides, lexed, source, surface);
        }
        Declaration::Evolve(evolve) => {
            add_evolve_type_annotation_overrides(overrides, lexed, source, evolve);
        }
        Declaration::Enum(_) => {}
    }
}

fn add_const_type_annotation_overrides(
    overrides: &mut HashMap<ByteSpan, TokenStyle>,
    lexed: &LexedSource,
    source: &str,
    const_decl: &ConstDecl,
) {
    if let Some(ty) = &const_decl.ty {
        add_type_annotation_overrides(overrides, lexed, source, ty);
    }
}

fn add_function_type_annotation_overrides(
    overrides: &mut HashMap<ByteSpan, TokenStyle>,
    lexed: &LexedSource,
    source: &str,
    function: &FunctionDecl,
) {
    for param in &function.params {
        add_type_annotation_overrides(overrides, lexed, source, &param.ty);
    }
    if let Some(ty) = &function.return_type {
        add_type_annotation_overrides(overrides, lexed, source, ty);
    }
    add_block_type_annotation_overrides(overrides, lexed, source, &function.body);
}

fn add_resource_type_annotation_overrides(
    overrides: &mut HashMap<ByteSpan, TokenStyle>,
    lexed: &LexedSource,
    source: &str,
    resource: &ResourceDecl,
) {
    for member in &resource.members {
        add_resource_member_type_annotation_overrides(overrides, lexed, source, member);
    }
}

fn add_store_type_annotation_overrides(
    overrides: &mut HashMap<ByteSpan, TokenStyle>,
    lexed: &LexedSource,
    source: &str,
    store: &StoreDecl,
) {
    for key in &store.root.keys {
        add_type_annotation_overrides(overrides, lexed, source, &key.ty);
    }
}

fn add_surface_type_annotation_overrides(
    overrides: &mut HashMap<ByteSpan, TokenStyle>,
    lexed: &LexedSource,
    source: &str,
    surface: &SurfaceDecl,
) {
    for key in &surface.store.keys {
        add_type_annotation_overrides(overrides, lexed, source, &key.ty);
    }
}

fn add_evolve_type_annotation_overrides(
    overrides: &mut HashMap<ByteSpan, TokenStyle>,
    lexed: &LexedSource,
    source: &str,
    evolve: &EvolveDecl,
) {
    for step in &evolve.steps {
        add_evolve_step_type_annotation_overrides(overrides, lexed, source, step);
    }
}

fn add_evolve_step_type_annotation_overrides(
    overrides: &mut HashMap<ByteSpan, TokenStyle>,
    lexed: &LexedSource,
    source: &str,
    step: &EvolveStep,
) {
    match step {
        EvolveStep::Transform { body, .. } => {
            add_block_type_annotation_overrides(overrides, lexed, source, body);
        }
        EvolveStep::Rename { .. } | EvolveStep::Default { .. } | EvolveStep::Retire { .. } => {}
    }
}

fn add_block_type_annotation_overrides(
    overrides: &mut HashMap<ByteSpan, TokenStyle>,
    lexed: &LexedSource,
    source: &str,
    block: &Block,
) {
    for statement in &block.statements {
        add_statement_type_annotation_overrides(overrides, lexed, source, statement);
    }
}

fn add_statement_type_annotation_overrides(
    overrides: &mut HashMap<ByteSpan, TokenStyle>,
    lexed: &LexedSource,
    source: &str,
    statement: &Statement,
) {
    match statement {
        Statement::Const { ty, .. } => {
            if let Some(ty) = ty {
                add_type_annotation_overrides(overrides, lexed, source, ty);
            }
        }
        Statement::Var { keys, ty, .. } => {
            for key in keys {
                add_type_annotation_overrides(overrides, lexed, source, &key.ty);
            }
            if let Some(ty) = ty {
                add_type_annotation_overrides(overrides, lexed, source, ty);
            }
        }
        Statement::If {
            then_block,
            else_ifs,
            else_block,
            ..
        }
        | Statement::IfConst {
            then_block,
            else_ifs,
            else_block,
            ..
        } => {
            add_block_type_annotation_overrides(overrides, lexed, source, then_block);
            for else_if in else_ifs {
                add_block_type_annotation_overrides(overrides, lexed, source, &else_if.block);
            }
            if let Some(else_block) = else_block {
                add_block_type_annotation_overrides(overrides, lexed, source, else_block);
            }
        }
        Statement::While { body, .. }
        | Statement::For { body, .. }
        | Statement::Transaction { body, .. } => {
            add_block_type_annotation_overrides(overrides, lexed, source, body);
        }
        Statement::Try { body, catch, .. } => {
            add_block_type_annotation_overrides(overrides, lexed, source, body);
            if let Some(catch) = catch {
                if let Some(ty) = &catch.ty {
                    add_type_annotation_overrides(overrides, lexed, source, ty);
                }
                add_block_type_annotation_overrides(overrides, lexed, source, &catch.block);
            }
        }
        Statement::Match { arms, .. } => {
            for arm in arms {
                add_block_type_annotation_overrides(overrides, lexed, source, &arm.block);
            }
        }
        Statement::Assign { .. }
        | Statement::Delete { .. }
        | Statement::Return { .. }
        | Statement::ReturnAbsent { .. }
        | Statement::Break { .. }
        | Statement::Continue { .. }
        | Statement::Throw { .. }
        | Statement::Expr { .. } => {}
    }
}

fn add_resource_member_type_annotation_overrides(
    overrides: &mut HashMap<ByteSpan, TokenStyle>,
    lexed: &LexedSource,
    source: &str,
    member: &ResourceMember,
) {
    match member {
        ResourceMember::Field(field) => {
            for key in &field.keys {
                add_type_annotation_overrides(overrides, lexed, source, &key.ty);
            }
            add_type_annotation_overrides(overrides, lexed, source, &field.ty);
        }
        ResourceMember::Group(group) => {
            for key in &group.keys {
                add_type_annotation_overrides(overrides, lexed, source, &key.ty);
            }
            for member in &group.members {
                add_resource_member_type_annotation_overrides(overrides, lexed, source, member);
            }
        }
    }
}

fn add_type_annotation_overrides(
    overrides: &mut HashMap<ByteSpan, TokenStyle>,
    lexed: &LexedSource,
    source: &str,
    ty: &TypeRef,
) {
    let significant_tokens = lexed
        .tokens
        .iter()
        .filter(|token| token_in_span(token, ty.span) && !is_trivia(token.kind))
        .collect::<Vec<_>>();

    for (index, tokens) in significant_tokens.windows(5).enumerate() {
        let [id_token, open, caret, root_token, close] = tokens else {
            continue;
        };
        if !is_path_segment_token(id_token.kind)
            || id_token.text(source) != "Id"
            || open.kind != TokenKind::LeftParen
            || caret.kind != TokenKind::Caret
            || !is_path_segment_token(root_token.kind)
            || close.kind != TokenKind::RightParen
            || index
                .checked_sub(1)
                .and_then(|previous| significant_tokens.get(previous))
                .is_some_and(|token| token.kind == TokenKind::DoubleColon)
            || significant_tokens
                .get(index + 5)
                .is_some_and(|token| token.kind == TokenKind::DoubleColon)
        {
            continue;
        }

        overrides.insert(
            (id_token.span.start_byte, id_token.span.end_byte),
            TokenStyle::plain(TYPE_STRUCT),
        );
    }
}
