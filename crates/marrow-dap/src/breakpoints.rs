//! Resolve client breakpoint lines to the statements the runtime can actually
//! stop on.
//!
//! A `.mw` debugger stops at statement granularity: the hook fires before each
//! statement, carrying that statement's span. A client sets a breakpoint on a
//! source line, which may be blank, a comment, or the middle of a multi-line
//! statement. We bind each requested line to the first statement whose span
//! starts at or after it (in the same file), and report that statement's line
//! back as the *verified* breakpoint line — so the editor's marker lands where
//! the run will really stop, and a line past the last statement is rejected.

use std::collections::BTreeSet;
use std::path::Path;

use marrow_check::CheckedProgram;
use marrow_syntax::{Block, Statement};

/// The statement start lines of every function body in `file`, gathered in
/// ascending order with duplicates removed. The set a breakpoint request snaps
/// against: only these lines are real stop points.
pub fn statement_lines(program: &CheckedProgram, file: &Path) -> BTreeSet<u32> {
    let mut lines = BTreeSet::new();
    for module in &program.modules {
        if same_file(&module.source_file, file) {
            for function in &module.functions {
                collect_block(&function.body, &mut lines);
            }
        }
    }
    lines
}

/// Whether two paths name the same source file. The client's `source.path` and
/// the analyzed `source_file` may differ in canonical form (symlinked temp dirs,
/// `..` segments), so compare canonicalized forms, falling back to raw equality
/// when a path cannot be canonicalized (e.g. it does not exist on disk).
fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Collect the start line of every statement in `block`, descending into the
/// blocks nested statements own (the bodies of `if`/`while`/`for`/`transaction`/
/// `lock`/`try`). Each nested statement is itself a stop point, so its line is
/// collected too — the runtime fires the hook before it.
fn collect_block(block: &Block, lines: &mut BTreeSet<u32>) {
    for statement in &block.statements {
        lines.insert(statement_line(statement));
        for nested in nested_blocks(statement) {
            collect_block(nested, lines);
        }
    }
}

/// The source line a statement begins on (1-based), from its span.
fn statement_line(statement: &Statement) -> u32 {
    statement_span(statement).line
}

/// A statement's own span, for every statement variant.
fn statement_span(statement: &Statement) -> marrow_syntax::SourceSpan {
    match statement {
        Statement::Const { span, .. }
        | Statement::Var { span, .. }
        | Statement::Assign { span, .. }
        | Statement::Delete { span, .. }
        | Statement::Merge { span, .. }
        | Statement::Return { span, .. }
        | Statement::Break { span, .. }
        | Statement::Continue { span, .. }
        | Statement::Throw { span, .. }
        | Statement::Expr { span, .. }
        | Statement::If { span, .. }
        | Statement::While { span, .. }
        | Statement::For { span, .. }
        | Statement::Transaction { span, .. }
        | Statement::Lock { span, .. }
        | Statement::Try { span, .. } => *span,
    }
}

/// The blocks a statement encloses, so their statements are reachable stop
/// points too. Leaf statements enclose none.
fn nested_blocks(statement: &Statement) -> Vec<&Block> {
    match statement {
        Statement::If {
            then_block,
            else_ifs,
            else_block,
            ..
        } => {
            let mut blocks = vec![then_block];
            blocks.extend(else_ifs.iter().map(|clause| &clause.block));
            if let Some(block) = else_block {
                blocks.push(block);
            }
            blocks
        }
        Statement::While { body, .. }
        | Statement::For { body, .. }
        | Statement::Transaction { body, .. }
        | Statement::Lock { body, .. } => vec![body],
        Statement::Try {
            body,
            catch,
            finally,
            ..
        } => {
            let mut blocks = vec![body];
            if let Some(clause) = catch {
                blocks.push(&clause.block);
            }
            if let Some(block) = finally {
                blocks.push(block);
            }
            blocks
        }
        _ => Vec::new(),
    }
}

/// Resolve one requested breakpoint `line` (1-based) against the statement lines
/// of a file: the first statement line at or after `line`, or `None` when the
/// request falls past the last statement (an unverifiable breakpoint). Snapping
/// forward means a breakpoint on a blank line, a comment, or a signature lands on
/// the next executable statement — exactly where the run will stop.
pub fn resolve_line(lines: &BTreeSet<u32>, line: u32) -> Option<u32> {
    lines.range(line..).next().copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use marrow_check::{ProjectSources, analyze_project};
    use marrow_project::ProjectConfig;
    use std::path::PathBuf;

    /// Analyze a one-file project from source and return its program, the file
    /// path the module came from, and the live temp dir (kept so the file stays on
    /// disk for `same_file`'s canonicalization).
    fn analyze(source: &str) -> (CheckedProgram, PathBuf, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let file = src.join("m.mw");
        std::fs::write(&file, source).unwrap();
        let config = ProjectConfig {
            source_roots: vec!["src".to_string()],
            default_entry: None,
            store: None,
            tests: Vec::new(),
        };
        let snapshot = analyze_project(&root, &config, &ProjectSources::new()).unwrap();
        (snapshot.program, file, dir)
    }

    #[test]
    fn statement_lines_are_collected_including_nested_blocks() {
        let source = "module m\n\
                       \n\
                       pub fn f(n: int): int\n\
                       \x20   var total: int = 0\n\
                       \x20   if n > 0\n\
                       \x20       total = n\n\
                       \x20   return total\n";
        let (program, file, _dir) = analyze(source);
        let lines = statement_lines(&program, &file);
        // var (line 4), if (5), the assignment inside it (6), return (7).
        assert!(lines.contains(&4), "{lines:?}");
        assert!(lines.contains(&5), "{lines:?}");
        assert!(lines.contains(&6), "{lines:?}");
        assert!(lines.contains(&7), "{lines:?}");
    }

    #[test]
    fn a_breakpoint_on_a_blank_line_snaps_to_the_next_statement() {
        let source = "module m\n\
                       \n\
                       pub fn f(): int\n\
                       \x20   const a: int = 1\n\
                       \n\
                       \x20   return a\n";
        let (program, file, _dir) = analyze(source);
        let lines = statement_lines(&program, &file);
        // Line 5 is blank; it should snap forward to the return on line 6.
        assert_eq!(resolve_line(&lines, 5), Some(6));
        // A request exactly on the const stays there.
        assert_eq!(resolve_line(&lines, 4), Some(4));
    }

    #[test]
    fn a_breakpoint_past_the_last_statement_is_unresolved() {
        let source = "module m\n\
                       \n\
                       pub fn f(): int\n\
                       \x20   return 1\n";
        let (program, file, _dir) = analyze(source);
        let lines = statement_lines(&program, &file);
        assert_eq!(resolve_line(&lines, 99), None);
    }

    #[test]
    fn a_breakpoint_on_the_signature_snaps_into_the_body() {
        let source = "module m\n\
                       \n\
                       pub fn f(): int\n\
                       \x20   return 1\n";
        let (program, file, _dir) = analyze(source);
        let lines = statement_lines(&program, &file);
        // Line 3 is the signature; the first statement is the return on line 4.
        assert_eq!(resolve_line(&lines, 3), Some(4));
    }
}
