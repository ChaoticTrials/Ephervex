//! Turning a provider's project icon into something the desktop can draw.
//!
//! # The bug this module exists to prevent
//!
//! A mod catalog serves whatever image format it likes. Modrinth serves a good
//! deal of WebP. The desktop's image decoder is built with PNG and JPEG only,
//! so an artwork cache that stored what arrived — under the source address's own
//! extension, which is what it used to do — produced entries the frontend could
//! never open:
//!
//! ```text
//! Error loading image from …/artwork/modrinth/LOpKHB2A.webp:
//! The image format WebP is not supported
//! ```
//!
//! and produced them *permanently*, because the cached file was there, so
//! nothing ever fetched it again.
//!
//! The fix is to make the cache promise something: everything in it is PNG.
//! What a provider sent is decoded once, re-encoded, and published under a name
//! that says nothing about where it came from. A frontend then does what a
//! frontend should — load a local file, and fall back if it cannot.
//!
//! Adding a WebP decoder to *this* crate happens to give the desktop one too,
//! because Cargo unifies features across a build and the frontend depends on
//! this crate. That is a coincidence rather than the fix: it holds only while
//! both resolve to the same `image` release and would disappear silently the
//! day they stop. What the frontend is handed is PNG either way.
//!
//! # Decoding is the dangerous part
//!
//! A bounded download is not a bounded image. Every compressed format can
//! expand enormously: a few kilobytes of PNG can declare a 30000×30000 canvas,
//! which is 3.6 GB of pixels the decoder would try to allocate. So the size of
//! the *decoded* image is bounded too, before anything is allocated, and by
//! numbers chosen for project icons rather than for photographs.
//!
//! Every refusal here is ordinary. Artwork is decoration: a rejected image
//! costs a thumbnail and nothing else, and the mod it belongs to stays exactly
//! where it was in the list.

use std::path::{Path, PathBuf};

use ephervex_download::{
    CacheLayout, DomainAllowlist, DownloadManager, DownloadRequest, OperationReporter,
};
use ephervex_mod_platform::ModProject;
use ephervex_pack::PackError;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::error::{CoreError, CoreResult};

/// The largest a project icon may be on either side.
///
/// Generous for something drawn at 38 logical pixels, and small enough that the
/// worst case is a rounding error rather than a memory event.
const MAX_ARTWORK_DIMENSION: u32 = 2048;

/// The largest a project icon may be in total pixels.
///
/// Separate from the per-side bound because the two catch different things: a
/// 2048×2048 image passes both side limits and is still four megapixels, and a
/// 16×100000 image is inside no reasonable idea of an icon at all.
const MAX_ARTWORK_PIXELS: u64 = 1024 * 1024;

/// The ceiling the decoder itself is given, in bytes.
///
/// Enforced by the decoder while it works rather than checked afterwards, so a
/// format that lies about its dimensions still cannot allocate its way through
/// the machine. Four bytes per pixel at the pixel ceiling, doubled for the
/// intermediate buffers a decoder is entitled to.
const MAX_DECODE_BYTES: u64 = MAX_ARTWORK_PIXELS * 4 * 2;

/// The PNG signature, which is what a usable cache entry starts with.
const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// Puts one project's icon in the shared artwork cache and returns its path.
///
/// Already-normalized artwork is served without a request or a decode, which is
/// what keeps opening a workspace cheap. "Already normalized" is checked by
/// looking at the file rather than by trusting its name: an entry left by an
/// older build can be named `.png` and contain something else, and the whole
/// point of this change is that such an entry must not keep failing forever.
///
/// An icon has no published hash, so there is nothing to verify it against and
/// nothing that would make re-fetching it more correct. What protects the
/// launcher is the same transport and host policy an untrusted pack's downloads
/// are held to, a ceiling on the response, and the decode bounds below.
pub(crate) async fn cache_project_artwork(
    manager: &DownloadManager,
    cache: &CacheLayout,
    allowlist: &DomainAllowlist,
    project: &ModProject,
    icon_url: &str,
    reporter: &OperationReporter,
    cancel: &CancellationToken,
) -> CoreResult<PathBuf> {
    let url = allowlist
        .validate(icon_url)
        .map_err(|source| PackError::DownloadUrlRejected {
            path: project.id.clone(),
            source,
        })?;

    // The destination is derived from the provider and project alone. Nothing
    // a remote server said chooses where this lands, including the format it
    // decided to answer with.
    let destination = cache
        .project_artwork(project.provider.as_str(), &project.id)
        .map_err(|error| PackError::EntryPathUnsafe {
            entry: error.to_string(),
        })?;
    if is_normalized(&destination) {
        return Ok(destination);
    }

    let staging = staging_path(&destination);
    let fetched = manager
        .download(
            &DownloadRequest::new(url, staging.clone()).labeled(project.id.clone()),
            reporter,
            cancel,
        )
        .await;
    let outcome = match fetched {
        Ok(_) => publish_normalized(&staging, &destination, &project.id),
        Err(source) => Err(PackError::DownloadFailed {
            path: project.id.clone(),
            source: Box::new(source),
        }
        .into()),
    };

    // The download is scratch either way. A leftover would be read as a
    // half-finished artifact by the next attempt.
    discard(&staging);
    outcome.map(|()| destination)
}

/// Whether a cache entry is a PNG this build can hand to a frontend.
///
/// Reads the signature rather than the extension. An entry written by a build
/// that named files after their source address may be a `.png` holding WebP,
/// and believing the name is how that entry survives forever.
fn is_normalized(path: &Path) -> bool {
    let Ok(head) = read_prefix(path, PNG_MAGIC.len()) else {
        return false;
    };
    head == PNG_MAGIC
}

fn read_prefix(path: &Path, bytes: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    let mut file = std::fs::File::open(path)?;
    let mut head = vec![0; bytes];
    let read = file.read(&mut head)?;
    head.truncate(read);
    Ok(head)
}

/// Decodes what was fetched and publishes it as PNG, atomically.
///
/// The encoded image is written beside its destination and renamed into place,
/// so a frontend reading the cache concurrently sees either the previous entry
/// or the complete new one, never a partially written file.
fn publish_normalized(source: &Path, destination: &Path, project: &str) -> CoreResult<()> {
    let png = normalize(source, project)?;

    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).map_err(|source| PackError::Unwritable {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let temporary = staging_path(destination);
    std::fs::write(&temporary, &png).map_err(|source| PackError::Unwritable {
        path: temporary.clone(),
        source,
    })?;
    if let Err(source) = std::fs::rename(&temporary, destination) {
        discard(&temporary);
        return Err(PackError::Unwritable {
            path: destination.to_path_buf(),
            source,
        }
        .into());
    }
    Ok(())
}

/// Decodes one downloaded image and re-encodes it as PNG.
///
/// The format is taken from the bytes rather than from the file name, because
/// the file name came from a URL and a URL is not evidence of anything. A
/// format this build was not given a decoder for, malformed data, and an image
/// too large to be an icon all come back as the same kind of refusal: this is
/// not usable artwork.
pub(crate) fn normalize(source: &Path, project: &str) -> CoreResult<Vec<u8>> {
    let (width, height) = dimensions(source, project)?;
    let pixels = u64::from(width) * u64::from(height);
    if width > MAX_ARTWORK_DIMENSION || height > MAX_ARTWORK_DIMENSION {
        return Err(CoreError::ArtworkUnusable {
            project: project.to_owned(),
            detail: format!("{width}x{height} is larger than a project icon can be"),
        });
    }
    if pixels > MAX_ARTWORK_PIXELS {
        return Err(CoreError::ArtworkUnusable {
            project: project.to_owned(),
            detail: format!("{pixels} pixels is more than a project icon can be"),
        });
    }

    let mut reader = reader(source, project)?;
    // The decoder enforces this while it works. The checks above have already
    // read the header, but a header is a claim: a format that under-reports its
    // size would otherwise allocate past every bound this function states.
    let mut limits = image::Limits::no_limits();
    limits.max_image_width = Some(MAX_ARTWORK_DIMENSION);
    limits.max_image_height = Some(MAX_ARTWORK_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);

    let decoded = reader
        .decode()
        .map_err(|error| CoreError::ArtworkUnusable {
            project: project.to_owned(),
            detail: error.to_string(),
        })?;

    let mut png = Vec::new();
    decoded
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|error| CoreError::ArtworkUnusable {
            project: project.to_owned(),
            detail: error.to_string(),
        })?;
    Ok(png)
}

/// What the image says it is, before anything is decoded.
fn dimensions(source: &Path, project: &str) -> CoreResult<(u32, u32)> {
    reader(source, project)?
        .into_dimensions()
        .map_err(|error| CoreError::ArtworkUnusable {
            project: project.to_owned(),
            detail: error.to_string(),
        })
}

/// A reader whose format came from the file's own bytes.
fn reader(
    source: &Path,
    project: &str,
) -> CoreResult<image::ImageReader<std::io::BufReader<std::fs::File>>> {
    image::ImageReader::open(source)
        .and_then(image::ImageReader::with_guessed_format)
        .map_err(|error| CoreError::ArtworkUnusable {
            project: project.to_owned(),
            detail: error.to_string(),
        })
}

/// Where a file waits while it is being fetched or written.
fn staging_path(destination: &Path) -> PathBuf {
    let mut name = destination.as_os_str().to_owned();
    name.push(".part");
    PathBuf::from(name)
}

/// Removes scratch, reporting rather than propagating.
///
/// A leftover in the artwork store is invisible to the game and to the user,
/// and it is never the failure worth returning from an operation whose real
/// result is a missing thumbnail.
fn discard(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => warn!(
            path = %path.display(),
            %error,
            "could not remove a partly fetched artwork file"
        ),
    }
}

/// Reports one project's artwork failure at the level it deserves.
pub(crate) fn report_failure(project: &str, error: &CoreError) {
    debug!(%project, %error, "no artwork for a managed mod");
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use image::{ImageFormat, RgbaImage};

    use super::*;

    /// An image of the given size, encoded in the given format.
    fn encoded(width: u32, height: u32, format: ImageFormat) -> Vec<u8> {
        let image = RgbaImage::from_fn(width, height, |x, y| {
            let channel = |value: u32| u8::try_from(value % 256).unwrap_or(0);
            image::Rgba([channel(x), channel(y), 128, 255])
        });
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut std::io::Cursor::new(&mut bytes), format)
            .expect("encode the fixture");
        bytes
    }

    fn written(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes).expect("write the fixture");
        path
    }

    #[test]
    fn a_webp_icon_becomes_a_png_the_desktop_decoder_can_open() {
        // The exact case that produced "The image format WebP is not
        // supported" in front of a user.
        let dir = tempfile::tempdir().expect("temp dir");
        let source = written(dir.path(), "icon.webp", &encoded(64, 64, ImageFormat::WebP));

        let png = normalize(&source, "LOpKHB2A").expect("a normalized image");
        assert_eq!(
            png.get(..PNG_MAGIC.len()),
            Some(PNG_MAGIC.as_slice()),
            "the cache stores PNG whatever the provider sent"
        );
        assert_eq!(
            image::guess_format(&png).expect("a format"),
            ImageFormat::Png
        );
        let decoded = image::load_from_memory(&png).expect("the result decodes");
        assert_eq!((decoded.width(), decoded.height()), (64, 64));
    }

    #[test]
    fn an_icon_that_was_already_png_normalizes_to_png_as_well() {
        // Nothing is special-cased for the format that happens to match: one
        // path means the cache's promise holds for every entry.
        let dir = tempfile::tempdir().expect("temp dir");
        let source = written(dir.path(), "icon.png", &encoded(96, 96, ImageFormat::Png));

        let png = normalize(&source, "u6dRKJwZ").expect("a normalized image");
        assert_eq!(png.get(..PNG_MAGIC.len()), Some(PNG_MAGIC.as_slice()));
        assert_eq!(image::load_from_memory(&png).expect("decodes").width(), 96);
    }

    #[test]
    fn a_jpeg_icon_normalizes_too() {
        let dir = tempfile::tempdir().expect("temp dir");
        let source = written(dir.path(), "icon.jpg", &encoded(48, 48, ImageFormat::Jpeg));
        let png = normalize(&source, "9s6osm5g").expect("a normalized image");
        assert_eq!(png.get(..PNG_MAGIC.len()), Some(PNG_MAGIC.as_slice()));
    }

    #[test]
    fn the_format_comes_from_the_bytes_rather_than_the_file_name() {
        // The name came from a URL, and a URL is not evidence of anything.
        let dir = tempfile::tempdir().expect("temp dir");
        let source = written(dir.path(), "icon.png", &encoded(32, 32, ImageFormat::WebP));

        let png = normalize(&source, "LOpKHB2A").expect("the bytes are what count");
        assert_eq!(png.get(..PNG_MAGIC.len()), Some(PNG_MAGIC.as_slice()));
    }

    #[test]
    fn malformed_image_data_is_refused_rather_than_panicking() {
        let dir = tempfile::tempdir().expect("temp dir");
        for (name, bytes) in [
            ("empty.png", b"".as_slice()),
            ("text.png", b"this is not an image at all"),
            // A real PNG signature followed by nothing that can be decoded.
            ("truncated.png", &encoded(16, 16, ImageFormat::Png)[..20]),
        ] {
            let source = written(dir.path(), name, bytes);
            let error = normalize(&source, "LOpKHB2A").expect_err("must refuse");
            assert!(
                matches!(error, CoreError::ArtworkUnusable { .. }),
                "{name} produced {error:?}"
            );
        }
    }

    #[test]
    fn a_format_this_build_has_no_decoder_for_is_refused() {
        // Only the codecs a mod catalog actually serves are compiled in. One
        // it does not is a refusal, not a crash and not a stored file.
        let dir = tempfile::tempdir().expect("temp dir");
        // A BMP header, which this build deliberately cannot read.
        let source = written(
            dir.path(),
            "icon.bmp",
            b"BM\x46\x00\x00\x00\x00\x00\x00\x00",
        );
        assert!(matches!(
            normalize(&source, "LOpKHB2A"),
            Err(CoreError::ArtworkUnusable { .. })
        ));
    }

    #[test]
    fn an_image_too_large_to_be_an_icon_is_refused_before_it_is_decoded() {
        let dir = tempfile::tempdir().expect("temp dir");
        // Well inside the per-side bound and far past the pixel bound.
        let source = written(
            dir.path(),
            "wide.png",
            &encoded(2048, 2048, ImageFormat::Png),
        );

        let error = normalize(&source, "LOpKHB2A").expect_err("must refuse");
        assert!(
            matches!(error, CoreError::ArtworkUnusable { ref detail, .. } if detail.contains("pixels")),
            "the pixel ceiling is what should have caught it: {error:?}"
        );
    }

    #[test]
    fn an_image_wider_than_any_icon_is_refused_on_its_side_length() {
        let dir = tempfile::tempdir().expect("temp dir");
        let source = written(dir.path(), "wide.png", &encoded(4096, 8, ImageFormat::Png));

        let error = normalize(&source, "LOpKHB2A").expect_err("must refuse");
        assert!(
            matches!(error, CoreError::ArtworkUnusable { ref detail, .. } if detail.contains("4096x8")),
            "the side ceiling is what should have caught it: {error:?}"
        );
    }

    #[test]
    fn an_icon_at_the_limits_is_still_accepted() {
        // The bounds exist to refuse wallpaper, not to refuse a large icon.
        let dir = tempfile::tempdir().expect("temp dir");
        let source = written(
            dir.path(),
            "big.png",
            &encoded(1024, 1024, ImageFormat::Png),
        );
        assert!(normalize(&source, "LOpKHB2A").is_ok());
    }

    #[test]
    fn a_cache_entry_is_reusable_only_when_it_really_is_png() {
        let dir = tempfile::tempdir().expect("temp dir");
        let good = written(dir.path(), "good.png", &encoded(16, 16, ImageFormat::Png));
        assert!(is_normalized(&good));

        // What an older build left behind: the right name, the wrong bytes.
        // Believing the name is how such an entry fails forever.
        let stale = written(dir.path(), "stale.png", &encoded(16, 16, ImageFormat::WebP));
        assert!(!is_normalized(&stale));

        assert!(!is_normalized(&dir.path().join("absent.png")));
        assert!(!is_normalized(dir.path()));
    }

    #[test]
    fn publishing_replaces_the_entry_atomically_and_leaves_no_scratch() {
        let dir = tempfile::tempdir().expect("temp dir");
        let source = written(dir.path(), "icon.webp", &encoded(24, 24, ImageFormat::WebP));
        let destination = dir.path().join("store").join("LOpKHB2A.png");

        publish_normalized(&source, &destination, "LOpKHB2A").expect("publish");
        assert!(is_normalized(&destination));
        assert!(
            !staging_path(&destination).exists(),
            "the temporary file is not left in the store"
        );
    }

    #[test]
    fn a_refused_image_publishes_nothing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let source = written(dir.path(), "icon.webp", b"not an image");
        let destination = dir.path().join("store").join("LOpKHB2A.png");

        assert!(publish_normalized(&source, &destination, "LOpKHB2A").is_err());
        assert!(
            !destination.exists(),
            "a cache entry that cannot be displayed is worse than none"
        );
        assert!(!staging_path(&destination).exists());
    }

    // -- the cache, end to end ---------------------------------------------

    use ephervex_download::{DownloaderConfig, HttpConfig, RetryPolicy};
    use ephervex_mod_platform::SideSupport;
    use ephervex_model::ModProvider;
    use wiremock::matchers::{method, path as route};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// A manager that gives up immediately, so a deliberate failure is quick.
    fn manager() -> DownloadManager {
        DownloadManager::new(DownloaderConfig {
            retry: RetryPolicy::none(),
            http: HttpConfig::for_loopback_tests(),
            ..DownloaderConfig::default()
        })
        .expect("client builds")
    }

    fn project(id: &str) -> ModProject {
        ModProject {
            provider: ModProvider::Modrinth,
            id: id.to_owned(),
            slug: Some(id.to_ascii_lowercase()),
            title: id.to_owned(),
            description: String::new(),
            author: None,
            client_side: SideSupport::Required,
            server_side: SideSupport::Required,
            icon_url: None,
        }
    }

    /// Serves one image and counts how often it was asked for.
    async fn serve(server: &MockServer, path: &str, bytes: Vec<u8>) {
        Mock::given(method("GET"))
            .and(route(path))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes))
            .mount(server)
            .await;
    }

    async fn fetch(
        server: &MockServer,
        cache: &CacheLayout,
        project: &ModProject,
        path: &str,
    ) -> CoreResult<PathBuf> {
        cache_project_artwork(
            &manager(),
            cache,
            &DomainAllowlist::for_loopback_tests("127.0.0.1"),
            project,
            &format!("{}{path}", server.uri()),
            &OperationReporter::silent("test-artwork"),
            &CancellationToken::new(),
        )
        .await
    }

    #[tokio::test]
    async fn a_webp_icon_is_cached_under_a_name_and_a_format_the_frontend_can_open() {
        let server = MockServer::start().await;
        serve(&server, "/icon.webp", encoded(64, 64, ImageFormat::WebP)).await;
        let dir = tempfile::tempdir().expect("temp dir");
        let cache = CacheLayout::new(dir.path());

        let cached = fetch(&server, &cache, &project("LOpKHB2A"), "/icon.webp")
            .await
            .expect("artwork");

        assert_eq!(
            cached.extension().and_then(|it| it.to_str()),
            Some("png"),
            "the entry is named for what it is, not for where it came from"
        );
        assert!(is_normalized(&cached));
        assert!(image::open(&cached).is_ok(), "and it opens as an image");
    }

    #[tokio::test]
    async fn a_cached_icon_is_reused_without_a_second_request() {
        let server = MockServer::start().await;
        serve(&server, "/icon.webp", encoded(48, 48, ImageFormat::WebP)).await;
        let dir = tempfile::tempdir().expect("temp dir");
        let cache = CacheLayout::new(dir.path());
        let project = project("LOpKHB2A");

        let first = fetch(&server, &cache, &project, "/icon.webp")
            .await
            .expect("artwork");
        let second = fetch(&server, &cache, &project, "/icon.webp")
            .await
            .expect("artwork");

        assert_eq!(first, second);
        let requests = server.received_requests().await.unwrap_or_default();
        assert_eq!(
            requests.len(),
            1,
            "opening a workspace again must not re-fetch what is already usable"
        );
    }

    #[tokio::test]
    async fn an_entry_an_older_build_left_behind_does_not_trap_the_cache() {
        // Two shapes of legacy state at once: the `.webp` file the old naming
        // produced, which the new path never consults, and a `.png` file
        // holding the WebP bytes that produced the reported error, which is
        // regenerated rather than believed.
        let server = MockServer::start().await;
        serve(&server, "/icon.webp", encoded(32, 32, ImageFormat::WebP)).await;
        let dir = tempfile::tempdir().expect("temp dir");
        let cache = CacheLayout::new(dir.path());
        let project = project("LOpKHB2A");

        let canonical = cache
            .project_artwork(project.provider.as_str(), &project.id)
            .expect("a cache path");
        std::fs::create_dir_all(canonical.parent().expect("parent")).expect("store");
        std::fs::write(
            canonical.with_extension("webp"),
            encoded(8, 8, ImageFormat::WebP),
        )
        .expect("legacy entry");
        std::fs::write(&canonical, encoded(8, 8, ImageFormat::WebP)).expect("stale entry");

        let cached = fetch(&server, &cache, &project, "/icon.webp")
            .await
            .expect("artwork");

        assert_eq!(cached, canonical);
        assert!(
            is_normalized(&cached),
            "the entry the frontend reads is PNG now"
        );
        assert_eq!(
            image::open(&cached).expect("opens").width(),
            32,
            "and it is the freshly fetched image rather than the stale one"
        );
    }

    #[tokio::test]
    async fn an_undecodable_response_leaves_no_cache_entry_and_no_scratch() {
        let server = MockServer::start().await;
        serve(&server, "/icon.webp", b"not an image at all".to_vec()).await;
        let dir = tempfile::tempdir().expect("temp dir");
        let cache = CacheLayout::new(dir.path());
        let project = project("LOpKHB2A");

        let error = fetch(&server, &cache, &project, "/icon.webp")
            .await
            .expect_err("must refuse");
        assert!(matches!(error, CoreError::ArtworkUnusable { .. }));

        let canonical = cache
            .project_artwork(project.provider.as_str(), &project.id)
            .expect("a cache path");
        assert!(!canonical.exists(), "nothing unusable was published");
        assert!(
            !staging_path(&canonical).exists(),
            "and no scratch was kept"
        );
    }

    #[tokio::test]
    async fn an_address_the_policy_refuses_never_reaches_the_network() {
        let dir = tempfile::tempdir().expect("temp dir");
        let cache = CacheLayout::new(dir.path());
        let error = cache_project_artwork(
            &manager(),
            &cache,
            &DomainAllowlist::modrinth_pack_defaults(),
            &project("LOpKHB2A"),
            "https://evil.example/icon.png",
            &OperationReporter::silent("test-artwork"),
            &CancellationToken::new(),
        )
        .await
        .expect_err("must refuse");
        assert_eq!(error.message_id(), "error-pack-download-url-rejected");
    }
}
