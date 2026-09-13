# ADR 0011 - Everything a launch can re-download is launcher-level, including natives

**Status:** Accepted

## Context

Ephervex shares assets, libraries, and game JARs rather than copying them into
every instance. Disposable instances are the product default, so every
re-downloadable or reproducible artifact needs an explicit shared-store home.
Unpacked natives require special treatment because they are derived rather than
downloaded directly.

Natives are also the only one that is **derived** rather than downloaded. Up to
1.18.x a library ships a `natives` classifier jar whose shared objects must be
on disk before the JVM starts, because `-Djava.library.path` cannot read inside
an archive. Unpacking them into the instance would be the obvious thing to do,
and it is what several launchers do.

Two facts decided against it:

- The unpacked contents are a pure function of *(version, platform)*. Two
  instances of 1.12.2 on the same machine produce byte-identical directories.
- 1.12.2 unpacks roughly 8 MB of shared objects. That is small next to the
  assets, but the whole point of a temporary instance is that creating one is
  free, and "free" stops being true once every one of them unpacks its own copy
  of LWJGL and OpenAL.

From 1.19 onward there is nothing to unpack - natives became ordinary rule-gated
classpath jars - but the *directory* is still needed: it is what
`-Djava.library.path`, `org.lwjgl.system.SharedLibraryExtractPath`,
`jna.tmpdir`, and `io.netty.native.workdir` are pointed at, and the 26.x
releases point each of them at a different subdirectory of it.

## Decision

Every file a launch could re-download or re-derive lives under the cache root
(ADR 0003), keyed by content or by a validated logical key. Nothing that a
launch produces from a remote artifact is written into an instance.

| Artifact | Location | Keyed by |
|---|---|---|
| Libraries | `$XDG_CACHE_HOME/ephervex/libraries/` | validated Maven coordinate |
| Client jar, logging config, natives jars | `…/downloads/` | SHA-1 |
| Asset index, canonical | `…/metadata/` | SHA-1 |
| Asset index, launch view | `…/assets/indexes/<id>.json` | validated index id |
| Asset objects | `…/assets/objects/<xx>/<sha1>` | SHA-1, Mojang's own layout |
| **Unpacked natives** | `…/natives/<version>/<os>-<arch>/` | validated version id and platform |

`natives/` is a store under the cache root. `CacheLayout::natives` is fallible
because both of its components are
logical keys rather than digests - a Minecraft version id arrives from Mojang's
manifest - and the project's "never trust a name to choose a destination path"
has to hold for the one store whose path is not derived from a hash.

The asset-index launch view is a small atomic copy of the verified canonical
file, not a second download and not an instance-local artifact. Minecraft is
given only an assets root and index id and unconditionally derives
`assets/indexes/<id>.json`; keeping only the SHA-1 path therefore makes every
asset object unreachable even when all of those objects were downloaded. A
copy is used instead of a hard link so editing the browsable launch view cannot
corrupt the canonical file whose presence means it passed verification.

Instance directories hold only what a user or the game produced: saves,
`options.txt`, resource packs, logs, crash reports, and later mods and pack
overrides.

The client jar is content-addressed rather than filed under a synthetic
`com.mojang:minecraft:<version>` coordinate, even though the latter reads better
in a log. A logical key means a path that does not change when the bytes do, and
the download manager's "already at the destination" short-circuit would then
serve a stale jar for a republished version. Libraries get a coordinate only
because the shared store has to be browsable as a Maven repository for Forge
and NeoForge installers.

Some loader metadata classifies classpath entries by filename. In particular,
Forge 1.20.1's BootstrapLauncher uses `-DignoreList` prefixes to keep the
vanilla client out of its Java module scan, while the official metadata names a
version-layout jar. The launch builder preserves those official prefixes and
adds the actual SHA-1 filename when necessary; it does not rename or duplicate
the canonical client jar.

## Consequences

- Clearing the cache costs re-downloading and re-unpacking, never an instance.
  That is exactly the promise ADR 0003 makes about the cache root, and natives
  keep it because they can be regenerated from the libraries store.
- A second instance of a version already launched starts with no network I/O at
  all: every artifact is already at its destination, and atomic finalization
  means presence there is proof it was verified.
- One version's natives directory is shared by every instance of that version,
  which is what Mojang's own launcher does (`versions/<id>/natives`). For 1.19
  and later the directory is scratch space rather than content, and the three
  libraries that write into it - LWJGL, JNA, and Netty - each create and manage
  their own subdirectory, so concurrent instances of one version do not collide.
- Unpacking is idempotent and overwrites in place, so a natives directory is
  never deleted while a game might have its contents `dlopen`ed.
- The platform is part of the key, so a future cross-platform build cannot mix
  one operating system's shared objects into another's launch.
