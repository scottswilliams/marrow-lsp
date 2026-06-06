//! Signature help for incomplete call expressions, driven from cached analysis.

use std::path::Path;

use lsp_types::{
    Documentation, MarkupContent, MarkupKind, ParameterInformation, ParameterLabel, SignatureHelp,
    SignatureInformation,
};
use marrow_check::{
    AnalysisSnapshot, CheckedFunction, CheckedParamMode, CheckedProgram, Def, DefItem, Resolution,
    ResolvableKind, resolve,
};
use marrow_schema::{NodeKind, ResourceSchema, stdlib};
use marrow_syntax::{
    Declaration, FunctionDecl, Keyword, LexedSource, SourceSpan, Token, TokenKind,
};

use crate::{language_facts, types::render_type};

pub fn signature_help(
    program: &CheckedProgram,
    docs: Option<&AnalysisSnapshot>,
    file: &Path,
    source: &str,
    lexed: &LexedSource,
    offset: usize,
) -> Option<SignatureHelp> {
    let call = active_call(source, lexed, offset)?;
    let from_module = module_of_file(program, file)?;
    let signature = signature_for(program, docs, from_module, &call.segments)?;
    Some(help(signature, call.active_parameter, call.named_argument))
}

struct ActiveCall {
    segments: Vec<String>,
    active_parameter: usize,
    named_argument: Option<String>,
}

struct Signature {
    label: String,
    documentation: Option<String>,
    params: Vec<Parameter>,
}

struct Parameter {
    name: Option<String>,
    label: String,
    documentation: Option<String>,
}

fn active_call(source: &str, lexed: &LexedSource, offset: usize) -> Option<ActiveCall> {
    let tokens = significant_tokens(lexed);
    let mut stack = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        if token.span.start_byte >= offset {
            break;
        }
        match token.kind {
            TokenKind::LeftParen => stack.push(index),
            TokenKind::RightParen => {
                stack.pop();
            }
            _ => {}
        }
    }

    let open = *stack.last()?;
    if enclosing_parens_suppress_signature_help(source, &tokens, &stack) {
        return None;
    }
    let segments = callee_path_before(source, &tokens, open)?;
    let (active_parameter, argument_start, argument_end) = active_argument(&tokens, open, offset);
    let named_argument = named_argument(source, &tokens, argument_start, argument_end);
    Some(ActiveCall {
        segments,
        active_parameter,
        named_argument,
    })
}

fn callee_path_before(source: &str, tokens: &[Token], open: usize) -> Option<Vec<String>> {
    let mut i = open.checked_sub(1)?;
    if !is_name_segment(tokens[i].kind) {
        return None;
    }
    let mut segments = Vec::new();
    let mut root = i;
    loop {
        segments.push(tokens[i].text(source).to_string());
        let Some(double_colon) = i.checked_sub(1) else {
            break;
        };
        if tokens[double_colon].kind != TokenKind::DoubleColon {
            break;
        }
        i = double_colon.checked_sub(1)?;
        if !is_name_segment(tokens[i].kind) {
            return None;
        }
        root = i;
    }
    if !starts_at_callee_root(source, tokens, root, open) {
        return None;
    }
    segments.reverse();
    Some(segments)
}

fn starts_at_callee_root(source: &str, tokens: &[Token], index: usize, open: usize) -> bool {
    if has_declaration_prefix(source, tokens, index) {
        return false;
    }
    if looks_like_type_annotation(source, tokens, index) {
        return false;
    }
    if looks_like_resource_member_key_list(source, tokens, index, open) {
        return false;
    }
    true
}

fn has_declaration_prefix(source: &str, tokens: &[Token], index: usize) -> bool {
    let Some(previous) = index
        .checked_sub(1)
        .and_then(|previous| tokens.get(previous))
    else {
        return false;
    };
    if !same_line_between(source, previous, &tokens[index]) {
        return false;
    }
    matches!(
        previous.kind,
        TokenKind::At
            | TokenKind::DoubleColon
            | TokenKind::Dot
            | TokenKind::QuestionDot
            | TokenKind::Caret
            | TokenKind::Keyword(
                Keyword::Const
                    | Keyword::Enum
                    | Keyword::Fn
                    | Keyword::Index
                    | Keyword::Module
                    | Keyword::Required
                    | Keyword::Resource
                    | Keyword::Use
                    | Keyword::Var,
            )
    )
}

fn enclosing_parens_suppress_signature_help(
    source: &str,
    tokens: &[Token],
    stack: &[usize],
) -> bool {
    stack
        .iter()
        .take(stack.len().saturating_sub(1))
        .copied()
        .any(|open| paren_suppresses_signature_help(source, tokens, open))
}

fn paren_suppresses_signature_help(source: &str, tokens: &[Token], open: usize) -> bool {
    let Some(root) = callee_root_before(tokens, open) else {
        return false;
    };
    !starts_at_callee_root(source, tokens, root, open)
}

fn looks_like_type_annotation(source: &str, tokens: &[Token], root: usize) -> bool {
    let Some(colon_index) = root.checked_sub(1) else {
        return false;
    };
    let colon = &tokens[colon_index];
    if colon.kind != TokenKind::Colon || !same_line_between(source, colon, &tokens[root]) {
        return false;
    }
    !colon_is_named_argument_value(source, tokens, colon_index)
}

fn colon_is_named_argument_value(source: &str, tokens: &[Token], colon_index: usize) -> bool {
    let Some(name_index) = colon_index.checked_sub(1) else {
        return false;
    };
    if tokens[name_index].kind != TokenKind::Identifier
        || !same_line_between(source, &tokens[name_index], &tokens[colon_index])
    {
        return false;
    }
    let Some(open) = innermost_open_paren_before(tokens, colon_index) else {
        return false;
    };
    let Some(root) = callee_root_before(tokens, open) else {
        return false;
    };
    !has_declaration_prefix(source, tokens, root)
        && !looks_like_resource_member_key_list(source, tokens, root, open)
}

fn innermost_open_paren_before(tokens: &[Token], index: usize) -> Option<usize> {
    let mut stack = Vec::new();
    for (candidate, token) in tokens.iter().enumerate().take(index) {
        match token.kind {
            TokenKind::LeftParen => stack.push(candidate),
            TokenKind::RightParen => {
                stack.pop();
            }
            _ => {}
        }
    }
    stack.last().copied()
}

fn callee_root_before(tokens: &[Token], open: usize) -> Option<usize> {
    let mut i = open.checked_sub(1)?;
    if !is_name_segment(tokens[i].kind) {
        return None;
    }
    let mut root = i;
    while let Some(double_colon) = i.checked_sub(1) {
        if tokens[double_colon].kind != TokenKind::DoubleColon {
            break;
        }
        i = double_colon.checked_sub(1)?;
        if !is_name_segment(tokens[i].kind) {
            return None;
        }
        root = i;
    }
    Some(root)
}

fn looks_like_resource_member_key_list(
    source: &str,
    tokens: &[Token],
    root: usize,
    open: usize,
) -> bool {
    if !is_first_significant_token_on_line(source, tokens, root) {
        return false;
    }
    if key_list_has_type_suffix(source, tokens, open) {
        return true;
    }
    is_in_resource_body(source, tokens[root].span.start_byte)
}

fn key_list_has_type_suffix(source: &str, tokens: &[Token], open: usize) -> bool {
    let Some(close) = matching_right_paren(tokens, open) else {
        return false;
    };
    let Some(next) = tokens.get(close + 1) else {
        return false;
    };
    same_line_between(source, &tokens[close], next) && next.kind == TokenKind::Colon
}

fn matching_right_paren(tokens: &[Token], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (index, token) in tokens.iter().enumerate().skip(open + 1) {
        match token.kind {
            TokenKind::LeftParen => depth += 1,
            TokenKind::RightParen => {
                if depth == 0 {
                    return Some(index);
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    None
}

fn is_first_significant_token_on_line(source: &str, tokens: &[Token], index: usize) -> bool {
    let Some(previous) = index
        .checked_sub(1)
        .and_then(|previous| tokens.get(previous))
    else {
        return true;
    };
    !same_line_between(source, previous, &tokens[index])
}

fn same_line_between(source: &str, before: &Token, after: &Token) -> bool {
    !source[before.span.end_byte..after.span.start_byte].contains('\n')
}

fn is_in_resource_body(source: &str, byte: usize) -> bool {
    let current_line_start = line_start(source, byte);
    let current_indent = indentation(&source[current_line_start..byte]);
    if current_indent == 0 {
        return false;
    }

    for line in source[..current_line_start].lines().rev() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            continue;
        }
        let indent = indentation(line);
        if indent >= current_indent {
            continue;
        }
        if starts_resource_declaration(trimmed) {
            return true;
        }
        if starts_function_declaration(trimmed) {
            return false;
        }
    }
    false
}

fn line_start(source: &str, byte: usize) -> usize {
    source[..byte].rfind('\n').map_or(0, |index| index + 1)
}

fn indentation(line: &str) -> usize {
    line.chars()
        .take_while(|ch| matches!(ch, ' ' | '\t'))
        .count()
}

fn starts_resource_declaration(trimmed: &str) -> bool {
    trimmed.starts_with("resource ") || trimmed.starts_with("pub resource ")
}

fn starts_function_declaration(trimmed: &str) -> bool {
    trimmed.starts_with("fn ") || trimmed.starts_with("pub fn ")
}

fn active_argument(tokens: &[Token], open: usize, offset: usize) -> (usize, usize, usize) {
    let mut active = 0usize;
    let mut argument_start = open + 1;
    let mut argument_end = argument_start;
    let mut paren_depth = 0usize;
    let mut bracket_depth = 0usize;

    for (index, token) in tokens.iter().enumerate().skip(open + 1) {
        if token.span.start_byte >= offset {
            break;
        }
        argument_end = index + 1;
        match token.kind {
            TokenKind::LeftParen => paren_depth += 1,
            TokenKind::RightParen => {
                if paren_depth == 0 {
                    argument_end = index;
                    break;
                }
                paren_depth -= 1;
            }
            TokenKind::LeftBracket => bracket_depth += 1,
            TokenKind::RightBracket => bracket_depth = bracket_depth.saturating_sub(1),
            TokenKind::Comma if paren_depth == 0 && bracket_depth == 0 => {
                active += 1;
                argument_start = index + 1;
                argument_end = argument_start;
            }
            _ => {}
        }
    }

    (active, argument_start, argument_end)
}

fn named_argument(
    source: &str,
    tokens: &[Token],
    argument_start: usize,
    argument_end: usize,
) -> Option<String> {
    let name = tokens.get(argument_start)?;
    let colon = tokens.get(argument_start + 1)?;
    (argument_start + 1 < argument_end
        && name.kind == TokenKind::Identifier
        && colon.kind == TokenKind::Colon)
        .then(|| name.text(source).to_string())
}

fn signature_for(
    program: &CheckedProgram,
    docs: Option<&AnalysisSnapshot>,
    from_module: &str,
    segments: &[String],
) -> Option<Signature> {
    if let Some(signature) = builtin_signature(segments) {
        return Some(signature);
    }
    if let Some(signature) = resource_signature(program, from_module, segments) {
        return Some(signature);
    }
    function_signature(program, docs, from_module, segments)
}

fn builtin_signature(segments: &[String]) -> Option<Signature> {
    if let [first, module, op] = segments
        && first == "std"
    {
        return stdlib::lookup(module, op).map(std_signature);
    }

    let [name] = segments else {
        return None;
    };
    if let Some(builtin) = language_facts::bare_function_builtins()
        .iter()
        .find(|builtin| builtin.name == name)
    {
        let mut signature = signature_from_label(builtin.detail);
        signature.documentation = Some(builtin.description.to_string());
        return Some(signature);
    }
    language_facts::scalar_conversion_detail(name).map(|detail| signature_from_label(&detail))
}

fn resource_signature(
    program: &CheckedProgram,
    from_module: &str,
    segments: &[String],
) -> Option<Signature> {
    match resolve(program, from_module, segments, ResolvableKind::Resource) {
        Resolution::Found(Def {
            item: DefItem::Resource(resource),
            ..
        }) => Some(resource_constructor_signature(resource)),
        _ => None,
    }
}

fn resource_constructor_signature(resource: &ResourceSchema) -> Signature {
    let params =
        resource
            .members
            .iter()
            .filter_map(|node| match &node.kind {
                NodeKind::Slot { ty, .. } if node.key_params.is_empty() => Some(
                    named_param_with_docs(&node.name, &ty.to_string(), join_docs(&node.docs)),
                ),
                _ => None,
            })
            .collect::<Vec<_>>();
    Signature {
        label: format!(
            "{}({}): {}",
            resource.name,
            joined_param_labels(&params),
            resource.name
        ),
        documentation: join_docs(&resource.docs),
        params,
    }
}

fn function_signature(
    program: &CheckedProgram,
    docs: Option<&AnalysisSnapshot>,
    from_module: &str,
    segments: &[String],
) -> Option<Signature> {
    match resolve(program, from_module, segments, ResolvableKind::Function) {
        Resolution::Found(def) => match def.item {
            DefItem::Function(function) => {
                let docs = docs.and_then(|snapshot| {
                    function_decl(snapshot, &def.module.source_file, function.span)
                });
                Some(checked_function_signature(function, docs))
            }
            _ => None,
        },
        _ => None,
    }
}

fn checked_function_signature(
    function: &CheckedFunction,
    docs: Option<&FunctionDecl>,
) -> Signature {
    let params = function
        .params
        .iter()
        .enumerate()
        .map(|(index, param)| {
            let mode = match param.mode {
                Some(CheckedParamMode::InOut) => "inout ",
                None => "",
            };
            let documentation = docs
                .and_then(|function| function.params.get(index))
                .filter(|decl| decl.name == param.name)
                .and_then(|decl| join_docs(&decl.docs));
            Parameter {
                name: Some(param.name.clone()),
                label: format!("{mode}{}: {}", param.name, render_type(&param.ty)),
                documentation,
            }
        })
        .collect::<Vec<_>>();
    let params_label = joined_param_labels(&params);
    let label = match &function.return_type {
        Some(ty) => format!("{}({params_label}): {}", function.name, render_type(ty)),
        None => format!("{}({params_label})", function.name),
    };
    Signature {
        label,
        documentation: docs.and_then(|function| join_docs(&function.docs)),
        params,
    }
}

fn std_signature(op: &stdlib::StdOp) -> Signature {
    let params = op
        .params
        .iter()
        .map(|param| Parameter {
            name: None,
            label: std_param_label(param),
            documentation: None,
        })
        .collect::<Vec<_>>();
    let params_label = joined_param_labels(&params);
    let label = match std_return_label(&op.ret) {
        Some(ret) => format!("std::{}::{}({params_label}): {ret}", op.module, op.op),
        None => format!("std::{}::{}({params_label})", op.module, op.op),
    };
    Signature {
        label,
        documentation: None,
        params,
    }
}

fn signature_from_label(label: &str) -> Signature {
    let params = label
        .split_once('(')
        .and_then(|(_, rest)| rest.split_once(')'))
        .map(|(params, _)| {
            if params.trim().is_empty() {
                Vec::new()
            } else {
                params
                    .split(',')
                    .map(|param| {
                        let label = param.trim().to_string();
                        let name = label
                            .split_once(':')
                            .map(|(name, _)| name.trim().to_string())
                            .or_else(|| Some(label.clone()));
                        Parameter {
                            name,
                            label,
                            documentation: None,
                        }
                    })
                    .collect()
            }
        })
        .unwrap_or_default();
    Signature {
        label: label.to_string(),
        documentation: None,
        params,
    }
}

fn named_param_with_docs(name: &str, ty: &str, documentation: Option<String>) -> Parameter {
    Parameter {
        name: Some(name.to_string()),
        label: format!("{name}: {ty}"),
        documentation,
    }
}

fn joined_param_labels(params: &[Parameter]) -> String {
    params
        .iter()
        .map(|param| param.label.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn std_param_label(param: &stdlib::ParamType) -> String {
    match param {
        stdlib::ParamType::Scalar(scalar) => scalar.name().to_string(),
        stdlib::ParamType::Error => "Error".to_string(),
        stdlib::ParamType::Path => "path".to_string(),
    }
}

fn std_return_label(ret: &stdlib::ReturnType) -> Option<String> {
    match ret {
        stdlib::ReturnType::Scalar(scalar) => Some(scalar.name().to_string()),
        stdlib::ReturnType::Sequence(scalar) => Some(format!("sequence[{}]", scalar.name())),
        stdlib::ReturnType::Void => None,
    }
}

fn function_decl<'a>(
    snapshot: &'a AnalysisSnapshot,
    file: &Path,
    span: SourceSpan,
) -> Option<&'a FunctionDecl> {
    let analyzed = snapshot
        .files
        .iter()
        .find(|file_info| file_info.path == file)?;
    analyzed
        .parsed
        .file
        .declarations
        .iter()
        .find_map(|declaration| match declaration {
            Declaration::Function(function) if function.span == span => Some(function),
            _ => None,
        })
}

fn join_docs(lines: &[String]) -> Option<String> {
    let joined = lines
        .iter()
        .map(|line| line.trim())
        .collect::<Vec<_>>()
        .join(" ");
    let joined = joined.trim();
    if joined.is_empty() {
        None
    } else {
        Some(joined.to_string())
    }
}

fn help(signature: Signature, active: usize, named_argument: Option<String>) -> SignatureHelp {
    let active = active_parameter(&signature.params, active, named_argument.as_deref());
    SignatureHelp {
        signatures: vec![SignatureInformation {
            label: signature.label,
            documentation: markdown_documentation(signature.documentation),
            parameters: Some(
                signature
                    .params
                    .into_iter()
                    .map(|param| ParameterInformation {
                        label: ParameterLabel::Simple(param.label),
                        documentation: markdown_documentation(param.documentation),
                    })
                    .collect(),
            ),
            active_parameter: None,
        }],
        active_signature: Some(0),
        active_parameter: active,
    }
}

fn markdown_documentation(value: Option<String>) -> Option<Documentation> {
    value.map(|value| {
        Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        })
    })
}

fn active_parameter(
    params: &[Parameter],
    comma_index: usize,
    named_argument: Option<&str>,
) -> Option<u32> {
    if params.is_empty() {
        return None;
    }
    if let Some(name) = named_argument
        && let Some(index) = params
            .iter()
            .position(|param| param.name.as_deref() == Some(name))
    {
        return Some(index as u32);
    }
    Some(comma_index.min(params.len() - 1) as u32)
}

fn module_of_file<'p>(program: &'p CheckedProgram, file: &Path) -> Option<&'p str> {
    program
        .modules
        .iter()
        .find(|module| module.source_file == file)
        .map(|module| module.name.as_str())
}

fn significant_tokens(lexed: &LexedSource) -> Vec<Token> {
    lexed
        .tokens
        .iter()
        .filter(|token| {
            !matches!(
                token.kind,
                TokenKind::Indent
                    | TokenKind::Dedent
                    | TokenKind::Newline
                    | TokenKind::Eof
                    | TokenKind::Comment
                    | TokenKind::DocComment
            )
        })
        .copied()
        .collect()
}

fn is_name_segment(kind: TokenKind) -> bool {
    matches!(kind, TokenKind::Identifier | TokenKind::Keyword(_))
}

#[cfg(test)]
mod tests;
