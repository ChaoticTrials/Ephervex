//! Showing the chosen theme's logo and window icon.
//!
//! The markup falls back to the embedded default whenever an image here is
//! empty, so a built-in theme or a theme without branding never touches the
//! filesystem.
//!
//! The window icon is also handed to the windowing layer directly. The toolkit
//! re-sends its `icon` property only when the image's cache key changes, and
//! every image decoded at run time has the same empty key, so switching from
//! one themed icon to another would otherwise leave the first in place.

use ephervex_core::theme_package::BrandingImage;
use slint::winit_030::WinitWindowAccessor as _;
use slint::winit_030::winit;
use slint::{ComponentHandle, Image, Rgba8Pixel, SharedPixelBuffer};

use crate::theme_files::BrandingImages;
use crate::ui::AppWindow;

/// Shows `images`, touching only what changed.
pub(crate) fn apply_branding(ui: &AppWindow, images: &BrandingImages) {
    if !shows(&ui.get_theme_logo(), images.logo.as_ref()) {
        ui.set_theme_logo(to_image(images.logo.as_ref()));
    }
    if !shows(&ui.get_theme_icon(), images.icon.as_ref()) {
        ui.set_theme_icon(to_image(images.icon.as_ref()));
        // Returning to the default changes the cache key, so the toolkit
        // restores that one itself.
        if let Some(icon) = &images.icon {
            set_window_icon(ui, icon);
        }
    }
}

/// Whether `current` already holds `wanted`. Images compare by buffer
/// identity, so the pixels are compared instead.
fn shows(current: &Image, wanted: Option<&BrandingImage>) -> bool {
    match wanted {
        None => current.size().width == 0,
        Some(wanted) => current.to_rgba8().is_some_and(|buffer| {
            buffer.width() == wanted.width()
                && buffer.height() == wanted.height()
                && buffer.as_bytes() == wanted.rgba()
        }),
    }
}

fn to_image(image: Option<&BrandingImage>) -> Image {
    image.map_or_else(Image::default, |image| {
        Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
            image.rgba(),
            image.width(),
            image.height(),
        ))
    })
}

/// Hands `icon` to the native window, if it exists yet. A window created later
/// takes the icon from the markup.
fn set_window_icon(ui: &AppWindow, icon: &BrandingImage) {
    let Ok(icon) =
        winit::window::Icon::from_rgba(icon.rgba().to_vec(), icon.width(), icon.height())
    else {
        return;
    };
    ui.window()
        .with_winit_window(|window| window.set_window_icon(Some(icon)));
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use ephervex_core::test_support::solid_png;
    use ephervex_core::theme_package::{BrandingReference, BrandingRole, read_branding_image};

    use super::*;

    fn decoded(rgba: [u8; 4]) -> BrandingImage {
        let folder = tempfile::tempdir().expect("temp dir");
        let branding = folder.path().join("t").join("branding");
        std::fs::create_dir_all(&branding).expect("folder");
        std::fs::write(branding.join("a.png"), solid_png(2, 1, rgba)).expect("write");
        read_branding_image(
            folder.path(),
            "t",
            &BrandingReference::parse("a.png").expect("reference"),
            BrandingRole::Logo,
        )
        .expect("decodes")
    }

    #[test]
    fn a_shown_image_is_recognized_by_its_pixels() {
        let image = decoded([1, 2, 3, 255]);
        let other = decoded([1, 2, 4, 255]);
        let shown = to_image(Some(&image));
        assert!(shows(&shown, Some(&image)));
        assert!(
            shows(&to_image(Some(&image)), Some(&image)),
            "a fresh copy is the same"
        );
        assert!(!shows(&shown, Some(&other)));
        assert!(!shows(&shown, None));
        assert!(shows(&Image::default(), None));
        assert!(!shows(&Image::default(), Some(&image)));
    }

    #[test]
    fn every_logo_keeps_its_aspect_ratio() {
        let markup = crate::test_support::ui_markup();
        let drawn: Vec<&str> = markup
            .lines()
            .filter(|line| line.contains("source: root.logo"))
            .collect();
        assert!(!drawn.is_empty());
        for line in drawn {
            assert!(line.contains("image-fit: contain"), "{line}");
        }
    }
}
