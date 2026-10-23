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
/// under `crates/gui/ui`, sorted. `build.rs` generates the list from the
/// directory, so a new module is read by the markup tests without being
/// registered.
pub(crate) const UI_SOURCES: &[(&str, &str)] = include!(concat!(env!("OUT_DIR"), "/ui_sources.rs"));

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
fn the_markup_sources_include_the_window_and_the_theme() {
    let names: Vec<&str> = UI_SOURCES.iter().map(|(name, _)| *name).collect();
    assert!(names.contains(&"app.slint"));
    assert!(names.contains(&"theme.slint"));
    assert!(names.windows(2).all(|pair| pair[0] < pair[1]));
}
