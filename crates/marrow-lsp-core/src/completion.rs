//! Context-aware completion driven by the error-recovering lexer.
//!
//! The cursor often sits in source that does not parse — right after a `^`, a
//! `.`, or a `::` the user just typed. So this never relies on a clean AST at the
//! cursor: it classifies the context from the cached [`LexedSource`] tokens to the
//! left of the offset, then draws candidates from the last clean snapshot
//! ([`CheckedProgram`]) and the lexical scope ([`marrow_check::scope_at`]). Both
//! the lex and the snapshot are cached, so completion re-lexes nothing and
//! triggers no project recompute, and it still answers when the file has parse
//! errors elsewhere.

use std::path::Path;

use lsp_types::{CompletionItem, CompletionItemKind, Documentation, MarkupContent, MarkupKind};
use marrow_check::{CheckedModule, CheckedProgram, scope_at};
use marrow_schema::{Element, Node, ResourceSchema, stdlib};
use marrow_syntax::{LexedSource, ParsedSource, Token, TokenKind};

use crate::types::render_type;

/// The Marrow keywords offered in statement/expression position. The type
/// keywords are offered separately in type position, so they are left out here.
const KEYWORDS: &[&str] = &[
    "const",
    "var",
    "if",
    "else",
    "while",
    "for",
    "in",
    "break",
    "continue",
    "return",
    "delete",
    "merge",
    "transaction",
    "lock",
    "try",
    "catch",
    "finally",
    "throw",
    "true",
    "false",
    "not",
    "and",
    "or",
];

/// The single-name builtins offered in expression position, each with a short
/// signature hint. These dispatch before user functions at runtime.
const BUILTINS: &[(&str, &str)] = &[
    ("exists", "exists(path): bool"),
    ("keys", "keys(layer): sequence"),
    ("values", "values(layer): sequence"),
    ("entries", "entries(layer): sequence"),
    ("count", "count(layer): int"),
    ("append", "append(layer, value): int"),
    ("nextId", "nextId(^root): Id"),
    ("print", "print(value)"),
    ("write", "write(value)"),
];

/// The built-in type names offered in type position.
const TYPE_NAMES: &[&str] = &[
    "int", "decimal", "bool", "string", "bytes", "date", "instant", "duration", "sequence",
    "unknown",
];

/// The completions for byte `offset` in `file`, classified from the cached lex
/// and drawn from the cached snapshot program.
///
/// `source` is the document's text (the lex's spans index into it); `parsed` is
/// the document's cached parse (used to type the lexical scope); `lexed` is the
/// cached lex the context is read from; `program` is the last clean snapshot's
/// checked program. The list is empty rather than absent when nothing fits, so
/// the editor shows no stale suggestions.
pub fn completion(
    program: &CheckedProgram,
    file: &Path,
    source: &str,
    parsed: &ParsedSource,
    lexed: &LexedSource,
    offset: usize,
) -> Vec<CompletionItem> {
    match classify(source, lexed, offset) {
        Context::Root => root_completions(program),
        Context::SavedPath { root, layers } => saved_path_completions(program, &root, &layers),
        Context::SavedIndex { root } => saved_index_completions(program, &root),
        Context::Namespace { qualifier } => namespace_completions(program, file, &qualifier),
        Context::Type => type_completions(program),
        Context::Bare => bare_completions(program, file, parsed, offset),
    }
}

/// What the tokens left of the cursor say the cursor is completing.
enum Context {
    /// Just after a `^`: the durable saved roots.
    Root,
    /// After `^root(...).` (`layers` are the names already descended past the
    /// key): the resource's fields and deeper layers.
    SavedPath { root: String, layers: Vec<String> },
    /// After `^root.` with no key: the resource's index names.
    SavedIndex { root: String },
    /// After `qualifier::`: what the qualifier (a resource, a used module, or a
    /// `std` module path) exposes.
    Namespace { qualifier: Vec<String> },
    /// A type annotation position (after a `:` introducing a type): type names and
    /// resource identities.
    Type,
    /// A bare identifier in expression position: in-scope names, keywords, builtins.
    Bare,
}

/// Classify the completion context from the tokens at or left of `offset`.
///
/// The driving token is the last non-trivia token whose span starts before the
/// offset; the classifier reads it and, where a path needs reconstructing, the
/// run of tokens before it. This stays entirely on the lexer because the cursor
/// commonly sits in source no parser would accept (a dangling `^`, `.`, or `::`).
fn classify(source: &str, lexed: &LexedSource, offset: usize) -> Context {
    let tokens = significant_tokens(lexed);
    // The index of the last token that begins strictly before the cursor. A token
    // the cursor sits inside (an identifier prefix being typed) counts as the one
    // before its delimiter, so look one further left when the cursor is mid-word.
    let Some(driver) = tokens
        .iter()
        .rposition(|token| token.span.start_byte < offset)
    else {
        return Context::Bare;
    };

    // If the cursor is inside an identifier (typing a prefix), the meaningful
    // delimiter is the token before that identifier.
    let anchor =
        if tokens[driver].kind == TokenKind::Identifier && tokens[driver].span.end_byte >= offset {
            driver.checked_sub(1)
        } else {
            Some(driver)
        };

    let Some(anchor) = anchor else {
        return Context::Bare;
    };

    match tokens[anchor].kind {
        TokenKind::Caret => Context::Root,
        TokenKind::DoubleColon => match qualifier_before(source, &tokens, anchor) {
            Some(qualifier) => Context::Namespace { qualifier },
            None => Context::Bare,
        },
        TokenKind::Dot | TokenKind::QuestionDot => {
            saved_context_before(source, &tokens, anchor).unwrap_or(Context::Bare)
        }
        // A `:` that introduces a type annotation (`name: <here>`, `): <here>`).
        TokenKind::Colon => {
            if introduces_type(&tokens, anchor) {
                Context::Type
            } else {
                Context::Bare
            }
        }
        _ => Context::Bare,
    }
}

/// Reconstruct the `::`-separated name path immediately before the `::` at
/// `colon_index`, e.g. the `std`, `clock` of `std::clock::`. `None` when the path
/// does not start at an identifier.
fn qualifier_before(source: &str, tokens: &[Token], colon_index: usize) -> Option<Vec<String>> {
    let mut segments = Vec::new();
    let mut i = colon_index;
    loop {
        let ident = i.checked_sub(1)?;
        if tokens[ident].kind != TokenKind::Identifier {
            return None;
        }
        segments.push(tokens[ident].text(source).to_string());
        match ident.checked_sub(1) {
            Some(prev) if tokens[prev].kind == TokenKind::DoubleColon => i = prev,
            _ => break,
        }
    }
    segments.reverse();
    Some(segments)
}

/// Decide whether the `.` at `dot_index` lies on a saved path (`^root...`), and
/// classify it. Two shapes complete:
///
/// - `^root(keys).` and any deeper `.layer` or `.layer(keys)` chain — a keyed
///   read: the resource's fields and the members of the innermost named layer.
/// - `^root.` with no key application — the resource's index names.
///
/// The chain left of the final `.` is a run of segments, each a layer name with
/// an optional `(keys)` application, separated by `.`; this walks it leftward
/// segment by segment back to the `^root`. Anything else yields `None`, so the
/// caller falls back to a bare completion.
fn saved_context_before(source: &str, tokens: &[Token], dot_index: usize) -> Option<Context> {
    let mut layers: Vec<String> = Vec::new();
    let mut cursor = dot_index;

    loop {
        let mut segment = cursor.checked_sub(1)?;
        // An optional `(keys)` application on this segment: skip it to reach the
        // layer name that owns it. Whether the segment is keyed decides, at the
        // root, between an index read (`^root.`) and a field read (`^root(k).`).
        let keyed = tokens[segment].kind == TokenKind::RightParen;
        if keyed {
            segment = matching_open_paren(tokens, segment)?.checked_sub(1)?;
        }
        if tokens[segment].kind != TokenKind::Identifier {
            return None;
        }
        let before = segment.checked_sub(1)?;
        match tokens[before].kind {
            // `^name` — the segment is the root. A bare `^root.` (no key) lists the
            // resource's index names; a keyed `^root(keys).…` lists its fields and
            // layers, descending any gathered group names.
            TokenKind::Caret => {
                let root = tokens[segment].text(source).to_string();
                if !keyed && layers.is_empty() {
                    return Some(Context::SavedIndex { root });
                }
                layers.reverse();
                return Some(Context::SavedPath { root, layers });
            }
            // `.name` continues the chain to the left; record this layer and step.
            TokenKind::Dot | TokenKind::QuestionDot => {
                layers.push(tokens[segment].text(source).to_string());
                cursor = before;
            }
            _ => return None,
        }
    }
}

/// The index of the `(` matching the `)` at `close_index`, accounting for nested
/// parentheses. `None` if unbalanced.
fn matching_open_paren(tokens: &[Token], close_index: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = close_index;
    loop {
        match tokens[i].kind {
            TokenKind::RightParen => depth += 1,
            TokenKind::LeftParen => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i = i.checked_sub(1)?;
    }
}

/// Whether the `:` at `colon_index` introduces a type annotation rather than a
/// labeled argument or a key. A type-introducing `:` follows a parameter or
/// binding name and is itself preceded by an identifier or a `)` (a function
/// signature's `): ` return type). A labeled call argument `name:` sits inside a
/// `(...)`, which we cannot tell apart purely lexically; the common binding and
/// signature cases are what type completion targets.
fn introduces_type(tokens: &[Token], colon_index: usize) -> bool {
    let Some(before) = colon_index.checked_sub(1) else {
        return false;
    };
    matches!(
        tokens[before].kind,
        TokenKind::Identifier | TokenKind::RightParen
    )
}

/// The durable saved roots declared across every module: one item per resource
/// that has a saved root.
fn root_completions(program: &CheckedProgram) -> Vec<CompletionItem> {
    let mut items = Vec::new();
    for resource in resources(program) {
        if let Some(saved) = &resource.saved_root {
            items.push(
                item(&saved.root, CompletionItemKind::STRUCT)
                    .detail(format!("saved root of {}", resource.name))
                    .docs_from(&resource.docs),
            );
        }
    }
    dedup(items)
}

/// The fields and deeper layers of the resource whose saved root is `root`, after
/// descending the already-typed `layers`. With no layers this is the resource's
/// top-level members; otherwise the members of the innermost layer.
fn saved_path_completions(
    program: &CheckedProgram,
    root: &str,
    layers: &[String],
) -> Vec<CompletionItem> {
    let Some(resource) = resource_by_root(program, root) else {
        return Vec::new();
    };
    let members: &[Node] = if layers.is_empty() {
        &resource.members
    } else {
        let chain: Vec<&str> = layers.iter().map(String::as_str).collect();
        match resource.descend_layers(&chain) {
            Some(node) => &node.members,
            None => return Vec::new(),
        }
    };
    members.iter().map(member_item).collect()
}

/// The index names of the resource whose saved root is `root` (`^root.`).
fn saved_index_completions(program: &CheckedProgram, root: &str) -> Vec<CompletionItem> {
    let Some(resource) = resource_by_root(program, root) else {
        return Vec::new();
    };
    resource
        .indexes
        .iter()
        .map(|index| {
            item(&index.name, CompletionItemKind::METHOD)
                .detail(format!("index({})", index.args.join(", ")))
                .docs_from(&index.docs)
        })
        .collect()
}

/// What `qualifier::` exposes: a resource's `Id`, a used module's public
/// functions and constants, or a `std` module's ops.
fn namespace_completions(
    program: &CheckedProgram,
    file: &Path,
    qualifier: &[String],
) -> Vec<CompletionItem> {
    // `std::<module>::` — enumerate that module's ops.
    if qualifier.first().map(String::as_str) == Some("std") {
        if let [_, module] = qualifier {
            return std_module_completions(module);
        }
        return Vec::new();
    }

    if let [name] = qualifier {
        // A resource name exposes its identity type.
        if resources(program).any(|resource| &resource.name == name) {
            return vec![item("Id", CompletionItemKind::CLASS).detail(format!("{name}::Id"))];
        }
        // A used module exposes its public functions and constants.
        if let Some(module) = module_named(program, file, name) {
            return module_member_completions(module);
        }
    }
    Vec::new()
}

/// The ops of `std::<module>::`, each with its rendered signature as detail.
fn std_module_completions(module: &str) -> Vec<CompletionItem> {
    stdlib::all()
        .iter()
        .filter(|op| op.module == module)
        .map(|op| item(op.op, CompletionItemKind::FUNCTION).detail(std_signature(op)))
        .collect()
}

/// The public functions and constants of a used module.
fn module_member_completions(module: &CheckedModule) -> Vec<CompletionItem> {
    let mut items = Vec::new();
    for function in &module.functions {
        if function.public {
            items.push(
                item(&function.name, CompletionItemKind::FUNCTION)
                    .detail(function_signature(function)),
            );
        }
    }
    for constant in &module.constants {
        items.push(
            item(&constant.name, CompletionItemKind::CONSTANT).detail(constant_detail(constant)),
        );
    }
    items
}

/// Type-position completions: the built-in type names plus every resource name
/// and its `::Id` identity.
fn type_completions(program: &CheckedProgram) -> Vec<CompletionItem> {
    let mut items: Vec<CompletionItem> = TYPE_NAMES
        .iter()
        .map(|name| item(name, CompletionItemKind::KEYWORD).detail("type".to_string()))
        .collect();
    for resource in resources(program) {
        items.push(item(&resource.name, CompletionItemKind::STRUCT).detail("resource".to_string()));
        items.push(
            item(&format!("{}::Id", resource.name), CompletionItemKind::CLASS)
                .detail("identity".to_string()),
        );
    }
    dedup(items)
}

/// Bare-identifier completions: the in-scope bindings (typed by the checker),
/// then the keywords and builtins.
fn bare_completions(
    program: &CheckedProgram,
    file: &Path,
    parsed: &ParsedSource,
    offset: usize,
) -> Vec<CompletionItem> {
    let mut items = Vec::new();
    for (name, ty) in scope_at(program, file, parsed, offset) {
        items.push(item(&name, CompletionItemKind::VARIABLE).detail(render_type(&ty)));
    }
    for keyword in KEYWORDS {
        items.push(item(keyword, CompletionItemKind::KEYWORD));
    }
    for (name, signature) in BUILTINS {
        items.push(item(name, CompletionItemKind::FUNCTION).detail((*signature).to_string()));
    }
    items
}

/// Every resource schema declared across all modules of the program.
fn resources(program: &CheckedProgram) -> impl Iterator<Item = &ResourceSchema> {
    program
        .modules
        .iter()
        .flat_map(|module| module.resources.iter())
}

/// The resource whose saved root is `root`, searched across all modules. The
/// first match wins; saved roots are unique per project in practice.
fn resource_by_root<'a>(program: &'a CheckedProgram, root: &str) -> Option<&'a ResourceSchema> {
    resources(program).find(|resource| {
        resource
            .saved_root
            .as_ref()
            .is_some_and(|saved| saved.root == root)
    })
}

/// The checked module a `use`d name refers to from `file`. A `use` records the
/// imported module's qualified name; the local alias is its last segment, so a
/// bare `name::` matches the module whose name ends in `name` and is imported by
/// the file's own module.
fn module_named<'a>(
    program: &'a CheckedProgram,
    file: &Path,
    name: &str,
) -> Option<&'a CheckedModule> {
    let current = program
        .modules
        .iter()
        .find(|module| module.source_file == file);
    let imported = current.map(|module| &module.imports);
    program.modules.iter().find(|module| {
        let last = module.name.rsplit("::").next().unwrap_or(&module.name);
        last == name
            && imported.is_none_or(|imports| {
                imports
                    .iter()
                    .any(|import| import == &module.name || import.ends_with(name))
            })
    })
}

/// A completion item for one resource member: a field, a keyed leaf, or a group.
fn member_item(node: &Node) -> CompletionItem {
    let (kind, detail) = match &node.element {
        Element::Slot { ty, .. } if node.key_params.is_empty() => {
            (CompletionItemKind::FIELD, ty.to_string())
        }
        Element::Slot { ty, .. } => (
            CompletionItemKind::METHOD,
            format!("({}): {ty}", key_params(node)),
        ),
        Element::Group => (
            CompletionItemKind::MODULE,
            format!("group({})", key_params(node)),
        ),
    };
    item(&node.name, kind).detail(detail).docs_from(&node.docs)
}

/// The `name: type` list of a node's key parameters, for a layer's detail.
fn key_params(node: &Node) -> String {
    node.key_params
        .iter()
        .map(|key| format!("{}: {}", key.name, key.ty))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A one-line rendering of a std op's signature, e.g. `(string, string): bool`.
fn std_signature(op: &stdlib::StdOp) -> String {
    let params = op
        .params
        .iter()
        .map(param_type_name)
        .collect::<Vec<_>>()
        .join(", ");
    match return_type_name(&op.ret) {
        Some(ret) => format!("({params}): {ret}"),
        None => format!("({params})"),
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

/// A function's one-line signature for completion detail.
fn function_signature(function: &marrow_check::CheckedFunction) -> String {
    let params = function
        .params
        .iter()
        .map(|param| format!("{}: {}", param.name, render_type(&param.ty)))
        .collect::<Vec<_>>()
        .join(", ");
    match &function.return_type {
        Some(ty) => format!("({params}): {}", render_type(ty)),
        None => format!("({params})"),
    }
}

fn constant_detail(constant: &marrow_check::CheckedConst) -> String {
    match &constant.ty {
        Some(ty) => render_type(ty),
        None => "const".to_string(),
    }
}

/// The significant tokens of a lex: everything but layout trivia and the EOF, in
/// source order. Comments are dropped too; they never bound a completion context.
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

/// A small builder over [`CompletionItem`] so each site reads as one expression.
trait ItemExt {
    fn detail(self, detail: String) -> Self;
    fn docs_from(self, docs: &[String]) -> Self;
}

impl ItemExt for CompletionItem {
    fn detail(mut self, detail: String) -> Self {
        self.detail = Some(detail);
        self
    }
    fn docs_from(mut self, docs: &[String]) -> Self {
        if !docs.is_empty() {
            self.documentation = Some(Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value: docs.join("\n"),
            }));
        }
        self
    }
}

/// A base completion item with a label and kind.
fn item(label: &str, kind: CompletionItemKind) -> CompletionItem {
    CompletionItem {
        label: label.to_string(),
        kind: Some(kind),
        ..CompletionItem::default()
    }
}

/// Drop items with a duplicate label, keeping the first. Saved roots and type
/// names can repeat across modules; the editor wants each once.
fn dedup(items: Vec<CompletionItem>) -> Vec<CompletionItem> {
    let mut seen = std::collections::HashSet::new();
    items
        .into_iter()
        .filter(|item| seen.insert(item.label.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use marrow_check::{ProjectSources, analyze_project};
    use marrow_project::parse_config;
    use marrow_syntax::{lex_source, parse_source};

    /// A two-file project: `shelf::books` declares the `Book` saved resource and a
    /// public helper; `shelf::app` uses it. Returns the checked program and the
    /// path of the file completion runs in (the `app` file).
    fn project() -> (CheckedProgram, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("marrow.json"), r#"{ "sourceRoots": ["src"] }"#).unwrap();
        let pkg = root.join("src/shelf");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(
            pkg.join("books.mw"),
            "\
module shelf::books

resource Book at ^books(id: int)
    required title: string
    required shelf: string
    tags(pos: int): string

    notes(noteId: string)
        text: string

    index byShelf(shelf, id)

pub fn titleOf(id: Book::Id): string
    return ^books(id).title

const LIMIT: int = 100
",
        )
        .unwrap();
        let app = pkg.join("app.mw");
        std::fs::write(
            &app,
            "\
module shelf::app

use shelf::books

pub fn run(count: int): int
    const total: int = count
    return total
",
        )
        .unwrap();
        let config = parse_config(r#"{ "sourceRoots": ["src"] }"#).unwrap();
        let snapshot = analyze_project(root, &config, &ProjectSources::new()).unwrap();
        std::mem::forget(dir);
        (snapshot.program, app)
    }

    /// Run completion on `source` at the cursor marked `|`, against `program` for
    /// the file at `file`. The marker is stripped before lexing.
    fn complete(program: &CheckedProgram, file: &std::path::Path, source: &str) -> Vec<String> {
        let offset = source.find('|').expect("a cursor marker `|`");
        let source = source.replacen('|', "", 1);
        let parsed = parse_source(&source);
        let lexed = lex_source(&source);
        completion(program, file, &source, &parsed, &lexed, offset)
            .into_iter()
            .map(|item| item.label)
            .collect()
    }

    #[test]
    fn after_caret_lists_durable_roots() {
        let (program, file) = project();
        let labels = complete(
            &program,
            &file,
            "module shelf::app\n\npub fn f()\n    delete ^|\n",
        );
        assert!(
            labels.contains(&"books".to_string()),
            "after `^` the saved root `books` should be offered, got {labels:?}"
        );
    }

    #[test]
    fn after_keyed_root_dot_lists_fields_and_layers() {
        let (program, file) = project();
        let labels = complete(
            &program,
            &file,
            "module shelf::app\n\npub fn f(id: int)\n    const x = ^books(id).|\n",
        );
        assert!(
            labels.contains(&"title".to_string()),
            "fields, got {labels:?}"
        );
        assert!(
            labels.contains(&"shelf".to_string()),
            "fields, got {labels:?}"
        );
        assert!(
            labels.contains(&"tags".to_string()),
            "keyed leaf, got {labels:?}"
        );
        assert!(
            labels.contains(&"notes".to_string()),
            "group, got {labels:?}"
        );
    }

    #[test]
    fn after_keyed_root_dot_group_dot_lists_nested_members() {
        let (program, file) = project();
        let labels = complete(
            &program,
            &file,
            "module shelf::app\n\npub fn f(id: int, n: string)\n    const x = ^books(id).notes(n).|\n",
        );
        assert!(
            labels.contains(&"text".to_string()),
            "the `notes` group's field, got {labels:?}"
        );
    }

    #[test]
    fn after_unkeyed_root_dot_lists_index_names() {
        let (program, file) = project();
        let labels = complete(
            &program,
            &file,
            "module shelf::app\n\npub fn f()\n    const x = ^books.|\n",
        );
        assert!(
            labels.contains(&"byShelf".to_string()),
            "after `^books.` the index `byShelf` should be offered, got {labels:?}"
        );
    }

    #[test]
    fn bare_identifier_lists_locals_and_keywords() {
        let (program, file) = project();
        let labels = complete(
            &program,
            &file,
            "module shelf::app\n\npub fn f(count: int)\n    const total: int = 1\n    return t|\n",
        );
        assert!(
            labels.contains(&"count".to_string()),
            "a param local, got {labels:?}"
        );
        assert!(
            labels.contains(&"total".to_string()),
            "a const local, got {labels:?}"
        );
        assert!(
            labels.contains(&"return".to_string()),
            "a keyword, got {labels:?}"
        );
        assert!(
            labels.contains(&"exists".to_string()),
            "a builtin, got {labels:?}"
        );
    }

    #[test]
    fn resource_namespace_lists_id() {
        let (program, file) = project();
        let labels = complete(
            &program,
            &file,
            "module shelf::app\n\npub fn f()\n    const x: Book::|\n",
        );
        assert_eq!(labels, vec!["Id".to_string()], "`Book::` offers only `Id`");
    }

    #[test]
    fn std_module_namespace_lists_ops() {
        let (program, file) = project();
        let labels = complete(
            &program,
            &file,
            "module shelf::app\n\npub fn f()\n    const x = std::clock::|\n",
        );
        assert!(
            labels.contains(&"now".to_string()),
            "a clock op, got {labels:?}"
        );
        assert!(
            labels.contains(&"today".to_string()),
            "a clock op, got {labels:?}"
        );
        assert!(
            !labels.contains(&"length".to_string()),
            "ops from other modules must not leak in, got {labels:?}"
        );
    }

    #[test]
    fn used_module_namespace_lists_public_members() {
        let (program, file) = project();
        let labels = complete(
            &program,
            &file,
            "module shelf::app\n\nuse shelf::books\n\npub fn f()\n    const x = books::|\n",
        );
        assert!(
            labels.contains(&"titleOf".to_string()),
            "the used module's pub function, got {labels:?}"
        );
        assert!(
            labels.contains(&"LIMIT".to_string()),
            "the used module's constant, got {labels:?}"
        );
    }

    #[test]
    fn type_position_lists_resources_and_builtin_types() {
        let (program, file) = project();
        let labels = complete(&program, &file, "module shelf::app\n\npub fn f(x: |\n");
        assert!(
            labels.contains(&"int".to_string()),
            "a builtin type, got {labels:?}"
        );
        assert!(
            labels.contains(&"Book".to_string()),
            "a resource, got {labels:?}"
        );
        assert!(
            labels.contains(&"Book::Id".to_string()),
            "a resource identity, got {labels:?}"
        );
    }

    #[test]
    fn completion_works_with_a_parse_error_elsewhere_in_the_file() {
        let (program, file) = project();
        // A broken declaration above the cursor: the file does not parse cleanly,
        // but the lexer recovers and the cached snapshot program still has `Book`.
        let labels = complete(
            &program,
            &file,
            "module shelf::app\n\npub fn broken(:\n\npub fn ok()\n    delete ^|\n",
        );
        assert!(
            labels.contains(&"books".to_string()),
            "completion must survive a parse error elsewhere, got {labels:?}"
        );
    }
}
