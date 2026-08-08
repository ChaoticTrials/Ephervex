//! The machine a launch is being built for.
//!
//! Mojang's rules are written against Mojang's names, and those names are not
//! Rust's: `osx` rather than `macos`, `x86_64` rather than `x86_64` only by
//! coincidence, and `arm64` where Rust says `aarch64`. Translating once, here,
//! is what keeps `x86` from being compared against `x86_64` somewhere in the
//! middle of rule evaluation — the same argument that made Java runtime support translate
//! `amd64` to Adoptium's `x64` at the API boundary rather than passing the
//! JVM's spelling through.
//!
//! # Why this is a value and not a set of `cfg!`s
//!
//! A launcher has to be able to *explain* a launch: which arguments were
//! included, and under which conditions. Tests need to evaluate the same rules
//! against Windows and macOS to prove that a Linux launch drops what it should,
//! and `cfg!` cannot be pointed at another platform. [`Platform::host`] is the
//! only place the real machine is read.

use crate::rules::OsName;

/// The operating system, architecture, and OS version a launch targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Platform {
    pub os: OsName,
    /// Mojang's spelling: `x86`, `x86_64`, `arm64`.
    pub arch: String,
    /// The OS version, as the OS reports it. Empty when it could not be read.
    ///
    /// Only ever consulted by an `os.version` condition, and every such
    /// condition Mojang has published in the supported range also names
    /// Windows — so on Linux this value decides nothing. It is carried anyway
    /// because a rule that *could* be evaluated should be, and because a
    /// condition silently ignored is a condition nobody notices going wrong.
    pub os_version: String,
}

impl Platform {
    /// The machine this process is running on.
    #[must_use]
    pub fn host() -> Self {
        Self {
            os: host_os(),
            arch: host_arch().to_owned(),
            os_version: host_os_version(),
        }
    }

    /// A platform with an explicit OS and architecture, for tests and for
    /// explaining what *would* happen elsewhere.
    #[must_use]
    pub fn new(os: OsName, arch: impl Into<String>, os_version: impl Into<String>) -> Self {
        Self {
            os,
            arch: arch.into(),
            os_version: os_version.into(),
        }
    }

    /// The directory name natives for this platform are cached under.
    ///
    /// Part of a cache path, so it is deliberately built from values this
    /// module produced rather than from anything that arrived in metadata.
    #[must_use]
    pub fn cache_key(&self) -> String {
        format!("{}-{}", self.os, self.arch)
    }

    /// The classifier suffix a pre-1.19 `natives` map names this OS by.
    #[must_use]
    pub fn natives_key(&self) -> &'static str {
        match self.os {
            OsName::Linux => "linux",
            OsName::Windows => "windows",
            OsName::Osx => "osx",
            OsName::Other(_) => "",
        }
    }

    /// The value a pre-1.19 natives classifier's `${arch}` placeholder takes.
    ///
    /// Mojang used this on exactly one library — `com.mojang:text2speech`,
    /// whose Windows classifier is `natives-windows-${arch}` — and the values
    /// it published are `32` and `64`. It never applies on Linux, and it is
    /// implemented rather than ignored because an unsubstituted `${arch}` would
    /// silently look up a classifier that does not exist.
    #[must_use]
    pub fn natives_arch(&self) -> &'static str {
        match self.arch.as_str() {
            "x86" => "32",
            _ => "64",
        }
    }
}

impl Default for Platform {
    fn default() -> Self {
        Self::host()
    }
}

impl std::fmt::Display for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(&self.cache_key())
    }
}

fn host_os() -> OsName {
    match std::env::consts::OS {
        "linux" => OsName::Linux,
        "windows" => OsName::Windows,
        // Mojang spells it `osx` in every version JSON; Rust says `macos`.
        "macos" => OsName::Osx,
        other => OsName::Other(other.to_owned()),
    }
}

/// Rust's architecture name in Mojang's spelling.
fn host_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86" => "x86",
        "aarch64" => "arm64",
        other => other,
    }
}

/// The kernel release, which is what a Linux "OS version" usefully means.
///
/// `/proc/sys/kernel/osrelease` rather than shelling out to `uname`: forking a
/// process to read one line is the kind of cost a launch does not need, and it
/// would make the value unavailable in a sandbox that permits reads but not
/// execs.
fn host_os_version() -> String {
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|value| value.trim().to_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn the_host_platform_speaks_mojangs_names_rather_than_rusts() {
        let platform = Platform::host();
        assert!(
            matches!(
                platform.arch.as_str(),
                "x86"
                    | "x86_64"
                    | "arm64"
                    | "arm"
                    | "riscv64"
                    | "powerpc64"
                    | "s390x"
                    | "loongarch64"
            ),
            "unexpected architecture spelling: {}",
            platform.arch
        );
        // The translation that actually matters is `aarch64` -> `arm64`: a
        // library rule reading `arm64` must not be compared against Rust's word.
        assert_ne!(platform.arch, "aarch64");
        assert_ne!(platform.os, OsName::Other("macos".to_owned()));
    }

    #[test]
    fn a_cache_key_names_the_os_and_the_architecture() {
        let platform = Platform::new(OsName::Linux, "x86_64", "6.8.0");
        assert_eq!(platform.cache_key(), "linux-x86_64");
        assert_eq!(platform.to_string(), "linux-x86_64");
    }

    #[test]
    fn the_natives_arch_placeholder_takes_mojangs_bit_width() {
        // `natives-windows-${arch}` is the only place this appears, and the
        // published classifiers are `natives-windows-32` and `-64`.
        assert_eq!(
            Platform::new(OsName::Windows, "x86", "10.0").natives_arch(),
            "32"
        );
        assert_eq!(
            Platform::new(OsName::Windows, "x86_64", "10.0").natives_arch(),
            "64"
        );
    }

    #[test]
    fn the_natives_key_is_the_word_a_pre_1_19_natives_map_uses() {
        assert_eq!(
            Platform::new(OsName::Linux, "x86_64", "").natives_key(),
            "linux"
        );
        assert_eq!(Platform::new(OsName::Osx, "arm64", "").natives_key(), "osx");
        assert_eq!(
            Platform::new(OsName::Windows, "x86_64", "").natives_key(),
            "windows"
        );
        assert!(
            Platform::new(OsName::Other("haiku".to_owned()), "x86_64", "")
                .natives_key()
                .is_empty(),
            "a platform Mojang has never shipped natives for matches no classifier"
        );
    }
}
