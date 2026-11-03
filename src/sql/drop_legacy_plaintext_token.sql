-- One-time cleanup for deployments that used the legacy plaintext column.
-- Run only after verifying `PRAGMA table_info(tokens)` lists token_value.
ALTER TABLE tokens DROP COLUMN token_value;
