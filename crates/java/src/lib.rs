//! Java runtime discovery, loader-aware compatibility rules, and managed
//! OpenJDK provisioning.
//!
//! # The question this crate answers
//!
//! *Which `java` binary should run this Minecraft version, and if there isn't
//! one, where does it come from?* Everything here is in service of that, and
//! the answer is deliberately never approximate: a runtime that will crash on
//! launch is worse than a clear refusal, because the crash arrives forty
//! seconds later wearing a Minecraft stack trace.
//!
//! # Reading the crate
//!
//! | Module | Holds |
//! |---|---|
//! | [`version`] | [`JavaVersion`] - parsing both of Java's numbering schemes |
//! | [`probe`] | asking one JVM what it is |
//! | [`discovery`] | finding candidates, deduplicating them, and probing them |
//! | [`requirement`] | the Java major a Minecraft version needs |
//! | [`rules`] | the hook a loader uses to constrain the choice further |
//! | [`expected`] | the requirement a host states, as a major plus loader bounds |
//! | [`selection`] | choosing, including instance and launch overrides |
//! | [`adoptium`] | the Temurin release index |
//! | [`managed`] | downloading and installing a runtime, without root |
//! | [`archive`] | unpacking a tarball under the security rules |
//!
//! # Three things that are easy to get wrong
//!
//! **Java 8 puts its major in the second component.** `1.8.0_492` is Java 8,
//! and reading "the first component" makes it Java 1. See [`version`].
//!
//! **The same JDK appears many times.** A Debian `/usr/lib/jvm` is mostly
//! symlinks; twelve directories routinely resolve to four executables.
//! Candidates are canonicalized before they are probed. See [`discovery`].
//!
//! **`os.arch` is not Adoptium's architecture name.** Every x86-64 JVM says
//! `amd64`; Adoptium says `x64`, and asking for `amd64` returns an empty list
//! rather than an error. See [`adoptium`].
//!
//! **A runtime for another architecture is refused, not ranked.** Selection and
//! pinned-runtime checks compare the probed `os.arch` with the host's through one
//! shared normalization, and a runtime that reported none is refused as unknown.
//! See [`rules::HostArchitecture`].
//!
//! # What this crate does not do
//!
//! * **It does not know about loaders.** `ephervex-loaders` depends on this
//!   crate, never the other way round. [`rules`] defines the vocabulary a
//!   loader states its constraints in; `ephervex-loaders` supplies the
//!   constraints themselves.
//! * **It does not launch anything.** Vanilla launch support builds the command line;
//!   [`selection::EffectiveJvmSettings`] is the memory and argument half of
//!   the selected requirement resolved and waiting for it.
//! * **It does not persist the inventory.** Probing is cached in memory by the
//!   caller - `ephervex-core` holds one inventory per process and exposes a
//!   Refresh. Nothing is written to disk, so a stale cache can
//!   never outlive the process that made it.
//!
//! # Deliberate gap: the extractor is local
//!
//! [`archive`] implements the security rules for one archive shape rather than using
//! the shared hardened extractor, which is Modrinth pack import's. When that exists this
//! module should call it instead of standing alongside it - two extractors mean
//! two sets of rules to keep in step.

pub mod adoptium;
pub mod archive;
pub mod discovery;
pub mod error;
pub mod expected;
mod install_lock;
pub mod managed;
pub mod probe;
pub mod requirement;
pub mod rules;
pub mod selection;
pub mod version;

pub use adoptium::{ADOPTIUM_API_BASE, AdoptiumArchitecture, AdoptiumClient, ManagedRelease};
pub use archive::{ArchiveError, ExtractionLimits, ExtractionReport, extract_tar_gz};
pub use discovery::{
    JavaDiscovery, JavaOrigin, JavaRuntime, JavaSourceKind, ProbeFailure, RuntimeInventory,
    SYSTEM_JVM_DIRECTORIES,
};
pub use error::{JavaError, JavaResult, ProbeError};
pub use expected::ExpectedJava;
pub use managed::{ManagedInstallation, ManagedJavaProvider};
pub use probe::{PROBE_TIMEOUT, ProbeReport, probe};
pub use requirement::{RequiredJava, RequirementBasis, fallback_major};
pub use rules::{
    HostArchitecture, JavaCompatibilityRule, LEGACY_FORGE_MAX_JAVA_8_UPDATE, RejectionReason,
    RuleRejection, RuleVerdict, VersionBound,
};
pub use selection::{
    EffectiveJvmSettings, GlobalJavaSettings, JavaPathChoice, JavaPreference, JvmField,
    JvmSettingLayer, MemoryRangeViolation, PinSource, RejectedRuntime, Selection, check_pinned,
    compare_preference, evaluate_runtime, select,
};
pub use version::JavaVersion;
