# Ephervex — English (United States) source resource.
#
# This file is the fallback language: every message id used anywhere in the
# application must exist here, and `fl!()` checks that at compile time.
#
# Rules for anything added below:
#   * Provider names (Modrinth, CurseForge, Forge, NeoForge, Prism), technical
#     ids, filesystem paths, version strings, and Minecraft log text stay
#     untranslated. They are passed in as arguments, never baked into a message.
#   * Message ids are stable. Rewording English is free; renaming an id breaks
#     every other locale, so treat renames as deletions plus additions.
#   * Errors get a concise summary and, where one exists, an actionable next
#     step as a `.action` attribute.

## Application identity

app-name = Ephervex
app-tagline = Disposable Minecraft instances for mod development and bug reproduction

## Languages
#
# Language names are written in their own language so a user who cannot read
# the current interface language can still find their own.

language-en-US = English (United States)

## Storage classes 

storage-class-temporary = Temporary
storage-class-persistent = Persistent
storage-class-temporary-description = Lives under the system temporary directory. The operating system decides when it is cleaned up; Ephervex never deletes it on its own.
storage-class-persistent-description = Lives under the configured instance directory and survives reboots until you delete it.

## Instance and server listings

instances-list-empty = No instances yet.
servers-list-empty = No servers yet.

instances-list-count = { $count ->
        [one] { $count } instance
       *[other] { $count } instances
    }

servers-list-count = { $count ->
        [one] { $count } server
       *[other] { $count } servers
    }

# $name is user-authored and $version is a Minecraft version string; neither is
# translated.
instance-summary = { $name } — Minecraft { $version }, { $loader }

## Startup reconciliation (ADR 0006)
#
# A vanished temporary root is the expected result of the operating system
# clearing its temporary directory. It is reported as information, never as an
# error. A vanished persistent root is the opposite and keeps a warning.

reconcile-removed-stale-temporary = { $count ->
        [one] Removed { $count } temporary instance whose directory no longer exists.
       *[other] Removed { $count } temporary instances whose directories no longer exist.
    }

reconcile-missing-persistent = { $count ->
        [one] { $count } persistent instance is missing its directory.
       *[other] { $count } persistent instances are missing their directories.
    }
.action = Ephervex kept the record so you can restore the directory from a backup. Delete the instance explicitly if it is gone for good.

reconcile-reindexed = { $count ->
        [one] Recovered { $count } instance found on disk but missing from the index.
       *[other] Recovered { $count } instances found on disk but missing from the index.
    }

reconcile-nothing-to-do = Everything in the index matches what is on disk.

# $name is user-authored, $path is a filesystem path. Untranslated.
reconcile-persistent-root-missing-detail = { $name } expected its files at { $path }.

## Attention reasons 
#
# Ephervex never presents an instance as faithfully runnable when it is not.

attention-missing-root = The instance directory is missing.
attention-unsupported-loader = { $loader } is not a supported mod loader.
attention-unsupported-launch-customization = The source used launch settings Ephervex cannot reproduce: { $detail }
attention-ignored-unsafe-customization = An imported command or hook was found and deliberately not run: { $detail }
attention-best-effort-server-conversion = This server was generated from a client instance and may still contain client-only mods.
attention-manual-download-required = Some files must be downloaded manually: { $detail }

## Configuration errors 

error-config-unreadable = The configuration file could not be read.
.action = Check that { $path } exists and is readable, or delete it to start from defaults.

error-config-invalid = The configuration file is not valid TOML.
.action = Fix the syntax at { $path }, or delete the file to start from defaults.

error-config-unwritable = The configuration file could not be saved.
.action = Check that { $path } is writable and that the filesystem is not full.

error-config-unsupported-schema = This configuration was written by a newer version of { app-name }.
.action = Update { app-name }, or move { $path } aside to start from defaults. Ephervex will not guess at a schema it does not know.

# ADR 0004: secrets belong in the desktop keyring, never in a file that users
# routinely attach to bug reports.
error-config-secret-rejected = Refusing to save a credential into the configuration file.
.action = The key { $key } looks like a secret. Secrets are stored in the system keyring instead, and configuration only records that one exists.

## Path resolution errors (ADR 0003)

error-paths-no-home-directory = Ephervex could not work out where your home directory is.
.action = Set HOME, or set the XDG base directory variables explicitly.

error-paths-directory-creation = A required directory could not be created.
.action = Check permissions on { $path }.

## Index and migration errors 

error-index-unopenable = The launcher index database could not be opened.
.action = Check permissions on { $path }. Instances are recoverable without it: their own metadata files are the source of truth.

error-index-query-failed = The launcher index database reported an error.

error-index-migration-failed = The launcher index database could not be upgraded to the current schema.
.action = Move { $path } aside and restart. Ephervex rebuilds the index by scanning your persistent instances.

error-index-schema-from-future = The launcher index database was written by a newer version of { app-name }.
.action = Update { app-name }. Ephervex will not downgrade a schema it does not understand.

## Registry errors 

error-registry-instance-unknown = No such instance.
error-registry-server-unknown = No such server.

error-registry-metadata-unwritable = The instance metadata file could not be written.
.action = Check that { $path } is writable. Ephervex writes this file before touching the index so a crash leaves a recoverable instance rather than a dangling record.

error-registry-metadata-unreadable = The instance metadata file could not be read.

error-registry-metadata-invalid = The instance metadata file is not valid Ephervex metadata.

error-registry-metadata-unsupported-version = The instance metadata at { $path } was written by a newer version of { app-name }.

error-registry-root-occupied = That instance directory already contains files.
    .action = Choose a different name, or remove { $path } first.

## Minecraft version metadata 
#
# Version ids, release channels, and file paths are never translated; they are
# passed in as arguments.

minecraft-versions-list-empty = No supported Minecraft releases were found.

minecraft-versions-list-count = { $count ->
        [one] { $count } supported release
       *[other] { $count } supported releases
    }

# Shown alongside a version list that could not be refreshed, so the user knows
# what they are looking at rather than watching a spinner.
minecraft-versions-stale = Showing the last version list Ephervex was able to fetch. Mojang could not be reached.

minecraft-version-id-unrecognized = { $version } uses a version-numbering scheme this build does not know. It is listed because Mojang published it after { $minimum }.

## Minecraft metadata errors 

error-minecraft-manifest-unavailable = The Minecraft version list could not be fetched, and no cached copy is available.
    .action = Check your network connection and try again. Instances that are already installed can still be launched.

error-minecraft-manifest-invalid = Mojang returned something that is not a Minecraft version manifest.
    .action = This is usually a captive portal or a proxy answering instead of Mojang. Try again once you have real internet access.

error-minecraft-version-unknown = That Minecraft version is not in Mojang's version list.
    .action = Check the version id. Only released versions appear in the list.

error-minecraft-version-unsupported = That Minecraft version is older than Ephervex supports.
    .action = Ephervex supports Minecraft releases from 1.12.2 onward.

error-minecraft-version-not-a-release = That is a snapshot, pre-release, or release candidate rather than a Minecraft release.
    .action = Ephervex deliberately offers only released versions, so that a reproduction can be repeated later against the same build.

error-minecraft-version-metadata-unavailable = The metadata for that Minecraft version could not be fetched.
    .action = Check your network connection and try again.

# Verifying the SHA-1 Mojang publishes for each version document is a release
# blocker, not a nicety .
error-minecraft-version-metadata-corrupt = The metadata Mojang served for that version did not match the checksum Mojang published for it.
    .action = Ephervex refused the file rather than using it. If this repeats, something between you and Mojang is rewriting the response.

error-minecraft-version-metadata-invalid = The metadata for that Minecraft version could not be understood.

error-minecraft-version-metadata-unreadable = The cached metadata for that Minecraft version could not be read.
    .action = Check permissions on { $path }, or delete the file so Ephervex fetches it again.

error-minecraft-metadata-incomplete = That version's metadata is missing something a launch needs.
    .action = Mod-loader version files extend a vanilla version rather than standing alone. Ephervex cannot resolve one on its own yet.

error-minecraft-metadata-url-invalid = Mojang's metadata contains a download address Ephervex will not use.
    .action = Ephervex only downloads over HTTPS and never from an address that embeds credentials.

error-minecraft-cache-key-invalid = A download could not be given a safe location in the cache.

## Java runtime errors 

# A missing compatible runtime leads to a compatible installation or a
# managed download, never a near-miss the launcher picked on the user's behalf.
error-java-no-compatible-runtime = None of the Java runtimes on this computer can run that Minecraft version.
    .action = Let Ephervex download a compatible Java runtime, or install one yourself and refresh the list.

error-java-pinned-incompatible = The Java runtime chosen for this instance cannot run that Minecraft version.
    .action = Clear the Java override so Ephervex picks a compatible runtime, or point it at one that matches.

error-java-runtime-unusable = That path is not a usable Java runtime.
    .action = Point Ephervex at the `java` program itself, usually inside a `bin` folder.

error-java-auto-detect-disabled = Automatic Java detection is turned off and no Java path is set.
    .action = Turn automatic detection back on in Settings, or set a preferred Java path.

error-java-managed-download-disabled = No installed Java runtime is compatible, and automatic Java downloads are turned off.
    .action = Turn on "Auto-download compatible Java" in Settings, or install a matching Java runtime yourself.

error-java-managed-arch-unsupported = No Java runtime is published for this computer's processor architecture.
    .action = Install a Java runtime through your distribution's package manager instead.

error-java-managed-api-unusable = The Java download service address is not usable.

error-java-managed-index-unavailable = The list of downloadable Java runtimes could not be fetched.
    .action = Check your network connection and try again. Java runtimes already installed still work.

error-java-managed-index-invalid = The Java download service returned something Ephervex could not understand.

error-java-managed-release-missing = The Java download service publishes no build of that Java version for this computer.

error-java-managed-release-malformed = The published Java build is missing a download address or a checksum.
    .action = Ephervex refuses to install a Java runtime it cannot verify.

error-java-managed-download-failed = The Java runtime could not be downloaded.
    .action = Check your network connection and try again.

# Extraction limits and traversal checks are security boundaries, and
# a refusal has to say that something was actually wrong with the archive.
error-java-archive-unsafe = The downloaded Java archive tried to write files outside its own folder.
    .action = Ephervex refused the archive and installed nothing. If this repeats, something between you and the download service is altering it.

error-java-archive-too-large = The downloaded Java archive is larger than Ephervex is willing to unpack.

error-java-archive-unreadable = The downloaded Java archive could not be read.

error-java-archive-unwritable = A file could not be written while unpacking the Java runtime.
    .action = Check free space and permissions on { $path }.

error-java-archive-empty = The downloaded Java archive contained nothing Ephervex could install.

error-java-managed-extraction-failed = Unpacking the Java runtime did not finish.

error-java-managed-runtime-unusable = The downloaded Java runtime could not be started.
    .action = Ephervex did not use it. Try again, or install a Java runtime through your distribution's package manager.

error-java-managed-cache-key-invalid = The Java runtime could not be given a safe location in the cache.

error-java-io = A file needed for the Java runtime could not be written.
    .action = Check free space and permissions on { $path }.

error-java-probe-not-executable = That Java program could not be started.

error-java-probe-unintelligible = That program did not report a Java version.

error-java-probe-timed-out = That Java program did not respond in time.

## Java runtime output 

java-runtimes-list-empty = No Java runtimes were found on this computer.

java-runtimes-list-count = { $count ->
        [one] { $count } Java runtime
       *[other] { $count } Java runtimes
    }

java-runtime-probe-failed = { $path } could not be identified as a Java runtime.
