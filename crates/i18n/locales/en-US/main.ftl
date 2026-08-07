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
