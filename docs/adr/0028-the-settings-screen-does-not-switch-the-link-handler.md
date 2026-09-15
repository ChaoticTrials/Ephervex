# ADR 0028 - The settings screen does not switch the link handler

**Status:** Superseded by [ADR 0029](0029-the-allowed-sites-list-carries-its-own-context.md)

Amends [ADR 0027](0027-a-pack-address-is-a-second-trust-category.md), whose
scheme-registration decision named "the command line or the settings switch" as
the two ways to register. There is now one. Everything else in ADR 0027 stands:
the two trust categories, per-host consent, the bounded fetch, and the rule that
installing the package is what makes claiming the scheme acceptable.

## Context

ADR 0027 put a switch on the settings screen and then had to explain, on the
same screen, that it does not work when a package manager installed the handler:
a greyed control, a line saying the package put it there, and a line telling the
user to turn it off and on again from where Ephervex is now. Three pieces of
copy for a control that answers "yes" in one case and nothing at all in the
other.

The control is also the wrong shape for what it does. Registering writes a
desktop entry and asks the desktop to prefer it; unregistering deletes that
entry - or, for the package's entry, cannot. That is installation, and it
happens once. A settings screen is for preferences a person revisits, and
putting an install step there is what produced a switch that had to argue with
itself.

## Decision

**The settings screen offers no control over the scheme handler.** It keeps the
section, because that is where the sites agreed to for modpack downloads are
managed and they need the context, and it drops the switch and the three lines
that existed to explain it.

**Registration is the package's, or the command line's.** `apt install` claims
the scheme; `apt remove` gives it back. For the AppImage,
`ephervex url-handler register`, `status` and `unregister` are the whole
interface, and they are documented where the artifact is.

**Nothing a package manager owns is touched, by anything.** That part of ADR
0027 was right and is now simply true by construction: there is no path from the
desktop application to that file at all.

**An `Exec` is resolved the way a desktop resolves it.** `ephervex url-handler
status` reports whether the registered command still exists, and checking a bare
command name as a path relative to the running process's working directory
reported every packaged installation as broken. A bare name is looked up on
`PATH`; only a path is checked as a path.

## Consequences

- One screen fewer to explain, and no control whose meaning depends on how the
  application was installed.
- Turning link handling off on a Debian system is `apt remove`, or a desktop's
  own default-applications settings, which is where a user changes any other
  association.
- An AppImage user registers from the command line. That was already one of the
  two documented ways and is now the documented way.
- `ephervex-core` keeps `register_url_handler`, `unregister_url_handler` and
  `url_handler_status`; the command-line frontend is their only caller.
