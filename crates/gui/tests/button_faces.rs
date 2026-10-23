//! A button is a bordered face with a hard shadow, filled by its tone, and a
//! button that cannot be pressed is flat and faded.

#![allow(clippy::expect_used, clippy::panic)]

use i_slint_backend_testing::{TestingBackend, TestingBackendOptions};
use slint::{ComponentHandle, Rgba8Pixel, SharedPixelBuffer};

slint::slint! {
    import { AppButton } from "../ui/components/controls.slint";
    import { Theme } from "../ui/theme.slint";

    export component ButtonProbe inherits Window {
        out property <brush> primary-fill: Theme.primary-fill;
        out property <brush> danger-fill: Theme.danger-fill;
        out property <brush> success-fill: Theme.state-running;
        out property <brush> info-fill: Theme.state-stopped;
        out property <brush> surface: Theme.surface;
        out property <brush> outline: Theme.border-strong;
        out property <brush> shadow: Theme.shadow-hard;
        out property <brush> floor: Theme.background;
        out property <int> height-px: Theme.button-height / 1px;
        out property <int> small-height-px: Theme.button-height-small / 1px;
        background: Theme.background;
        HorizontalLayout {
            padding: 20px;
            spacing: 20px;
            alignment: start;
            AppButton { text: "Primary"; tone: "primary"; width: 120px; }
            AppButton { text: "Secondary"; width: 120px; }
            AppButton { text: "Danger"; tone: "danger"; width: 120px; }
            AppButton { text: "Success"; tone: "success"; width: 120px; }
            AppButton { text: "Info"; tone: "info"; width: 120px; }
            AppButton { text: "Disabled"; tone: "primary"; width: 120px; enabled: false; hint: "Not available."; }
            AppButton { text: "Compact"; compact: true; width: 120px; }
            AppButton { text: "Unnamed tone"; tone: "default"; width: 120px; }
        }
    }
}

type Pixel = [u8; 3];

fn window() -> ButtonProbe {
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        renderer_name: Some("software".into()),
        mock_time: true,
        threading: false,
    })))
    .expect("testing backend");
    let ui = ButtonProbe::new().expect("window");
    ui.window().set_size(slint::PhysicalSize::new(1280, 120));
    ui
}

fn render(ui: &ButtonProbe) -> SharedPixelBuffer<Rgba8Pixel> {
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render")
}

fn pixel(image: &SharedPixelBuffer<Rgba8Pixel>, x: u32, y: u32) -> Pixel {
    let at = image.as_slice()[(y * image.width() + x) as usize];
    [at.r, at.g, at.b]
}

fn rgb(brush: &slint::Brush) -> Pixel {
    let color = brush.color();
    [color.red(), color.green(), color.blue()]
}

#[test]
fn each_tone_fills_the_face_and_every_button_has_an_outline_and_a_hard_shadow() {
    let ui = window();
    let image = render(&ui);
    let height = u32::try_from(ui.get_height_px()).expect("a positive height");
    let fills = [
        rgb(&ui.get_primary_fill()),
        rgb(&ui.get_surface()),
        rgb(&ui.get_danger_fill()),
        rgb(&ui.get_success_fill()),
        rgb(&ui.get_info_fill()),
    ];
    let (outline, shadow) = (rgb(&ui.get_outline()), rgb(&ui.get_shadow()));
    // Buttons are 120px wide at 20px spacing, starting 20px in from the corner.
    let top = 20;
    for (index, fill) in fills.into_iter().enumerate() {
        let left = 20 + u32::try_from(index).expect("a small index") * 140;
        assert_eq!(
            pixel(&image, left + 8, top + height / 2),
            fill,
            "tone {index}"
        );
        assert_eq!(
            pixel(&image, left, top + height / 2),
            outline,
            "outline {index}"
        );
        // The shadow of a resting button falls 4px, so three rows under the
        // face are shadow and the fourth is page.
        assert_eq!(
            pixel(&image, left + 60, top + height + 3),
            shadow,
            "shadow {index}"
        );
        assert_ne!(
            pixel(&image, left + 60, top + height + 4),
            shadow,
            "shadow reach {index}"
        );
    }
}

#[test]
fn a_compact_button_is_shorter_and_its_shadow_falls_less_far() {
    let ui = window();
    let image = render(&ui);
    let compact_height = u32::try_from(ui.get_small_height_px()).expect("a positive height");
    let left = 20 + 6 * 140;
    let top = 20;
    let shadow = rgb(&ui.get_shadow());
    assert_eq!(pixel(&image, left + 60, top + compact_height + 2), shadow);
    assert_ne!(pixel(&image, left + 60, top + compact_height + 3), shadow);
}

#[test]
fn a_disabled_button_is_flat_and_faded() {
    let ui = window();
    let image = render(&ui);
    let height = u32::try_from(ui.get_height_px()).expect("a positive height");
    let top = 20;
    let left = 20 + 5 * 140;
    assert_eq!(
        pixel(&image, left + 60, top + height + 1),
        rgb(&ui.get_floor()),
        "a disabled button casts no shadow"
    );
    assert_ne!(
        pixel(&image, left + 8, top + height / 2),
        rgb(&ui.get_primary_fill()),
        "the fill is faded"
    );
}

#[test]
fn a_tone_that_is_not_named_looks_like_the_secondary_button() {
    let ui = window();
    let image = render(&ui);
    let height = u32::try_from(ui.get_height_px()).expect("a positive height");
    let (top, left) = (20, 20 + 7 * 140);
    assert_eq!(
        pixel(&image, left + 8, top + height / 2),
        rgb(&ui.get_surface())
    );
}
