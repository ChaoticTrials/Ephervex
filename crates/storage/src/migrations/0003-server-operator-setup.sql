-- New-server operator intent is durable so it can be applied when a suitable
-- account identity becomes available. Existing servers intentionally have no
-- inherited intent.
ALTER TABLE servers ADD COLUMN operator_setup TEXT;
