# The CurseForge API key

Ephervex reaches CurseForge through the official CurseForge third-party API.
That API requires a key, and the key is yours rather than Ephervex's: the
launcher ships no key of its own and cannot make a CurseForge request on your
behalf until you have configured one.

## Why Ephervex needs one

CurseForge does not offer an anonymous read path for the data a launcher needs,
and since 2026 direct CurseForge file downloads require an API key as well.
Without a key, CurseForge features stay unavailable; every other part of
Ephervex, including Modrinth workflows, works exactly as before.

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
Settings reports it as *Configured* or *Not configured* and nothing more, so
replacing a key means entering the new one. Removing a key never requires
revealing the old one.

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

Importing a CurseForge modpack export. The archive itself is read locally, so
Ephervex identifies the file, names the pack, and reports its exact Minecraft
version, loader build, and file counts with no key configured at all. What needs
the key is looking those files up: a CurseForge manifest names each one by
project id and file id, and their names, sizes, checksums, and addresses exist
only in the API. The import then downloads and verifies each one.

The key travels to two places and nowhere else: the CurseForge API, and
CurseForge's own file-delivery hosts. Ephervex will not follow a redirect that
leaves either, so no response can send the key somewhere it was not issued for,
and one import reads it once rather than once per file.

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
