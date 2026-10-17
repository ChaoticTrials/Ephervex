# ADR 0051 - Server property defaults fill a new server's file

**Status:** Accepted

## Context

A developer who always wants the same world settings, such as a flat world,
peaceful difficulty or a shorter view distance, had to set them on every new
server. Ephervex already keeps defaults for new instances' Minecraft options
and for a new server's network and operator setup, but nothing for the rest of
`server.properties`.

`server.properties` is the server's own file. A server that exists may have
been tuned by hand or by the game, and a pack's published server files can
carry a file the pack author chose.

## Decision

**Defaults are a sparse set of typed catalog values.** Settings has a Server
Properties page with the same grouped controls as a server's Properties tab.
A key either has a default or does not; one without leaves the server to its
own vanilla default. Only keys of the typed catalog (`property_catalog.rs`)
can have one, each checked as a server's own value is, and the typed network
settings keep their own defaults.

**They fill a new server's file once.** When a server is created, or a
scaffolded server is installed, every default whose key the new
`server.properties` does not hold is written into it. A value the file
already has, such as one from a pack's server files, stays. Servers that exist
are never changed when a default changes, which is how Minecraft option
defaults treat existing instances.

**A server shows its own file.** A server's Properties tab shows what its
file holds, and for a key it lacks, the value its release falls back to. That
is the vanilla default, not the global one, because the server never received
it.

## Consequences

Defaults are stored as `[server_defaults.properties]` in the configuration
file. A choice such as difficulty is written as its position, which every
release reads. A stored default this build cannot vouch for, such as one a
newer build wrote for a key this one does not know, stays stored and is left
out of new servers rather than failing their creation, and saving the other
defaults keeps it. A default for a key a server's release does not read is written and
ignored by that release, which is harmless. `level-type` is free text, so a
default written for one release's format reaches older releases unchanged.
