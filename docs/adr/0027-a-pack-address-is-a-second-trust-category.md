# ADR 0027 - A pack address the user activated is a second trust category

**Status:** Accepted. Its decision about where the scheme is registered from is
amended by
[ADR 0028](0028-the-settings-screen-does-not-switch-the-link-handler.md);
everything else stands.

## Context

`ephervex-download`'s allowlist module states a rule and gives a good reason for
it: a `.mrpack`'s `files[].downloads` array is attacker-controlled text out of a
bug report, so following whatever it names would turn the launcher into a
confused deputy. The answer is a fixed, audited list of hosts, and nothing
outside it is fetched.

Opening a pack from a web page asks the launcher to fetch an address that is
not on that list and never could be. A link says
`ephervex://import?url=<some https address>`, and the whole value of the feature
is that the address is not one Ephervex knew about in advance.

Read against the existing rule, that looks like a hole being cut in it. Left
unrecorded it would read that way to the next person too, and the rule would
erode by precedent: the allowlist would gain entries for one site, then another,
until it no longer describes anything.

## Decision

An address that a person activated a link for is a **different trust category**
from an address that came out of an archive, and it gets its own gate rather
than an entry on the existing list.

**The two categories never mix.** A host agreed to here may serve *an archive*,
and nothing else. It does not become a permitted host for a pack's
`files[].downloads`; those stay on `MODRINTH_PACK_HOSTS` exactly as before. A
pack downloaded from an agreed host is still untrusted input and is still
subject to every existing check, because agreeing to fetch something is not
agreeing to believe it.

**Consent is explicit, informed, and per host.** Before anything is fetched the
user is shown the address with its query and fragment removed and asked. The
query is where a link carries identifiers and tokens, and a confirmation prompt
that printed one would be teaching users to skim past secrets; what is being
asked is which machine Ephervex will talk to, and that is what is shown.

**The answer is remembered, revocably, in configuration.** A host the user
agreed to goes in `config.toml` under `[import]`, so the second import from the
same site is one click. It is a host name rather than a credential, which is why
it belongs in a file users attach to bug reports, and it can be taken back.
Answering "just this once" records nothing.

**The gate lives in the application service, not in each frontend.** Downloading
refuses unless the host is already agreed to or the caller passes the consent it
just obtained. A frontend cannot reach the download by forgetting to ask,
because there is no path to it that does not go through the check.

**The archive itself is bounded.** `ExtractionLimits` bounds what an archive
expands into; nothing bounded the archive. An unbounded fetch of an address a
web page chose is a disk-exhaustion primitive, so a ceiling is enforced while
the body streams and an oversized one is abandoned as it crosses the line.

**The scheme is registered when installing said so, and not before.** Claiming a
system-wide URL scheme is not something a launcher should do quietly on first
run. Installing a package is not that: `apt install` is an explicit act, and a
package that registers a handler and removes it again on uninstall is ordinary
Debian behavior. So the package ships a second desktop entry that claims the
scheme, and the AppImage does not: it is not installed, has no moment to make
the choice at, and is a file the user may move or delete, which would leave an
entry naming a path that no longer exists. For that artifact registration stays
an explicit action, from the command line or the settings switch. *(Amended by
ADR 0028: the settings switch is gone, and the command line is the one way.)*

A registration the package owns is reported honestly and is not the
application's to undo: the switch shows that links work and says the package
manager put it there, rather than offering to delete a file `dpkg` believes it
still owns.

## Consequences

- The allowlist module keeps its rule intact and keeps its reason. A reader who
  finds this ADR can tell which category an address falls into, which is the
  thing that was at risk of being lost.
- A pack downloaded this way is indistinguishable from one the user saved and
  opened by hand, because it is: the fetch produces a file, and the existing
  import pipeline reads it with no new behavior.
- The downloaded archive is a temporary input, not something the launcher keeps.
  It lives under the temporary root and is removed when the import that wanted
  it is done, whether that import succeeded or not.
- The two artifacts differ in one visible way, and it is the right way round:
  installing the package makes links work, while the AppImage asks first.
- Plaintext HTTP stays refused in production. Development against a local
  DumpViewer uses the loopback escape hatch `ephervex-download` already
  documents for tests, so the production default is unchanged.
- Nothing here is specific to DumpViewer. The launcher downloads whatever pack
  the address serves, and the same link works for any pack served over HTTPS.
