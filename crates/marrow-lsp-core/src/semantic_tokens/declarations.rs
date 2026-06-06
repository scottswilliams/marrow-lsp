use std::collections::HashMap;

use marrow_syntax::{
    Declaration, EnumDecl, EnumMember, EvolveDecl, EvolveStep, FieldDecl, FunctionDecl, GroupDecl,
    IndexDecl, Keyword, LexedSource, ResourceDecl, ResourceMember, SourceFile, SourceSpan,
    StoreDecl, Token, TokenKind,
};

use super::{
    ByteSpan, MOD_READONLY, TYPE_ENUM, TYPE_ENUM_MEMBER, TYPE_FUNCTION, TYPE_KEYWORD,
    TYPE_NAMESPACE, TYPE_PARAMETER, TYPE_PROPERTY, TYPE_STRUCT, TYPE_VARIABLE, TokenStyle,
    syntax::{is_path_segment_token, token_in_span},
};

pub(super) fn const_declaration_overrides(
    lexed: &LexedSource,
    file: &SourceFile,
    source: &str,
) -> HashMap<ByteSpan, TokenStyle> {
    let mut overrides = HashMap::new();
    for declaration in &file.declarations {
        if let Declaration::Const(const_decl) = declaration {
            add_first_identifier_style_override(
                &mut overrides,
                lexed,
                source,
                const_decl.span,
                &const_decl.name,
                TokenStyle {
                    token_type: TYPE_VARIABLE,
                    modifiers: MOD_READONLY,
                },
            );
        }
    }
    overrides
}

pub(super) fn declaration_overrides(
    lexed: &LexedSource,
    file: &SourceFile,
    source: &str,
) -> HashMap<ByteSpan, u32> {
    let mut overrides = HashMap::new();

    if let Some(module) = &file.module {
        add_qualified_path_after_keyword(
            &mut overrides,
            lexed,
            source,
            module.span,
            Keyword::Module,
            &module.name,
            TYPE_NAMESPACE,
        );
    }
    for use_decl in &file.uses {
        add_qualified_path_after_keyword(
            &mut overrides,
            lexed,
            source,
            use_decl.span,
            Keyword::Use,
            &use_decl.name,
            TYPE_NAMESPACE,
        );
    }

    for declaration in &file.declarations {
        add_declaration_overrides(&mut overrides, lexed, source, declaration);
    }

    overrides
}

fn add_declaration_overrides(
    overrides: &mut HashMap<ByteSpan, u32>,
    lexed: &LexedSource,
    source: &str,
    declaration: &Declaration,
) {
    match declaration {
        Declaration::Const(_) => {}
        Declaration::Function(function) => {
            add_function_declaration_overrides(overrides, lexed, source, function);
        }
        Declaration::Resource(resource) => {
            add_resource_declaration_overrides(overrides, lexed, source, resource);
        }
        Declaration::Store(store) => {
            add_store_declaration_overrides(overrides, lexed, source, store)
        }
        Declaration::Enum(enum_decl) => {
            add_enum_declaration_overrides(overrides, lexed, source, enum_decl);
        }
        Declaration::Evolve(evolve) => {
            add_evolve_declaration_overrides(overrides, lexed, source, evolve);
        }
    }
}

fn add_function_declaration_overrides(
    overrides: &mut HashMap<ByteSpan, u32>,
    lexed: &LexedSource,
    source: &str,
    function: &FunctionDecl,
) {
    if let Some(name_index) = add_first_identifier_override(
        overrides,
        lexed,
        source,
        function.span,
        &function.name,
        TYPE_FUNCTION,
    ) && !function.params.is_empty()
        && let Some((open, close)) =
            matching_parens_after(lexed, function.span, lexed.tokens[name_index].span.end_byte)
    {
        add_colon_name_overrides(
            overrides,
            lexed,
            source,
            open,
            close,
            function.params.iter().map(|param| param.name.as_str()),
            TYPE_PARAMETER,
        );
    }
}

fn add_resource_declaration_overrides(
    overrides: &mut HashMap<ByteSpan, u32>,
    lexed: &LexedSource,
    source: &str,
    resource: &ResourceDecl,
) {
    add_first_identifier_override(
        overrides,
        lexed,
        source,
        resource.span,
        &resource.name,
        TYPE_STRUCT,
    );
    for member in &resource.members {
        add_resource_member_overrides(overrides, lexed, source, member);
    }
}

fn add_store_declaration_overrides(
    overrides: &mut HashMap<ByteSpan, u32>,
    lexed: &LexedSource,
    source: &str,
    store: &StoreDecl,
) {
    if !store.root.keys.is_empty() {
        add_saved_root_key_overrides(
            overrides,
            lexed,
            source,
            store.span,
            &store.root.root,
            store.root.keys.iter().map(|key| key.name.as_str()),
        );
    }
    for index in &store.indexes {
        add_index_overrides(overrides, lexed, source, index);
    }
}

fn add_enum_declaration_overrides(
    overrides: &mut HashMap<ByteSpan, u32>,
    lexed: &LexedSource,
    source: &str,
    enum_decl: &EnumDecl,
) {
    add_first_identifier_override(
        overrides,
        lexed,
        source,
        enum_decl.span,
        &enum_decl.name,
        TYPE_ENUM,
    );
    for member in &enum_decl.members {
        add_enum_member_overrides(overrides, lexed, source, member);
    }
}

fn add_evolve_declaration_overrides(
    overrides: &mut HashMap<ByteSpan, u32>,
    lexed: &LexedSource,
    source: &str,
    evolve: &EvolveDecl,
) {
    for step in &evolve.steps {
        add_evolve_step_overrides(overrides, lexed, source, step);
    }
}

fn add_evolve_step_overrides(
    overrides: &mut HashMap<ByteSpan, u32>,
    lexed: &LexedSource,
    source: &str,
    step: &EvolveStep,
) {
    let name = match step {
        EvolveStep::Rename { .. } => "rename",
        EvolveStep::Default { .. } => "default",
        EvolveStep::Retire { .. } => "retire",
        EvolveStep::Transform { .. } => "transform",
    };
    add_first_identifier_override(overrides, lexed, source, step.span(), name, TYPE_KEYWORD);
}

fn add_resource_member_overrides(
    overrides: &mut HashMap<ByteSpan, u32>,
    lexed: &LexedSource,
    source: &str,
    member: &ResourceMember,
) {
    match member {
        ResourceMember::Field(field) => add_field_overrides(overrides, lexed, source, field),
        ResourceMember::Group(group) => add_group_overrides(overrides, lexed, source, group),
    }
}

fn add_field_overrides(
    overrides: &mut HashMap<ByteSpan, u32>,
    lexed: &LexedSource,
    source: &str,
    field: &FieldDecl,
) {
    if let Some(name_index) = add_first_identifier_override(
        overrides,
        lexed,
        source,
        field.span,
        &field.name,
        TYPE_PROPERTY,
    ) && !field.keys.is_empty()
        && let Some((open, close)) =
            matching_parens_after(lexed, field.span, lexed.tokens[name_index].span.end_byte)
    {
        add_colon_name_overrides(
            overrides,
            lexed,
            source,
            open,
            close,
            field.keys.iter().map(|key| key.name.as_str()),
            TYPE_PARAMETER,
        );
    }
}

fn add_group_overrides(
    overrides: &mut HashMap<ByteSpan, u32>,
    lexed: &LexedSource,
    source: &str,
    group: &GroupDecl,
) {
    if let Some(name_index) = add_first_identifier_override(
        overrides,
        lexed,
        source,
        group.span,
        &group.name,
        TYPE_PROPERTY,
    ) && !group.keys.is_empty()
        && let Some((open, close)) =
            matching_parens_after(lexed, group.span, lexed.tokens[name_index].span.end_byte)
    {
        add_colon_name_overrides(
            overrides,
            lexed,
            source,
            open,
            close,
            group.keys.iter().map(|key| key.name.as_str()),
            TYPE_PARAMETER,
        );
    }
    for member in &group.members {
        add_resource_member_overrides(overrides, lexed, source, member);
    }
}

fn add_index_overrides(
    overrides: &mut HashMap<ByteSpan, u32>,
    lexed: &LexedSource,
    source: &str,
    index: &IndexDecl,
) {
    if let Some(name_index) = add_first_identifier_override(
        overrides,
        lexed,
        source,
        index.span,
        &index.name,
        TYPE_PROPERTY,
    ) && !index.args.is_empty()
        && let Some((open, close)) =
            matching_parens_after(lexed, index.span, lexed.tokens[name_index].span.end_byte)
    {
        add_argument_name_overrides(
            overrides,
            lexed,
            source,
            open,
            close,
            index.args.iter().map(|arg| arg.as_str()),
            TYPE_PARAMETER,
        );
    }
}

fn add_enum_member_overrides(
    overrides: &mut HashMap<ByteSpan, u32>,
    lexed: &LexedSource,
    source: &str,
    member: &EnumMember,
) {
    add_first_identifier_override(
        overrides,
        lexed,
        source,
        member.span,
        &member.name,
        TYPE_ENUM_MEMBER,
    );
    for child in &member.members {
        add_enum_member_overrides(overrides, lexed, source, child);
    }
}

fn add_qualified_path_after_keyword(
    overrides: &mut HashMap<ByteSpan, u32>,
    lexed: &LexedSource,
    source: &str,
    span: SourceSpan,
    keyword: Keyword,
    path: &str,
    token_type: u32,
) {
    let Some(keyword_token) = lexed
        .tokens
        .iter()
        .find(|token| token_in_span(token, span) && token.kind == TokenKind::Keyword(keyword))
    else {
        return;
    };
    let mut cursor = keyword_token.span.end_byte;
    for segment in path.split("::") {
        let Some(token) = lexed.tokens.iter().find(|token| {
            token.span.start_byte >= cursor
                && token_in_span(token, span)
                && is_path_segment_token(token.kind)
                && token.text(source) == segment
        }) else {
            return;
        };
        insert_override(overrides, token, token_type);
        cursor = token.span.end_byte;
    }
}

fn add_saved_root_key_overrides<'a>(
    overrides: &mut HashMap<ByteSpan, u32>,
    lexed: &LexedSource,
    source: &str,
    span: SourceSpan,
    root: &str,
    names: impl IntoIterator<Item = &'a str>,
) {
    let Some(caret_index) = lexed
        .tokens
        .iter()
        .position(|token| token_in_span(token, span) && token.kind == TokenKind::Caret)
    else {
        return;
    };
    let Some(root_token) = lexed.tokens[caret_index + 1..].iter().find(|token| {
        token_in_span(token, span)
            && token.kind == TokenKind::Identifier
            && token.text(source) == root
    }) else {
        return;
    };
    let Some((open, close)) = matching_parens_after(lexed, span, root_token.span.end_byte) else {
        return;
    };
    add_colon_name_overrides(overrides, lexed, source, open, close, names, TYPE_PARAMETER);
}

fn add_first_identifier_override(
    overrides: &mut HashMap<ByteSpan, u32>,
    lexed: &LexedSource,
    source: &str,
    span: SourceSpan,
    name: &str,
    token_type: u32,
) -> Option<usize> {
    let index = first_identifier_index(lexed, source, span, name)?;
    insert_override(overrides, &lexed.tokens[index], token_type);
    Some(index)
}

fn add_first_identifier_style_override(
    overrides: &mut HashMap<ByteSpan, TokenStyle>,
    lexed: &LexedSource,
    source: &str,
    span: SourceSpan,
    name: &str,
    style: TokenStyle,
) -> Option<usize> {
    let index = first_identifier_index(lexed, source, span, name)?;
    insert_style_override(overrides, &lexed.tokens[index], style);
    Some(index)
}

fn first_identifier_index(
    lexed: &LexedSource,
    source: &str,
    span: SourceSpan,
    name: &str,
) -> Option<usize> {
    if name.is_empty() {
        return None;
    }
    lexed.tokens.iter().position(|token| {
        token_in_span(token, span)
            && token.kind == TokenKind::Identifier
            && token.text(source) == name
    })
}

fn add_colon_name_overrides<'a>(
    overrides: &mut HashMap<ByteSpan, u32>,
    lexed: &LexedSource,
    source: &str,
    open: usize,
    close: usize,
    names: impl IntoIterator<Item = &'a str>,
    token_type: u32,
) {
    let names: Vec<&str> = names.into_iter().collect();
    for index in open + 1..close {
        let token = &lexed.tokens[index];
        if token.kind == TokenKind::Identifier
            && names.iter().any(|name| *name == token.text(source))
            && lexed
                .tokens
                .get(index + 1)
                .is_some_and(|next| next.kind == TokenKind::Colon)
        {
            insert_override(overrides, token, token_type);
        }
    }
}

fn add_argument_name_overrides<'a>(
    overrides: &mut HashMap<ByteSpan, u32>,
    lexed: &LexedSource,
    source: &str,
    open: usize,
    close: usize,
    names: impl IntoIterator<Item = &'a str>,
    token_type: u32,
) {
    let names: Vec<&str> = names.into_iter().collect();
    for index in open + 1..close {
        let token = &lexed.tokens[index];
        if token.kind == TokenKind::Identifier
            && names.iter().any(|name| *name == token.text(source))
        {
            insert_override(overrides, token, token_type);
        }
    }
}

fn matching_parens_after(
    lexed: &LexedSource,
    span: SourceSpan,
    after_byte: usize,
) -> Option<(usize, usize)> {
    let open = lexed.tokens.iter().position(|token| {
        token.span.start_byte >= after_byte
            && token_in_span(token, span)
            && token.kind == TokenKind::LeftParen
    })?;
    let mut depth = 0usize;
    for index in open..lexed.tokens.len() {
        let token = &lexed.tokens[index];
        if !token_in_span(token, span) {
            break;
        }
        match token.kind {
            TokenKind::LeftParen => depth += 1,
            TokenKind::RightParen => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some((open, index));
                }
            }
            _ => {}
        }
    }
    None
}

fn insert_override(overrides: &mut HashMap<ByteSpan, u32>, token: &Token, token_type: u32) {
    overrides.insert((token.span.start_byte, token.span.end_byte), token_type);
}

fn insert_style_override(
    overrides: &mut HashMap<ByteSpan, TokenStyle>,
    token: &Token,
    style: TokenStyle,
) {
    overrides.insert((token.span.start_byte, token.span.end_byte), style);
}
