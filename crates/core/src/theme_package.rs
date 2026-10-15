//! Reading files from a user theme package without leaving it.
//!
//! A package is a folder in the themes folder holding `theme.toml` and an
//! optional `branding/` folder of images (`docs/themes.md`, ADR 0047). What a
//! package names is untrusted, so every step below the themes folder is opened
//! relative to the directory opened before it, one component at a time, with
//! `O_NOFOLLOW`. A symbolic link anywhere below the themes folder is refused,
//! and swapping a directory for a link after it was opened changes nothing,
//! because the walk never resolves a path string again.

use std::io::Read as _;
use std::os::fd::OwnedFd;
use std::path::Path;
use std::sync::Arc;

use rustix::fs::{AtFlags, FileType, Mode, OFlags};
use rustix::io::Errno;

use crate::ApplicationTheme;

/// The file in a package folder that holds the theme itself.
pub const THEME_MANIFEST: &str = "theme.toml";
/// The folder in a package that branding references are resolved in.
pub const BRANDING_FOLDER: &str = "branding";

/// Largest branding file read, in bytes.
pub const MAX_BRANDING_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// Largest branding image on either side, in pixels.
pub const MAX_BRANDING_DIMENSION: u32 = 1024;
/// Ceiling the decoder may allocate: 16-bit RGBA at the largest size, twice.
const MAX_DECODE_BYTES: u64 = (MAX_BRANDING_DIMENSION as u64).pow(2) * 8 * 2;
/// Longest branding reference, in bytes.
pub const MAX_REFERENCE_BYTES: usize = 255;
/// Most path components a branding reference may have, the file included.
pub const MAX_REFERENCE_DEPTH: usize = 4;

/// An image format a branding file may be in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrandingFormat {
    Png,
    Jpeg,
    WebP,
}

impl BrandingFormat {
    /// The format a file name's extension declares, ignoring case.
    fn from_file_name(name: &str) -> Option<Self> {
        let (_, extension) = name.rsplit_once('.')?;
        match extension.to_ascii_lowercase().as_str() {
            "png" => Some(Self::Png),
            "jpg" | "jpeg" => Some(Self::Jpeg),
            "webp" => Some(Self::WebP),
            _ => None,
        }
    }

    fn image_format(self) -> image::ImageFormat {
        match self {
            Self::Png => image::ImageFormat::Png,
            Self::Jpeg => image::ImageFormat::Jpeg,
            Self::WebP => image::ImageFormat::WebP,
        }
    }
}

/// A file a theme names under `[branding]`, relative to its `branding/` folder.
///
/// Holding one proves the text is a plain relative path of ordinary names with
/// a supported image extension. It is not proof the file is safe to open; the
/// reader enforces that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrandingReference {
    text: String,
    format: BrandingFormat,
}

/// Why a branding reference was refused before anything was opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BrandingReferenceError {
    #[error("the reference is empty")]
    Empty,
    #[error("the reference is longer than {MAX_REFERENCE_BYTES} bytes")]
    TooLong,
    #[error("the reference contains a control character")]
    ControlCharacter,
    #[error("the reference contains a backslash")]
    Backslash,
    #[error("the reference names a drive, a URL or a URI scheme")]
    Scheme,
    #[error("the reference is an absolute path")]
    Absolute,
    #[error("the reference has an empty path component")]
    EmptyComponent,
    #[error("the reference uses \".\" or \"..\"")]
    Traversal,
    #[error("the reference is nested more than {MAX_REFERENCE_DEPTH} components deep")]
    TooDeep,
    #[error("the reference does not name a .png, .jpg, .jpeg or .webp file")]
    UnsupportedFormat,
}

impl BrandingReference {
    /// Validates what a theme wrote, as text only.
    ///
    /// # Errors
    ///
    /// Anything other than a relative path of non-empty names, without `.`,
    /// `..`, a backslash, a colon or a control character, ending in a
    /// supported image extension.
    pub fn parse(text: &str) -> Result<Self, BrandingReferenceError> {
        if text.is_empty() {
            return Err(BrandingReferenceError::Empty);
        }
        if text.len() > MAX_REFERENCE_BYTES {
            return Err(BrandingReferenceError::TooLong);
        }
        if text.chars().any(char::is_control) {
            return Err(BrandingReferenceError::ControlCharacter);
        }
        // Rules out Windows separators and UNC paths.
        if text.contains('\\') {
            return Err(BrandingReferenceError::Backslash);
        }
        // Rules out drive letters, URLs and every other URI scheme.
        if text.contains(':') {
            return Err(BrandingReferenceError::Scheme);
        }
        if text.starts_with('/') {
            return Err(BrandingReferenceError::Absolute);
        }
        let components: Vec<&str> = text.split('/').collect();
        if components.iter().any(|component| component.is_empty()) {
            return Err(BrandingReferenceError::EmptyComponent);
        }
        if components
            .iter()
            .any(|component| matches!(*component, "." | ".."))
        {
            return Err(BrandingReferenceError::Traversal);
        }
        if components.len() > MAX_REFERENCE_DEPTH {
            return Err(BrandingReferenceError::TooDeep);
        }
        let format = components
            .last()
            .and_then(|name| BrandingFormat::from_file_name(name))
            .ok_or(BrandingReferenceError::UnsupportedFormat)?;
        Ok(Self {
            text: text.to_owned(),
            format,
        })
    }

    /// The reference as the theme wrote it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// The format the file name declares.
    #[must_use]
    pub fn format(&self) -> BrandingFormat {
        self.format
    }

    /// The folders to walk and the file to open at the end.
    fn split(&self) -> (Vec<&str>, &str) {
        let mut components: Vec<&str> = self.text.split('/').collect();
        let file = components.pop().unwrap_or_default();
        (components, file)
    }
}

/// What a branding image is for. An icon is centered on a square canvas so a
/// window manager cannot stretch it; a logo is drawn as it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrandingRole {
    Logo,
    Icon,
}

/// A decoded branding image, as straight (not premultiplied) RGBA8 pixels.
#[derive(Clone, PartialEq, Eq)]
pub struct BrandingImage {
    width: u32,
    height: u32,
    rgba: Arc<[u8]>,
}

impl BrandingImage {
    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Row-major pixels, four bytes each.
    #[must_use]
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }
}

impl std::fmt::Debug for BrandingImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrandingImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

/// Why a file in a theme package could not be used.
#[derive(Debug, thiserror::Error)]
pub enum ThemePackageError {
    #[error("the folder name is not a usable theme id")]
    InvalidPackageName,
    #[error("the file does not exist")]
    Missing,
    #[error("a symbolic link is in the way, and links are not followed")]
    SymbolicLink,
    #[error("the path does not lead to a regular file")]
    NotAFile,
    #[error("the file is larger than {limit} bytes")]
    TooLarge { limit: u64 },
    #[error("the file could not be read: {0}")]
    Unreadable(#[source] std::io::Error),
    #[error("the file's contents are not the image format its name says")]
    FormatMismatch,
    #[error(
        "the image is {width}x{height} pixels, more than {MAX_BRANDING_DIMENSION}x{MAX_BRANDING_DIMENSION}"
    )]
    TooManyPixels { width: u32, height: u32 },
    #[error("decoding the image needs more memory than branding may use")]
    DecodeLimit,
    #[error("the image could not be decoded: {0}")]
    Malformed(String),
}

/// Reads `<themes_folder>/<id>/theme.toml`, at most `limit` bytes of it.
///
/// # Errors
///
/// [`ThemePackageError::Missing`] when the folder holds no manifest, and the
/// other variants when the package folder or manifest is a link, is not what
/// it should be, is too large, or cannot be read.
pub fn read_theme_manifest(
    themes_folder: &Path,
    id: &str,
    limit: u64,
) -> Result<Vec<u8>, ThemePackageError> {
    let package = open_package(themes_folder, id)?;
    read_beneath(&package, &[], THEME_MANIFEST, limit)
}

/// Reads and decodes one branding image from `<themes_folder>/<id>/branding/`.
///
/// # Errors
///
/// Every way the file can be absent, escape the package, or fail to be a
/// bounded image in the format its name declares.
pub fn read_branding_image(
    themes_folder: &Path,
    id: &str,
    reference: &BrandingReference,
    role: BrandingRole,
) -> Result<BrandingImage, ThemePackageError> {
    let package = open_package(themes_folder, id)?;
    let (folders, file) = reference.split();
    let mut path = vec![BRANDING_FOLDER];
    path.extend(folders);
    let bytes = read_beneath(&package, &path, file, MAX_BRANDING_FILE_BYTES)?;
    decode(&bytes, reference.format, role)
}

/// Opens a package folder. The themes folder itself may be a link, since it is
/// where the user keeps configuration; the package folder may not.
fn open_package(themes_folder: &Path, id: &str) -> Result<OwnedFd, ThemePackageError> {
    if ApplicationTheme::user(id).is_none() {
        return Err(ThemePackageError::InvalidPackageName);
    }
    let root = rustix::fs::open(
        themes_folder,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|errno| match errno {
        Errno::NOENT => ThemePackageError::Missing,
        errno => ThemePackageError::Unreadable(errno.into()),
    })?;
    open_directory_beneath(&root, id)
}

/// Walks `folders` down from `start` and reads `file` at the end of them.
fn read_beneath(
    start: &OwnedFd,
    folders: &[&str],
    file: &str,
    limit: u64,
) -> Result<Vec<u8>, ThemePackageError> {
    let mut opened: Option<OwnedFd> = None;
    for folder in folders {
        let next = open_directory_beneath(opened.as_ref().unwrap_or(start), folder)?;
        opened = Some(next);
    }
    let (file, size) = open_file_beneath(opened.as_ref().unwrap_or(start), file)?;
    if size > limit {
        return Err(ThemePackageError::TooLarge { limit });
    }
    // The size was checked from the open file, but it can still grow.
    let mut bytes = Vec::with_capacity(usize::try_from(size).unwrap_or_default());
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(ThemePackageError::Unreadable)?;
    if bytes.len() as u64 > limit {
        return Err(ThemePackageError::TooLarge { limit });
    }
    Ok(bytes)
}

fn open_directory_beneath(parent: &OwnedFd, name: &str) -> Result<OwnedFd, ThemePackageError> {
    rustix::fs::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|errno| refusal(parent, name, errno))
}

/// Opens a regular file and returns it with its size. `NONBLOCK` keeps a FIFO
/// from stalling the open; it is refused by type straight after.
fn open_file_beneath(
    parent: &OwnedFd,
    name: &str,
) -> Result<(std::fs::File, u64), ThemePackageError> {
    let fd = rustix::fs::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|errno| refusal(parent, name, errno))?;
    let stat =
        rustix::fs::fstat(&fd).map_err(|errno| ThemePackageError::Unreadable(errno.into()))?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
        return Err(ThemePackageError::NotAFile);
    }
    let size = u64::try_from(stat.st_size).unwrap_or_default();
    Ok((std::fs::File::from(fd), size))
}

/// Names a failed open. Only the message depends on this; the open already
/// refused whatever was there.
fn refusal(parent: &OwnedFd, name: &str, errno: Errno) -> ThemePackageError {
    if errno == Errno::NOENT {
        return ThemePackageError::Missing;
    }
    // Linux reports a link refused by `O_DIRECTORY | O_NOFOLLOW` as `ENOTDIR`.
    match rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) if FileType::from_raw_mode(stat.st_mode) == FileType::Symlink => {
            ThemePackageError::SymbolicLink
        }
        Ok(_) if errno == Errno::NOTDIR => ThemePackageError::NotAFile,
        Err(Errno::NOENT) => ThemePackageError::Missing,
        Err(_) if errno == Errno::LOOP => ThemePackageError::SymbolicLink,
        _ => ThemePackageError::Unreadable(errno.into()),
    }
}

/// Decodes an image compiled into the application, such as a built-in theme's
/// branding, under the same bounds as one read from a theme folder.
///
/// # Errors
///
/// When `bytes` is not a bounded image in `format`.
pub fn decode_branding_image(
    bytes: &[u8],
    format: BrandingFormat,
    role: BrandingRole,
) -> Result<BrandingImage, ThemePackageError> {
    decode(bytes, format, role)
}

/// A decoder for `bytes` that can never allocate past [`MAX_DECODE_BYTES`],
/// including while it reads the header.
fn bounded_decoder(
    bytes: &[u8],
    format: image::ImageFormat,
) -> Result<impl image::ImageDecoder + '_, ThemePackageError> {
    let mut limits = image::Limits::no_limits();
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), format);
    reader.limits(limits);
    reader.into_decoder().map_err(decode_error)
}

fn decode_error(error: image::ImageError) -> ThemePackageError {
    match error {
        image::ImageError::Limits(_) => ThemePackageError::DecodeLimit,
        error => ThemePackageError::Malformed(error.to_string()),
    }
}

/// Decodes `bytes` as `format`, within the dimension and allocation bounds.
fn decode(
    bytes: &[u8],
    format: BrandingFormat,
    role: BrandingRole,
) -> Result<BrandingImage, ThemePackageError> {
    use image::ImageDecoder as _;

    let format = format.image_format();
    if image::guess_format(bytes).ok() != Some(format) {
        return Err(ThemePackageError::FormatMismatch);
    }
    let decoder = bounded_decoder(bytes, format)?;
    let (width, height) = decoder.dimensions();
    if width > MAX_BRANDING_DIMENSION || height > MAX_BRANDING_DIMENSION {
        return Err(ThemePackageError::TooManyPixels { width, height });
    }
    let mut pixels = image::DynamicImage::from_decoder(decoder)
        .map_err(decode_error)?
        .into_rgba8();
    if pixels.width() == 0 || pixels.height() == 0 {
        return Err(ThemePackageError::Malformed(
            "the image is empty".to_owned(),
        ));
    }
    if role == BrandingRole::Icon {
        pixels = centered_on_square(pixels);
    }
    Ok(BrandingImage {
        width: pixels.width(),
        height: pixels.height(),
        rgba: pixels.into_raw().into(),
    })
}

/// Pads `image` with transparency to a square, keeping it centered.
fn centered_on_square(image: image::RgbaImage) -> image::RgbaImage {
    let (width, height) = image.dimensions();
    if width == height {
        return image;
    }
    let side = width.max(height);
    let mut canvas = image::RgbaImage::new(side, side);
    image::imageops::replace(
        &mut canvas,
        &image,
        i64::from((side - width) / 2),
        i64::from((side - height) / 2),
    );
    canvas
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

    use std::path::PathBuf;

    use super::*;

    fn encoded(width: u32, height: u32, format: image::ImageFormat) -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(width, height, image::Rgba([10, 20, 30, 255]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(image)
            .to_rgb8()
            .write_to(&mut std::io::Cursor::new(&mut bytes), format)
            .expect("encode");
        bytes
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(width, height, image::Rgba([10, 20, 30, 255]));
        let mut bytes = Vec::new();
        image
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .expect("encode");
        bytes
    }

    /// The PNG chunk checksum (ISO 3309 CRC-32).
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = u32::MAX;
        for byte in bytes {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    /// A 1x1 PNG whose color profile inflates to `profile` bytes.
    fn png_with_profile(profile: usize) -> Vec<u8> {
        use image::ImageEncoder as _;

        let mut bytes = Vec::new();
        let mut encoder = image::codecs::png::PngEncoder::new(&mut bytes);
        encoder.set_icc_profile(vec![0; profile]).expect("profile");
        encoder
            .write_image(&[1, 2, 3, 255], 1, 1, image::ExtendedColorType::Rgba8)
            .expect("encode");
        bytes
    }

    /// A themes folder with one package, `mine`, and its `branding/` folder.
    fn package() -> (tempfile::TempDir, PathBuf) {
        let folder = tempfile::tempdir().expect("temp dir");
        let branding = folder.path().join("mine").join(BRANDING_FOLDER);
        std::fs::create_dir_all(&branding).expect("branding folder");
        std::fs::write(
            folder.path().join("mine").join(THEME_MANIFEST),
            "name = \"Mine\"",
        )
        .expect("manifest");
        (folder, branding)
    }

    fn reference(text: &str) -> BrandingReference {
        BrandingReference::parse(text).expect("valid reference")
    }

    fn read(folder: &Path, text: &str) -> Result<BrandingImage, ThemePackageError> {
        read_branding_image(folder, "mine", &reference(text), BrandingRole::Logo)
    }

    #[test]
    fn a_reference_is_a_relative_path_to_a_supported_image() {
        for valid in [
            "logo.png",
            "Logo.PNG",
            "icons/app.webp",
            "a/b/c/d.jpeg",
            "x.jpg",
            "~.png",
        ] {
            assert!(BrandingReference::parse(valid).is_ok(), "{valid}");
        }
        assert_eq!(reference("icons/app.webp").format(), BrandingFormat::WebP);
        assert_eq!(reference("x.JPG").format(), BrandingFormat::Jpeg);
    }

    #[test]
    fn a_reference_that_could_leave_the_branding_folder_is_refused() {
        use BrandingReferenceError as E;
        let long = format!("{}.png", "a".repeat(MAX_REFERENCE_BYTES));
        for (text, expected) in [
            ("", E::Empty),
            (long.as_str(), E::TooLong),
            ("logo\0.png", E::ControlCharacter),
            ("logo\n.png", E::ControlCharacter),
            ("..\\other\\logo.png", E::Backslash),
            ("\\\\server\\share\\logo.png", E::Backslash),
            ("C:/logo.png", E::Scheme),
            ("https://example.com/logo.png", E::Scheme),
            ("file:///etc/logo.png", E::Scheme),
            ("data:image/png;base64,AAAA.png", E::Scheme),
            ("/etc/logo.png", E::Absolute),
            ("//server/share/logo.png", E::Absolute),
            ("icons//logo.png", E::EmptyComponent),
            ("icons/", E::EmptyComponent),
            ("../logo.png", E::Traversal),
            ("../other/branding/logo.png", E::Traversal),
            ("icons/../../logo.png", E::Traversal),
            ("./logo.png", E::Traversal),
            ("..", E::Traversal),
            ("a/b/c/d/e.png", E::TooDeep),
            ("logo.svg", E::UnsupportedFormat),
            ("logo.gif", E::UnsupportedFormat),
            ("logo", E::UnsupportedFormat),
            ("run.sh", E::UnsupportedFormat),
        ] {
            assert_eq!(BrandingReference::parse(text), Err(expected), "{text:?}");
        }
    }

    #[test]
    fn a_valid_image_in_the_branding_folder_is_decoded() {
        let (folder, branding) = package();
        std::fs::write(branding.join("logo.png"), png(40, 20)).expect("write");
        std::fs::create_dir(branding.join("icons")).expect("subfolder");
        std::fs::write(
            branding.join("icons").join("app.jpg"),
            encoded(8, 8, image::ImageFormat::Jpeg),
        )
        .expect("write");
        std::fs::write(
            branding.join("app.webp"),
            encoded(6, 6, image::ImageFormat::WebP),
        )
        .expect("write");

        let logo = read(folder.path(), "logo.png").expect("logo");
        assert_eq!((logo.width(), logo.height()), (40, 20));
        assert_eq!(logo.rgba().len(), 40 * 20 * 4);
        assert_eq!(&logo.rgba()[..4], &[10, 20, 30, 255]);
        let jpeg = read(folder.path(), "icons/app.jpg").expect("jpeg");
        assert_eq!((jpeg.width(), jpeg.height()), (8, 8));
        let webp = read(folder.path(), "app.webp").expect("webp");
        assert_eq!((webp.width(), webp.height()), (6, 6));
    }

    #[test]
    fn an_icon_is_centered_on_a_square_and_a_logo_keeps_its_shape() {
        let (folder, branding) = package();
        std::fs::write(branding.join("wide.png"), png(40, 20)).expect("write");
        let icon = read_branding_image(
            folder.path(),
            "mine",
            &reference("wide.png"),
            BrandingRole::Icon,
        )
        .expect("icon");
        assert_eq!((icon.width(), icon.height()), (40, 40));
        let pixel = |x: usize, y: usize| &icon.rgba()[(y * 40 + x) * 4..(y * 40 + x) * 4 + 4];
        assert_eq!(pixel(0, 0), [0, 0, 0, 0], "padding is transparent");
        assert_eq!(
            pixel(20, 20),
            [10, 20, 30, 255],
            "the image sits in the middle"
        );
        assert_eq!(pixel(20, 9), [0, 0, 0, 0]);
        assert_eq!(pixel(20, 10), [10, 20, 30, 255]);
        let logo = read(folder.path(), "wide.png").expect("logo");
        assert_eq!((logo.width(), logo.height()), (40, 20));
    }

    #[test]
    fn missing_malformed_and_mislabeled_files_are_refused() {
        let (folder, branding) = package();
        assert!(matches!(
            read(folder.path(), "absent.png"),
            Err(ThemePackageError::Missing)
        ));
        assert!(matches!(
            read(folder.path(), "nested/absent.png"),
            Err(ThemePackageError::Missing)
        ));
        std::fs::write(branding.join("text.png"), "not an image").expect("write");
        assert!(matches!(
            read(folder.path(), "text.png"),
            Err(ThemePackageError::FormatMismatch)
        ));
        std::fs::write(
            branding.join("photo.png"),
            encoded(4, 4, image::ImageFormat::Jpeg),
        )
        .expect("write");
        assert!(matches!(
            read(folder.path(), "photo.png"),
            Err(ThemePackageError::FormatMismatch)
        ));
        let mut truncated = png(64, 64);
        truncated.truncate(truncated.len() / 2);
        std::fs::write(branding.join("truncated.png"), truncated).expect("write");
        assert!(matches!(
            read(folder.path(), "truncated.png"),
            Err(ThemePackageError::Malformed(_))
        ));
        std::fs::create_dir(branding.join("folder.png")).expect("dir");
        assert!(matches!(
            read(folder.path(), "folder.png"),
            Err(ThemePackageError::NotAFile)
        ));
        assert!(matches!(
            read(folder.path(), "text.png/inner.png"),
            Err(ThemePackageError::NotAFile)
        ));
    }

    #[test]
    fn an_image_too_large_to_be_branding_is_refused_before_it_is_decoded() {
        let (folder, branding) = package();
        std::fs::write(
            branding.join("wide.png"),
            png(MAX_BRANDING_DIMENSION + 1, 1),
        )
        .expect("write");
        assert!(matches!(
            read(folder.path(), "wide.png"),
            Err(ThemePackageError::TooManyPixels { width, height: 1 })
                if width == MAX_BRANDING_DIMENSION + 1
        ));

        // A tiny file whose header claims a canvas of gigabytes.
        let mut bomb = png(1, 1);
        bomb[16..20].copy_from_slice(&30_000u32.to_be_bytes());
        bomb[20..24].copy_from_slice(&30_000u32.to_be_bytes());
        let crc = crc32(&bomb[12..29]);
        bomb[29..33].copy_from_slice(&crc.to_be_bytes());
        std::fs::write(branding.join("bomb.png"), bomb).expect("write");
        assert!(matches!(
            read(folder.path(), "bomb.png"),
            Err(ThemePackageError::TooManyPixels {
                width: 30_000,
                height: 30_000
            })
        ));

        let large = std::fs::File::create(branding.join("large.png")).expect("create");
        large.set_len(MAX_BRANDING_FILE_BYTES + 1).expect("grow");
        assert!(matches!(
            read(folder.path(), "large.png"),
            Err(ThemePackageError::TooLarge { .. })
        ));
    }

    /// A compressed chunk that inflates past the decode ceiling is never
    /// inflated in full, not even while only the header is read.
    #[test]
    fn a_profile_that_inflates_past_the_ceiling_is_not_inflated() {
        use image::ImageDecoder as _;

        let large = usize::try_from(MAX_DECODE_BYTES).unwrap() * 2;
        let bytes = png_with_profile(large);
        assert!(
            (bytes.len() as u64) < MAX_BRANDING_FILE_BYTES,
            "a small file"
        );
        // Unbounded, the profile really is that large.
        let mut unbounded =
            image::ImageReader::with_format(std::io::Cursor::new(&bytes), image::ImageFormat::Png)
                .into_decoder()
                .expect("decoder");
        assert_eq!(
            unbounded.icc_profile().expect("profile").map(|p| p.len()),
            Some(large)
        );

        match bounded_decoder(&bytes, image::ImageFormat::Png) {
            Ok(mut decoder) => assert!(
                !matches!(decoder.icc_profile(), Ok(Some(profile)) if profile.len() as u64 > MAX_DECODE_BYTES),
                "the profile was inflated past the ceiling"
            ),
            Err(error) => assert!(matches!(error, ThemePackageError::DecodeLimit), "{error}"),
        }
        let (folder, branding) = package();
        std::fs::write(branding.join("profile.png"), &bytes).expect("write");
        assert!(matches!(
            read(folder.path(), "profile.png"),
            Ok(_) | Err(ThemePackageError::DecodeLimit)
        ));
    }

    #[test]
    fn a_compiled_in_image_is_decoded_under_the_same_bounds() {
        let logo = include_bytes!("../../../assets/logo.png");
        let image = decode_branding_image(logo, BrandingFormat::Png, BrandingRole::Logo)
            .expect("the shipped logo is valid branding");
        assert_eq!((image.width(), image.height()), (620, 620));
        assert!(matches!(
            decode_branding_image(logo, BrandingFormat::Jpeg, BrandingRole::Logo),
            Err(ThemePackageError::FormatMismatch)
        ));
    }

    #[test]
    fn a_symbolic_link_anywhere_below_the_themes_folder_is_refused() {
        let outside = tempfile::tempdir().expect("outside");
        std::fs::write(outside.path().join("logo.png"), png(2, 2)).expect("write");
        std::fs::create_dir(outside.path().join(BRANDING_FOLDER)).expect("dir");
        std::fs::write(
            outside.path().join(BRANDING_FOLDER).join("logo.png"),
            png(2, 2),
        )
        .expect("write");
        std::fs::write(outside.path().join(THEME_MANIFEST), "").expect("write");

        // The file itself.
        let (folder, branding) = package();
        std::os::unix::fs::symlink(outside.path().join("logo.png"), branding.join("logo.png"))
            .expect("link");
        assert!(matches!(
            read(folder.path(), "logo.png"),
            Err(ThemePackageError::SymbolicLink)
        ));

        // A folder between `branding/` and the file.
        std::os::unix::fs::symlink(outside.path(), branding.join("icons")).expect("link");
        assert!(matches!(
            read(folder.path(), "icons/logo.png"),
            Err(ThemePackageError::SymbolicLink)
        ));

        // `branding/` itself.
        std::fs::remove_dir_all(&branding).expect("remove");
        std::os::unix::fs::symlink(outside.path(), &branding).expect("link");
        assert!(matches!(
            read(folder.path(), "logo.png"),
            Err(ThemePackageError::SymbolicLink)
        ));

        // The package folder.
        std::os::unix::fs::symlink(outside.path(), folder.path().join("linked")).expect("link");
        assert!(matches!(
            read_branding_image(
                folder.path(),
                "linked",
                &reference("logo.png"),
                BrandingRole::Logo
            ),
            Err(ThemePackageError::SymbolicLink)
        ));
        assert!(matches!(
            read_theme_manifest(folder.path(), "linked", 1024),
            Err(ThemePackageError::SymbolicLink)
        ));

        // A link to a file elsewhere in the same package is refused as well.
        let (folder, branding) = package();
        std::os::unix::fs::symlink("../theme.toml", branding.join("manifest.png")).expect("link");
        assert!(matches!(
            read(folder.path(), "manifest.png"),
            Err(ThemePackageError::SymbolicLink)
        ));
    }

    #[test]
    fn a_fifo_is_refused_without_waiting_for_a_writer() {
        let (folder, branding) = package();
        rustix::fs::mknodat(
            rustix::fs::CWD,
            branding.join("pipe.png"),
            FileType::Fifo,
            Mode::RUSR | Mode::WUSR,
            0,
        )
        .expect("fifo");
        assert!(matches!(
            read(folder.path(), "pipe.png"),
            Err(ThemePackageError::NotAFile)
        ));
    }

    #[test]
    fn a_package_name_that_is_not_a_theme_id_is_refused() {
        let (folder, _) = package();
        for id in ["..", ".", "", "Mine", "a/b", "../mine"] {
            assert!(
                matches!(
                    read_theme_manifest(folder.path(), id, 1024),
                    Err(ThemePackageError::InvalidPackageName)
                ),
                "{id:?}"
            );
        }
    }

    #[test]
    fn the_manifest_is_read_within_its_limit() {
        let (folder, _) = package();
        assert_eq!(
            read_theme_manifest(folder.path(), "mine", 1024).expect("manifest"),
            b"name = \"Mine\""
        );
        assert!(matches!(
            read_theme_manifest(folder.path(), "mine", 4),
            Err(ThemePackageError::TooLarge { limit: 4 })
        ));
        assert!(matches!(
            read_theme_manifest(folder.path(), "absent", 1024),
            Err(ThemePackageError::Missing)
        ));
    }

    /// What was opened is what is read: replacing an opened folder with a link
    /// to somewhere else does not redirect the rest of the walk.
    #[test]
    fn a_folder_replaced_after_it_was_opened_does_not_redirect_the_read() {
        let outside = tempfile::tempdir().expect("outside");
        std::fs::write(outside.path().join("logo.png"), b"outside").expect("write");
        let (folder, branding) = package();
        std::fs::write(branding.join("logo.png"), b"inside").expect("write");

        let package = open_package(folder.path(), "mine").expect("package");
        let moved = folder.path().join("moved");
        std::fs::rename(folder.path().join("mine"), &moved).expect("move");
        std::fs::create_dir(folder.path().join("mine")).expect("replacement");
        std::os::unix::fs::symlink(
            outside.path(),
            folder.path().join("mine").join(BRANDING_FOLDER),
        )
        .expect("link");

        let bytes = read_beneath(&package, &[BRANDING_FOLDER], "logo.png", 1024).expect("read");
        assert_eq!(bytes, b"inside");
        // A fresh walk meets the link and stops.
        assert!(matches!(
            read(folder.path(), "logo.png"),
            Err(ThemePackageError::SymbolicLink)
        ));
    }

    /// A folder flipping between a real directory and a link to outside the
    /// package while it is read never yields the outside file.
    #[test]
    fn a_racing_link_swap_never_reaches_outside_the_package() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let outside = tempfile::tempdir().expect("outside");
        std::fs::write(outside.path().join("logo.png"), b"outside").expect("write");
        let (folder, branding) = package();
        std::fs::write(branding.join("logo.png"), b"inside").expect("write");
        let parked = folder.path().join("mine").join("parked");

        let stop = Arc::new(AtomicBool::new(false));
        let swapper = {
            let stop = Arc::clone(&stop);
            let branding = branding.clone();
            let outside = outside.path().to_owned();
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    std::fs::rename(&branding, &parked).expect("park");
                    std::os::unix::fs::symlink(&outside, &branding).expect("link");
                    std::fs::remove_file(&branding).expect("unlink");
                    std::fs::rename(&parked, &branding).expect("restore");
                }
            })
        };
        let mut inside = 0;
        for _ in 0..2000 {
            let package = open_package(folder.path(), "mine").expect("package");
            // Which refusal a race produces depends on where it lands.
            if let Ok(bytes) = read_beneath(&package, &[BRANDING_FOLDER], "logo.png", 1024) {
                assert_eq!(bytes, b"inside", "read through a swapped link");
                inside += 1;
            }
        }
        stop.store(true, Ordering::Relaxed);
        swapper.join().expect("swapper");
        assert!(inside > 0, "the real folder was never read");
    }
}
