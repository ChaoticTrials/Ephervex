//! Java overrides attached to one instance or server.
//!
//! These started life in `ephervex-storage`, which was the only crate that read
//! them. Java runtime support gave them a second reader: `ephervex-java` resolves an
//! override against the global settings before it selects a runtime, and it
//! must not depend on the storage crate to do so. A type two layers need is
//! what this crate is for.
//!
//! # Why an absent override is not the same as an empty one
//!
//! The per-instance panel has a "Use global settings" checkbox, and the
//! distinction it draws survives all the way down here. `None` means the
//! checkbox is ticked and the instance follows the global settings wherever
//! they go. `Some(JvmSettings { max_memory_mib: Some(4096),.. })` means the
//! user pinned 4096 MiB — which stays 4096 even after the global maximum
//! changes, and which is a different thing from "currently agrees with the
//! global value". The same reasoning applies field by field inside
//! `JvmSettings`, which is why every field is itself optional.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A specific Java runtime pinned to one instance.
///
/// Imported archives may *name* a Java path; that path is never trusted or
/// executed just because it appeared in an imported archive.
/// A value here means the user chose it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JavaOverride {
    pub path: PathBuf,
}

impl JavaOverride {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

/// Per-instance JVM settings overriding the global ones.
///
/// Every field is optional so that overriding the heap size does not silently
/// freeze the JVM arguments at whatever they happened to be that day.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct JvmSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_memory_mib: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_memory_mib: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub jvm_args: Vec<String>,
}

impl JvmSettings {
    /// Whether this override actually pins anything.
    ///
    /// A `Some(JvmSettings::default())` is a real state — the user disabled
    /// "use global settings" but has not yet changed a value — and it is worth
    /// being able to name rather than collapsing it into `None`.
    #[must_use]
    pub fn pins_nothing(&self) -> bool {
        self.min_memory_mib.is_none() && self.max_memory_mib.is_none() && self.jvm_args.is_empty()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn an_override_that_pins_nothing_is_still_an_override() {
        let explicit = JvmSettings::default();
        assert!(explicit.pins_nothing());
        // The type system distinguishes an explicit compatibility mode from `None`.
        let stored: Option<JvmSettings> = Some(explicit);
        assert!(stored.is_some());
    }

    #[test]
    fn an_empty_override_serializes_without_null_fields() {
        // The instance metadata file is something users attach to bug reports;
        // three `null`s per instance is noise nobody has to read.
        let json = serde_json::to_string(&JvmSettings::default()).expect("serialize");
        assert_eq!(json, "{}");
    }
}
