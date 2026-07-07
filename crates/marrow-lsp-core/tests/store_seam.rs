//! Architecture gate for the read-only store seam (roadmap S0.5).
//!
//! Every production read of a project's native store flows through `store_view.rs`, so a
//! store-engine change lands against one seam. This gate fails if any other core source
//! file names `marrow_store` in production code, which would re-scatter the store contact
//! the seam exists to contain.
//!
//! Test-module code (`#[cfg(test)] mod tests { ... }`) is exempt: fixtures legitimately
//! construct and write stores to set up a scenario, and the seam is read-only by design.

mod support;

use std::path::Path;
use support::{production_rust_sources, production_source};

#[test]
fn marrow_store_is_named_only_in_the_store_view_seam() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    production_rust_sources(&src, &mut files);
    assert!(
        !files.is_empty(),
        "found no core source files under {src:?}"
    );

    let mut offenders = Vec::new();
    for file in files {
        // `store_view.rs` is the seam itself and legitimately names `marrow_store`.
        if file.file_name().is_some_and(|name| name == "store_view.rs") {
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
