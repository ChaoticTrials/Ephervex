//! Naming a fixed-width font family the renderer can actually find.
//!
//! # Why this is needed
//!
//! Slint's `font-family` is a name, not a CSS family list: it is passed through
//! as `fontique::QueryFamily::Named(...)`, and the family lookup behind that is
//! a plain search for a real family name with no resolution of the generic
//! aliases. No installed font is called "monospace", so asking for it matches
//! nothing and rendering falls back to the sans-serif default. Generic families
//! resolve only through `QueryFamily::Generic`, which markup cannot produce.
//!
//! So a console asking for "monospace" silently renders proportional, which is
//! wrong for log output whose columns are meant to line up, and fatal for any
//! layout that computes a column from a character advance.
//!
//! # What is resolved
//!
//! The concrete family behind the `monospace` alias, which on this platform is
//! whatever fontconfig's `<alias>` blocks name. The result is a family a font
//! on this machine actually carries, taken from the matched face's own name
//! rather than from the alias, so the string handed to the renderer is one it
//! can look up by the same rule that failed before.
//!
//! `None` means no fixed-width family could be found at all, which a machine
//! with no fonts installed is entitled to. Callers keep their own default
//! rather than treating that as an error.

use fontdb::{Database, Family, Query};

/// Families to try when the system alias resolves to nothing installed.
///
/// Long-standing defaults across the distributions this targets. Tried in order
/// so two machines with the same fonts resolve the same way.
const WELL_KNOWN_FAMILIES: &[&str] = &[
    "DejaVu Sans Mono",
    "Liberation Mono",
    "Noto Sans Mono",
    "Ubuntu Mono",
    "Courier New",
];

/// The name of a fixed-width family installed on this machine.
#[must_use]
pub fn resolve_monospace_family() -> Option<String> {
    let mut database = Database::new();
    // Reads fontconfig's configuration, which is what records the concrete
    // family the `monospace` alias stands for.
    database.load_system_fonts();
    resolve_in(&database)
}

/// The resolution itself, over an already-populated database.
fn resolve_in(database: &Database) -> Option<String> {
    let preferred = std::iter::once(Family::Monospace)
        .chain(WELL_KNOWN_FAMILIES.iter().copied().map(Family::Name));
    for family in preferred {
        if let Some(name) = fixed_width_family(database, family) {
            return Some(name);
        }
    }

    // Nothing named a fixed-width family, so take one the fonts themselves
    // declare. Alphabetical rather than in load order: a directory walk is not
    // the same on two machines holding the same fonts, and a console that
    // renders in a different face on each of them is a bug report nobody can
    // reproduce.
    database
        .faces()
        .filter(|face| face.monospaced)
        .filter_map(|face| face.families.first().map(|(name, _)| name.clone()))
        .min()
}

/// The family name of the face this query matches, if that face is fixed-width.
///
/// The monospaced check is not redundant with asking for a monospace family:
/// the alias is configuration, and configuration can point anywhere. What the
/// face declares about itself is the only thing that settles it.
fn fixed_width_family(database: &Database, family: Family<'_>) -> Option<String> {
    let id = database.query(&Query {
        families: &[family],
        ..Query::default()
    })?;
    let face = database.face(id)?;
    if !face.monospaced {
        return None;
    }
    face.families.first().map(|(name, _)| name.clone())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    /// A machine with no fonts is not an error, and must not be a panic.
    #[test]
    fn an_empty_font_database_resolves_to_nothing() {
        assert_eq!(resolve_in(&Database::new()), None);
    }

    /// Whatever comes back has to be a family the renderer can look up by name,
    /// and it has to be fixed-width, which is the entire point.
    #[test]
    fn a_resolved_family_is_installed_and_fixed_width() {
        let mut database = Database::new();
        database.load_system_fonts();
        let Some(resolved) = resolve_in(&database) else {
            // No fonts on this machine. The contract is that this is `None`
            // rather than a wrong answer, which is what was just observed.
            return;
        };

        let matching: Vec<_> = database
            .faces()
            .filter(|face| {
                face.families
                    .iter()
                    .any(|(name, _)| name.eq_ignore_ascii_case(&resolved))
            })
            .collect();
        assert!(
            !matching.is_empty(),
            "{resolved} is not a family this machine has"
        );
        assert!(
            matching.iter().any(|face| face.monospaced),
            "{resolved} is not fixed-width"
        );
    }
}
