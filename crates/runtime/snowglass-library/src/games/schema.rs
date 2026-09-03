pub(super) const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS binaries (
    id               BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
    blake3           BLOB NOT NULL UNIQUE CHECK (length(blake3) = 32),
    platform         TEXT NOT NULL CHECK (length(platform) > 0),
    serial           TEXT,
    title_id         TEXT,
    parent_binary_id BLOB REFERENCES binaries(id) ON DELETE RESTRICT,
    CHECK (parent_binary_id IS NULL OR parent_binary_id != id)
) STRICT;

CREATE TABLE IF NOT EXISTS launchables (
    id        BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
    binary_id BLOB NOT NULL REFERENCES binaries(id) ON DELETE CASCADE,
    path      TEXT NOT NULL UNIQUE CHECK (length(path) > 0)
) STRICT;

CREATE TABLE IF NOT EXISTS media_objects (
    blake3      BLOB PRIMARY KEY NOT NULL CHECK (length(blake3) = 32),
    object_key TEXT NOT NULL UNIQUE CHECK (length(object_key) > 0),
    byte_length INTEGER NOT NULL CHECK (byte_length >= 0)
) STRICT;

CREATE TABLE IF NOT EXISTS metadata (
    id               BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
    parent_binary_id BLOB NOT NULL REFERENCES binaries(id) ON DELETE CASCADE,
    relation         TEXT NOT NULL CHECK (relation IN (
        'title', 'description', 'releasedate', 'publisher',
        'boxartfront', 'boxartback', 'boxartspine', 'boxartfull',
        'screenshot', 'logo', 'video', 'icon', 'banner', 'media'
    )),
    language         TEXT,
    source           TEXT NOT NULL CHECK (length(source) > 0),
    data_kind        TEXT NOT NULL CHECK (data_kind IN ('text', 'year', 'inline', 'object')),
    text_value       TEXT,
    release_year     INTEGER,
    inline_data      BLOB,
    object_blake3    BLOB REFERENCES media_objects(blake3) ON DELETE RESTRICT,
    media_type       TEXT,
    byte_length      INTEGER,
    CHECK (
        (data_kind = 'text' AND relation IN ('title', 'description', 'publisher')
            AND text_value IS NOT NULL AND release_year IS NULL AND inline_data IS NULL
            AND object_blake3 IS NULL AND media_type IS NULL AND byte_length IS NULL)
        OR
        (data_kind = 'year' AND relation = 'releasedate'
            AND release_year BETWEEN 1 AND 9999 AND text_value IS NULL AND inline_data IS NULL
            AND object_blake3 IS NULL AND media_type IS NULL AND byte_length IS NULL)
        OR
        (data_kind = 'inline' AND relation NOT IN ('title', 'description', 'releasedate', 'publisher')
            AND inline_data IS NOT NULL AND object_blake3 IS NULL AND media_type IS NOT NULL
            AND byte_length = length(inline_data) AND text_value IS NULL AND release_year IS NULL)
        OR
        (data_kind = 'object' AND relation NOT IN ('title', 'description', 'releasedate', 'publisher')
            AND object_blake3 IS NOT NULL AND inline_data IS NULL AND media_type IS NOT NULL
            AND byte_length >= 0 AND text_value IS NULL AND release_year IS NULL)
    )
) STRICT;

CREATE INDEX IF NOT EXISTS binaries_parent ON binaries(parent_binary_id);
CREATE INDEX IF NOT EXISTS binaries_platform ON binaries(platform);
CREATE INDEX IF NOT EXISTS launchables_binary ON launchables(binary_id);
CREATE INDEX IF NOT EXISTS metadata_parent ON metadata(parent_binary_id, relation, language, source);
CREATE INDEX IF NOT EXISTS metadata_object ON metadata(object_blake3);

PRAGMA user_version = 1;
"#;
