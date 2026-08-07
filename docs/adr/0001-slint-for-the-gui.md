# ADR 0001 — Slint for the GUI

**Status:** Accepted

## Context

Ephervex is a native Rust desktop application for Linux. Its UI is dense and
developer-oriented: instance lists, a mod browser, a live server console, log
viewers. None of that needs a browser engine or a web runtime.

The obvious alternatives were Tauri (web frontend in a system webview), egui
(immediate-mode), and GTK/Qt bindings.

## Decision

Use Slint for the GUI, in the `ephervex-gui` crate.

All launcher logic stays outside `.slint` files. The GUI crate depends only on
`ephervex-core`, and long operations reach it as `Operation` progress events
rather than as blocking calls.

Tauri is explicitly rejected unless a future requirement genuinely needs a web
frontend.

## Consequences

- The GUI must be both Wayland- and X11-capable. Backend selection is Slint's
  concern; ours is never to assume one.
- No blocking work on the UI thread. Anything touching network or filesystem
  goes through `ephervex-core` on a Tokio runtime and reports back as events.
- Slint's licensing terms need a review before any public distribution — it is
  not a plain permissive license. Deferred alongside ADR 0007.
- A live server console and log viewer need efficient handling of continuously
  appended text. Slint's model types are expected to cover this; if they do not,
  that limitation surfaces and gets its own superseding ADR.

## Verification

Slint **1.17.1** was verified on x86_64 Linux with Rust 1.97.1, X11,
fontconfig, and xkbcommon. The `ephervex-gui` crate keeps the dependency
load-bearing so toolchain and backend regressions surface in ordinary builds.
