#![cfg(test)]

use std::path::PathBuf;

use chrono::Utc;

pub(crate) fn process_evidence() -> ephervex_core::ProcessEvidence {
    ephervex_core::ProcessEvidence {
        role: ephervex_core::ProcessRole::Supervisor,
        pid: 1,
        boot_id: "test-boot".to_owned(),
        start_ticks: 1,
        executable: PathBuf::from("/usr/bin/java"),
        executable_device: None,
        executable_inode: None,
        started_at: Utc::now(),
    }
}

/// Every standalone `.slint` module of the desktop interface, by its path
/// under `crates/gui/ui`.
///
/// Listed by hand because `include_str!` takes a literal path. The list is
/// checked against the directory, so a module cannot be left out of the tests
/// that read the markup.
pub(crate) const UI_SOURCES: &[(&str, &str)] = &[
    ("app.slint", include_str!("../ui/app.slint")),
    (
        "components/artwork.slint",
        include_str!("../ui/components/artwork.slint"),
    ),
    (
        "components/controls.slint",
        include_str!("../ui/components/controls.slint"),
    ),
    (
        "components/text.slint",
        include_str!("../ui/components/text.slint"),
    ),
    ("globals.slint", include_str!("../ui/globals.slint")),
    (
        "home/activity-cards.slint",
        include_str!("../ui/home/activity-cards.slint"),
    ),
    (
        "home/host-cards.slint",
        include_str!("../ui/home/host-cards.slint"),
    ),
    ("models.slint", include_str!("../ui/models.slint")),
    ("palettes.slint", include_str!("../ui/palettes.slint")),
    (
        "settings/application.slint",
        include_str!("../ui/settings/application.slint"),
    ),
    (
        "settings/minecraft-defaults.slint",
        include_str!("../ui/settings/minecraft-defaults.slint"),
    ),
    ("theme.slint", include_str!("../ui/theme.slint")),
    (
        "workspace/diagnostics.slint",
        include_str!("../ui/workspace/diagnostics.slint"),
    ),
    (
        "workspace/host-settings.slint",
        include_str!("../ui/workspace/host-settings.slint"),
    ),
    (
        "workspace/host-workspace.slint",
        include_str!("../ui/workspace/host-workspace.slint"),
    ),
    (
        "workspace/isolation.slint",
        include_str!("../ui/workspace/isolation.slint"),
    ),
    (
        "workspace/logs.slint",
        include_str!("../ui/workspace/logs.slint"),
    ),
    (
        "workspace/mods.slint",
        include_str!("../ui/workspace/mods.slint"),
    ),
    (
        "workspace/snapshots.slint",
        include_str!("../ui/workspace/snapshots.slint"),
    ),
];

/// The whole interface markup as one text, for tests that read the markup
/// rather than drive a window.
pub(crate) fn ui_markup() -> &'static str {
    static MARKUP: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    MARKUP.get_or_init(|| {
        UI_SOURCES
            .iter()
            .map(|(_, source)| *source)
            .collect::<Vec<_>>()
            .join("\n")
    })
}

/// One top-level declaration of the markup, from the line that opens it up to
/// the next declaration or the end of its module.
///
/// A top-level declaration is the only thing written from the first column,
/// apart from its closing brace and comments.
pub(crate) fn ui_declaration<'a>(source: &'a str, opening: &str) -> Option<&'a str> {
    let start = source.find(opening)?;
    let body = &source[start..];
    let end = body
        .match_indices('\n')
        .map(|(at, _)| at + 1)
        .find(|&at| {
            body[at..]
                .chars()
                .next()
                .is_some_and(|first| first.is_ascii_alphabetic())
        })
        .unwrap_or(body.len());
    Some(&body[..end])
}

#[test]
#[allow(clippy::expect_used)]
fn every_slint_module_is_read_by_the_markup_tests() {
    fn collect(directory: &std::path::Path, root: &std::path::Path, found: &mut Vec<String>) {
        for entry in std::fs::read_dir(directory).expect("the markup directory") {
            let path = entry.expect("a directory entry").path();
            if path.is_dir() {
                collect(&path, root, found);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "slint")
            {
                let relative = path.strip_prefix(root).expect("under the markup root");
                found.push(relative.to_string_lossy().replace('\\', "/"));
            }
        }
    }

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("ui");
    let mut on_disk = Vec::new();
    collect(&root, &root, &mut on_disk);
    on_disk.sort();
    let mut listed: Vec<String> = UI_SOURCES
        .iter()
        .map(|(path, _)| (*path).to_owned())
        .collect();
    listed.sort();
    assert_eq!(listed, on_disk);
}
