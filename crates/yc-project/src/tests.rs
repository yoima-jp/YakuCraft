use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use rusqlite::Connection;
use uuid::Uuid;
use yc_language::{
    EntryFlag, LanguageFileFormat, ReviewSettings, TranslationEntry, TranslationOrigin,
};

use crate::{
    AnalysisData, EntryUpdate, GlossaryTermInput, JarReplacementRecord, LanguageRecord, LockRecord,
    ModRecord, NamespaceRecord, NestedArchiveRecord, ProjectError, ProjectLock, RecentProject,
    RecentProjects, TranslationProgress, UpdateOrigin, create_project, database::open_database,
    delete_project, list_projects, load_settings, normalize_project_name, open_project,
    save_settings,
};

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("yc-project-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).expect("create temporary test directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn project_creation_copies_source_and_matches_schema_versions() {
    let temporary = TestDirectory::new();
    let projects_root = temporary.path().join("projects");
    let source_jar = temporary.path().join("sample.jar");
    let original_bytes = b"source jar bytes";
    fs::write(&source_jar, original_bytes).expect("write source fixture");

    let project = create_project(&projects_root, std::slice::from_ref(&source_jar), None)
        .expect("create Project");
    let project_directory = projects_root.join(&project.id);
    assert_eq!(
        fs::read(&source_jar).expect("read original"),
        original_bytes
    );
    assert_eq!(
        fs::read(project_directory.join("sources/1.jar")).expect("read Project source"),
        original_bytes
    );
    assert_eq!(project.mod_count, 1);

    let connection =
        Connection::open(project_directory.join("project.sqlite")).expect("open Project database");
    let user_version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("read user_version");
    let schema_version: i64 = connection
        .query_row("SELECT schema_version FROM project_meta", [], |row| {
            row.get(0)
        })
        .expect("read schema_version");
    assert_eq!(user_version, schema_version);
    assert!(user_version > 0);
    drop(connection);

    assert_eq!(
        crate::list_projects(&projects_root)
            .expect("list Projects")
            .len(),
        1
    );
    delete_project(
        &projects_root,
        Uuid::parse_str(&project.id).expect("Project UUID"),
    )
    .expect("delete Project");
    assert!(!project_directory.exists());
}

#[test]
fn glossary_repository_and_writer_cover_crud_and_case_insensitive_upsert() {
    let temporary = TestDirectory::new();
    let projects_root = temporary.path().join("projects");
    let source_jar = temporary.path().join("sample.jar");
    fs::write(&source_jar, b"source jar bytes").expect("write source fixture");
    let project = create_project(&projects_root, &[source_jar], None).expect("create Project");
    let opened = open_project(
        &projects_root,
        Uuid::parse_str(&project.id).expect("Project UUID"),
    )
    .expect("open Project");

    opened
        .writer()
        .upsert_glossary_term(GlossaryTermInput {
            source_term: "Mana".to_owned(),
            target_term: "マナ".to_owned(),
            note: Some("魔力".to_owned()),
            case_sensitive: false,
        })
        .expect("insert glossary term");
    let first = opened
        .repository()
        .list_glossary_terms()
        .expect("list glossary terms");
    assert_eq!(first.len(), 1);
    let id = first[0].id;

    opened
        .writer()
        .upsert_glossary_term(GlossaryTermInput {
            source_term: "MANA".to_owned(),
            target_term: "魔力".to_owned(),
            note: None,
            case_sensitive: false,
        })
        .expect("update case-insensitive term");
    opened
        .writer()
        .upsert_glossary_term(GlossaryTermInput {
            source_term: "mana".to_owned(),
            target_term: "マナ小文字".to_owned(),
            note: None,
            case_sensitive: true,
        })
        .expect("insert case-sensitive term");
    let updated = opened
        .repository()
        .list_glossary_terms()
        .expect("list updated glossary terms");
    assert_eq!(updated.len(), 2);
    assert!(
        updated
            .iter()
            .any(|term| term.id == id && term.target_term == "魔力")
    );
    assert!(updated.iter().any(|term| term.case_sensitive));

    opened
        .writer()
        .delete_glossary_term(id)
        .expect("delete glossary term");
    let remaining = opened
        .repository()
        .list_glossary_terms()
        .expect("list remaining glossary terms");
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].source_term, "mana");
}

#[test]
fn glossary_edit_updates_its_id_and_rejects_an_existing_source_term() {
    let temporary = TestDirectory::new();
    let projects_root = temporary.path().join("projects");
    let source_jar = temporary.path().join("sample.jar");
    fs::write(&source_jar, b"source jar bytes").expect("write source fixture");
    let project = create_project(&projects_root, &[source_jar], None).expect("create Project");
    let opened = open_project(
        &projects_root,
        Uuid::parse_str(&project.id).expect("Project UUID"),
    )
    .expect("open Project");

    opened
        .writer()
        .upsert_glossary_terms(vec![
            GlossaryTermInput {
                source_term: "Mana".to_owned(),
                target_term: "マナ".to_owned(),
                note: None,
                case_sensitive: false,
            },
            GlossaryTermInput {
                source_term: "Focus".to_owned(),
                target_term: "集中".to_owned(),
                note: None,
                case_sensitive: false,
            },
        ])
        .expect("insert glossary terms");
    let initial = opened
        .repository()
        .list_glossary_terms()
        .expect("list glossary terms");
    let mana_id = initial
        .iter()
        .find(|term| term.source_term == "Mana")
        .expect("Mana term")
        .id;

    let conflict = opened.writer().update_glossary_term(
        mana_id,
        GlossaryTermInput {
            source_term: "focus".to_owned(),
            target_term: "焦点".to_owned(),
            note: None,
            case_sensitive: false,
        },
    );
    assert!(conflict.is_err());
    assert_eq!(
        opened
            .repository()
            .list_glossary_terms()
            .expect("list terms after rejected edit"),
        initial
    );

    opened
        .writer()
        .update_glossary_term(
            mana_id,
            GlossaryTermInput {
                source_term: "Essence".to_owned(),
                target_term: "精髄".to_owned(),
                note: Some("魔力の本質".to_owned()),
                case_sensitive: false,
            },
        )
        .expect("update glossary term by id");
    let updated = opened
        .repository()
        .list_glossary_terms()
        .expect("list updated terms");
    assert_eq!(updated.len(), 2);
    assert!(updated.iter().any(|term| {
        term.id == mana_id
            && term.source_term == "Essence"
            && term.target_term == "精髄"
            && term.note.as_deref() == Some("魔力の本質")
    }));
    assert!(
        updated
            .iter()
            .any(|term| { term.source_term == "Focus" && term.target_term == "集中" })
    );
}

#[test]
fn project_name_validation_trims_and_rejects_invalid_values() {
    assert_eq!(
        normalize_project_name("  Example  ").expect("trim project name"),
        "Example"
    );
    assert_eq!(
        normalize_project_name(&"界".repeat(100)).expect("100 characters"),
        "界".repeat(100)
    );
    assert!(normalize_project_name(" \t ").is_err());
    assert!(normalize_project_name(&"x".repeat(101)).is_err());
    assert!(normalize_project_name("bad\nname").is_err());
}

#[test]
fn review_resave_updates_automatic_name_but_preserves_a_user_name() {
    let temporary = TestDirectory::new();
    let projects_root = temporary.path().join("projects");
    let source_jar = temporary.path().join("sample.jar");
    fs::write(&source_jar, b"source jar bytes").expect("write source fixture");
    let created = create_project(&projects_root, &[source_jar], None).expect("create Project");
    let opened = open_project(
        &projects_root,
        Uuid::parse_str(&created.id).expect("Project UUID"),
    )
    .expect("open Project");

    let save_analysis_named = |name: &str| {
        let mut analysis = analysis_with_entries(Vec::new());
        analysis.mods[0].name = Some(name.to_owned());
        opened
            .writer()
            .save_analysis(1, analysis)
            .expect("save analysis");
    };
    let current_name = || {
        list_projects(&projects_root)
            .expect("list Projects")
            .into_iter()
            .find(|project| project.id == created.id)
            .expect("find Project")
            .display_name
    };

    save_analysis_named("Initial Mod");
    assert_eq!(current_name(), "Initial Mod");
    save_analysis_named("Updated Mod");
    assert_eq!(current_name(), "Updated Mod");
    opened
        .writer()
        .save_review_settings(1, ReviewSettings::default(), false)
        .expect("leave Review setup incomplete");

    opened
        .repository()
        .rename_project("My project name")
        .expect("rename Project");
    save_analysis_named("Final Mod");
    opened
        .writer()
        .save_review_settings(1, ReviewSettings::default(), true)
        .expect("complete Review setup");
    assert_eq!(current_name(), "My project name");
}

#[test]
fn entry_summary_does_not_count_whitespace_only_targets_as_translated() {
    let temporary = TestDirectory::new();
    let projects_root = temporary.path().join("projects");
    let source_jar = temporary.path().join("sample.jar");
    fs::write(&source_jar, b"source jar bytes").expect("write source fixture");
    let created = create_project(&projects_root, &[source_jar], None).expect("create Project");
    let opened = open_project(
        &projects_root,
        Uuid::parse_str(&created.id).expect("Project UUID"),
    )
    .expect("open Project");
    let mut analysis = analysis_with_entries(vec![
        translation_entry(
            "translated",
            "Source",
            TranslationOrigin::Existing,
            "翻訳",
            Vec::new(),
        ),
        translation_entry(
            "empty",
            "Source",
            TranslationOrigin::Existing,
            "",
            Vec::new(),
        ),
        translation_entry(
            "whitespace",
            "Source",
            TranslationOrigin::Existing,
            " \t\u{3000}",
            Vec::new(),
        ),
    ]);
    analysis.entries[2].target_original_text = Some("\u{00a0}".to_owned());
    opened
        .writer()
        .save_analysis(1, analysis)
        .expect("save analysis");

    let summary = opened
        .repository()
        .entry_summary(Some(1))
        .expect("read entry summary");
    assert_eq!(summary.total, 3);
    assert_eq!(summary.translated, 1);
    assert_eq!(summary.existing_japanese, 0);
}

#[test]
fn removing_mod_unit_cascades_entries_and_removes_its_stored_jar() {
    let temporary = TestDirectory::new();
    let projects_root = temporary.path().join("projects");
    let first_source = temporary.path().join("first.jar");
    let second_source = temporary.path().join("second.jar");
    fs::write(&first_source, b"first jar").expect("write first source");
    fs::write(&second_source, b"second jar").expect("write second source");
    let created = create_project(&projects_root, &[first_source, second_source], None)
        .expect("create multi-unit Project");
    let opened = open_project(
        &projects_root,
        Uuid::parse_str(&created.id).expect("Project UUID"),
    )
    .expect("open Project");
    opened
        .writer()
        .save_analysis(
            1,
            analysis_with_entries(vec![translation_entry(
                "remove.me",
                "Remove me",
                TranslationOrigin::Edited,
                "削除対象",
                Vec::new(),
            )]),
        )
        .expect("seed translation entry");

    let repository = opened.repository();
    let stored_jar = repository.mod_unit_source_path(1).expect("find stored jar");
    assert_eq!(
        repository
            .list_entries(Some(1))
            .expect("list entries")
            .len(),
        1
    );

    repository.remove_mod_unit(1).expect("remove mod unit");

    assert!(
        repository
            .list_entries(Some(1))
            .expect("list entries")
            .is_empty()
    );
    assert!(!stored_jar.exists());
    assert_eq!(
        repository.list_mod_units().expect("list mod units").len(),
        1
    );
    assert!(matches!(
        repository.remove_mod_unit(2),
        Err(ProjectError::LastModUnit)
    ));
}

#[test]
fn empty_version_zero_database_is_backed_up_before_migration() {
    let temporary = TestDirectory::new();
    let database_path = temporary.path().join("project.sqlite");
    let old_database = Connection::open(&database_path).expect("create old database");
    old_database
        .pragma_update(None, "user_version", 0_i64)
        .expect("set old database version");
    drop(old_database);

    let connection = open_database(&database_path).expect("migrate old database");
    let current_version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("read migrated version");
    assert_eq!(current_version, 5);
    drop(connection);

    let backup_path = temporary.path().join("project.sqlite.backup");
    assert!(backup_path.is_file());
    let backup = Connection::open(backup_path).expect("open pre-migration backup");
    let backup_version: i64 = backup
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("read backup version");
    assert_eq!(backup_version, 0);
    let project_meta_exists: bool = backup.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'project_meta')",
        [],
        |row| row.get(0),
    ).expect("check backup schema");
    assert!(!project_meta_exists);
}

#[test]
fn v010_database_migrates_and_keeps_legacy_entries_readable_and_writable() {
    let temporary = TestDirectory::new();
    let database_path = temporary.path().join("project.sqlite");
    let old_database = Connection::open(&database_path).expect("create v0.1.0 database");
    old_database
        .execute_batch(include_str!("../migrations/20261004122242_init.sql"))
        .expect("apply init migration");
    old_database
        .execute_batch(include_str!(
            "../migrations/20261004141408_jar_replacements.sql"
        ))
        .expect("apply jar replacements migration");
    old_database
        .execute_batch(include_str!("../migrations/20261005124747_glossary.sql"))
        .expect("apply glossary migration");
    old_database
        .execute_batch(include_str!(
            "../migrations/20261005232035_asset_type_counts.sql"
        ))
        .expect("apply asset counts migration");
    old_database
        .execute_batch(
            "INSERT INTO project_meta (
                id, schema_version, created_at, updated_at, display_name
             ) VALUES ('legacy-project', 4, '2026-10-01T00:00:00Z', '2026-10-01T00:00:00Z', 'Legacy');
             INSERT INTO mod_units (
                id, sort_index, source_filename, source_sha256, stored_path,
                original_path, created_at
             ) VALUES (7, 0, 'legacy.jar', 'jar-hash', 'sources/7.jar', 'legacy.jar',
                       '2026-10-01T00:00:00Z');
             INSERT INTO translation_entries (
                id, mod_unit_id, namespace, translation_key, source_text,
                target_text, target_origin, sort_index, updated_at
             ) VALUES (11, 7, 'legacy', 'legacy.key', 'Legacy source', '旧訳',
                       'existing', 0, '2026-10-01T00:00:00Z');
             PRAGMA user_version = 4;",
        )
        .expect("insert legacy project rows");
    drop(old_database);

    let connection = open_database(&database_path).expect("migrate legacy project");
    let user_version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("read migrated user_version");
    let schema_version: i64 = connection
        .query_row("SELECT schema_version FROM project_meta", [], |row| {
            row.get(0)
        })
        .expect("read migrated schema_version");
    assert_eq!(user_version, 5);
    assert_eq!(schema_version, user_version);
    drop(connection);

    let repository = crate::Repository::new(&database_path);
    let entries = repository.list_entries(None).expect("read legacy entry");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].target_text.as_deref(), Some("旧訳"));
    assert!(
        repository
            .list_document_entries(None)
            .expect("read legacy document entries")
            .is_empty()
    );
    assert_eq!(
        repository.list_mod_units().expect("read legacy unit")[0].source_kind,
        crate::ModUnitSourceKind::Jar
    );

    let writer = crate::ProjectWriter::start(&database_path).expect("start migrated writer");
    writer
        .update_entry(EntryUpdate {
            id: 11,
            target_text: "更新訳".to_owned(),
            origin: UpdateOrigin::Manual,
        })
        .expect("write legacy entry");
    let updated = repository.list_entries(None).expect("read updated entry");
    assert_eq!(updated[0].target_text.as_deref(), Some("更新訳"));
    assert_eq!(updated[0].origin, yc_language::TranslationOrigin::Edited);
}

#[test]
fn failed_migration_rolls_back_and_keeps_the_backup() {
    let temporary = TestDirectory::new();
    let database_path = temporary.path().join("project.sqlite");
    let old_database = Connection::open(&database_path).expect("create conflicting old database");
    old_database
        .execute_batch(
            "CREATE TABLE project_meta (marker TEXT);
             INSERT INTO project_meta VALUES ('preserved');",
        )
        .expect("add pre-migration data");
    drop(old_database);

    assert!(matches!(
        open_database(&database_path),
        Err(ProjectError::Migration { .. })
    ));
    assert!(temporary.path().join("project.sqlite.backup").is_file());

    let after_failure = Connection::open(&database_path).expect("open rolled-back database");
    let marker: String = after_failure
        .query_row("SELECT marker FROM project_meta", [], |row| row.get(0))
        .expect("read preserved row");
    assert_eq!(marker, "preserved");
    let new_table_exists: bool = after_failure
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'mods')",
            [],
            |row| row.get(0),
        )
        .expect("check rolled-back schema");
    assert!(!new_table_exists);
}

#[test]
fn analysis_persists_as_one_result_set_and_lists_flags() {
    let temporary = TestDirectory::new();
    let projects_root = temporary.path().join("projects");
    let source_jar = temporary.path().join("analysis.jar");
    fs::write(&source_jar, b"jar").expect("write source fixture");
    let created = create_project(&projects_root, std::slice::from_ref(&source_jar), None)
        .expect("create Project");
    let id = Uuid::parse_str(&created.id).expect("Project UUID");
    let opened = open_project(&projects_root, id).expect("open Project");
    let writer = opened.writer();

    writer
        .save_analysis(
            1,
            analysis_with_entries(vec![translation_entry(
                "block.test",
                "Hello",
                TranslationOrigin::Missing,
                "",
                vec![EntryFlag::SourceFallback, EntryFlag::NeedsReview],
            )]),
        )
        .expect("save analysis");

    let entries = opened
        .repository()
        .list_entries(None)
        .expect("list entries");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].key, "block.test");
    assert_eq!(entries[0].flags.len(), 2);
    assert!(entries[0].flags.contains(&EntryFlag::SourceFallback));
    assert!(entries[0].flags.contains(&EntryFlag::NeedsReview));
    writer
        .add_flag(entries[0].id, EntryFlag::AssetMissing)
        .expect("add entry flag");
    assert!(
        opened
            .repository()
            .list_entries(None)
            .expect("read added flag")[0]
            .flags
            .contains(&EntryFlag::AssetMissing)
    );
    writer
        .remove_flag(entries[0].id, EntryFlag::AssetMissing)
        .expect("remove entry flag");
    assert!(
        !opened
            .repository()
            .list_entries(None)
            .expect("read removed flag")[0]
            .flags
            .contains(&EntryFlag::AssetMissing)
    );

    let connection = Connection::open(opened.directory().join("project.sqlite"))
        .expect("open database for persisted rows");
    for table in [
        "mods",
        "namespaces",
        "nested_archives",
        "languages",
        "translation_entries",
        "entry_flags",
    ] {
        let row_count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("count analysis records");
        assert!(row_count > 0, "analysis table {table} should have records");
    }
}

#[test]
fn jar_replacements_are_saved_listed_and_marked_restored() {
    let temporary = TestDirectory::new();
    let projects_root = temporary.path().join("projects");
    let source_jar = temporary.path().join("replacement.jar");
    fs::write(&source_jar, b"jar").expect("write source fixture");
    let created = create_project(&projects_root, std::slice::from_ref(&source_jar), None)
        .expect("create Project");
    let opened = open_project(
        &projects_root,
        Uuid::parse_str(&created.id).expect("Project UUID"),
    )
    .expect("open Project");
    let replacement = JarReplacementRecord {
        id: 0,
        mod_unit_id: 1,
        target_path: source_jar.to_string_lossy().into_owned(),
        backup_path: temporary
            .path()
            .join("backup.jar")
            .to_string_lossy()
            .into_owned(),
        previous_sha256: "source-hash".to_owned(),
        written_sha256: "written-hash".to_owned(),
        created_at: "2026-10-04T00:00:00Z".to_owned(),
        restored_at: None,
    };

    opened
        .writer()
        .save_jar_replacement(replacement.clone())
        .expect("save replacement");
    let saved = opened
        .repository()
        .list_jar_replacements(false)
        .expect("list active replacements");
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].mod_unit_id, replacement.mod_unit_id);
    assert_eq!(saved[0].target_path, replacement.target_path);
    assert_eq!(saved[0].backup_path, replacement.backup_path);
    assert_eq!(saved[0].written_sha256, replacement.written_sha256);

    let mut repeated_replacement = replacement.clone();
    repeated_replacement.backup_path = temporary
        .path()
        .join("later-backup.jar")
        .to_string_lossy()
        .into_owned();
    repeated_replacement.previous_sha256 = "later-previous-hash".to_owned();
    repeated_replacement.written_sha256 = "written-hash-2".to_owned();
    opened
        .writer()
        .save_jar_replacement(repeated_replacement)
        .expect("update the active replacement");
    let updated = opened
        .repository()
        .list_jar_replacements(false)
        .expect("list updated replacement");
    assert_eq!(updated.len(), 1);
    assert_eq!(updated[0].backup_path, replacement.backup_path);
    assert_eq!(updated[0].previous_sha256, replacement.previous_sha256);
    assert_eq!(updated[0].written_sha256, "written-hash-2");

    opened
        .writer()
        .mark_jar_replacement_restored(updated[0].id, "2026-10-04T00:01:00Z".to_owned())
        .expect("mark replacement restored");
    assert!(
        opened
            .repository()
            .list_jar_replacements(false)
            .expect("list active replacements after restore")
            .is_empty()
    );
    assert_eq!(
        opened
            .repository()
            .list_jar_replacements(true)
            .expect("list history")[0]
            .restored_at
            .as_deref(),
        Some("2026-10-04T00:01:00Z")
    );
}

#[test]
fn same_namespace_and_key_are_isolated_between_mod_units() {
    let temporary = TestDirectory::new();
    let projects_root = temporary.path().join("projects");
    let first_source = temporary.path().join("first.jar");
    let second_source = temporary.path().join("second.jar");
    fs::write(&first_source, b"first jar").expect("write first source");
    fs::write(&second_source, b"second jar").expect("write second source");
    let created = create_project(&projects_root, &[first_source, second_source], None)
        .expect("create multi-unit Project");
    let opened = open_project(
        &projects_root,
        Uuid::parse_str(&created.id).expect("Project UUID"),
    )
    .expect("open Project");

    opened
        .writer()
        .save_analysis(
            1,
            analysis_with_entries(vec![translation_entry(
                "shared.key",
                "First source",
                TranslationOrigin::Edited,
                "First translation",
                Vec::new(),
            )]),
        )
        .expect("save first unit");
    opened
        .writer()
        .save_analysis(
            2,
            analysis_with_entries(vec![translation_entry(
                "shared.key",
                "Second source",
                TranslationOrigin::Edited,
                "Second translation",
                Vec::new(),
            )]),
        )
        .expect("save second unit");

    let first = opened
        .repository()
        .list_entries(Some(1))
        .expect("list first unit");
    let second = opened
        .repository()
        .list_entries(Some(2))
        .expect("list second unit");
    assert_eq!(first.len(), 1);
    assert_eq!(second.len(), 1);
    assert_ne!(first[0].id, second[0].id);
    assert_eq!(first[0].mod_unit_id, 1);
    assert_eq!(second[0].mod_unit_id, 2);
    assert_eq!(first[0].source_text.as_deref(), Some("First source"));
    assert_eq!(second[0].source_text.as_deref(), Some("Second source"));
    assert_eq!(first[0].target_text.as_deref(), Some("First translation"));
    assert_eq!(second[0].target_text.as_deref(), Some("Second translation"));
}

#[test]
fn ai_and_manual_updates_set_origin_and_source_snapshot() {
    let temporary = TestDirectory::new();
    let projects_root = temporary.path().join("projects");
    let source_jar = temporary.path().join("translations.jar");
    fs::write(&source_jar, b"jar").expect("write source fixture");
    let created = create_project(&projects_root, std::slice::from_ref(&source_jar), None)
        .expect("create Project");
    let opened = open_project(
        &projects_root,
        Uuid::parse_str(&created.id).expect("Project UUID"),
    )
    .expect("open Project");
    let writer = opened.writer();
    writer
        .save_analysis(
            1,
            analysis_with_entries(vec![
                translation_entry("first", "Hello", TranslationOrigin::Missing, "", vec![]),
                translation_entry("second", "World", TranslationOrigin::Missing, "", vec![]),
            ]),
        )
        .expect("save analysis");
    let entries = opened
        .repository()
        .list_entries(None)
        .expect("get inserted IDs");

    writer
        .update_entries(vec![
            EntryUpdate {
                id: entries[0].id,
                target_text: "AI訳".to_owned(),
                origin: UpdateOrigin::Ai,
            },
            EntryUpdate {
                id: entries[1].id,
                target_text: "手動訳".to_owned(),
                origin: UpdateOrigin::Manual,
            },
        ])
        .expect("batch update entries");
    let after_batch = opened
        .repository()
        .list_entries(None)
        .expect("read batch updates");
    assert_eq!(after_batch[0].origin, TranslationOrigin::Ai);
    assert_eq!(
        after_batch[0].translated_source_text.as_deref(),
        Some("Hello")
    );
    assert_eq!(after_batch[1].origin, TranslationOrigin::Edited);
    assert_eq!(
        after_batch[1].translated_source_text.as_deref(),
        Some("World")
    );

    writer
        .update_entry(EntryUpdate {
            id: entries[0].id,
            target_text: "修正訳".to_owned(),
            origin: UpdateOrigin::Manual,
        })
        .expect("manually edit AI translation");
    let after_manual_edit = opened
        .repository()
        .list_entries(None)
        .expect("read manual edit");
    assert_eq!(after_manual_edit[0].origin, TranslationOrigin::Edited);
    assert_eq!(
        after_manual_edit[0].translated_source_text.as_deref(),
        Some("Hello")
    );

    writer
        .save_review_settings(
            1,
            ReviewSettings {
                source_locale: "en_us".to_owned(),
                included_namespaces: vec!["test".to_owned()],
                ..ReviewSettings::default()
            },
            true,
        )
        .expect("save review settings");
    let settings = opened
        .repository()
        .review_settings()
        .expect("read review settings");
    assert_eq!(settings.source_locale.as_deref(), Some("en_us"));
    assert!(settings.setup_completed);
    assert_eq!(settings.included_namespaces.len(), 1);
    assert_eq!(settings.included_namespaces[0].mod_unit_id, 1);
    assert_eq!(settings.included_namespaces[0].namespace, "test");
}

#[test]
fn project_lock_rejects_second_owner_and_recovers_stale_record() {
    let temporary = TestDirectory::new();
    let project_directory = temporary.path().join("project");
    fs::create_dir(&project_directory).expect("create Project directory");
    let lock = ProjectLock::acquire(&project_directory).expect("acquire first lock");
    assert!(matches!(
        ProjectLock::acquire(&project_directory),
        Err(ProjectError::ProjectAlreadyOpen)
    ));
    drop(lock);
    assert!(!project_directory.join("project.lock").exists());

    let stale = LockRecord {
        pid: u32::MAX,
        updated_at: "2000-01-01T00:00:00Z".to_owned(),
        token: Uuid::new_v4(),
    };
    fs::write(
        project_directory.join("project.lock"),
        serde_json::to_vec(&stale).expect("encode stale lock"),
    )
    .expect("write stale lock");
    let recovered =
        ProjectLock::acquire_with_stale_after(&project_directory, Duration::from_secs(60))
            .expect("recover stale lock");
    assert_ne!(recovered.record().token, stale.token);
    drop(recovered);

    let old_but_live = LockRecord {
        pid: std::process::id(),
        updated_at: "2000-01-01T00:00:00Z".to_owned(),
        token: Uuid::new_v4(),
    };
    fs::write(
        project_directory.join("project.lock"),
        serde_json::to_vec(&old_but_live).expect("encode old live lock"),
    )
    .expect("write old live lock");
    let recovered =
        ProjectLock::acquire_with_stale_after(&project_directory, Duration::from_secs(60))
            .expect("recover lock by timestamp");
    assert_ne!(recovered.record().token, old_but_live.token);
}

#[test]
fn opening_project_recovers_lock_left_by_a_terminated_process() {
    let temporary = TestDirectory::new();
    let projects_root = temporary.path().join("projects");
    let source_jar = temporary.path().join("sample.jar");
    fs::write(&source_jar, b"source jar bytes").expect("write source JAR");
    let summary = create_project(&projects_root, std::slice::from_ref(&source_jar), None)
        .expect("create Project");
    let project_id = Uuid::parse_str(&summary.id).expect("Project UUID");
    let project_directory = projects_root.join(&summary.id);
    let stale = LockRecord {
        pid: u32::MAX,
        updated_at: "2000-01-01T00:00:00Z".to_owned(),
        token: Uuid::new_v4(),
    };
    fs::write(
        project_directory.join("project.lock"),
        serde_json::to_vec(&stale).expect("encode stale lock"),
    )
    .expect("leave the previous process lock file");

    let reopened = open_project(&projects_root, project_id).expect("recover and open Project");
    let recovered: LockRecord = serde_json::from_slice(
        &fs::read(project_directory.join("project.lock")).expect("read recovered lock"),
    )
    .expect("decode recovered lock");
    assert_eq!(recovered.pid, std::process::id());
    assert_ne!(recovered.token, stale.token);
    assert!(reopened.directory().join("sources/1.jar").is_file());
}

#[test]
fn autosave_process_crash_child_waits_with_uncommitted_write() {
    let (Some(database_path), Some(ready_path)) = (
        std::env::var_os("YAKUCRAFT_CRASH_TEST_DATABASE"),
        std::env::var_os("YAKUCRAFT_CRASH_TEST_READY"),
    ) else {
        return;
    };

    let database_path = PathBuf::from(database_path);
    let mut connection = open_database(&database_path).expect("open WAL database in child");
    let transaction = connection
        .transaction()
        .expect("start uncommitted autosave transaction");
    transaction
        .execute(
            "UPDATE translation_entries SET target_text = 'partial' WHERE translation_key = 'crash.test'",
            [],
        )
        .expect("write autosave value inside transaction");
    fs::write(ready_path, b"transaction is open").expect("notify parent that write is pending");
    loop {
        thread::sleep(Duration::from_secs(1));
    }
}

#[test]
fn autosave_transaction_is_rolled_back_after_process_termination() {
    let temporary = TestDirectory::new();
    let projects_root = temporary.path().join("projects");
    let source_jar = temporary.path().join("sample.jar");
    fs::write(&source_jar, b"source jar bytes").expect("write source JAR");
    let summary = create_project(&projects_root, std::slice::from_ref(&source_jar), None)
        .expect("create Project");
    let project_id = Uuid::parse_str(&summary.id).expect("Project UUID");
    let opened = open_project(&projects_root, project_id).expect("open Project");
    opened
        .writer()
        .save_analysis(
            1,
            analysis_with_entries(vec![translation_entry(
                "crash.test",
                "Original",
                TranslationOrigin::Edited,
                "committed",
                Vec::new(),
            )]),
        )
        .expect("save committed value");
    let database_path = opened.directory().join("project.sqlite");
    drop(opened);

    let ready_path = temporary.path().join("transaction-open");
    let mut child = Command::new(std::env::current_exe().expect("test executable path"))
        .args([
            "--exact",
            "tests::autosave_process_crash_child_waits_with_uncommitted_write",
        ])
        .env("YAKUCRAFT_CRASH_TEST_DATABASE", &database_path)
        .env("YAKUCRAFT_CRASH_TEST_READY", &ready_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn crash simulation process");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready_path.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    if !ready_path.exists() {
        let _ = child.kill();
        let _ = child.wait();
        panic!("child did not reach the uncommitted write");
    }
    child
        .kill()
        .expect("terminate child during autosave transaction");
    let status = child.wait().expect("wait for terminated child");
    assert!(
        !status.success(),
        "child process should be terminated forcibly"
    );

    let recovered_database = open_database(&database_path).expect("recover WAL database");
    let integrity: String = recovered_database
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .expect("check recovered database integrity");
    assert_eq!(integrity, "ok");
    let journal_mode: String = recovered_database
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .expect("read journal mode");
    assert_eq!(journal_mode, "wal");
    drop(recovered_database);

    let reopened = open_project(&projects_root, project_id).expect("reopen Project after crash");
    let entries = reopened
        .repository()
        .list_entries(None)
        .expect("read committed entries after crash");
    assert_eq!(entries[0].target_text.as_deref(), Some("committed"));
}

#[test]
#[ignore = "手動ベンチマーク: 5万件のEditor一覧とJSON IPC payloadを計測する"]
fn benchmark_list_entries_with_fifty_thousand_entries() {
    let temporary = TestDirectory::new();
    let projects_root = temporary.path().join("projects");
    let source_jar = temporary.path().join("benchmark.jar");
    fs::write(&source_jar, b"benchmark source").expect("write benchmark source");
    let summary = create_project(&projects_root, std::slice::from_ref(&source_jar), None)
        .expect("create benchmark Project");
    let project_id = Uuid::parse_str(&summary.id).expect("Project UUID");
    let opened = open_project(&projects_root, project_id).expect("open benchmark Project");
    let entries = (0..50_000)
        .map(|index| {
            let flags = if index % 100 == 0 {
                vec![EntryFlag::NeedsReview]
            } else {
                Vec::new()
            };
            translation_entry(
                &format!("entry.{index:05}"),
                "Source text",
                TranslationOrigin::Missing,
                "",
                flags,
            )
        })
        .collect();
    opened
        .writer()
        .save_analysis(1, analysis_with_entries(entries))
        .expect("seed benchmark entries");

    let started = Instant::now();
    let listed = opened
        .repository()
        .list_entries(None)
        .expect("list benchmark entries");
    let elapsed = started.elapsed();
    let serialized = serde_json::to_vec(&listed).expect("serialize IPC payload");
    assert_eq!(listed.len(), 50_000);
    println!(
        "list_entries: {:.3}s for {} entries; JSON IPC payload: {} bytes",
        elapsed.as_secs_f64(),
        listed.len(),
        serialized.len()
    );
}

#[test]
fn settings_ignore_unknown_fields_and_replace_file_atomically() {
    let temporary = TestDirectory::new();
    let path = temporary.path().join("settings.json");
    fs::write(
        &path,
        br#"{"schemaVersion":1,"futureOption":{"enabled":true}}"#,
    )
    .expect("write settings fixture");
    let mut settings = load_settings(&path).expect("load settings with unknown field");
    assert_eq!(settings.schema_version, 1);
    assert_eq!(settings.values["futureOption"]["enabled"], true);
    settings
        .values
        .insert("fontSize".to_owned(), serde_json::json!(14));

    save_settings(&path, &settings).expect("atomically save settings");
    let reloaded = load_settings(&path).expect("reload saved settings");
    assert_eq!(reloaded.values["fontSize"], 14);
    assert_eq!(reloaded.values["futureOption"]["enabled"], true);
    let siblings = fs::read_dir(temporary.path())
        .expect("read settings directory")
        .map(|entry| entry.expect("read directory entry").file_name())
        .collect::<Vec<_>>();
    assert_eq!(siblings.len(), 1);

    let recent: RecentProjects =
        serde_json::from_str(r#"{"schemaVersion":1,"projects":[],"futureField":true}"#)
            .expect("ignore unknown recent field");
    assert_eq!(recent.projects.len(), 0);

    let existing_id = Uuid::new_v4();
    let missing_id = Uuid::new_v4();
    fs::create_dir(temporary.path().join(existing_id.to_string()))
        .expect("create existing Project directory");
    let mut recent = RecentProjects {
        schema_version: 1,
        projects: vec![
            RecentProject {
                id: existing_id.to_string(),
                display_name: "Present".to_owned(),
                mod_version: None,
                mod_count: 1,
                instance_path: None,
                last_opened: "2026-09-30T00:00:00Z".to_owned(),
                translation_progress: TranslationProgress::default(),
            },
            RecentProject {
                id: missing_id.to_string(),
                display_name: "Missing".to_owned(),
                mod_version: None,
                mod_count: 1,
                instance_path: None,
                last_opened: "2026-09-30T00:00:00Z".to_owned(),
                translation_progress: TranslationProgress::default(),
            },
        ],
    };
    assert_eq!(recent.retain_existing(temporary.path()), 1);
    assert_eq!(recent.projects[0].id, existing_id.to_string());
}

fn analysis_with_entries(entries: Vec<TranslationEntry>) -> AnalysisData {
    AnalysisData {
        mods: vec![ModRecord {
            mod_unit_id: 0,
            mod_id: Some("test".to_owned()),
            name: Some("Test mod".to_owned()),
            version: Some("1.0".to_owned()),
            loader: Some("fabric".to_owned()),
            minecraft_version: None,
            metadata_source: Some("fabric.mod.json".to_owned()),
        }],
        namespaces: vec![NamespaceRecord {
            mod_unit_id: 0,
            namespace: "test".to_owned(),
            container_path: String::new(),
            included: true,
            entry_count: entries.len(),
        }],
        nested_archives: vec![NestedArchiveRecord {
            mod_unit_id: 0,
            archive_path: "META-INF/jars/nested.jar".to_owned(),
            mod_id: Some("nested".to_owned()),
            name: Some("Nested".to_owned()),
            version: None,
            has_languages: true,
            skipped_reason: None,
        }],
        languages: vec![LanguageRecord {
            mod_unit_id: 0,
            namespace: "test".to_owned(),
            locale: "en_us".to_owned(),
            format: LanguageFileFormat::Json,
            container_path: String::new(),
            archive_path: "assets/test/lang/en_us.json".to_owned(),
            entry_count: entries.len(),
            is_target: false,
        }],
        entries,
    }
}

fn translation_entry(
    key: &str,
    source_text: &str,
    origin: TranslationOrigin,
    target_text: &str,
    flags: Vec<EntryFlag>,
) -> TranslationEntry {
    TranslationEntry {
        namespace: "test".to_owned(),
        key: key.to_owned(),
        source_text: Some(source_text.to_owned()),
        source_locale: Some("en_us".to_owned()),
        target_text: target_text.to_owned(),
        target_original_text: None,
        translated_source_text: None,
        origin,
        flags,
        sort_index: 0,
    }
}
