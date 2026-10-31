//! Reading what a binding editor is waiting for.
//!
//! Slint's `KeyEvent` carries a text, a set of modifier booleans and a repeat
//! flag, and nothing else. A printed character identifies a character rather
//! than a key: the same `1` comes from the number row and from the keypad, and
//! `z` is a different key on QWERTY and on QWERTZ. Minecraft's own bindings are
//! physical keys, so the key is read one layer down, out of the windowing event
//! that still carries it.
//!
//! Mouse buttons are read here for a different reason. The toolkit reports a
//! press to whatever it lands on, so an editor could only ever see the clicks
//! aimed at itself, and "press any key or mouse button" meant "press it on this
//! one small control".
//!
//! Nothing is read, and nothing is taken away from the rest of the window,
//! unless an editor is waiting. On a backend that is not winit this is never
//! registered at all, and a capture falls back to the character the toolkit
//! reported, to the small set of keys a character can identify on its own, and
//! to a click on the editor itself.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use ephervex_core::{MouseButton, PhysicalKey};
use slint::winit_030::{CustomApplicationHandler, EventResult, winit};
use winit::keyboard::KeyCode;

/// One press a binding editor can be waiting for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapturedInput {
    Key(PhysicalKey),
    Mouse(MouseButton),
}

/// Whatever the frontend wants told about a press.
type Listener = Rc<dyn Fn(CapturedInput)>;

/// The seam between the windowing handler and the frontend.
///
/// Two-part for the same reason file drops are: the handler has to exist
/// before the window does, because the backend is chosen first.
#[derive(Clone, Default)]
pub struct BindingCapture {
    /// Whether a binding editor is waiting.
    armed: Rc<Cell<bool>>,
    listener: Rc<RefCell<Option<Listener>>>,
}

impl std::fmt::Debug for BindingCapture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BindingCapture")
            .field("armed", &self.armed.get())
            .field("listening", &self.listener.borrow().is_some())
            .finish()
    }
}

impl BindingCapture {
    /// Sends every press to `listener`, replacing any previous one.
    pub fn listen(&self, listener: impl Fn(CapturedInput) + 'static) {
        *self.listener.borrow_mut() = Some(Rc::new(listener));
    }

    /// Starts reading presses, and taking them away from the window.
    pub fn arm(&self) {
        self.armed.set(true);
    }

    /// Stops reading them again.
    pub fn disarm(&self) {
        self.armed.set(false);
    }

    #[must_use]
    pub fn is_armed(&self) -> bool {
        self.armed.get()
    }

    /// Delivers a press as the windowing handler would, for tests of the
    /// frontend's listener.
    #[cfg(test)]
    pub(crate) fn deliver_press(&self, input: CapturedInput) {
        self.deliver(input);
    }

    /// Delivers one press, with the listener taken out of the cell first so a
    /// listener that arms or disarms does not borrow it twice.
    fn deliver(&self, input: CapturedInput) {
        let listener = self.listener.borrow().clone();
        if let Some(listener) = listener {
            listener(input);
        }
    }
}

/// Reads presses out of the windowing events while a capture is open.
#[derive(Debug)]
pub struct BindingCaptureReader {
    keys: BindingCapture,
    /// The press that arrived since the window last settled, if any.
    pending: Option<CapturedInput>,
}

impl BindingCaptureReader {
    #[must_use]
    pub const fn new(keys: BindingCapture) -> Self {
        Self {
            keys,
            pending: None,
        }
    }

    /// Hands the frontend whatever was pressed since the last settle.
    fn flush(&mut self) {
        if let Some(input) = self.pending.take() {
            self.keys.deliver(input);
        }
    }
}

impl CustomApplicationHandler for BindingCaptureReader {
    fn window_event(
        &mut self,
        _event_loop: &winit::event_loop::ActiveEventLoop,
        _window_id: winit::window::WindowId,
        _winit_window: Option<&winit::window::Window>,
        _slint_window: Option<&slint::Window>,
        event: &winit::event::WindowEvent,
    ) -> EventResult {
        if !self.keys.is_armed() {
            return EventResult::Propagate;
        }
        match event {
            winit::event::WindowEvent::KeyboardInput {
                event,
                is_synthetic: false,
                ..
            } => {
                if event.state.is_pressed()
                    && !event.repeat
                    && let winit::keyboard::PhysicalKey::Code(code) = event.physical_key
                    && let Some(key) = physical_key_of_code(code)
                {
                    self.pending = Some(CapturedInput::Key(key));
                }
            }
            winit::event::WindowEvent::MouseInput { state, button, .. } => {
                if state.is_pressed()
                    && let Some(button) = mouse_button_of_winit(*button)
                {
                    self.pending = Some(CapturedInput::Mouse(button));
                }
            }
            // Everything else, pointer motion included, is left alone: only
            // the events that could bind something are this module's.
            _ => return EventResult::Propagate,
        }
        // Consumed whether or not this build can name the input. A capture is
        // open and the user is pressing something at it, so letting the press
        // through would move focus, type into whatever is behind the editor, or
        // press the control the pointer happens to be over.
        EventResult::PreventDefault
    }

    /// Delivers the press once the windowing system's events are drained, so a
    /// key held down long enough to repeat arrives as one press.
    fn about_to_wait(&mut self, _event_loop: &winit::event_loop::ActiveEventLoop) -> EventResult {
        self.flush();
        EventResult::Propagate
    }
}

/// The button one windowing button stands for, when Minecraft names it.
///
/// The window system numbers the buttons past the first three, and Minecraft
/// labels them one-based, so its `Button4` is the system's `Back`.
#[must_use]
pub fn mouse_button_of_winit(button: winit::event::MouseButton) -> Option<MouseButton> {
    use winit::event::MouseButton as Winit;

    match button {
        Winit::Left => Some(MouseButton::Left),
        Winit::Right => Some(MouseButton::Right),
        Winit::Middle => Some(MouseButton::Middle),
        Winit::Back => Some(MouseButton::Button4),
        Winit::Forward => Some(MouseButton::Button5),
        Winit::Other(index) => MouseButton::from_index(u8::try_from(index).ok()?),
    }
}

/// The key one windowing code stands for, when Minecraft names that key.
///
/// The codes are physical positions rather than characters, which is the same
/// thing Minecraft's own bindings are, so the two models line up entry for
/// entry. Every key the game names has one here; a code with no counterpart
/// is answered with nothing rather than with a near miss, and a capture then
/// stays open for another key.
#[must_use]
// One arm per key. Splitting the table would only put half of it elsewhere.
#[allow(clippy::too_many_lines)]
pub const fn physical_key_of_code(code: KeyCode) -> Option<PhysicalKey> {
    Some(match code {
        KeyCode::Space => PhysicalKey::Space,
        KeyCode::Quote => PhysicalKey::Apostrophe,
        KeyCode::Comma => PhysicalKey::Comma,
        KeyCode::Minus => PhysicalKey::Minus,
        KeyCode::Period => PhysicalKey::Period,
        KeyCode::Slash => PhysicalKey::Slash,
        KeyCode::Digit0 => PhysicalKey::Digit0,
        KeyCode::Digit1 => PhysicalKey::Digit1,
        KeyCode::Digit2 => PhysicalKey::Digit2,
        KeyCode::Digit3 => PhysicalKey::Digit3,
        KeyCode::Digit4 => PhysicalKey::Digit4,
        KeyCode::Digit5 => PhysicalKey::Digit5,
        KeyCode::Digit6 => PhysicalKey::Digit6,
        KeyCode::Digit7 => PhysicalKey::Digit7,
        KeyCode::Digit8 => PhysicalKey::Digit8,
        KeyCode::Digit9 => PhysicalKey::Digit9,
        KeyCode::Semicolon => PhysicalKey::Semicolon,
        KeyCode::Equal => PhysicalKey::Equal,
        KeyCode::KeyA => PhysicalKey::A,
        KeyCode::KeyB => PhysicalKey::B,
        KeyCode::KeyC => PhysicalKey::C,
        KeyCode::KeyD => PhysicalKey::D,
        KeyCode::KeyE => PhysicalKey::E,
        KeyCode::KeyF => PhysicalKey::F,
        KeyCode::KeyG => PhysicalKey::G,
        KeyCode::KeyH => PhysicalKey::H,
        KeyCode::KeyI => PhysicalKey::I,
        KeyCode::KeyJ => PhysicalKey::J,
        KeyCode::KeyK => PhysicalKey::K,
        KeyCode::KeyL => PhysicalKey::L,
        KeyCode::KeyM => PhysicalKey::M,
        KeyCode::KeyN => PhysicalKey::N,
        KeyCode::KeyO => PhysicalKey::O,
        KeyCode::KeyP => PhysicalKey::P,
        KeyCode::KeyQ => PhysicalKey::Q,
        KeyCode::KeyR => PhysicalKey::R,
        KeyCode::KeyS => PhysicalKey::S,
        KeyCode::KeyT => PhysicalKey::T,
        KeyCode::KeyU => PhysicalKey::U,
        KeyCode::KeyV => PhysicalKey::V,
        KeyCode::KeyW => PhysicalKey::W,
        KeyCode::KeyX => PhysicalKey::X,
        KeyCode::KeyY => PhysicalKey::Y,
        KeyCode::KeyZ => PhysicalKey::Z,
        KeyCode::BracketLeft => PhysicalKey::LeftBracket,
        KeyCode::Backslash => PhysicalKey::Backslash,
        KeyCode::BracketRight => PhysicalKey::RightBracket,
        KeyCode::Backquote => PhysicalKey::GraveAccent,
        KeyCode::Escape => PhysicalKey::Escape,
        KeyCode::Enter => PhysicalKey::Enter,
        KeyCode::Tab => PhysicalKey::Tab,
        KeyCode::Backspace => PhysicalKey::Backspace,
        KeyCode::Insert => PhysicalKey::Insert,
        KeyCode::Delete => PhysicalKey::Delete,
        KeyCode::ArrowRight => PhysicalKey::Right,
        KeyCode::ArrowLeft => PhysicalKey::Left,
        KeyCode::ArrowDown => PhysicalKey::Down,
        KeyCode::ArrowUp => PhysicalKey::Up,
        KeyCode::PageUp => PhysicalKey::PageUp,
        KeyCode::PageDown => PhysicalKey::PageDown,
        KeyCode::Home => PhysicalKey::Home,
        KeyCode::End => PhysicalKey::End,
        KeyCode::CapsLock => PhysicalKey::CapsLock,
        KeyCode::ScrollLock => PhysicalKey::ScrollLock,
        KeyCode::NumLock => PhysicalKey::NumLock,
        KeyCode::PrintScreen => PhysicalKey::PrintScreen,
        KeyCode::Pause => PhysicalKey::Pause,
        KeyCode::F1 => PhysicalKey::F1,
        KeyCode::F2 => PhysicalKey::F2,
        KeyCode::F3 => PhysicalKey::F3,
        KeyCode::F4 => PhysicalKey::F4,
        KeyCode::F5 => PhysicalKey::F5,
        KeyCode::F6 => PhysicalKey::F6,
        KeyCode::F7 => PhysicalKey::F7,
        KeyCode::F8 => PhysicalKey::F8,
        KeyCode::F9 => PhysicalKey::F9,
        KeyCode::F10 => PhysicalKey::F10,
        KeyCode::F11 => PhysicalKey::F11,
        KeyCode::F12 => PhysicalKey::F12,
        KeyCode::Numpad0 => PhysicalKey::Keypad0,
        KeyCode::Numpad1 => PhysicalKey::Keypad1,
        KeyCode::Numpad2 => PhysicalKey::Keypad2,
        KeyCode::Numpad3 => PhysicalKey::Keypad3,
        KeyCode::Numpad4 => PhysicalKey::Keypad4,
        KeyCode::Numpad5 => PhysicalKey::Keypad5,
        KeyCode::Numpad6 => PhysicalKey::Keypad6,
        KeyCode::Numpad7 => PhysicalKey::Keypad7,
        KeyCode::Numpad8 => PhysicalKey::Keypad8,
        KeyCode::Numpad9 => PhysicalKey::Keypad9,
        KeyCode::NumpadDecimal => PhysicalKey::KeypadDecimal,
        KeyCode::NumpadDivide => PhysicalKey::KeypadDivide,
        KeyCode::NumpadMultiply => PhysicalKey::KeypadMultiply,
        KeyCode::NumpadSubtract => PhysicalKey::KeypadSubtract,
        KeyCode::NumpadAdd => PhysicalKey::KeypadAdd,
        KeyCode::NumpadEnter => PhysicalKey::KeypadEnter,
        KeyCode::NumpadEqual => PhysicalKey::KeypadEqual,
        KeyCode::ShiftLeft => PhysicalKey::LeftShift,
        KeyCode::ControlLeft => PhysicalKey::LeftControl,
        KeyCode::AltLeft => PhysicalKey::LeftAlt,
        KeyCode::SuperLeft => PhysicalKey::LeftWin,
        KeyCode::ShiftRight => PhysicalKey::RightShift,
        KeyCode::ControlRight => PhysicalKey::RightControl,
        KeyCode::AltRight => PhysicalKey::RightAlt,
        KeyCode::SuperRight => PhysicalKey::RightWin,
        // Everything the shipped model does not name: the international keys,
        // the function keys past F12, the media and browser keys, and the
        // keypad separators no layout in the model has.
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use std::cell::RefCell;

    use super::*;

    /// Every key the shipped model names has a windowing code that produces
    /// it. A gap is a key the game offers and a capture can never bind.
    #[test]
    fn every_key_minecraft_names_has_a_windowing_counterpart() {
        let named: Vec<PhysicalKey> = CODES
            .iter()
            .filter_map(|code| physical_key_of_code(*code))
            .collect();
        let missing: Vec<PhysicalKey> = PhysicalKey::all()
            .filter(|key| !named.contains(key))
            .collect();
        assert!(
            missing.is_empty(),
            "keys with no windowing code: {missing:?}"
        );
    }

    /// The number row and the keypad print the same character and are
    /// different keys. Reading the key rather than the character is the whole
    /// reason this module exists.
    #[test]
    fn the_keypad_and_the_number_row_are_different_keys() {
        assert_eq!(
            physical_key_of_code(KeyCode::Numpad7),
            Some(PhysicalKey::Keypad7)
        );
        assert_eq!(
            physical_key_of_code(KeyCode::Digit7),
            Some(PhysicalKey::Digit7)
        );
    }

    /// A code the game has no name for is answered with nothing, which leaves
    /// the capture open rather than storing a near miss.
    #[test]
    fn a_key_the_game_does_not_name_identifies_nothing() {
        for code in [KeyCode::F13, KeyCode::IntlYen, KeyCode::MediaPlayPause] {
            assert_eq!(physical_key_of_code(code), None, "{code:?}");
        }
    }

    /// Minecraft labels the buttons past the middle one by number, one-based,
    /// so the window system's `Back` is its fourth button.
    #[test]
    fn every_mouse_button_minecraft_names_has_a_windowing_counterpart() {
        use winit::event::MouseButton as Winit;

        assert_eq!(mouse_button_of_winit(Winit::Left), Some(MouseButton::Left));
        assert_eq!(
            mouse_button_of_winit(Winit::Middle),
            Some(MouseButton::Middle)
        );
        assert_eq!(
            mouse_button_of_winit(Winit::Back),
            Some(MouseButton::Button4)
        );
        assert_eq!(
            mouse_button_of_winit(Winit::Forward),
            Some(MouseButton::Button5)
        );
        assert_eq!(
            mouse_button_of_winit(Winit::Other(5)),
            Some(MouseButton::Button6),
            "a button the system reports by number is still one the game names"
        );
        assert_eq!(
            mouse_button_of_winit(Winit::Other(64)),
            None,
            "and one past what the game names is refused rather than guessed"
        );
    }

    #[test]
    fn a_mouse_press_is_delivered_the_same_way_a_key_press_is() {
        let (keys, seen) = recording();
        keys.arm();
        let mut reader = BindingCaptureReader::new(keys);
        reader.pending = Some(CapturedInput::Mouse(MouseButton::Button4));

        reader.flush();

        assert_eq!(
            seen.borrow().as_slice(),
            [CapturedInput::Mouse(MouseButton::Button4)]
        );
    }

    #[test]
    fn a_press_is_delivered_once_the_window_settles() {
        let (keys, seen) = recording();
        keys.arm();
        let mut reader = BindingCaptureReader::new(keys);
        reader.pending = Some(CapturedInput::Key(PhysicalKey::W));

        assert!(seen.borrow().is_empty(), "not from inside the event itself");
        reader.flush();

        assert_eq!(
            seen.borrow().as_slice(),
            [CapturedInput::Key(PhysicalKey::W)]
        );
    }

    #[test]
    fn a_key_held_down_arrives_as_one_press() {
        let (keys, seen) = recording();
        keys.arm();
        let mut reader = BindingCaptureReader::new(keys);
        reader.pending = Some(CapturedInput::Key(PhysicalKey::W));
        reader.pending = Some(CapturedInput::Key(PhysicalKey::W));

        reader.flush();
        reader.flush();

        assert_eq!(
            seen.borrow().as_slice(),
            [CapturedInput::Key(PhysicalKey::W)]
        );
    }

    #[test]
    fn nothing_is_read_while_no_capture_is_open() {
        let (keys, _) = recording();
        assert!(
            !keys.is_armed(),
            "a window with no editor open reads nothing"
        );
        keys.arm();
        assert!(keys.is_armed());
        keys.disarm();
        assert!(!keys.is_armed());
    }

    /// Everything a listener was told, in order.
    fn recording() -> (BindingCapture, Rc<RefCell<Vec<CapturedInput>>>) {
        let keys = BindingCapture::default();
        let seen = Rc::new(RefCell::new(Vec::new()));
        let record = Rc::clone(&seen);
        keys.listen(move |key| record.borrow_mut().push(key));
        (keys, seen)
    }

    /// Every windowing code this build maps, so the coverage check reads the
    /// same list the mapping does.
    const CODES: &[KeyCode] = &[
        KeyCode::Space,
        KeyCode::Quote,
        KeyCode::Comma,
        KeyCode::Minus,
        KeyCode::Period,
        KeyCode::Slash,
        KeyCode::Digit0,
        KeyCode::Digit1,
        KeyCode::Digit2,
        KeyCode::Digit3,
        KeyCode::Digit4,
        KeyCode::Digit5,
        KeyCode::Digit6,
        KeyCode::Digit7,
        KeyCode::Digit8,
        KeyCode::Digit9,
        KeyCode::Semicolon,
        KeyCode::Equal,
        KeyCode::KeyA,
        KeyCode::KeyB,
        KeyCode::KeyC,
        KeyCode::KeyD,
        KeyCode::KeyE,
        KeyCode::KeyF,
        KeyCode::KeyG,
        KeyCode::KeyH,
        KeyCode::KeyI,
        KeyCode::KeyJ,
        KeyCode::KeyK,
        KeyCode::KeyL,
        KeyCode::KeyM,
        KeyCode::KeyN,
        KeyCode::KeyO,
        KeyCode::KeyP,
        KeyCode::KeyQ,
        KeyCode::KeyR,
        KeyCode::KeyS,
        KeyCode::KeyT,
        KeyCode::KeyU,
        KeyCode::KeyV,
        KeyCode::KeyW,
        KeyCode::KeyX,
        KeyCode::KeyY,
        KeyCode::KeyZ,
        KeyCode::BracketLeft,
        KeyCode::Backslash,
        KeyCode::BracketRight,
        KeyCode::Backquote,
        KeyCode::Escape,
        KeyCode::Enter,
        KeyCode::Tab,
        KeyCode::Backspace,
        KeyCode::Insert,
        KeyCode::Delete,
        KeyCode::ArrowRight,
        KeyCode::ArrowLeft,
        KeyCode::ArrowDown,
        KeyCode::ArrowUp,
        KeyCode::PageUp,
        KeyCode::PageDown,
        KeyCode::Home,
        KeyCode::End,
        KeyCode::CapsLock,
        KeyCode::ScrollLock,
        KeyCode::NumLock,
        KeyCode::PrintScreen,
        KeyCode::Pause,
        KeyCode::F1,
        KeyCode::F2,
        KeyCode::F3,
        KeyCode::F4,
        KeyCode::F5,
        KeyCode::F6,
        KeyCode::F7,
        KeyCode::F8,
        KeyCode::F9,
        KeyCode::F10,
        KeyCode::F11,
        KeyCode::F12,
        KeyCode::Numpad0,
        KeyCode::Numpad1,
        KeyCode::Numpad2,
        KeyCode::Numpad3,
        KeyCode::Numpad4,
        KeyCode::Numpad5,
        KeyCode::Numpad6,
        KeyCode::Numpad7,
        KeyCode::Numpad8,
        KeyCode::Numpad9,
        KeyCode::NumpadDecimal,
        KeyCode::NumpadDivide,
        KeyCode::NumpadMultiply,
        KeyCode::NumpadSubtract,
        KeyCode::NumpadAdd,
        KeyCode::NumpadEnter,
        KeyCode::NumpadEqual,
        KeyCode::ShiftLeft,
        KeyCode::ControlLeft,
        KeyCode::AltLeft,
        KeyCode::SuperLeft,
        KeyCode::ShiftRight,
        KeyCode::ControlRight,
        KeyCode::AltRight,
        KeyCode::SuperRight,
    ];
}
