-- Apply once through the migration runner, after backing up existing data.
ALTER TABLE tokens ADD COLUMN auth_version INTEGER NOT NULL DEFAULT 0 CHECK (auth_version >= 0);
