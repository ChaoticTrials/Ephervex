//! The logo a documented theme folder ships in its `branding/` folder: the
//! Ephervex logo with every hue turned a third of the way around the color
//! wheel, so it reads as a different image.

use std::sync::OnceLock;

use image::{ImageFormat, RgbaImage};
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

static LOGO: OnceLock<Option<RgbaImage>> = OnceLock::new();

/// The theme's logo, or an empty image, which shows the default, if the
/// embedded logo cannot be decoded.
pub(crate) fn theme_logo() -> Image {
    let Some(logo) = LOGO.get_or_init(recolored_logo) else {
        return Image::default();
    };
    Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
        logo.as_raw(),
        logo.width(),
        logo.height(),
    ))
}

fn recolored_logo() -> Option<RgbaImage> {
    let mut logo = image::load_from_memory_with_format(
        include_bytes!("../../../../assets/logo.png"),
        ImageFormat::Png,
    )
    .ok()?
    .into_rgba8();
    // Moving each channel to the next turns every hue by 120 degrees.
    for pixel in logo.pixels_mut() {
        let [red, green, blue, alpha] = pixel.0;
        pixel.0 = [blue, red, green, alpha];
    }
    Some(logo)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_theme_logo_is_decoded_within_the_branding_size_limit() {
        let logo = theme_logo();
        let size = logo.size();
        assert!(size.width > 0 && size.width <= 1024);
        assert!(size.height > 0 && size.height <= 1024);
    }
}
