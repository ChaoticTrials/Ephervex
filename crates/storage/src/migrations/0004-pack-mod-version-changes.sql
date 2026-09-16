-- Per-instance opt-in to changing or updating a mod the imported pack
-- declares, independently of what the pack shipped. Existing instances have
-- not opted in.
ALTER TABLE instances ADD COLUMN allow_pack_mod_version_changes INTEGER NOT NULL DEFAULT 0;
