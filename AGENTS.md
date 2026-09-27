# Ephervex contributor instructions

These instructions apply to the entire repository unless a more specific `AGENTS.md` in a subdirectory overrides them.

## Product and architecture

- Ephervex is a Linux-first Minecraft launcher and developer tool for disposable bug-reproduction environments.
- Keep application logic independent of the frontend. GUI and CLI code should call `ephervex-core` instead of reaching through it into lower-level crates.
- Keep normalized pack and instance models provider-neutral. Provider-specific identifiers and metadata belong only in explicit provider/source fields.
- Preserve exact user-selected Minecraft, loader, and pack versions. Never silently upgrade, downgrade, substitute, or approximate an unsupported selection.
- Shared artifacts such as Minecraft assets, libraries, game JARs, managed runtimes, and verified downloads belong in shared stores rather than being duplicated into disposable instances unless the design explicitly requires an instance-local copy.
- Treat imported archives, metadata, URLs, filenames, paths, and launcher exports as untrusted input.

## Durable naming and documentation

- Name files, modules, types, functions, tests, fixtures, and commands after the behavior, domain concept, invariant, or feature they represent.
- Do not encode implementation chronology, milestones, sequencing, temporary work ownership, or development-session provenance into tracked source code or documentation.
- Do not use numbered implementation-stage names in filenames, test names, symbols, comments, doc comments, fixture names, environment variables, operation labels, or user-visible text.
- Do not mention coding agents, chat sessions, temporary checkpoints, handoffs, or similar development-process details in tracked project content.
- Comments and doc comments must explain durable intent, non-obvious behavior, invariants, compatibility constraints, or security reasoning. Delete comments that only narrate when or why a developer happened to add the code.
- When a durable project decision needs a citation, reference the relevant ADR. When behavior follows an external format or service, reference the upstream specification or documented contract.
- ADRs describe lasting decisions and rationale. Keep their status and wording independent of implementation chronology.
- Documentation should describe the current architecture, supported behavior, known limitations, and verification procedures rather than the order in which features were built.

## User-facing text

- Never expose internal engineering references, implementation chronology, source-code locations, or planning terminology in CLI help, GUI copy, errors, logs intended for users, or localization strings.
- Remember that Rust doc comments on `clap` commands and fields can become public `--help` output. Write them as product documentation for users.
- User-facing errors should be actionable and should not leak secrets, credentials, access tokens, authorization headers, or sensitive query parameters.
- Technical values such as provider names, paths, versions, hashes, and upstream identifiers should remain exact when displayed.

## User interface

- The desktop frontend's design tokens live in the `Theme` global. Colors and text sizes are named there and referenced by name everywhere else.
- Color values are written only in the built-in palettes in `palettes.slint`. Each `Theme` color token reads the active palette's field of the same name. Components never read the palette or branch on which one is active.
- Never write a literal color in markup. No `#rrggbb`, `#rrggbbaa`, `rgb()`, `rgba()`, `hsv()`, or `oklch()` outside the built-in palettes. `transparent` is not a color literal. Deriving from a token with `.with-alpha()`, `.darker()`, or `.brighter()` is fine; inventing a value is not.
- Never write a literal font size in markup, so interface text follows the text size the user chose. The separate size the diagnostics and console views use is the one exception, and it is read from a property rather than written out.
- Every built-in palette sets every color token. Slint fills a field a palette leaves out with transparent, so a missing one still compiles.
- Palettes hold colors only. Text sizes, the monospace family, and shared measurements are never a palette's to change.
- Palette token names are part of the user theme file format (`docs/themes.md`). Add tokens freely, but never rename or remove one, and document a new one there.
- Keep the shipped widgets in step: the std-widget palette's color scheme is written from the active palette's `widget-scheme`, so a widget never sits light on a dark page.
- A measurement several unrelated places must agree on, such as a control height, a navigation column width, or a fixed row height that scroll arithmetic depends on, belongs in `Theme`. Ordinary padding and spacing does not.
- A new token belongs in `Theme` with a comment saying what it is for, not inline at its first use. A new color token is also a `ThemePalette` field that every built-in palette sets.

## Language and spelling

- Use US English consistently in source code, identifiers, filenames, tests, comments, documentation, commit messages, and English localization text.
- Prefer spellings such as `behavior`, `color`, `favor`, `initialize`, `optimize`, `normalize`, `serialize`, `canceled`, `canceling`, `labeled`, `license`, and `center`.
- Do not mechanically alter third-party API names, dependency methods, protocol fields, quoted upstream text, fixture payloads, URLs, hashes, or identifiers whose spelling is defined externally. Keep exact upstream spellings at integration boundaries.

### Writing style

- Use US English in all Ephervex-owned content.
- Prefer plain technical-software punctuation and concise, natural wording.
- Use straight ASCII quotes (`"` and `'`), not typographic/smart quotes.
- Use three ASCII periods (`...`), not the Unicode ellipsis (`…`).
- Avoid em dashes (`—`) and similarly editorial punctuation when a comma,
  colon, parentheses, or separate sentence reads naturally.
- Avoid marketing-like, overly polished, or AI-characteristic prose. Write like
  a concise developer tool.
- Preserve exact upstream, protocol, API, filesystem, and user-provided text
  even when it does not follow these conventions.

## Rust and API quality

- Prefer typed domain models over loosely related strings and booleans when invalid combinations can otherwise be represented.
- Use `Result` and typed errors for recoverable failures. Do not add production `unwrap`, `expect`, `panic!`, `todo!`, or `dbg!` calls without a compelling invariant and an explicit justification.
- Preserve error context without exposing secrets. Keep user-facing localization separate from low-level technical diagnostics.
- Keep public APIs minimal. Do not make an item public solely to make a test easier.
- Avoid duplicating security, path-validation, version-selection, or compatibility logic across crates. Reuse the owning crate's implementation.
- Do not weaken a production safety rule to make a test pass. If tests require a special environment such as loopback HTTP, use an explicit test-only or narrowly scoped escape hatch whose production default remains strict.

## JSON serialization

- Keep machine-owned JSON compact. Production code must not pretty-print JSON that Ephervex writes for its own runtime, cache, metadata, state, registry, recipe, index, or other persistent machine-consumed storage. Use compact serializers such as `serde_json::to_writer`, `to_vec`, or `to_string`, not their pretty-printing equivalents.
- Pretty-printed JSON is acceptable only when human readability is the purpose of the file and storage efficiency is irrelevant, such as checked-in fixtures, test data, documentation examples, or explicitly human-facing exports.
- Do not reformat externally supplied JSON merely for storage. Preserve the original source when source retention requires it; otherwise normalize to the smallest appropriate machine representation.
- Whitespace must never be relied on for JSON identity or cache semantics.

## Import and filesystem safety

- Archive extraction must reject path traversal, absolute paths, unsafe prefixes, escapes through links, and writes into launcher-owned metadata areas.
- Enforce archive entry-count and uncompressed-size limits before untrusted data can exhaust disk or memory.
- Never execute imported pre-launch commands, post-exit commands, wrapper commands, arbitrary Java paths, or other executable customization from untrusted packs.
- Verify downloadable artifacts with the strongest hashes supplied by the source. A hash mismatch is a hard failure; never install the mismatching file.
- Keep production downloads on approved secure transports and hosts. Do not bypass third-party distribution restrictions.
- Write incomplete downloads and installations to staging locations and publish them atomically where practical.

## Tests and fixtures

- Test names must state the behavior or invariant under test. A reader should understand the purpose without consulting project history.
- Integration-test filenames must describe the covered subsystem or workflow, not when the test was introduced.
- Ordinary test runs must not depend on live Mojang, Microsoft, Modrinth, CurseForge, Forge, NeoForge, or other production services. Use fixtures and loopback servers.
- Keep real-world/manual smoke tests opt-in when they download or execute production artifacts.
- Fixtures should be minimal, deterministic, reviewable, and documented with durable provenance in `SOURCES.md` where appropriate.
- Do not derive general format behavior from one real-world sample. Implement documented formats and use samples only as validation.
- When fixing a bug, add or strengthen a regression test when practical.

## Validation

Before declaring repository changes complete, run the checks relevant to the touched code. For normal Rust changes, the expected baseline is:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

If CLI definitions or help text changed, also inspect the affected help output, including the top-level command when relevant:

```bash
cargo run -p ephervex-cli -- --help
```

If a required manual acceptance step cannot run because of an external environment limitation, report the exact blocker. Do not reinterpret an implementation failure as an environment limitation.

## Git hygiene

- Keep the working tree understandable and intentional. Do not commit scratch files, temporary checkpoints, generated debugging notes, or agent handoff documents.
- Do not create, rename, or delete branches, rewrite existing commits, force-push, or otherwise alter published history unless the user explicitly asks for that operation.
- Preserve unrelated user changes.
- Commit messages must describe the delivered feature or fix in durable product terms, not implementation chronology or agent activity.
- Write one line and stop. A commit message is its subject: no body, no explanation of the reasoning, no summary of what changed. The diff says what changed and the code's own comments say why.
- A commit produced with the help of an AI model must carry a `Co-Authored-By:` trailer naming that model, after one blank line. This holds for every model and every tool that drives one, including those whose own defaults leave the trailer out: add it without being asked. Name the model and use its vendor's no-reply address, in the form this repository's history already uses, such as `Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>` or `Co-Authored-By: Codex <codex@openai.com>`.
- That trailer is the only thing that may follow the subject. Never add a session, chat, transcript, or agent link of any kind, in any trailer or footer.
- Before finishing a change, check `git status` and report any intentionally uncommitted files.
- Use Conventional Commit-style subjects where they fit naturally, such as `feat:`, `fix:`, `refactor:`, `docs:`, `test:`, `chore:`, or `ci:`.
- Do not use implementation-stage numbers, planning references, WIP/checkpoint wording, session provenance, or temporary milestone names in commit subjects or bodies.
- Commit a change as soon as that change is finished, with its tests, its documentation, and the checks relevant to it. Finished work is not carried forward uncommitted to be sorted out later.
- Do not build several changes at once and divide the result into commits afterwards. A commit assembled that way was never verified on its own, and changes that grew together share lines and cannot be separated cleanly.
- Preserve meaningful feature boundaries in history. Do not squash unrelated changes together merely to make the history shorter.
