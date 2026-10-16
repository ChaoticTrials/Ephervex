# ADR 0047 - A theme folder carries branding confined to its own branding folder

**Status:** Accepted (amends ADR 0042)

## Context

ADR 0042 makes a user theme one TOML file of colors. Some themes want their own
logo and window icon as well, which means a theme now names files. Themes are
copied between people, so a theme that can name a file can try to name any
file: another theme's, one elsewhere in the home directory, or one reached
through a symbolic link that is swapped in after it was checked.

## Decision

**A theme with images is a folder.** `themes/<id>/theme.toml` is a theme in the
format of ADR 0042, and the folder name is its id. A standalone `<id>.toml`
stays valid and stays colors only. When both exist and the folder is usable,
the folder is used.

**Branding names files inside `branding/`, and nothing else.** `[branding]`
takes `logo` and `icon`, each a relative path inside the folder's own
`branding/` folder. The text is refused before anything is opened when it is
absolute, uses `.` or `..`, contains a backslash or a colon, or does not end in
`.png`, `.jpg`, `.jpeg` or `.webp`.

**The filesystem boundary is enforced by how files are opened.** Below the
themes folder every component is opened with `openat` relative to the directory
opened before it, with `O_NOFOLLOW`, so a symbolic link anywhere in the path is
refused and replacing a directory after it was opened cannot redirect the read.
The file must be a regular file, checked on the open descriptor. The themes
folder itself is opened normally, since it is the user's own configuration.
This lives in `ephervex-core` so the frontend never handles the paths itself.

**Images are bounded and decoded once.** A file is read up to 4 MiB, its
contents must be the format its extension names, and it must be at most 1024
pixels on each side, with the decoder's allocation capped from the moment it
reads the header. Only the chosen theme's images are decoded, on the backend
thread.

**Every failure costs the image, not the theme.** A refused or unusable image
is reported under the theme setting and the embedded default is shown in its
place. Logo and icon fall back independently.

**A built-in theme's branding is compiled in.** A built-in theme may name
images embedded in the binary, decoded through the same bounds; it never reads
a file. Dark and Light name none and show the embedded default logo.

**The window icon follows the theme where the windowing system allows.** It is
replaced at run time on X11. Wayland compositors take the icon from the desktop
entry, so there it stays the installed icon.

## Consequences

- A theme can now name files, but only its own, and only images.
- A theme folder can be copied between machines as one unit.
- Packaging a theme as a symbolic link to another location does not work, by
  design. The themes folder as a whole may still be a link.
- `[branding]` keys join the theme format. A key can be added; renaming or
  removing one breaks the themes that set it.
