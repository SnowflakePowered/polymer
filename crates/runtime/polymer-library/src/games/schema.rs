pub(super) const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS binaries (
    id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
    blake3 BLOB NOT NULL CHECK (length(blake3) = 32),
    platform TEXT NOT NULL CHECK (length(platform) > 0),
    serial TEXT,
    title_id TEXT,
    disc_number INTEGER CHECK (disc_number IS NULL OR disc_number BETWEEN 1 AND 255),
    parent_binary_id BLOB REFERENCES binaries(id) ON DELETE RESTRICT,
    CHECK (parent_binary_id IS NULL OR parent_binary_id != id)
) STRICT;
CREATE INDEX IF NOT EXISTS binaries_parent ON binaries(parent_binary_id);
CREATE TABLE IF NOT EXISTS launchables (
    id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
    binary_id BLOB NOT NULL REFERENCES binaries(id) ON DELETE CASCADE,
    path TEXT NOT NULL UNIQUE CHECK (length(path) > 0),
    entrypoint TEXT CHECK (entrypoint IS NULL OR length(entrypoint) > 0),
    package TEXT CHECK (package IS NULL OR length(package) > 0)
) STRICT;
CREATE TABLE IF NOT EXISTS media_objects (
    blake3 BLOB PRIMARY KEY NOT NULL CHECK (length(blake3) = 32),
    object_key TEXT NOT NULL UNIQUE CHECK (length(object_key) > 0),
    byte_length INTEGER NOT NULL CHECK (byte_length >= 0)
) STRICT;
CREATE TABLE IF NOT EXISTS metadata (
    id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
    parent_binary_id BLOB NOT NULL REFERENCES binaries(id) ON DELETE CASCADE,
    key TEXT NOT NULL CHECK (length(key) > 0),
    source TEXT NOT NULL CHECK (length(source) > 0),
    producer TEXT NOT NULL CHECK (length(producer) > 0),
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    data_kind TEXT NOT NULL CHECK (data_kind IN ('string', 'number', 'boolean', 'inline', 'object')),
    string_value TEXT,
    number_value REAL,
    boolean_value INTEGER,
    inline_data BLOB,
    object_blake3 BLOB REFERENCES media_objects(blake3) ON DELETE RESTRICT,
    media_type TEXT,
    byte_length INTEGER,
    source_url TEXT,
    CHECK (
      (data_kind = 'string' AND string_value IS NOT NULL AND number_value IS NULL AND boolean_value IS NULL AND inline_data IS NULL AND object_blake3 IS NULL AND media_type IS NULL AND byte_length IS NULL AND source_url IS NULL)
      OR (data_kind = 'number' AND string_value IS NULL AND number_value IS NOT NULL AND boolean_value IS NULL AND inline_data IS NULL AND object_blake3 IS NULL AND media_type IS NULL AND byte_length IS NULL AND source_url IS NULL)
      OR (data_kind = 'boolean' AND string_value IS NULL AND number_value IS NULL AND boolean_value IN (0, 1) AND inline_data IS NULL AND object_blake3 IS NULL AND media_type IS NULL AND byte_length IS NULL AND source_url IS NULL)
      OR (data_kind = 'inline' AND string_value IS NULL AND number_value IS NULL AND boolean_value IS NULL AND inline_data IS NOT NULL AND object_blake3 IS NULL AND media_type IS NOT NULL AND byte_length = length(inline_data))
      OR (data_kind = 'object' AND string_value IS NULL AND number_value IS NULL AND boolean_value IS NULL AND inline_data IS NULL AND object_blake3 IS NOT NULL AND media_type IS NOT NULL AND byte_length >= 0)
    ),
    UNIQUE(parent_binary_id, producer, ordinal)
) STRICT;
CREATE INDEX IF NOT EXISTS launchables_binary ON launchables(binary_id);
CREATE INDEX IF NOT EXISTS metadata_parent ON metadata(parent_binary_id, key, source, ordinal);
CREATE INDEX IF NOT EXISTS metadata_producer ON metadata(parent_binary_id, producer);
CREATE INDEX IF NOT EXISTS metadata_object ON metadata(object_blake3);
CREATE TABLE IF NOT EXISTS launchable_configurations (
    launchable_id BLOB NOT NULL REFERENCES launchables(id) ON DELETE CASCADE,
    plugin_id TEXT NOT NULL CHECK (length(plugin_id) > 0),
    schema TEXT NOT NULL CHECK (length(schema) > 0),
    values_json TEXT NOT NULL,
    PRIMARY KEY (launchable_id, plugin_id, schema)
) STRICT;
CREATE TABLE IF NOT EXISTS launchable_emulators (
    launchable_id BLOB PRIMARY KEY NOT NULL REFERENCES launchables(id) ON DELETE CASCADE,
    plugin_id TEXT NOT NULL CHECK (length(plugin_id) > 0)
) STRICT;
CREATE TABLE IF NOT EXISTS platform_settings (
    platform TEXT PRIMARY KEY NOT NULL CHECK (length(platform) > 0),
    emulator TEXT CHECK (emulator IS NULL OR length(emulator) > 0)
) STRICT;
CREATE TABLE IF NOT EXISTS launchable_manifests (
    launchable_id BLOB PRIMARY KEY NOT NULL REFERENCES launchables(id) ON DELETE CASCADE,
    revision TEXT NOT NULL,
    manifest_json TEXT NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS title_updates (
    launchable_id BLOB NOT NULL REFERENCES launchables(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    candidate_json TEXT NOT NULL,
    checked_at INTEGER NOT NULL,
    PRIMARY KEY (launchable_id, ordinal)
) STRICT;
PRAGMA user_version = 12;
"#;
