//! Architecture gate for the read-only store seam (roadmap S0.5).
//!
//! Every production read of a project's native store flows through `store_view.rs`, so a
//! store-engine change lands against one seam. This gate fails if any other core source
//! file names `marrow_store` in production code, which would re-scatter the store contact
//! the seam exists to contain.
//!
//! Test-module code (`#[cfg(test)] mod tests { ... }`) is exempt: fixtures legitimately
//! construct and write stores to set up a scenario, and the seam is read-only by design.

use std::path::{Path, PathBuf};

/// Strip `#[cfg(test)] mod tests { ... }` blocks so only production source remains. Brace
/// depth is tracked to find each test module's matching close; the codebase's test modules
/// are conventional, so naive brace counting is exact enough for the invariant.
fn production_source(text: &str) -> String {
    let mut out = String::new();
    let mut depth: i32 = 0;
    let mut test_mod_open_depth: Option<i32> = None;
    let mut prev_line_was_cfg_test = false;

    for line in text.lines() {
        let trimmed = line.trim_start();
        if test_mod_open_depth.is_none() && prev_line_was_cfg_test && trimmed.starts_with("mod ") {
            test_mod_open_depth = Some(depth);
        }
        if test_mod_open_depth.is_none() {
            out.push_str(line);
            out.push('\n');
        }
        for c in line.chars() {
            match c {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
        }
        if let Some(open_depth) = test_mod_open_depth
            && depth <= open_depth
        {
            test_mod_open_depth = None;
        }
        prev_line_was_cfg_test = trimmed == "#[cfg(test)]";
    }
    out
}

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read core src dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn marrow_store_is_named_only_in_the_store_view_seam() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_sources(&src, &mut files);
    assert!(
        !files.is_empty(),
        "found no core source files under {src:?}"
    );

    let mut offenders = Vec::new();
    for file in files {
        // `store_view.rs` is the seam itself; a `tests.rs` submodule file is entirely a
        // `#[cfg(test)] mod tests;` module, so its store-writing fixtures are exempt.
        if file
            .file_name()
            .is_some_and(|name| name == "store_view.rs" || name == "tests.rs")
        {
            continue;
        }
        let text = std::fs::read_to_string(&file).expect("read source");
        if production_source(&text).contains("marrow_store") {
            offenders.push(
                file.strip_prefix(&src)
                    .unwrap_or(&file)
                    .display()
                    .to_string(),
            );
        }
    }

    assert!(
        offenders.is_empty(),
        "production code outside store_view.rs names marrow_store; route it through the \
         read-only store seam instead: {offenders:?}"
    );
}
