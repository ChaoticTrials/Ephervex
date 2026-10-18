# ADR 0052 - Menus are drawn by the application

**Status:** Accepted

## Context

A theme is a complete color palette (ADR 0041), and the interface paints every
color from the active palette's tokens. The toolkit's own context menu does not
take part. It is drawn by the toolkit's style from colors the application can
read but not write, and the only thing a theme can set for it is the light or
dark scheme. With a palette that differs strongly from the stock grays, such as
a seasonal one, the menu that opens on a right-click stands out from the rest
of the window.

The toolkit offers no way to restyle that menu or to give it custom rows, and
it opens the menu itself on a right-click without letting the application see
the click first.

## Decision

**Every menu is a popup the application draws.** A menu is a list of entries
painted from `Theme` tokens, in the same surface, border, text and selection
tokens the dropdown lists use. The toolkit's menu element is not used.

**A menu is data.** Its entries are rows with an id, a label and flags for
disabled, hidden, separator and sub-menu. A row that is hidden takes no room
and cannot be reached, so one list holds every entry a menu might offer and
the markup shows only some. A caller that has a model of its own, such as the
accounts a launch can be made as, lists it first and the fixed entries after.

**The application keeps what the toolkit's menu gave.** Arrow keys, Home and
End move over the entries that can be chosen, Enter or Space chooses, Escape
closes, and every entry is exposed to assistive technology as a button with its
label. A menu closes when the element it was opened from moves, because a
popup is placed once.

**A secondary-button press is routed by the application.** A touch area
accepts a press of any button, so a card's menu is opened from the touch areas
that sit over the card, each of which reports a secondary press and where it
landed. A press on a card's button therefore opens the card's menu instead of
being swallowed by the button.

## Consequences

- A menu follows the palette, including a user theme and a seasonal one.
- A new menu is a list of rows and a handler. It does not need a new popup.
- A sub-menu is a second menu opened where the first was, not a cascade.
- An element that should offer a menu where it is pressed has to report the
  secondary press itself. A control that does not leaves its press to itself.
