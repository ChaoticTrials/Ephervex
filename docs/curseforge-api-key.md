# The CurseForge API key

Ephervex reaches CurseForge through the official CurseForge third-party API.
That API requires a key. An official release carries a key issued to Ephervex
itself as a fallback (ADR 0030), so CurseForge features work out of the box;
a key you configure yourself always takes precedence over that fallback, and
a build without one (any local development build, or a private/forked build
that does not set the release secret) cannot make a CurseForge request until
you configure your own.

## Why Ephervex needs one

CurseForge does not offer an anonymous read path for the data a launcher needs,
and since 2026 direct CurseForge file downloads require an API key as well.
Without a key in effect, CurseForge features stay unavailable; every other
part of Ephervex, including Modrinth workflows, works exactly as before.

## Getting a key

Apply through CurseForge/Overwolf:

<https://support.curseforge.com/support/solutions/articles/9000208346-about-the-curseforge-api-and-how-to-apply-for-a-key>

Approval is performed by CurseForge/Overwolf. Ephervex has no part in it and
cannot grant, speed up, or appeal a decision.

**This is not the CurseForge Upload API token.** The Upload API is a separate
service for authors publishing files, it uses a different credential, and it is
not what Ephervex asks for. If what you have is labeled as an upload token, it
will not work here.

## Where Ephervex keeps it

In your operating system's keyring, through the Secret Service (ADR 0004).

* It is never written to `config.toml`.
* It is never written to a pack recipe or to instance or server metadata.
* It is never written to a log, an error message, or a diagnostic bundle.
* There is no plaintext fallback. If no keyring is available, Ephervex says so
  rather than writing the key to a file.

Once stored, the key cannot be read back through the launcher's interface.
Settings reports which key is in effect and nothing more: *Configured* for one
of your own, *Default* where the build's own key is standing in for it, and
*Not configured* where there is neither. Replacing a key means entering the new
one. Removing a key never requires revealing the old one, and never touches a
build's own fallback key: there is nothing to remove until you configure one
yourself.

## Setting it

In the desktop application: **Settings → Providers & APIs → CurseForge**.

From the command line, the key is read from standard input so it does not reach
your shell history or the process list:

```bash
ephervex api set-curseforge-key < key.txt
ephervex api show
ephervex api clear-curseforge-key
```

## What it is used for

Importing a CurseForge modpack export, and fetching a CurseForge artifact again
after the shared artifact cache has been collected. The archive itself is read
locally, so
Ephervex identifies the file, names the pack, and reports its exact Minecraft
version, loader build, and file counts with no key configured at all. What needs
the key is looking those files up: a CurseForge manifest names each one by
project id and file id, and their names, sizes, checksums, and addresses exist
only in the API. The import then downloads and verifies each one.

The key travels to two places and nowhere else: the CurseForge API, and
CurseForge's own file-delivery hosts. Ephervex will not follow a redirect that
leaves either, so no response can send the key somewhere it was not issued for,
and one import reads it once rather than once per file.

Once a pack is installed, the instance owns its own copy of every file and needs
nothing from CurseForge to run. Building a linked server from it, or repairing a
mod, reads the instance's retained recipe rather than the API - but the files
those operations install come out of the shared artifact cache, and if the cache
budget has since collected one, the bytes have to come back from CurseForge. That
download needs the key, and Ephervex reports a missing, unreadable, or refused
key as exactly that rather than as a failed download. Raising the artifact cache
budget, or leaving it unlimited, is what avoids the situation.

Some projects have third-party distribution turned off by their authors. The API
answers about such a file and declines to say where it is, so Ephervex cannot
download it and says which project stopped the import. That is the author's
decision, and there is no setting that works around it.

## What Ephervex does not keep

CurseForge API responses are not cached. Project documents, file documents,
search results, and game metadata live in memory for as long as the operation
that asked for them and are then dropped. There is no on-disk cache, no
time-to-live, and no CurseForge namespace under the metadata store.

This is deliberate: the published CurseForge third-party API terms prohibit
saving or caching data obtained through the API. Verified downloaded artifacts
are a separate matter and follow Ephervex's ordinary shared-store rules.
