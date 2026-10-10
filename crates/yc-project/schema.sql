CREATE TABLE project_meta (
    id TEXT PRIMARY KEY,
    schema_version INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    display_name TEXT NOT NULL,
    instance_path TEXT,
    target_locale TEXT NOT NULL DEFAULT 'ja_jp',
    setup_completed INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE mod_units (
    id INTEGER PRIMARY KEY,
    sort_index INTEGER NOT NULL,
    source_filename TEXT NOT NULL,
    source_sha256 TEXT NOT NULL,
    stored_path TEXT NOT NULL,
    original_path TEXT NOT NULL,
    mod_id TEXT,
    name TEXT,
    version TEXT,
    loader TEXT,
    minecraft_version TEXT,
    review_settings_json TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL
);

CREATE TABLE mods (
    id INTEGER PRIMARY KEY,
    mod_unit_id INTEGER NOT NULL REFERENCES mod_units(id) ON DELETE CASCADE,
    mod_id TEXT,
    name TEXT,
    version TEXT,
    loader TEXT,
    minecraft_version TEXT,
    metadata_source TEXT
);

CREATE TABLE namespaces (
    id INTEGER PRIMARY KEY,
    mod_unit_id INTEGER NOT NULL REFERENCES mod_units(id) ON DELETE CASCADE,
    namespace TEXT NOT NULL,
    container_path TEXT NOT NULL DEFAULT '',
    included INTEGER NOT NULL DEFAULT 1,
    entry_count INTEGER NOT NULL DEFAULT 0,
    UNIQUE(mod_unit_id, namespace)
);

CREATE TABLE nested_archives (
    id INTEGER PRIMARY KEY,
    mod_unit_id INTEGER NOT NULL REFERENCES mod_units(id) ON DELETE CASCADE,
    archive_path TEXT NOT NULL,
    mod_id TEXT,
    name TEXT,
    version TEXT,
    has_languages INTEGER NOT NULL DEFAULT 0,
    skipped_reason TEXT,
    UNIQUE(mod_unit_id, archive_path)
);

CREATE TABLE languages (
    id INTEGER PRIMARY KEY,
    mod_unit_id INTEGER NOT NULL REFERENCES mod_units(id) ON DELETE CASCADE,
    namespace TEXT NOT NULL,
    locale TEXT NOT NULL,
    format TEXT NOT NULL,
    container_path TEXT NOT NULL DEFAULT '',
    archive_path TEXT NOT NULL,
    entry_count INTEGER NOT NULL,
    is_target INTEGER NOT NULL DEFAULT 0,
    UNIQUE(mod_unit_id, namespace, locale, container_path, archive_path)
);

CREATE TABLE translation_entries (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    mod_unit_id INTEGER NOT NULL REFERENCES mod_units(id) ON DELETE CASCADE,
    namespace TEXT NOT NULL,
    translation_key TEXT NOT NULL,
    source_text TEXT,
    source_locale TEXT,
    target_text TEXT,
    target_original_text TEXT,
    translated_source_text TEXT,
    target_origin TEXT NOT NULL,
    sort_index INTEGER NOT NULL,
    asset_kind TEXT,
    asset_id TEXT,
    asset_confidence TEXT,
    updated_at TEXT NOT NULL,
    UNIQUE(mod_unit_id, namespace, translation_key)
);

CREATE TABLE entry_flags (
    entry_id INTEGER NOT NULL REFERENCES translation_entries(id) ON DELETE CASCADE,
    flag TEXT NOT NULL,
    PRIMARY KEY(entry_id, flag)
);

CREATE TABLE assets (
    id INTEGER PRIMARY KEY,
    mod_unit_id INTEGER NOT NULL REFERENCES mod_units(id) ON DELETE CASCADE,
    namespace TEXT NOT NULL,
    asset_type TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    archive_path TEXT NOT NULL,
    metadata_json TEXT,
    UNIQUE(mod_unit_id, archive_path)
);

CREATE TABLE asset_type_counts (
    mod_unit_id INTEGER NOT NULL REFERENCES mod_units(id) ON DELETE CASCADE,
    asset_type TEXT NOT NULL,
    asset_count INTEGER NOT NULL,
    PRIMARY KEY(mod_unit_id, asset_type)
);

CREATE TABLE asset_links (
    entry_id INTEGER NOT NULL REFERENCES translation_entries(id) ON DELETE CASCADE,
    asset_id INTEGER NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    relation TEXT NOT NULL,
    confidence TEXT NOT NULL,
    PRIMARY KEY(entry_id, asset_id, relation)
);

CREATE TABLE ai_jobs (
    id TEXT PRIMARY KEY,
    provider TEXT NOT NULL,
    model TEXT,
    status TEXT NOT NULL,
    total_items INTEGER NOT NULL,
    completed_items INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    finished_at TEXT,
    error_message TEXT
);

CREATE TABLE jar_replacements (
    id INTEGER PRIMARY KEY,
    mod_unit_id INTEGER NOT NULL REFERENCES mod_units(id) ON DELETE CASCADE,
    target_path TEXT NOT NULL,
    backup_path TEXT NOT NULL,
    previous_sha256 TEXT NOT NULL,
    written_sha256 TEXT NOT NULL,
    created_at TEXT NOT NULL,
    restored_at TEXT
);

CREATE TABLE glossary_terms (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    source_term TEXT NOT NULL UNIQUE,
    target_term TEXT NOT NULL,
    note TEXT,
    case_sensitive INTEGER NOT NULL DEFAULT 0 CHECK (case_sensitive IN (0, 1)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE UNIQUE INDEX glossary_terms_source_insensitive
    ON glossary_terms(lower(source_term))
    WHERE case_sensitive = 0;

CREATE UNIQUE INDEX jar_replacements_active_target
    ON jar_replacements(mod_unit_id, target_path)
    WHERE restored_at IS NULL;
