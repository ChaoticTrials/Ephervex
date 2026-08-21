//! Asking a window manager to keep a window on one aspect ratio.
//!
//! Keeping a ratio through a resize belongs to whoever owns the window while
//! the user is dragging it, which is the window manager and not this build.
//! X11 has a property for exactly that: `WM_NORMAL_HINTS` carries a minimum
//! and a maximum aspect, and a manager clamps every movement of an interactive
//! resize to them. Asked this way, any edge or corner behaves the way a corner
//! does, because the manager adjusts the edge the user is not holding.
//!
//! The alternative, resizing the window from here as it is dragged, is this
//! build and the pointer dragging the same window: the correction gives the
//! manager a larger frame to re-derive the next size from, and it climbs away
//! from the user.
//!
//! Nothing here is required for the picker to work. Where the property cannot
//! be reached, on Wayland or on any other platform, the size on record still
//! follows the chosen ratio exactly; only the window stops being held to it.

/// Asks a window manager to hold `window` on `ratio`, or releases it when
/// `ratio` is `None`.
///
/// Best effort by design: a window that has no X11 identity, a display that
/// cannot be reached, and a manager that ignores the property all leave the
/// window resizing freely, which is the same window with one fewer courtesy.
#[cfg(target_os = "linux")]
pub fn hold(window: &slint::Window, ratio: Option<(u32, u32)>) {
    use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
    use x11rb::connection::Connection as _;
    use x11rb::properties::{AspectRatio, WmSizeHints};

    let native = window.window_handle();
    let Ok(handle) = native.window_handle() else {
        return;
    };
    let identity = match handle.as_ref() {
        RawWindowHandle::Xlib(handle) => u32::try_from(handle.window).ok(),
        RawWindowHandle::Xcb(handle) => Some(handle.window.get()),
        _ => None,
    };
    let Some(identity) = identity else {
        return;
    };
    let Ok((connection, _)) = x11rb::connect(None) else {
        return;
    };
    // Read before writing: the minimum size and the rest of the property
    // belong to the toolkit, and this is only adding one field to them.
    let Ok(cookie) = WmSizeHints::get_normal_hints(&connection, identity) else {
        return;
    };
    let Ok(hints) = cookie.reply() else {
        return;
    };
    // A window that has not published hints yet has nothing to preserve.
    let mut hints = hints.unwrap_or_default();
    hints.aspect = ratio.and_then(|(numerator, denominator)| {
        let numerator = i32::try_from(numerator).ok()?;
        let denominator = i32::try_from(denominator).ok()?;
        // The same ratio as both bounds is the ratio itself, exactly.
        let exact = AspectRatio::new(numerator, denominator);
        Some((exact, exact))
    });
    if WmSizeHints::set_normal_hints(&hints, &connection, identity).is_ok() {
        let _ = connection.flush();
    }
}

/// Where no display protocol offers the courtesy, the window resizes freely.
#[cfg(not(target_os = "linux"))]
pub fn hold(_window: &slint::Window, _ratio: Option<(u32, u32)>) {}
