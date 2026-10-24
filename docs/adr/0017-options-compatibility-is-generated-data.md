# 0017 - options.txt compatibility is generated data, distributed separately from the application

Status: Accepted

## Context

Ephervex applies a small set of normalized global preferences to every newly
created or imported client instance. Doing that correctly requires knowing, for
one exact Minecraft release, what the game actually stores:

- the key, which is not always the preference's name - a window size is
  `overrideWidth` and `overrideHeight`;
- the encoding, which changes across releases - key bindings are LWJGL2 integer
  codes before 1.13 and GLFW key names from then on, and the field of view is
  stored as `(degrees - 70) / 40`, written as a decimal, so its default reads
  `fov:0.0`;
- the domain, which also changes - the simulation-distance value set narrowed in
  1.21.9;
- whether the key existed at all - simulation distance arrived with 1.18;
- and, for the language, whether that release's own catalog contains the code,
  which is not a property of `options.txt` at all.

None of this can be inferred from a version number. Two consecutive patch
releases may store a setting identically or not, and the only way to know is to
look at what the client actually compiles.

The first implementation encoded three eras as hand-written adapters in Rust.
That had two failure modes that were only going to get worse. It was **wrong in
places** - it asserted that 1.13 removed the window-size keys, which the client
JARs disprove: the keys are still in the persistence path in 26.2. And it made a
**new Minecraft release an Ephervex release**: a launcher whose job is
reproducing bugs on the version that just broke cannot ship a build to learn
that version's option keys.

## Decision

Compatibility knowledge is **generated from static analysis of official client
JARs, committed as data, and distributed separately from the application**.

1. **Generated, not written.** `tools/minecraft-options/update.py` reads
   Mojang's version manifest, fetches and hash-verifies one release's official
   client JAR, analyzes it statically without ever executing Minecraft, and
   folds the result into the dataset. Nothing about a release's representation
   is hand-asserted anywhere.

2. **Three states, and unknown is not unsupported.** A release's schema names
   the settings whose representation was proven and, separately, the settings
   that provably did not exist. Anything in neither list is *unknown*. Only a
   proven representation may change an `options.txt`; unsupported and unknown
   both mean "leave it alone", and they stay distinct because "this release has
   no such setting" and "nobody has analyzed this release" are different answers
   to give a user.

3. **Schema families with content-derived identity.** Releases point at
   deduplicated families identified by a digest of the family's own expanded
   content. Two releases share a family only when their extracted data is the
   same conclusion. Being a hotfix never makes a release equivalent to its
   predecessor; only the extraction does.

   The setting registry is cumulative because historical families retain stable
   references. A separate current canonical id list comes from the newest
   supported release, so a historical setting that disappeared does not remain
   current by accident.

4. **Bundled baseline plus a remote feed.** The dataset ships compiled into the
   binary, so a fresh offline installation understands every release verified
   when that build shipped. A newer validated copy may come from a remote feed,
   which is what lets a new Minecraft release become configurable without an
   Ephervex release. The fallback order is: valid newer remote cache, then the
   bundled baseline.

5. **Codecs are a closed set, never expressions.** The feed is untrusted network
   input that decides how values are written into a file the game reads. It may
   only select among typed codec variants the binary implements - boolean,
   bounded integer, bounded decimal, an affine transform, enum id, enum token,
   the two key-code eras, text, and language code - with validated parameters. A
   formula the generator cannot recognize as one of those, or a transform on a
   setting the game writes as an integer, becomes *unknown* at generation time.
   Nothing evaluates anything at run time.

6. **Remote data extends compatibility; it does not create product surface.**
   Which settings exist as Ephervex preferences is compiled in. A canonical
   setting id the build does not know is ignored, so a feed can teach the
   launcher about a new *release* but never about a new *setting*. Exposing a
   new user-facing preference remains an application release.

7. **Validated whole, then cached atomically.** A document is parsed and fully
   validated before anything is kept. Its revision must also be newer than the
   last known-good cache; an older revision is a rollback, and an equal revision
   with different content is a collision. A corrupt, truncated, incompatible,
   inconsistent, rolled-back, or colliding publish never replaces the cache.
   Refresh failure is logged rather than raised; it must not become a startup
   failure or stop an instance being created.

8. **Language catalogs are Mojang's, not Ephervex's.** The set of languages a
   release offers comes from that release's own `pack.mcmeta` asset object and
   is cached per release, exactly like the version manifest. It is not bundled
   and not in the feed: duplicating it would create a second authority for
   something Mojang owns. Creation and import only consult an already-cached
   catalog. When none is available the language preference is simply not
   applied, because claiming support Ephervex cannot verify writes a code the
   game silently falls back from.

## Consequences

- A typical Minecraft hotfix needs a regenerated feed and no build. A release
  whose representation the shipped codecs cannot express, or a new user-facing
  preference, still needs one.
- The historical client JARs are needed only to establish a release the first
  time. The committed dataset carries the conclusions, so regenerating for a new
  release does not require re-analyzing the old ones.
- A setting discovered after an older release was analyzed stays *unknown* for
  that older release rather than being retroactively declared unsupported. That
  is the honest state and is never written.
- Applying a preference can now fail per release for a reason other than "no
  such setting": a value outside the release's proven domain is reported and
  skipped rather than clamped, because a clamp writes a setting nobody chose.
- The hand-written era adapters are gone. There is one authority for how a
  release stores a setting, and it is the generated dataset.
