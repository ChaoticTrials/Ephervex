# Documentation website TODO

Open work for the website and the screenshot tool. Remove an item when it is
done. Remove this file when the list is empty.

Sources: the GUI in `crates/gui/ui`, the CLI in `crates/cli/src/main.rs`,
`docs/`, the ADRs, and the issue tracker. Code is the source of truth where
the README disagrees (see "Known stale sources").

## 1. Site setup

- [ ] Replace the template placeholders in `docusaurus.config.ts`: title,
      tagline, `url`, `organizationName`, `projectName`, `editUrl`, navbar,
      footer, copyright, favicon, logo, social card.
- [ ] Remove the template content: `docs/tutorial-basics`,
      `docs/tutorial-extras`, `blog/`, `src/pages/markdown-page.mdx`, the
      homepage features, and `docs/intro.mdx`.
- [ ] Decide hosting, the production URL, and whether docs are versioned.
      `DOCUSAURUS_BASE_URL` already supports a subpath.
- [ ] Add a notice that Ephervex is not affiliated with or endorsed by Mojang
      or Microsoft.

## 2. Fixture corrections (`tools/auto-screenshot-tool/src/fixtures.rs`)

- [ ] `InstanceRow.source` must use the strings the app produces: `Manual`,
      `Modrinth`, `CurseForge`, `Prism`, `Server pack`. The current values
      render as "Modrinth pack 1.1.0 instance".
- [ ] Set `HostView.browser_next_offset` to -1 in `client_mods`. It removes
      the stray "Load More" button.
- [ ] Set `browser_provider`, browser results, `storage_root`, `mods_query`
      and `allow_pack_mod_changes` so the Mods surface looks populated.
- [ ] Replace the account names `_jeb` and `Dinnerbone`, which are real
      developer handles, with synthetic names. Keep names to 3-16 characters
      from `[A-Za-z0-9_]`.
- [ ] Check the capitalization of `AccountRow.kind` and `credential_state`
      against a live run (the app uses lower-case kinds and the credential
      strings "No credential", "Credential stored securely" and "Sign-in
      required").
- [ ] Check the toggle knob rendering at 2560x1440. If it is missing, step the
      mocked animation clock before the snapshot.
- [ ] Fixture `loader` is hardcoded to Forge. Derive it from the loader kind.

## 3. Fixture breadth

Keep values within what the product supports: Vanilla, Forge and NeoForge,
release versions 1.12.2 and newer. Never put an unreleased or unsupported
Minecraft version in a version field. Names and descriptions may use
Minecraft-flavored text. Do not use Mojang or Minecraft logos, textures, skins
or other artwork. Pack content beyond the documented CaveStone pack is
synthetic and recorded as such in `SOURCES.md`.

- [ ] Instances (6-8): Ready, Starting, Running (with Kill), needs attention,
      creating a linked server, restarting. Sources: Manual, Modrinth,
      CurseForge, Server pack. Include a 1.12.2 and a 26.x instance.
- [ ] Servers (4): stopped, starting, running, failed. Optionally one linked
      server with the linked-servers setting on.
- [ ] Accounts (4): two offline, one Microsoft with a stored credential, one
      "Sign-in required".
- [ ] Mods (8-10): mixed origin (pack, added, dependency, hand-placed), mixed
      provider, one disabled, one deleted, one crash suspect, one with an
      unknown version.
- [ ] Catalog browser results (4): one installed, one queued.
- [ ] Java runtimes: 8, 17, 21 and 25, one of them managed.
- [ ] Activity: running, queued, completed, failed and canceled rows.
- [ ] Running page: one client, one server.
- [ ] Easter eggs (names and text only): "Creeper Containment Lab", "Herobrine
      Sighting #1337" (needs attention), "Redstone Clock Debugging", "Spawn
      Chunks Server", "Totem of Undying QA", accounts `Steve`, `Alex`,
      `Herobrine`, `Enderman_QA`, snapshot "Before the Wither", crash text
      "Creeper Confetti tried to explode confetti twice". Mod names must not
      imitate real projects.
- [ ] Update the account test in `fixtures.rs` for the new rows.

## 4. New screenshot states (`catalog.rs`)

Fixture-only unless noted. Region crops of dialogs, pending cards, activity
cards and toasts need either an `accessible-label` on that element or a
geometry-based crop in the tool.

Getting started:
- [ ] `instances-empty` (no instances, New Instance and Import Modpack)
- [ ] `create-instance-dialog`
- [ ] `import-modpack-ready` (pack card, optional files, blocked files,
      duplicates)
- [ ] `import-modpack-blocked-files`
- [ ] `import-modpack-credential-missing` (error plus Providers & APIs link)
- [ ] `import-modpack-resolving` (frozen spinner)

In-progress work (the headless renderer uses mock time, so spinners and bars
are deterministic):
- [ ] `import-modpack-running` (pending card, 57% bar, stage and item count)
- [ ] `activity-import-in-progress`
- [ ] `instance-launching` (Starting state plus a Launching Minecraft row)
- [ ] `server-starting`, `server-running`
- [ ] `diagnostics-working`
- [ ] `accounts-device-code` (the only place the sign-in code is shown is a
      toast)
- [ ] `toasts-success-and-error`

Mods:
- [ ] `client-mods` rebuilt per section 2
- [ ] `mod-manager` (versions with Installed and Newest pills, Requires and
      Required by)
- [ ] `mod-impact` (breaks, needs, cascade)
- [ ] `mod-install-plan` (with linked servers)
- [ ] `mod-isolation-setup`, `mod-isolation-round`

Hosts:
- [ ] `client-overview-linked-server` (Paired pill, Start together)
- [ ] `client-settings`, `snapshots`, `snapshot-restore-dialog`
- [ ] `client-logs-live`, `client-crash-reports`
- [ ] `client-diagnostics-result`, `client-diagnostics-fix-preview`
- [ ] `client-run-failure-notice`
- [ ] `server-console`, `server-properties` (EULA card), `server-crash-notice`
- [ ] `split-workspace` (client logs beside server console)
- [ ] `running-page`, `activity-page`

Settings:
- [ ] `settings-general` (theme, text sizes, trusted hosts)
- [ ] `settings-minecraft-defaults`
- [ ] `settings-java`
- [ ] `settings-providers` (catalog, CurseForge key states)
- [ ] `settings-storage`, `settings-disk-usage`
- [ ] `about-licenses`

Not capturable without extending the tool (an interaction step): hover
tooltips and disabled-button reasons, context menus, open dropdowns, the
workspace "More" popup, the diagnostic bundle panel (folded by default),
`WindowSizePreview`. Do not add product UI just to make a screenshot.

## 5. Documentation pages

Write only what ships. Each task page uses `Tabs` with `groupId="interface"`
and `queryString="interface"` (GUI first, then CLI). GUI-only and CLI-only
features show one tab or a short note.

- [ ] Introduction: what Ephervex is, supported scope, limitations
- [ ] Install: AppImage, .deb, .rpm, FUSE, `ephervex doctor` (source:
      `docs/release.md`)
- [ ] Interface tour (replaces `interface-screenshots.mdx`)
- [ ] Accounts: offline and Microsoft, device code, session-only, keyring
- [ ] Instances: temporary vs persistent, create, launch, duplicate, make
      persistent, delete, per-instance Java and memory, change loader build
- [ ] Import: Modrinth and CurseForge, optional files, `ephervex://` links,
      trusted hosts, blocked files and watched folders, CurseForge key
- [ ] Mods: browse and install, enable and disable, remove, change version,
      add your own JARs, dependencies, verify and repair
- [ ] Mod isolation (source: `docs/mod-isolation.md`)
- [ ] Servers: create (standalone and linked), EULA, properties and network,
      start, stop, console, Launch Client & Connect, recovery
- [ ] Diagnostics, logs and crash reports, diagnostic bundles
- [ ] Snapshots
- [ ] Settings: General, Minecraft defaults, Java, Providers & APIs, Storage,
      Disk usage
- [ ] Themes (source: `docs/themes.md`)
- [ ] Files and locations, environment variables, working offline,
      credentials and privacy, upgrade and uninstall
- [ ] Limitations and troubleshooting

## 6. CLI

- [ ] Hand-written CLI tabs on the task pages, using real flags and noting
      confirmation prompts and exit behavior.
- [ ] Generated CLI reference under `docs/reference/cli/`, one page per
      top-level command, produced from `ephervex --help` and
      `ephervex help <path>`. A script runs before the build and a check fails
      when the committed output is stale, as for the screenshots.
- [ ] Hold the `--json` shape and exit-code reference until the open CLI
      output fixes land (#271, #272, #274, #275, #277, #294).
- [ ] CLI-only features that need a mention on their own pages: `server
      recover`, `java select`, `instance recipe`, `url-handler`, `doctor`,
      `completions`.

## 7. Known stale sources

- The README says CurseForge mod browsing is not available. It is: the mod
  browser has a Modrinth/CurseForge switch, and `ephervex mod search
  --provider curseforge` exists. It also says updates are unavailable, but a
  per-mod version change and removal exist. Bulk update-all does not.
- Fabric and Quilt are not supported. Prism/MultiMC exports are recognized
  and refused.
- The desktop UI is `en-US` only.

## 8. Wait for planned changes before documenting

These change screens that exist today. Write the surrounding pages so they are
easy to update, and capture screenshots late.

- #9 instance search, filters, sort and favorites
- #11 Java runtime visibility and compatibility checks
- #12 server status and configuration shortcuts
- #13 navigation and active-state polish
- #28 installing server mods from Modrinth and CurseForge
- #378 sandboxed branding assets and theme packages, #379 the Haunted Pumpkin
  theme (avoid pinning theme-list screenshots)
- #17 resource packs, shader packs and datapacks during import
- #10 cache and storage maintenance
- #341 clients and servers surviving an Ephervex restart
- #261 tooltips and disabled-button explanations, #257 keyboard reachability

## 9. Limitations the docs must state

- Linux only. Vanilla, Forge and NeoForge only. Release versions 1.12.2 and
  newer. No snapshots, Fabric, Quilt, Windows or macOS.
- CurseForge pack import needs an API key. Files whose authors disable
  third-party downloads cannot be fetched automatically.
- Resource packs, shader packs and datapacks in imported packs are not yet
  preserved (#17).
- Quitting Ephervex stops running clients and servers (#341).
- Do not run concurrent mod changes on one host, or mix mod edits with an
  active isolation session (#232, #208).
- Session-only sign-in drops its credential. A Secret Service outage can show
  "not signed in".
- AppImage needs FUSE or `APPIMAGE_EXTRACT_AND_RUN=1`. glibc 2.35 baseline.
  Wayland sessions without XWayland are untested, and file drag and drop
  needs X11 or XWayland.
- Linux ARM refuses some Minecraft versions that need x86-only natives.

## 10. Verification before merging

- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] `tools/licenses/generate.sh --check`
- [ ] `cd web && npm run screenshots:check && npm run typecheck && npm run build`
- [ ] Regenerate screenshots, run `npm run screenshots:sync`, and commit the
      WebP assets and manifest.
