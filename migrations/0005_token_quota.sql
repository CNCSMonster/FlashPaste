-- Per-token capacity for active shared content.
-- NULL means the deployment default (100 MiB for ordinary members).
ALTER TABLE tokens ADD COLUMN quota_bytes INTEGER CHECK (quota_bytes IS NULL OR quota_bytes > 0);
