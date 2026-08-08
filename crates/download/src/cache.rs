//! Cache keys for the shared stores.
//!
//! Temporary instances are created and thrown away constantly, so Mojang
//! assets, libraries, and installer artifacts live once at launcher level
//! rather than being copied into every instance. That makes the cache path a
//! security boundary: a path is only ever derived from a content hash or from a
//! *validated logical key*, never from a filename that came out of an untrusted
//! archive or a `Content-Disposition` header ("never trust
//! filename to choose destination path").
//!
//! The other half of the contract is [`crate::DownloadManager`]'s atomic
//! finalization: because a partial download can never appear at a cache path,
//! downstream consumers are entitled to treat presence at that path as proof of a
//! verified artifact.

use std::path::{Path, PathBuf};

use ephervex_model::{Hash, HashAlgorithm};

/// Why a cache key could not be formed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CacheKeyError {
    /// A store that keys on one specific algorithm was handed another.
    #[error("cache key needs a {expected} hash, got {actual}")]
    WrongHashAlgorithm {
        expected: HashAlgorithm,
        actual: HashAlgorithm,
    },

    /// A Maven coordinate that does not parse as `group:artifact:version`.
    #[error("`{coordinate}` is not a well-formed maven coordinate")]
    MalformedCoordinate { coordinate: String },

    /// A component that would escape the store root or otherwise mean
    /// something to the filesystem rather than to us.
    #[error("`{segment}` is not a safe path segment")]
    UnsafeSegment { segment: String },

    /// An extension that is not a plain suffix.
    #[error("`{extension}` is not a safe file extension")]
    UnsafeExtension { extension: String },
}

/// The shared stores under `$XDG_CACHE_HOME/ephervex/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContentStore {
    /// Mojang asset objects.
    Assets,
    /// Loader and Minecraft libraries, addressed by Maven coordinate because
    /// the launch classpath has to name them that way.
    Libraries,
    /// Forge/NeoForge/Mojang installer jars and other artifacts the launcher is
    /// permitted to execute.
    InstallerCache,
    /// Everything else fetched by hash: pack files, managed JDK archives,
    /// logging configuration.
    Downloads,
    /// Cached provider metadata with timestamps.
    Metadata,
    /// Natives unpacked from the pre-1.19 classifier jars, and the scratch
    /// directory 1.19-and-later releases extract into at run time.
    ///
    /// Launcher-level rather than instance-local because the contents are a
    /// pure function of (version, platform): copying them into every disposable
    /// instance would duplicate content that belongs in the shared store. Unlike
    /// the other stores this one is *derived*, not downloaded — see
    /// [`CacheLayout::natives`].
    Natives,
}

impl ContentStore {
    /// Directory name under the cache root. These strings are part of the
    /// on-disk layout users can inspect, so these names are stable.
    #[must_use]
    pub const fn directory(self) -> &'static str {
        match self {
            Self::Assets => "assets",
            Self::Libraries => "libraries",
            Self::InstallerCache => "installer-cache",
            Self::Downloads => "downloads",
            Self::Metadata => "metadata",
            Self::Natives => "natives",
        }
    }
}

/// A file extension that is safe to append to a generated filename.
///
/// Wrapping it in a type means an extension can only reach a path after being
/// checked once, at construction, rather than at every call site that felt like
/// it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FileExtension(String);

impl FileExtension {
    /// Longest extension we will accept. `tar.gz` is the realistic maximum;
    /// the limit exists so a hostile value cannot blow past a path length.
    const MAX_LEN: usize = 16;

    /// Validates an extension, with or without a leading dot.
    ///
    /// Only ASCII alphanumerics and interior dots are permitted, which rules
    /// out separators, `..`, NUL, and anything a shell would treat specially.
    pub fn new(extension: &str) -> Result<Self, CacheKeyError> {
        let trimmed = extension
            .trim()
            .trim_start_matches('.')
            .to_ascii_lowercase();
        let well_formed = !trimmed.is_empty()
            && trimmed.len() <= Self::MAX_LEN
            && !trimmed.starts_with('.')
            && !trimmed.ends_with('.')
            && !trimmed.contains("..")
            && trimmed
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.');
        if well_formed {
            Ok(Self(trimmed))
        } else {
            Err(CacheKeyError::UnsafeExtension {
                extension: extension.to_owned(),
            })
        }
    }

    /// The extension without its leading dot.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Checks one component of a validated logical key.
///
/// Rejects empty strings, `.`, `..`, and anything containing a separator or a
/// character that is not part of a Maven identifier. This is what makes a
/// coordinate a *validated* logical key rather than an attacker-chosen path.
pub(crate) fn safe_segment(segment: &str) -> Result<&str, CacheKeyError> {
    let unsafe_segment = || CacheKeyError::UnsafeSegment {
        segment: segment.to_owned(),
    };
    if segment.is_empty() || segment == "." || segment == ".." {
        return Err(unsafe_segment());
    }
    let permitted = segment
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+'));
    if !permitted || segment.contains("..") {
        return Err(unsafe_segment());
    }
    Ok(segment)
}

/// A Maven coordinate: the logical key libraries are addressed by.
///
/// Libraries cannot be purely content-addressed because the generated launch
/// classpath has to reference them by the path Mojang's and the loaders'
/// metadata expects. Every component is validated on construction, so the
/// resulting relative path cannot escape the libraries store.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MavenCoordinate {
    group: String,
    artifact: String,
    version: String,
    classifier: Option<String>,
    extension: FileExtension,
}

impl MavenCoordinate {
    /// Parses `group:artifact:version[:classifier][@extension]`.
    ///
    /// This is the form Mojang, Forge, and NeoForge metadata all use for
    /// library entries.
    pub fn parse(coordinate: &str) -> Result<Self, CacheKeyError> {
        let malformed = || CacheKeyError::MalformedCoordinate {
            coordinate: coordinate.to_owned(),
        };

        let (body, extension) = match coordinate.split_once('@') {
            Some((body, extension)) => (body, FileExtension::new(extension)?),
            None => (coordinate, FileExtension::new("jar")?),
        };

        let mut parts = body.split(':');
        let group = parts.next().ok_or_else(malformed)?;
        let artifact = parts.next().ok_or_else(malformed)?;
        let version = parts.next().ok_or_else(malformed)?;
        let classifier = parts.next();
        if parts.next().is_some() {
            return Err(malformed());
        }

        // The group is dotted; each dot-separated element becomes its own
        // directory, so each one is validated independently.
        if group.is_empty() {
            return Err(malformed());
        }
        for element in group.split('.') {
            safe_segment(element)?;
        }
        safe_segment(artifact)?;
        safe_segment(version)?;
        let classifier = match classifier {
            Some(value) => Some(safe_segment(value)?.to_owned()),
            None => None,
        };

        Ok(Self {
            group: group.to_owned(),
            artifact: artifact.to_owned(),
            version: version.to_owned(),
            classifier,
            extension,
        })
    }

    /// The path relative to the libraries store, in standard Maven layout.
    #[must_use]
    pub fn relative_path(&self) -> PathBuf {
        let mut path = PathBuf::new();
        for element in self.group.split('.') {
            path.push(element);
        }
        path.push(&self.artifact);
        path.push(&self.version);
        let classifier = self
            .classifier
            .as_ref()
            .map(|value| format!("-{value}"))
            .unwrap_or_default();
        path.push(format!(
            "{}-{}{}.{}",
            self.artifact,
            self.version,
            classifier,
            self.extension.as_str()
        ));
        path
    }
}

/// Resolves cache paths under a single root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheLayout {
    root: PathBuf,
}

impl CacheLayout {
    /// Wraps a cache root, normally `$XDG_CACHE_HOME/ephervex` as resolved by
    /// `ephervex-storage`.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Root of one store.
    #[must_use]
    pub fn store_root(&self, store: ContentStore) -> PathBuf {
        self.root.join(store.directory())
    }

    /// Path of a Mojang asset object.
    ///
    /// Deliberately mirrors Mojang's own `objects/<first-two>/<sha1>` layout:
    /// the asset index maps logical names to hashes, and the game locates the
    /// file by hash, so any other layout would need a translation step that
    /// could only introduce bugs.
    pub fn asset_object(&self, sha1: &Hash) -> Result<PathBuf, CacheKeyError> {
        if sha1.algorithm != HashAlgorithm::Sha1 {
            return Err(CacheKeyError::WrongHashAlgorithm {
                expected: HashAlgorithm::Sha1,
                actual: sha1.algorithm,
            });
        }
        let digest = sha1.digest();
        let prefix = digest.get(..2).unwrap_or(digest);
        Ok(self
            .store_root(ContentStore::Assets)
            .join("objects")
            .join(prefix)
            .join(digest))
    }

    /// Path of a library, addressed by its validated Maven coordinate.
    #[must_use]
    pub fn library(&self, coordinate: &MavenCoordinate) -> PathBuf {
        self.store_root(ContentStore::Libraries)
            .join(coordinate.relative_path())
    }

    /// Directory holding one version's natives for one platform.
    ///
    /// Fallible rather than infallible because both components are *logical*
    /// keys rather than digests: a Minecraft version id arrives from Mojang's
    /// manifest and a platform key is assembled from an OS and an architecture
    /// name that also came out of metadata. Validating them here is what keeps
    /// the project's "never trust a name to choose a destination path" true for
    /// the one store whose path is not derived from a hash.
    ///
    /// Two levels rather than one flattened `<version>-<platform>` segment so
    /// that clearing one version's natives is a single `rm -r`, and so a
    /// version id containing the separator cannot collide with another pair.
    pub fn natives(&self, version: &str, platform: &str) -> Result<PathBuf, CacheKeyError> {
        Ok(self
            .store_root(ContentStore::Natives)
            .join(safe_segment(version.trim())?)
            .join(safe_segment(platform.trim())?))
    }

    /// Path of any content-addressed artifact.
    ///
    /// The filename is the digest, optionally suffixed with a validated
    /// extension so that tooling (and the user browsing the cache) can tell a
    /// `.jar` from a `.tar.gz`. Two files with the same bytes therefore share
    /// one cache entry, which is exactly what makes disposable instances cheap.
    ///
    /// Sharding on the first two hex characters keeps directory sizes
    /// reasonable; a flat directory with tens of thousands of entries is slow
    /// to list on some filesystems and unpleasant to inspect by hand.
    #[must_use]
    pub fn content(
        &self,
        store: ContentStore,
        hash: &Hash,
        extension: Option<&FileExtension>,
    ) -> PathBuf {
        let digest = hash.digest();
        let prefix = digest.get(..2).unwrap_or(digest);
        let filename = match extension {
            Some(extension) => format!("{digest}.{}", extension.as_str()),
            None => digest.to_owned(),
        };
        self.store_root(store)
            .join(hash.algorithm.to_string())
            .join(prefix)
            .join(filename)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    const SHA1: &str = "da39a3ee5e6b4b0d3255bfef95601890afd80709";

    fn layout() -> CacheLayout {
        CacheLayout::new("/cache/ephervex")
    }

    fn sha1() -> Hash {
        Hash::new(HashAlgorithm::Sha1, SHA1).expect("valid sha1")
    }

    #[test]
    fn an_asset_object_uses_mojangs_two_character_shard_layout() {
        let path = layout().asset_object(&sha1()).expect("sha1 asset");
        assert_eq!(
            path,
            PathBuf::from(format!("/cache/ephervex/assets/objects/da/{SHA1}"))
        );
    }

    #[test]
    fn an_asset_object_refuses_a_non_sha1_hash() {
        let sha512 = Hash::new(HashAlgorithm::Sha512, &"a".repeat(128)).expect("valid sha512");
        let error = layout().asset_object(&sha512).unwrap_err();
        assert!(matches!(
            error,
            CacheKeyError::WrongHashAlgorithm {
                expected: HashAlgorithm::Sha1,
                actual: HashAlgorithm::Sha512
            }
        ));
    }

    #[test]
    fn a_content_path_is_derived_from_the_digest_and_not_from_any_filename() {
        let extension = FileExtension::new(".JAR").expect("jar is a safe extension");
        let path = layout().content(ContentStore::Downloads, &sha1(), Some(&extension));
        assert_eq!(
            path,
            PathBuf::from(format!("/cache/ephervex/downloads/sha1/da/{SHA1}.jar"))
        );
    }

    #[test]
    fn a_content_path_without_an_extension_is_just_the_digest() {
        let path = layout().content(ContentStore::InstallerCache, &sha1(), None);
        assert_eq!(
            path,
            PathBuf::from(format!("/cache/ephervex/installer-cache/sha1/da/{SHA1}"))
        );
    }

    #[test]
    fn a_maven_coordinate_becomes_the_standard_layout_path() {
        let coordinate = MavenCoordinate::parse("net.minecraftforge:forge:1.12.2-14.23.5.2860")
            .expect("well-formed coordinate");
        assert_eq!(
            layout().library(&coordinate),
            PathBuf::from(
                "/cache/ephervex/libraries/net/minecraftforge/forge/1.12.2-14.23.5.2860/forge-1.12.2-14.23.5.2860.jar"
            )
        );
    }

    #[test]
    fn a_maven_classifier_and_extension_are_honored() {
        let coordinate = MavenCoordinate::parse("net.neoforged:neoforge:21.1.0:installer@zip")
            .expect("well-formed coordinate");
        assert_eq!(
            layout().library(&coordinate),
            PathBuf::from(
                "/cache/ephervex/libraries/net/neoforged/neoforge/21.1.0/neoforge-21.1.0-installer.zip"
            )
        );
    }

    #[test]
    fn a_maven_coordinate_cannot_escape_the_libraries_store() {
        for hostile in [
            "..:forge:1.0",
            "net.forge:..:1.0",
            "net.forge:forge:../../etc",
            "net/forge:forge:1.0",
            "net.forge:forge:1.0:../evil",
            "net..forge:forge:1.0",
            ":forge:1.0",
            "net.forge:forge:",
        ] {
            assert!(
                MavenCoordinate::parse(hostile).is_err(),
                "`{hostile}` should be rejected"
            );
        }
    }

    #[test]
    fn a_hostile_extension_cannot_reach_a_path() {
        for hostile in ["../evil", "ja/r", "", ".", "j.._r", "a".repeat(64).as_str()] {
            assert!(
                FileExtension::new(hostile).is_err(),
                "`{hostile}` should be rejected"
            );
        }
        assert_eq!(
            FileExtension::new("tar.gz")
                .expect("tar.gz is safe")
                .as_str(),
            "tar.gz"
        );
    }

    #[test]
    fn store_directories_match_the_documented_cache_layout() {
        assert_eq!(ContentStore::Assets.directory(), "assets");
        assert_eq!(ContentStore::Libraries.directory(), "libraries");
        assert_eq!(ContentStore::InstallerCache.directory(), "installer-cache");
        assert_eq!(ContentStore::Downloads.directory(), "downloads");
        assert_eq!(ContentStore::Metadata.directory(), "metadata");
        assert_eq!(ContentStore::Natives.directory(), "natives");
    }

    #[test]
    fn a_natives_directory_is_one_version_and_one_platform_deep() {
        assert_eq!(
            layout().natives("1.12.2", "linux-x86_64").expect("safe"),
            PathBuf::from("/cache/ephervex/natives/1.12.2/linux-x86_64")
        );
    }

    #[test]
    fn a_natives_directory_cannot_be_escaped_by_a_version_id_from_metadata() {
        for (version, platform) in [
            ("../../etc", "linux-x86_64"),
            ("1.12.2", "../.."),
            ("1.12.2/..", "linux-x86_64"),
            ("", "linux-x86_64"),
            ("1.12.2", ""),
        ] {
            assert!(
                layout().natives(version, platform).is_err(),
                "`{version}` / `{platform}` should be rejected"
            );
        }
    }
}
