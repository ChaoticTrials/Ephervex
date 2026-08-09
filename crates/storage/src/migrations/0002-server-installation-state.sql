-- Existing server rows were scaffolds and never represented a completed base
-- installation. Preserve every existing setting while making that state
-- explicit. The legacy eula_accepted column remains inert for compatibility;
-- global consent in config.toml is the only authorization source.
ALTER TABLE servers ADD COLUMN installation TEXT NOT NULL DEFAULT '{"state":"not_installed"}';
