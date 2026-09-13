-- Migration 1 - instance and server index (ADR 0002).
--
-- This table is an *index*, not the source of truth. Everything here is also
-- written to the instance's own `.launcher/*.json`, which is what makes a lost
-- database a rebuild rather than a data loss.
--
-- Enums are stored as stable lowercase text rather than as integers: a text
-- value survives being read by `sqlite3` in a bug report, and reordering a Rust
-- enum can never silently change what a row means.

CREATE TABLE instances (
    id                 TEXT    PRIMARY KEY NOT NULL,
    name               TEXT    NOT NULL,
    -- Absolute path. Reconciliation checks this on every start.
    root               TEXT    NOT NULL,
    -- 'temporary' | 'persistent'
    storage_class      TEXT    NOT NULL,
    minecraft_version  TEXT    NOT NULL,
    -- 'vanilla' | 'forge' | 'neoforge'
    loader_kind        TEXT    NOT NULL,
    -- NULL only for vanilla.
    loader_version     TEXT,
    -- 'manual' | 'modrinth_pack' | 'curseforge_pack' | 'prism_export'
    source_kind        TEXT    NOT NULL,
    source_reference   TEXT,
    -- JSON: Option<JavaOverride>, Option<JvmSettings>, Vec<AttentionReason>.
    -- Kept as JSON because the launcher only ever reads them whole, and
    -- columns for them would be a schema change per settings addition.
    java_override      TEXT,
    jvm_override       TEXT,
    attention          TEXT    NOT NULL DEFAULT '[]',
    -- RFC 3339, UTC.
    created_at         TEXT    NOT NULL,
    updated_at         TEXT    NOT NULL,
    last_launched_at   TEXT
);

CREATE INDEX instances_storage_class ON instances (storage_class);

CREATE TABLE servers (
    id                 TEXT    PRIMARY KEY NOT NULL,
    name               TEXT    NOT NULL,
    root               TEXT    NOT NULL,
    storage_class      TEXT    NOT NULL,
    -- Several servers may link to one client. ON DELETE SET NULL
    -- rather than CASCADE: deleting a client must not silently take a server
    -- the user still wants with it.
    linked_client_id   TEXT    REFERENCES instances (id) ON DELETE SET NULL,
    minecraft_version  TEXT    NOT NULL,
    loader_kind        TEXT    NOT NULL,
    loader_version     TEXT,
    -- JSON: ServerNetworkSettings.
    network            TEXT    NOT NULL,
    -- Never set on the user's behalf.
    eula_accepted      INTEGER NOT NULL DEFAULT 0,
    source_kind        TEXT    NOT NULL,
    source_reference   TEXT,
    java_override      TEXT,
    jvm_override       TEXT,
    attention          TEXT    NOT NULL DEFAULT '[]',
    created_at         TEXT    NOT NULL,
    updated_at         TEXT    NOT NULL,
    last_started_at    TEXT
);

CREATE INDEX servers_linked_client_id ON servers (linked_client_id);
CREATE INDEX servers_storage_class ON servers (storage_class);
