//! The host picker marks the current host with a ring in the accent color
//! outside the card's ink outline, so the selection reads against any floor.

#![allow(clippy::expect_used, clippy::panic)]

use i_slint_backend_testing::{TestingBackend, TestingBackendOptions};
use slint::{ComponentHandle, Rgba8Pixel, SharedPixelBuffer};

slint::slint! {
    import { HostPickerCard } from "../ui/workspace/host-workspace.slint";
    import { Theme } from "../ui/theme.slint";

    export component PickerProbe inherits Window {
        out property <brush> ring: Theme.accent-primary;
        out property <brush> outline: Theme.border-strong;
        out property <brush> floor: Theme.background;
        out property <int> ring-width: Theme.selection-outline-width / 1px;
        background: Theme.background;
        // Both kinds and ids are empty, so this card is the current one.
        HostPickerCard { x: 40px; y: 40px; width: 200px; height: 60px; }
        HostPickerCard { x: 40px; y: 160px; width: 200px; height: 60px; current-id: "another"; }
    }
}

fn pixel(image: &SharedPixelBuffer<Rgba8Pixel>, x: usize, y: usize) -> [u8; 3] {
    let at = image.as_slice()[y * image.width() as usize + x];
    [at.r, at.g, at.b]
}

fn rgb(brush: &slint::Brush) -> [u8; 3] {
    let color = brush.color();
    [color.red(), color.green(), color.blue()]
}

#[test]
fn the_current_host_has_a_ring_outside_its_outline_and_the_others_do_not() {
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        renderer_name: Some("software".into()),
        mock_time: true,
        threading: false,
    })))
    .expect("testing backend");
    let ui = PickerProbe::new().expect("window");
    ui.window().set_size(slint::PhysicalSize::new(320, 300));
    ui.show().expect("show");
    slint::platform::update_timers_and_animations();
    let image = ui.window().take_snapshot().expect("render");
    let ring_width = usize::try_from(ui.get_ring_width()).expect("a positive width");
    let (ring, outline, floor) = (
        rgb(&ui.get_ring()),
        rgb(&ui.get_outline()),
        rgb(&ui.get_floor()),
    );

    // Above the current card: ring, then the card's own outline.
    assert_eq!(pixel(&image, 140, 40 - 1), ring);
    assert_eq!(pixel(&image, 140, 40 - ring_width), ring);
    assert_eq!(pixel(&image, 140, 40), outline);
    assert_eq!(pixel(&image, 40 - 1, 70), ring);
    // The card that is not current has no ring.
    assert_eq!(pixel(&image, 140, 160 - 1), floor);
    assert_eq!(pixel(&image, 140, 160), outline);
}
