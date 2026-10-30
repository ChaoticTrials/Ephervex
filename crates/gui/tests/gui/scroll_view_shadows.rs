//! A scroll view clips what lies outside its content, and the hard shadow of a
//! button lies outside the button, so content that ends in a button keeps room
//! for it.

#![allow(clippy::panic)]

use i_slint_backend_testing::{TestingBackend, TestingBackendOptions};
use slint::{ComponentHandle, Rgba8Pixel, SharedPixelBuffer};

slint::slint! {
    import { HostRuntimeForm } from "../../ui/workspace/host-settings.slint";
    import { Theme } from "../../ui/theme.slint";

    export component RuntimeFormProbe inherits Window {
        out property <brush> shadow: Theme.shadow-hard;
        out property <brush> primary: Theme.primary-fill;
        out property <brush> floor: Theme.background;
        out property <int> face-outline: Theme.strong-border-width / 1px;
        out property <int> shadow-reach: Theme.shadow-offset-card / 1px;
        background: Theme.background;
        HostRuntimeForm {
            width: parent.width;
            height: parent.height;
        }
    }
}

fn rgb(brush: &slint::Brush) -> [u8; 3] {
    let color = brush.color();
    [color.red(), color.green(), color.blue()]
}

fn pixel(image: &SharedPixelBuffer<Rgba8Pixel>, x: usize, y: usize) -> [u8; 3] {
    let at = image.as_slice()[y * image.width() as usize + x];
    [at.r, at.g, at.b]
}

#[test]
fn the_runtime_form_leaves_room_for_the_shadow_of_its_save_button() {
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        renderer_name: Some("software".into()),
        mock_time: true,
        threading: false,
    })))
    .expect("testing backend");
    let ui = RuntimeFormProbe::new().expect("window");
    ui.window().set_size(slint::PhysicalSize::new(480, 520));
    ui.show().expect("show");
    slint::platform::update_timers_and_animations();
    let image = ui.window().take_snapshot().expect("render");

    // The save button is the only primary-filled face; its fill ends inside the
    // outline, so the face reaches `face-outline` pixels past the last fill pixel.
    let primary = rgb(&ui.get_primary());
    let width = image.width() as usize;
    let (mut right, mut bottom) = (0, 0);
    for y in 0..image.height() as usize {
        for x in 0..width {
            if pixel(&image, x, y) == primary {
                right = right.max(x);
                bottom = bottom.max(y);
            }
        }
    }
    assert!(right > 0, "the form shows a primary save button");
    let outline = usize::try_from(ui.get_face_outline()).expect("a positive width");
    let (face_right, face_bottom) = (right + 1 + outline, bottom + 1 + outline);
    let shadow = rgb(&ui.get_shadow());
    // The last pixel of the shadow, beside the lower half of the face and under
    // its middle. It is visible only if the content leaves the whole shadow's room.
    let last = usize::try_from(ui.get_shadow_reach()).expect("a positive reach") - 1;
    assert_eq!(pixel(&image, face_right + last, face_bottom - 8), shadow);
    assert_eq!(pixel(&image, right - 20, face_bottom + last), shadow);
}
