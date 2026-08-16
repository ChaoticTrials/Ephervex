//! Guards the repository rule that launcher-owned JSON is written compactly.
//!
//! Files Ephervex writes for itself - instance metadata, recipes, locks,
//! runtime evidence, indexes, caches - are read back by the launcher, so
//! indentation only costs storage and I/O. This scans the workspace's
//! production sources for serde_json's pretty-printing serializers so a new
//! persistence path cannot quietly reintroduce them.
//!
//! Only production code is scanned: the trailing `#[cfg(test)]` unit-test
//! module of a source file is cut off, and integration tests, fixtures, and
//! documentation are never scanned at all.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

/// Production sources whose indented JSON is deliberate, with the reason it is.
const HUMAN_FACING: &[&str] = &[
    // `--json` command results are rendered into a terminal for a person to
    // read; they are output, not storage.
    "crates/cli/src/main.rs",
    // `ops.json` belongs to the Minecraft server, which writes it indented
    // itself and whose operators edit it by hand.
    "crates/server/src/operators.rs",
];

/// The suffix every pretty-printing serde_json entry point shares:
/// `to_string_pretty`, `to_vec_pretty`, `to_writer_pretty`.
const PRETTY: &str = "_pretty";

/// The formatter those entry points are built on, which a hand-rolled
/// `serde_json::Serializer` could reach for directly.
const PRETTY_FORMATTER: &str = "PrettyFormatter";

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

/// Every `.rs` file under `crates/*/src`, relative to the workspace root.
fn production_sources(root: &Path) -> Vec<PathBuf> {
    let mut sources = Vec::new();
    for crate_dir in std::fs::read_dir(root.join("crates")).expect("crates directory") {
        let src = crate_dir.expect("crate entry").path().join("src");
        if src.is_dir() {
            collect(&src, root, &mut sources);
        }
    }
    sources.sort();
    sources
}

fn collect(directory: &Path, root: &Path, sources: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(directory).expect("source directory") {
        let path = entry.expect("source entry").path();
        if path.is_dir() {
            collect(&path, root, sources);
        } else if path.extension().is_some_and(|kind| kind == "rs")
            // Support code compiled only for tests, by the name this workspace
            // gives it.
            && path.file_name().is_some_and(|name| name != "test_support.rs")
        {
            sources.push(
                path.strip_prefix(root)
                    .expect("a path under the workspace root")
                    .to_path_buf(),
            );
        }
    }
}

/// The part of a source file that is compiled into the product, dropping the
/// trailing unit-test module.
fn production_code(text: &str) -> &str {
    match text.find("#[cfg(test)]\nmod tests {") {
        Some(index) => &text[..index],
        None => text,
    }
}

#[test]
fn production_code_does_not_pretty_print_launcher_owned_json() {
    let root = workspace_root();
    let mut offenders = Vec::new();

    for source in production_sources(&root) {
        if HUMAN_FACING.contains(&source.to_string_lossy().as_ref()) {
            continue;
        }
        let text = std::fs::read_to_string(root.join(&source)).expect("read source");
        for (number, line) in production_code(&text).lines().enumerate() {
            if (line.contains(PRETTY) && line.contains("serde_json"))
                || line.contains(PRETTY_FORMATTER)
            {
                offenders.push(format!(
                    "{}:{}: {}",
                    source.display(),
                    number + 1,
                    line.trim()
                ));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "JSON Ephervex stores for itself must be compact; use serde_json::to_writer, \
         to_vec, or to_string instead:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn the_scan_reaches_the_sources_that_persist_json() {
    let sources = production_sources(&workspace_root());

    for expected in [
        "crates/storage/src/metadata.rs",
        "crates/storage/src/lock.rs",
        "crates/pack/src/store.rs",
        "crates/server/src/runtime.rs",
        "crates/mod-platform/src/modrinth/api.rs",
    ] {
        assert!(
            sources.iter().any(|source| source == Path::new(expected)),
            "{expected} is not being scanned"
        );
    }
}
