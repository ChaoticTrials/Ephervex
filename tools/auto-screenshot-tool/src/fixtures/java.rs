//! The Java runtimes the screenshots show as installed, and which settings pin them.

use ephervex_gui::java_view::reported_runtime_line;
use ephervex_gui::ui::JavaRow;
use ephervex_i18n::{Arg, lookup_args};

/// One runtime of the Java inventory.
pub(crate) struct Runtime {
    pub(crate) version: &'static str,
    pub(crate) vendor: &'static str,
    pub(crate) path: &'static str,
    /// Where discovery found it, as Settings > Java names the origin.
    pub(crate) origin: &'static str,
    pub(crate) major: u8,
}

const ARCH: &str = "amd64";

pub(crate) const JAVA_8: Runtime = Runtime {
    version: "1.8.0_432",
    vendor: "Azul Systems, Inc.",
    path: "/demo/usr/lib/jvm/zulu-8/bin/java",
    origin: "system",
    major: 8,
};

/// The runtime CaveStone pins, at the version its recorded diagnosis reported.
pub(crate) const JAVA_17: Runtime = Runtime {
    version: "17.0.20.1",
    vendor: "Eclipse Adoptium",
    path: "/demo/jvm/temurin-17/bin/java",
    origin: "java-home",
    major: 17,
};

pub(crate) const JAVA_21: Runtime = Runtime {
    version: "21.0.5",
    vendor: "Eclipse Adoptium",
    path: "/demo/ephervex/runtimes/temurin-21/bin/java",
    origin: "managed",
    major: 21,
};

pub(crate) const JAVA_25: Runtime = Runtime {
    version: "25.0.1",
    vendor: "Eclipse Adoptium",
    path: "/demo/home/.sdkman/candidates/java/25-tem/bin/java",
    origin: "sdkman",
    major: 25,
};

pub(crate) const INVENTORY: [&Runtime; 4] = [&JAVA_8, &JAVA_17, &JAVA_21, &JAVA_25];

/// The one host that pins its own runtime.
pub(crate) const PINNING_INSTANCE: &str = "CaveStone";

impl Runtime {
    /// "Java 17.0.20.1, Eclipse Adoptium, amd64".
    pub(crate) fn line(&self) -> String {
        reported_runtime_line(self.version, self.vendor, ARCH)
    }

    /// The settings that name this runtime, as Settings > Java lists them.
    fn pinning(&self) -> String {
        if self.path == JAVA_17.path {
            lookup_args(
                "gui-java-pinned-instance",
                [("name", Arg::from(PINNING_INSTANCE))],
            )
        } else {
            String::new()
        }
    }

    pub(crate) fn row(&self) -> JavaRow {
        let pinning = self.pinning();
        JavaRow {
            path: self.path.into(),
            version: self.version.into(),
            vendor: self.vendor.into(),
            origin: self.origin.into(),
            compatible: format!("Java {}", self.major).into(),
            title: self.line().into(),
            pinned_by: if pinning.is_empty() {
                pinning.clone()
            } else {
                lookup_args(
                    "gui-java-pinned-by",
                    [("hosts", Arg::from(pinning.as_str()))],
                )
            }
            .into(),
            pins_known: true,
            pinning: pinning.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::recorded;

    #[test]
    fn the_pinned_runtime_is_the_one_the_recorded_diagnosis_reported() {
        assert_eq!(JAVA_17.version, recorded::diagnostics().java);
    }

    #[test]
    fn only_the_pinned_runtime_names_its_pins() {
        let pinned = INVENTORY
            .iter()
            .filter(|runtime| !runtime.row().pinned_by.is_empty())
            .collect::<Vec<_>>();
        assert_eq!(pinned.len(), 1);
        assert_eq!(pinned[0].path, JAVA_17.path);
    }
}
