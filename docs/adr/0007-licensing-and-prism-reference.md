# ADR 0007 - Licensing deferred; Prism is behavioral reference only

**Status:** Superseded by ADR 0018

## Context

Ephervex starts as a private project but should not be architected in a way that
makes a later public release difficult.

Prism Launcher is the closest existing product and an excellent behavioral
reference. It is also GPL-licensed. Copying its code would impose derivative-work
obligations that the project has not chosen.

## Decision

**No `LICENSE` file yet.** The workspace declares `license = "UNLICENSED"` and
`publish = false`, which is an accurate statement of the current position rather
than a placeholder that might get mistaken for a grant.

Prism Launcher is used strictly as a **behavioral and product reference**:
observed file formats, documented interoperability, and UX decisions worth
learning from. Its source code is not copied, adapted, or transcribed.

Everything is implemented independently from published specifications and
observable interoperable formats.

## Consequences

- Both permissive and copyleft paths stay open. Choosing one later is a new ADR
  superseding this one, and requires no code rewrite.
- Third-party license terms that constrain distribution must be reviewed before
  any public release. Slint's terms (ADR 0001) are the known one.
- Import formats are implemented from their specifications, such as the
  Modrinth pack format and CurseForge export structure, not from reading
  another launcher's parser.
- Interoperating with a documented format is not derivation, but this decision
  does not commit Ephervex to supporting any particular launcher export.
