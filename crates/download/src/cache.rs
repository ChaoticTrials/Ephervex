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

use std::fmt;
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
    /// Project artwork fetched from a provider: decoration for a list, never
    /// something the launcher installs or executes.
    Artwork,
    /// Natives unpacked from the pre-1.19 classifier jars, and the scratch
    /// directory 1.19-and-later releases extract into at run time.
    ///
    /// Launcher-level rather than instance-local because the contents are a
    /// pure function of (version, platform): copying them into every disposable
    /// instance would duplicate content that belongs in the shared store. Unlike
    /// the other stores this one is *derived*, not downloaded - see
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
            Self::Artwork => "artwork",
            Self::Natives => "natives",
        }
    }

    /// Whether this store holds nothing but finalized, hash-addressed artifacts
    /// that can be fetched again from their source. A file the user supplied by
    /// hand is the exception and is recorded by [`crate::record_supplied_download`].
    ///
    /// This is the predicate the artifact-cache budget is defined over, and it
    /// is deliberately narrow. The other stores are excluded for reasons that
    /// are about *ownership*, not about size:
    ///
    /// * [`Self::Libraries`] is addressed by Maven coordinate and a retained
    ///   server's own `libraries` directory is a link to it, so an entry there
    ///   is part of an installed workspace.
    /// * [`Self::Assets`] is the directory a running game is handed as
    ///   `--assetsDir`, in Mojang's own layout rather than this launcher's.
    /// * [`Self::Natives`] is derived by unpacking rather than downloaded, and
    ///   holds directories rather than verified artifacts.
    /// * [`Self::Metadata`] carries provider documents and their freshness
    ///   envelopes: state, not artifacts.
    /// * [`Self::Artwork`] is keyed by provider and project because no provider
    ///   publishes a hash for an icon, so its entries are not verified
    ///   artifacts at all.
    #[must_use]
    pub const fn holds_reusable_artifacts(self) -> bool {
        match self {
            Self::Downloads | Self::InstallerCache => true,
            Self::Assets | Self::Libraries | Self::Metadata | Self::Artwork | Self::Natives => {
                false
            }
        }
    }
}

/// A finalized entry recognized under a content-addressed store.
///
/// Produced by [`CacheLayout::recognize_content`], which is the exact inverse of
/// [`CacheLayout::content`]. Having the two in one module is the point: a
/// collector that recognized entries by pattern-matching strings of its own
/// would drift away from the writer the first time the layout changed, and the
/// consequence of drifting is either an unswept store or a deleted file nobody
/// meant to delete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentAddress {
    pub hash: Hash,
    pub extension: Option<FileExtension>,
}

/// The extension that marks a transfer in flight rather than a finished
/// artifact.
///
/// Owned here, beside the layout it appears in, because two pieces of code
/// depend on it meaning the same thing: [`crate::DownloadManager`] writes a
/// partial download to a sibling carrying this suffix, and
/// [`CacheLayout::recognize_content`] refuses to call such a sibling a finalized
/// entry. If those two ever disagreed, maintenance could delete bytes a transfer
/// was still writing.
pub const PART_SUFFIX: &str = "part";

/// Directory under the cache root holding the provenance markers of files the
/// user supplied by hand. See [`CacheLayout::supplied_marker`].
const SUPPLIED_DIRECTORY: &str = "supplied-downloads";

/// The one format cached artwork is ever stored in.
///
/// Every desktop image decoder this launcher ships with reads PNG, which is the
/// whole reason the cache normalizes to it rather than keeping what a provider
/// happened to send.
pub const ARTWORK_FORMAT: &str = "png";

/// A file extension that is safe to append to a generated filename.
///
/// Wrapping it in a type means an extension can only reach a path after being
/// checked once, at construction, rather than at every call site that felt like
/// it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
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

/// The identity used when a child Minecraft version shadows a parent's
/// library entry.
///
/// Maven versions are intentionally absent: `g:a:2` replaces `g:a:1` during
/// version inheritance. Classifiers and extensions remain part of the key so
/// a native classifier or a non-JAR artifact is not mistaken for the main JAR.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MavenConflictKey {
    group: String,
    artifact: String,
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

    /// The version-excluding identity used for parent/child shadowing.
    #[must_use]
    pub fn conflict_key(&self) -> MavenConflictKey {
        MavenConflictKey {
            group: self.group.clone(),
            artifact: self.artifact.clone(),
            classifier: self.classifier.clone(),
            extension: self.extension.clone(),
        }
    }

    #[must_use]
    pub fn group(&self) -> &str {
        &self.group
    }

    #[must_use]
    pub fn artifact(&self) -> &str {
        &self.artifact
    }

    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    #[must_use]
    pub fn classifier(&self) -> Option<&str> {
        self.classifier.as_deref()
    }

    #[must_use]
    pub fn extension(&self) -> &FileExtension {
        &self.extension
    }
}

impl fmt::Display for MavenCoordinate {
    /// The coordinate in its canonical `group:artifact:version[:classifier]` form.
    ///
    /// Labels and errors that name a coordinate use this instead of the derived
    /// `Debug` dump, so the text reads `net.minecraftforge:forge:1.20.1-47.4.10`
    /// with the classifier attached (e.g. `:installer`) rather than a struct
    /// literal. The classifier is the artifact suffix; a coordinate parsed with
    /// it already carries it here.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.group, self.artifact, self.version)?;
        if let Some(classifier) = &self.classifier {
            write!(f, ":{classifier}")?;
        }
        Ok(())
    }
}
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

    /// Launch-visible path of a Mojang asset index.
    ///
    /// The verified canonical copy remains content-addressed in `metadata/`,
    /// but Minecraft does not receive that path. Given `--assetsDir <root>`
    /// and `--assetIndex <id>`, it unconditionally opens
    /// `<root>/indexes/<id>.json`. The id is therefore a validated logical key
    /// for a small presentation copy, never a download destination.
    pub fn asset_index(&self, id: &str) -> Result<PathBuf, CacheKeyError> {
        Ok(self
            .store_root(ContentStore::Assets)
            .join("indexes")
            .join(format!("{}.json", safe_segment(id.trim())?)))
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
    /// Where one provider project's artwork is cached.
    ///
    /// Keyed by provider and project id rather than by content hash, because a
    /// provider publishes no hash for an icon and there is nothing to verify
    /// against. That is also why artwork has a store of its own: everything in
    /// [`ContentStore::Downloads`] is addressed by a digest that was checked,
    /// and putting an unverifiable file among them would blur a distinction
    /// worth keeping sharp.
    ///
    /// # Why the name says nothing about where the image came from
    ///
    /// The extension is [`ARTWORK_FORMAT`] and never the source address's own.
    /// A provider serves whatever it likes - WebP, GIF, JPEG - and the one
    /// thing the cache promises is that what is *in* here can be displayed.
    /// Naming an entry after its origin made that promise unkeepable: a
    /// `.webp` entry is a file the desktop's decoder refuses, and it stays
    /// refused every time it is read. One canonical name per project also means
    /// a re-fetch replaces its predecessor instead of accumulating beside it.
    ///
    /// Both segments are validated, so a provider id that arrived from an API
    /// response cannot climb out of the store or name a hidden file.
    pub fn project_artwork(
        &self,
        provider: &str,
        project_id: &str,
    ) -> Result<PathBuf, CacheKeyError> {
        let provider = safe_segment(provider.trim())?;
        let project = safe_segment(project_id.trim())?;
        Ok(self
            .store_root(ContentStore::Artwork)
            .join(provider)
            .join(format!("{project}.{ARTWORK_FORMAT}")))
    }

    /// Where one provider's remembered mod display metadata is kept.
    ///
    /// Shared rather than per instance because every field in it belongs to a
    /// project or to a release, not to an installation: two instances holding
    /// the same mod describe it identically, and caching it twice would mean
    /// fetching it twice.
    ///
    /// Under the metadata store, beside the provider responses it is derived
    /// from. It is not one of them: it never expires and is never consulted to
    /// decide anything, so it carries no freshness of its own.
    pub fn mod_display(&self, provider: &str) -> Result<PathBuf, CacheKeyError> {
        let provider = safe_segment(provider.trim())?;
        Ok(self
            .store_root(ContentStore::Metadata)
            .join("mod-display")
            .join(format!("{provider}.json")))
    }

    /// Where the record that a content-addressed file was supplied by hand is
    /// kept.
    ///
    /// Beside the stores rather than inside one, so no store walk can mistake a
    /// marker for an entry and no store can be asked to collect its own
    /// provenance. Keyed by digest alone: every name a file has in a store
    /// shares the one marker.
    #[must_use]
    pub fn supplied_marker(&self, hash: &Hash) -> PathBuf {
        self.root
            .join(SUPPLIED_DIRECTORY)
            .join(hash.algorithm.to_string())
            .join(hash.digest())
    }

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

    /// Recognizes the three path components [`CacheLayout::content`] generates.
    ///
    /// `None` means "this is not one of ours". Everything a caller could
    /// otherwise be tempted to guess about is checked here: the algorithm
    /// directory has to be one this build writes, the shard has to be the
    /// digest's own first two characters, the digest has to be canonical
    /// lowercase hex of the algorithm's exact length, and a suffix has to be a
    /// [`FileExtension`] that is already in the normalized form `content`
    /// emits.
    ///
    /// A suffix that begins with [`PART_SUFFIX`] is refused outright, so a
    /// transfer in flight can never be mistaken for something finished no matter
    /// how the unique half of its name is generated.
    #[must_use]
    pub fn recognize_content(
        algorithm: &str,
        shard: &str,
        file_name: &str,
    ) -> Option<ContentAddress> {
        let algorithm = HashAlgorithm::ALL
            .into_iter()
            .find(|candidate| candidate.to_string() == algorithm)?;

        // Split at the first dot, because an extension may itself contain one
        // (`tar.gz`) while a hex digest never can.
        let (digest, suffix) = match file_name.split_once('.') {
            Some((digest, suffix)) => (digest, Some(suffix)),
            None => (file_name, None),
        };

        let hash = Hash::new(algorithm, digest)?;
        // `Hash::new` normalizes; requiring the name to survive that unchanged
        // is what rejects a mixed-case duplicate of a real entry.
        if hash.digest() != digest || !digest.starts_with(shard) || shard.len() != 2 {
            return None;
        }

        let extension = match suffix {
            Some(suffix) => {
                let extension = FileExtension::new(suffix).ok()?;
                if extension.as_str() != suffix
                    || extension.as_str().split('.').next() == Some(PART_SUFFIX)
                {
                    return None;
                }
                Some(extension)
            }
            None => None,
        };
        Some(ContentAddress { hash, extension })
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
    fn an_asset_index_uses_the_launchers_named_presentation_layout() {
        assert_eq!(
            layout().asset_index("1.12").expect("safe index id"),
            PathBuf::from("/cache/ephervex/assets/indexes/1.12.json")
        );
        assert_eq!(
            layout().asset_index("30").expect("safe index id"),
            PathBuf::from("/cache/ephervex/assets/indexes/30.json")
        );
    }

    #[test]
    fn an_asset_index_id_cannot_escape_the_assets_store() {
        for hostile in ["../evil", "a/b", "..", ""] {
            assert!(layout().asset_index(hostile).is_err(), "`{hostile}`");
        }
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
    fn recognition_is_the_exact_inverse_of_the_generated_content_path() {
        let extension = FileExtension::new("tar.gz").expect("safe extension");
        for suffix in [None, Some(&extension)] {
            let path = layout().content(ContentStore::Downloads, &sha1(), suffix);
            let mut components = path.components().rev();
            let file_name = components.next().expect("file name");
            let shard = components.next().expect("shard");
            let algorithm = components.next().expect("algorithm");

            let recognized = CacheLayout::recognize_content(
                algorithm.as_os_str().to_str().expect("utf-8"),
                shard.as_os_str().to_str().expect("utf-8"),
                file_name.as_os_str().to_str().expect("utf-8"),
            )
            .expect("a generated path must be recognized");
            assert_eq!(recognized.hash, sha1());
            assert_eq!(recognized.extension.as_ref(), suffix);
        }
    }

    #[test]
    fn a_transfer_in_flight_is_not_a_recognized_finalized_entry() {
        let part = format!("{SHA1}.jar.part.{}", "0".repeat(32));
        assert_eq!(
            CacheLayout::recognize_content("sha1", "da", &part),
            None,
            "a partial download must never look like a finalized artifact"
        );
        assert_eq!(
            CacheLayout::recognize_content("sha1", "da", &format!("{SHA1}.part.abc")),
            None
        );
    }

    #[test]
    fn nothing_but_a_canonical_content_entry_is_recognized() {
        let cases = [
            ("md5", "da", SHA1.to_owned()),
            ("sha1", "da", SHA1.to_ascii_uppercase()),
            ("sha1", "ab", SHA1.to_owned()),
            ("sha1", "d", SHA1.to_owned()),
            ("sha1", "da", format!("{SHA1}0")),
            ("sha1", "da", SHA1[..39].to_owned()),
            ("sha1", "da", format!("{SHA1}.JAR")),
            ("sha1", "da", format!("{SHA1}.")),
            ("sha1", "da", format!("{SHA1}.a/b")),
            ("sha1", "da", String::new()),
            ("sha512", "da", SHA1.to_owned()),
        ];
        for (algorithm, shard, file_name) in cases {
            assert_eq!(
                CacheLayout::recognize_content(algorithm, shard, &file_name),
                None,
                "`{algorithm}/{shard}/{file_name}` should not be recognized"
            );
        }
    }

    #[test]
    fn only_the_hash_addressed_download_stores_hold_reusable_artifacts() {
        assert!(ContentStore::Downloads.holds_reusable_artifacts());
        assert!(ContentStore::InstallerCache.holds_reusable_artifacts());
        for excluded in [
            ContentStore::Assets,
            ContentStore::Libraries,
            ContentStore::Metadata,
            ContentStore::Artwork,
            ContentStore::Natives,
        ] {
            assert!(
                !excluded.holds_reusable_artifacts(),
                "{excluded:?} is not a disposable artifact store"
            );
        }
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
    fn a_coordinate_displays_in_canonical_group_artifact_version_form() {
        let coordinate =
            MavenCoordinate::parse("net.minecraftforge:forge:1.20.1-47.4.10:installer@jar")
                .expect("well-formed coordinate");
        assert_eq!(
            coordinate.to_string(),
            "net.minecraftforge:forge:1.20.1-47.4.10:installer"
        );
    }

    #[test]
    fn the_inheritance_conflict_key_excludes_only_the_version() {
        let old = MavenCoordinate::parse("example.loader:bootstrap:1.0").expect("coordinate");
        let new = MavenCoordinate::parse("example.loader:bootstrap:2.0@JAR").expect("coordinate");
        let native = MavenCoordinate::parse("example.loader:bootstrap:2.0:natives-linux")
            .expect("coordinate");
        let zip = MavenCoordinate::parse("example.loader:bootstrap:2.0@zip").expect("coordinate");

        assert_eq!(old.conflict_key(), new.conflict_key());
        assert_ne!(old.conflict_key(), native.conflict_key());
        assert_ne!(old.conflict_key(), zip.conflict_key());
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
    fn remembered_mod_display_is_one_file_per_provider_under_metadata() {
        assert_eq!(
            layout().mod_display("modrinth").expect("safe"),
            PathBuf::from("/cache/ephervex/metadata/mod-display/modrinth.json")
        );
        // A provider name is a segment like any other, and one that arrived
        // from configuration must not be able to climb out of the store.
        assert!(layout().mod_display("../../etc").is_err());
        assert!(layout().mod_display("").is_err());
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
