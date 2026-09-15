# ADR 0029 - The allowed-sites list carries its own context

**Status:** Accepted

Supersedes [ADR 0028](0028-the-settings-screen-does-not-switch-the-link-handler.md).
The registration decisions there stand unchanged: a package manager or
`ephervex url-handler` claims and releases the scheme, and nothing on the
settings screen touches that. What this amends is only the heading ADR 0028
kept for context.

## Context

ADR 0028 removed the settings switch for link handling but kept a "Modpack
links" heading and a paragraph above the allowed-sites list, on the reasoning
that the list needed the context of what it was for. In practice the heading
duplicated what the list's own section already says: the allowed-sites list
has its own title, "Sites allowed to serve modpacks", and its own detail text
explaining what being on it means. A second heading immediately above it,
about a mechanism the settings screen no longer controls, read as a leftover
rather than as context - an empty section with nothing to act on, sitting
above the one that does something.

## Decision

**The settings screen shows the allowed-sites list and nothing else for link
handling.** The separate "Modpack links" heading and detail text are removed.
The allowed-sites list's own title and detail text are the only explanation
this area needs: what appears there is a fact about that list, not about the
link-handling mechanism in general.

**Nothing about registration or consent changes.** A link still opens a
consent dialog naming the exact address (ADR 0027); accepting it still offers
to remember the site so future links from it skip the prompt. This is a
settings-page presentation change only.

## Consequences

- The settings screen has one fewer heading on the general tab, and no text
  block that used to sit above the list it explained without controlling
  anything.
- `gui-link-handling` and `gui-link-handling-detail` are removed from the
  English locale; nothing else referenced them.
- The regression test guarding "no switch here" now anchors on the
  allowed-sites list's own bounds instead of the removed heading's.
