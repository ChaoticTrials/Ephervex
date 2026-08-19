//! Release-neutral physical input, for Minecraft's key bindings.
//!
//! # Why a launcher needs its own input vocabulary
//!
//! Minecraft has stored key bindings two incompatible ways. Releases before
//! 1.13 write an `org.lwjgl.input.Keyboard` scan code, and 1.13 onward writes
//! an `InputConstants` name such as `key.keyboard.left.shift`. The same
//! physical key is `42` in one era and `key.keyboard.left.shift` in the other,
//! so a configured binding cannot be stored in either era's own spelling
//! without becoming a value that is only honest for half the releases
//! Ephervex supports.
//!
//! So a binding is held as a physical key, and turning one into an exact
//! release's text is the compatibility layer's job.
//!
//! # Where the names come from
//!
//! [`PhysicalKey::name`] is Minecraft's own `InputConstants` key name, minus
//! the `key.keyboard.` prefix. The set is closed and reviewed: it is exactly
//! the table `tools/minecraft-options/analyze_options.py` uses to resolve a
//! release's proven key-binding defaults, which is in turn cross-checked
//! against the pre-1.13 to 1.13 default pairs the analyzer observes. A key
//! outside this set has no proven name and therefore no representation here.

use std::fmt;

/// A physical keyboard key, named the way Minecraft names it.
///
/// Closed by construction. An input the launcher cannot name is refused
/// rather than approximated: writing "the nearest key" into a binding is
/// exactly the kind of silent substitution that makes a reproduction
/// environment stop reproducing anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[allow(
    missing_docs,
    reason = "each variant is one named physical key; its name is its documentation"
)]
pub enum PhysicalKey {
    Space,
    Apostrophe,
    Comma,
    Minus,
    Period,
    Slash,
    Digit0,
    Digit1,
    Digit2,
    Digit3,
    Digit4,
    Digit5,
    Digit6,
    Digit7,
    Digit8,
    Digit9,
    Semicolon,
    Equal,
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
    LeftBracket,
    Backslash,
    RightBracket,
    GraveAccent,
    Escape,
    Enter,
    Tab,
    Backspace,
    Insert,
    Delete,
    Right,
    Left,
    Down,
    Up,
    PageUp,
    PageDown,
    Home,
    End,
    CapsLock,
    ScrollLock,
    NumLock,
    PrintScreen,
    Pause,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    Keypad0,
    Keypad1,
    Keypad2,
    Keypad3,
    Keypad4,
    Keypad5,
    Keypad6,
    Keypad7,
    Keypad8,
    Keypad9,
    KeypadDecimal,
    KeypadDivide,
    KeypadMultiply,
    KeypadSubtract,
    KeypadAdd,
    KeypadEnter,
    KeypadEqual,
    LeftShift,
    LeftControl,
    LeftAlt,
    LeftWin,
    RightShift,
    RightControl,
    RightAlt,
    RightWin,
}

/// Every physical key, in the order the analyzer's reference table lists
/// them, which is the key's own window-system code order.
///
/// Used for iteration and for reading a name back. The authoritative
/// name of each key is [`PhysicalKey::name`]'s exhaustive match: a variant
/// with no name would fail to compile rather than fall back to a name that
/// means something else.
const PHYSICAL_KEYS: &[PhysicalKey] = &[
    PhysicalKey::Space,
    PhysicalKey::Apostrophe,
    PhysicalKey::Comma,
    PhysicalKey::Minus,
    PhysicalKey::Period,
    PhysicalKey::Slash,
    PhysicalKey::Digit0,
    PhysicalKey::Digit1,
    PhysicalKey::Digit2,
    PhysicalKey::Digit3,
    PhysicalKey::Digit4,
    PhysicalKey::Digit5,
    PhysicalKey::Digit6,
    PhysicalKey::Digit7,
    PhysicalKey::Digit8,
    PhysicalKey::Digit9,
    PhysicalKey::Semicolon,
    PhysicalKey::Equal,
    PhysicalKey::A,
    PhysicalKey::B,
    PhysicalKey::C,
    PhysicalKey::D,
    PhysicalKey::E,
    PhysicalKey::F,
    PhysicalKey::G,
    PhysicalKey::H,
    PhysicalKey::I,
    PhysicalKey::J,
    PhysicalKey::K,
    PhysicalKey::L,
    PhysicalKey::M,
    PhysicalKey::N,
    PhysicalKey::O,
    PhysicalKey::P,
    PhysicalKey::Q,
    PhysicalKey::R,
    PhysicalKey::S,
    PhysicalKey::T,
    PhysicalKey::U,
    PhysicalKey::V,
    PhysicalKey::W,
    PhysicalKey::X,
    PhysicalKey::Y,
    PhysicalKey::Z,
    PhysicalKey::LeftBracket,
    PhysicalKey::Backslash,
    PhysicalKey::RightBracket,
    PhysicalKey::GraveAccent,
    PhysicalKey::Escape,
    PhysicalKey::Enter,
    PhysicalKey::Tab,
    PhysicalKey::Backspace,
    PhysicalKey::Insert,
    PhysicalKey::Delete,
    PhysicalKey::Right,
    PhysicalKey::Left,
    PhysicalKey::Down,
    PhysicalKey::Up,
    PhysicalKey::PageUp,
    PhysicalKey::PageDown,
    PhysicalKey::Home,
    PhysicalKey::End,
    PhysicalKey::CapsLock,
    PhysicalKey::ScrollLock,
    PhysicalKey::NumLock,
    PhysicalKey::PrintScreen,
    PhysicalKey::Pause,
    PhysicalKey::F1,
    PhysicalKey::F2,
    PhysicalKey::F3,
    PhysicalKey::F4,
    PhysicalKey::F5,
    PhysicalKey::F6,
    PhysicalKey::F7,
    PhysicalKey::F8,
    PhysicalKey::F9,
    PhysicalKey::F10,
    PhysicalKey::F11,
    PhysicalKey::F12,
    PhysicalKey::Keypad0,
    PhysicalKey::Keypad1,
    PhysicalKey::Keypad2,
    PhysicalKey::Keypad3,
    PhysicalKey::Keypad4,
    PhysicalKey::Keypad5,
    PhysicalKey::Keypad6,
    PhysicalKey::Keypad7,
    PhysicalKey::Keypad8,
    PhysicalKey::Keypad9,
    PhysicalKey::KeypadDecimal,
    PhysicalKey::KeypadDivide,
    PhysicalKey::KeypadMultiply,
    PhysicalKey::KeypadSubtract,
    PhysicalKey::KeypadAdd,
    PhysicalKey::KeypadEnter,
    PhysicalKey::KeypadEqual,
    PhysicalKey::LeftShift,
    PhysicalKey::LeftControl,
    PhysicalKey::LeftAlt,
    PhysicalKey::LeftWin,
    PhysicalKey::RightShift,
    PhysicalKey::RightControl,
    PhysicalKey::RightAlt,
    PhysicalKey::RightWin,
];

impl PhysicalKey {
    /// Every key this build can express, in a stable order.
    pub fn all() -> impl Iterator<Item = Self> {
        PHYSICAL_KEYS.iter().copied()
    }

    /// Minecraft's own name for this key, without the `key.keyboard.` prefix.
    ///
    /// Exhaustive rather than a table lookup with a fallback. The one name a
    /// fallback would naturally reach for is `unknown`, which is Minecraft's
    /// marker for *unbound* - so a missing entry would not produce a broken
    /// binding, it would silently unbind one.
    #[must_use]
    #[expect(
        clippy::too_many_lines,
        reason = "one arm per named physical key; exhaustiveness is the point and splitting it would only hide which keys are covered"
    )]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Space => "space",
            Self::Apostrophe => "apostrophe",
            Self::Comma => "comma",
            Self::Minus => "minus",
            Self::Period => "period",
            Self::Slash => "slash",
            Self::Digit0 => "0",
            Self::Digit1 => "1",
            Self::Digit2 => "2",
            Self::Digit3 => "3",
            Self::Digit4 => "4",
            Self::Digit5 => "5",
            Self::Digit6 => "6",
            Self::Digit7 => "7",
            Self::Digit8 => "8",
            Self::Digit9 => "9",
            Self::Semicolon => "semicolon",
            Self::Equal => "equal",
            Self::A => "a",
            Self::B => "b",
            Self::C => "c",
            Self::D => "d",
            Self::E => "e",
            Self::F => "f",
            Self::G => "g",
            Self::H => "h",
            Self::I => "i",
            Self::J => "j",
            Self::K => "k",
            Self::L => "l",
            Self::M => "m",
            Self::N => "n",
            Self::O => "o",
            Self::P => "p",
            Self::Q => "q",
            Self::R => "r",
            Self::S => "s",
            Self::T => "t",
            Self::U => "u",
            Self::V => "v",
            Self::W => "w",
            Self::X => "x",
            Self::Y => "y",
            Self::Z => "z",
            Self::LeftBracket => "left.bracket",
            Self::Backslash => "backslash",
            Self::RightBracket => "right.bracket",
            Self::GraveAccent => "grave.accent",
            Self::Escape => "escape",
            Self::Enter => "enter",
            Self::Tab => "tab",
            Self::Backspace => "backspace",
            Self::Insert => "insert",
            Self::Delete => "delete",
            Self::Right => "right",
            Self::Left => "left",
            Self::Down => "down",
            Self::Up => "up",
            Self::PageUp => "page.up",
            Self::PageDown => "page.down",
            Self::Home => "home",
            Self::End => "end",
            Self::CapsLock => "caps.lock",
            Self::ScrollLock => "scroll.lock",
            Self::NumLock => "num.lock",
            Self::PrintScreen => "print.screen",
            Self::Pause => "pause",
            Self::F1 => "f1",
            Self::F2 => "f2",
            Self::F3 => "f3",
            Self::F4 => "f4",
            Self::F5 => "f5",
            Self::F6 => "f6",
            Self::F7 => "f7",
            Self::F8 => "f8",
            Self::F9 => "f9",
            Self::F10 => "f10",
            Self::F11 => "f11",
            Self::F12 => "f12",
            Self::Keypad0 => "keypad.0",
            Self::Keypad1 => "keypad.1",
            Self::Keypad2 => "keypad.2",
            Self::Keypad3 => "keypad.3",
            Self::Keypad4 => "keypad.4",
            Self::Keypad5 => "keypad.5",
            Self::Keypad6 => "keypad.6",
            Self::Keypad7 => "keypad.7",
            Self::Keypad8 => "keypad.8",
            Self::Keypad9 => "keypad.9",
            Self::KeypadDecimal => "keypad.decimal",
            Self::KeypadDivide => "keypad.divide",
            Self::KeypadMultiply => "keypad.multiply",
            Self::KeypadSubtract => "keypad.subtract",
            Self::KeypadAdd => "keypad.add",
            Self::KeypadEnter => "keypad.enter",
            Self::KeypadEqual => "keypad.equal",
            Self::LeftShift => "left.shift",
            Self::LeftControl => "left.control",
            Self::LeftAlt => "left.alt",
            Self::LeftWin => "left.win",
            Self::RightShift => "right.shift",
            Self::RightControl => "right.control",
            Self::RightAlt => "right.alt",
            Self::RightWin => "right.win",
        }
    }

    /// Reads a key back from [`Self::name`].
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        PHYSICAL_KEYS
            .iter()
            .copied()
            .find(|candidate| candidate.name() == name)
    }
}

impl fmt::Display for PhysicalKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The durable token for a key binding that is deliberately bound to nothing.
const UNBOUND_TOKEN: &str = "unbound";
/// The durable prefix for a keyboard binding, as `key:<physical key name>`.
const KEYBOARD_PREFIX: &str = "key:";
/// The durable prefix for a mouse binding, as `mouse:<button name>`.
const MOUSE_PREFIX: &str = "mouse:";

/// A mouse button, as Minecraft's own input constants distinguish them.
///
/// `Extra` carries the zero-based button index the window system reports;
/// [`Self::name`] renders it the way Minecraft labels it, which is one-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    /// A button beyond the three named ones. Zero-based, and never one of
    /// `0`, `1`, or `2`, which are the three named buttons.
    Extra(u8),
}

impl MouseButton {
    /// The highest zero-based button index Minecraft's input layer reports.
    pub const MAXIMUM_INDEX: u8 = 7;

    /// The button for one zero-based index, or `None` when no such button
    /// exists.
    #[must_use]
    pub const fn from_index(index: u8) -> Option<Self> {
        match index {
            0 => Some(Self::Left),
            1 => Some(Self::Right),
            2 => Some(Self::Middle),
            3..=Self::MAXIMUM_INDEX => Some(Self::Extra(index)),
            _ => None,
        }
    }

    /// The zero-based index the window system reports for this button.
    #[must_use]
    pub const fn index(self) -> u8 {
        match self {
            Self::Left => 0,
            Self::Right => 1,
            Self::Middle => 2,
            Self::Extra(index) => index,
        }
    }

    /// The name Minecraft gives this button, which is one-based for every
    /// button past the middle one.
    #[must_use]
    pub fn name(self) -> String {
        match self {
            Self::Left => "left".to_owned(),
            Self::Right => "right".to_owned(),
            Self::Middle => "middle".to_owned(),
            Self::Extra(index) => (u16::from(index) + 1).to_string(),
        }
    }

    /// Reads a button back from [`Self::name`].
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
            "middle" => Some(Self::Middle),
            _ => {
                let label: u16 = name.parse().ok()?;
                let index = label.checked_sub(1)?;
                let index = u8::try_from(index).ok()?;
                match Self::from_index(index) {
                    Some(Self::Extra(index)) => Some(Self::Extra(index)),
                    _ => None,
                }
            }
        }
    }
}

/// What one key binding is bound to, independently of any Minecraft release.
///
/// Release-neutral on purpose: the pre-1.13 and modern eras spell the same
/// physical key completely differently, and a launcher that stored one era's
/// spelling would be storing a value it could not honestly apply to the other.
/// Turning this into an era's own text is
/// [`crate::codec::OptionCodec`]'s job.
///
/// Conflicts are not modeled here. Minecraft itself permits two actions bound
/// to one key and shows the clash rather than refusing it, so a launcher that
/// refused would be stricter than the game.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum InputBinding {
    /// Bound to nothing at all. A real, chosen state, not "unknown".
    Unbound,
    Keyboard(PhysicalKey),
    Mouse(MouseButton),
}

impl InputBinding {
    /// The durable text form stored in configuration.
    #[must_use]
    pub fn to_stored_text(self) -> String {
        match self {
            Self::Unbound => UNBOUND_TOKEN.to_owned(),
            Self::Keyboard(key) => format!("{KEYBOARD_PREFIX}{}", key.name()),
            Self::Mouse(button) => format!("{MOUSE_PREFIX}{}", button.name()),
        }
    }

    /// Reads the durable text form back.
    #[must_use]
    pub fn parse_stored(text: &str) -> Option<Self> {
        if text == UNBOUND_TOKEN {
            return Some(Self::Unbound);
        }
        if let Some(name) = text.strip_prefix(KEYBOARD_PREFIX) {
            return PhysicalKey::from_name(name).map(Self::Keyboard);
        }
        if let Some(name) = text.strip_prefix(MOUSE_PREFIX) {
            return MouseButton::from_name(name).map(Self::Mouse);
        }
        None
    }
}

impl fmt::Display for InputBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_stored_text())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn every_physical_key_appears_exactly_once_under_exactly_one_name() {
        // The table is the only place a key's name lives, so a duplicated
        // variant or a duplicated name would silently make one key
        // unreachable through `from_name`.
        let mut keys = BTreeSet::new();
        let mut names = BTreeSet::new();
        for key in PhysicalKey::all() {
            assert!(keys.insert(key), "`{key}` repeats a physical key");
            assert!(names.insert(key.name()), "`{key}` repeats a key name");
        }
        assert_eq!(keys.len(), PHYSICAL_KEYS.len());
        assert!(
            !names.contains("unknown"),
            "`unknown` is Minecraft's unbound marker and is never a physical key"
        );
    }

    #[test]
    fn a_physical_key_round_trips_through_its_minecraft_name() {
        for key in PhysicalKey::all() {
            assert_eq!(PhysicalKey::from_name(key.name()), Some(key), "{key}");
        }
        assert_eq!(
            PhysicalKey::from_name("left.shift"),
            Some(PhysicalKey::LeftShift)
        );
        assert_eq!(PhysicalKey::from_name("w"), Some(PhysicalKey::W));
        assert_eq!(
            PhysicalKey::from_name("key.keyboard.w"),
            None,
            "the prefix belongs to a release's own spelling, not to the model"
        );
        assert_eq!(PhysicalKey::from_name("hyperspace"), None);
    }

    #[test]
    fn a_mouse_button_is_named_the_way_minecraft_labels_it() {
        // Minecraft names the three known buttons and labels the rest
        // one-based, so button index 3 is `4`.
        assert_eq!(MouseButton::Left.name(), "left");
        assert_eq!(MouseButton::Right.name(), "right");
        assert_eq!(MouseButton::Middle.name(), "middle");
        assert_eq!(MouseButton::Extra(3).name(), "4");
        assert_eq!(MouseButton::Extra(7).name(), "8");

        for button in [
            MouseButton::Left,
            MouseButton::Right,
            MouseButton::Middle,
            MouseButton::Extra(3),
            MouseButton::Extra(7),
        ] {
            assert_eq!(MouseButton::from_name(&button.name()), Some(button));
            assert_eq!(MouseButton::from_index(button.index()), Some(button));
        }
        assert_eq!(MouseButton::from_index(8), None);
        assert_eq!(
            MouseButton::from_name("1"),
            None,
            "the first three buttons have names, not numbers"
        );
        assert_eq!(MouseButton::from_name("9"), None);
    }

    #[test]
    fn a_binding_round_trips_through_its_durable_text_form() {
        let cases = [
            (InputBinding::Unbound, "unbound"),
            (InputBinding::Keyboard(PhysicalKey::W), "key:w"),
            (
                InputBinding::Keyboard(PhysicalKey::LeftShift),
                "key:left.shift",
            ),
            (InputBinding::Mouse(MouseButton::Left), "mouse:left"),
            (InputBinding::Mouse(MouseButton::Extra(3)), "mouse:4"),
        ];
        for (binding, text) in cases {
            assert_eq!(binding.to_stored_text(), text);
            assert_eq!(InputBinding::parse_stored(text), Some(binding));
        }
    }

    #[test]
    fn unbound_is_a_chosen_state_rather_than_an_unreadable_one() {
        // "bound to nothing" is a real decision a user makes, and it must not
        // be reachable by accident from a value nobody could read.
        assert_eq!(
            InputBinding::parse_stored("unbound"),
            Some(InputBinding::Unbound)
        );
        for hostile in [
            "",
            "key:",
            "mouse:",
            "key:unknown",
            "none",
            "key.keyboard.w",
        ] {
            assert_eq!(InputBinding::parse_stored(hostile), None, "{hostile:?}");
        }
    }
}
