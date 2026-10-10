use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

use rusqlite::{Connection, OptionalExtension, params};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use yc_language::{EntryFlag, LanguageFileFormat, ReviewSettings};

use crate::{
    AnalysisData, EntryUpdate, GlossaryTerm, GlossaryTermInput, IncludedNamespace,
    JarReplacementRecord, ModUnitRecord, ProjectAssetIndex, ProjectEntry, ProjectEntrySummary,
    ProjectError, ProjectMetadata, ProjectReviewSettings, Result,
    database::{open_database, open_read_connection},
    model::{flag_from_db, flag_to_db, origin_from_db, origin_to_db},
};

// SQLiteのtrim()は半角スペースだけを除くため、Rustのstr::trim()と同じUnicode空白集合を渡す。
const SQLITE_WHITESPACE_CHARACTERS: &str = "char(9,10,11,12,13,32,133,160,5760,8192,8193,8194,8195,8196,8197,8198,8199,8200,8201,8202,8232,8233,8239,8287,12288)";

#[derive(Clone, Debug)]
pub struct Repository {
    database_path: PathBuf,
}

impl Repository {
    pub fn new(database_path: impl Into<PathBuf>) -> Self {
        Self {
            database_path: database_path.into(),
        }
    }

    /// Editor起動時に全translation entryとflagsをまとめて返す。
    pub fn list_entries(&self, mod_unit_id: Option<i64>) -> Result<Vec<ProjectEntry>> {
        let connection = open_read_connection(&self.database_path)?;
        list_entries(&connection, mod_unit_id)
    }

    pub fn list_glossary_terms(&self) -> Result<Vec<GlossaryTerm>> {
        let connection = open_read_connection(&self.database_path)?;
        list_glossary_terms(&connection)
    }

    pub fn review_settings(&self) -> Result<ProjectReviewSettings> {
        let connection = open_read_connection(&self.database_path)?;
        let (settings_json, setup_completed): (Option<String>, i64) = connection.query_row(
            "SELECT (SELECT review_settings_json FROM mod_units ORDER BY sort_index LIMIT 1),
                    setup_completed FROM project_meta LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let mut statement = connection.prepare(
            "SELECT mod_unit_id, namespace FROM namespaces
             WHERE included = 1 ORDER BY mod_unit_id, namespace",
        )?;
        let included_namespaces = statement
            .query_map([], |row| {
                Ok(IncludedNamespace {
                    mod_unit_id: row.get(0)?,
                    namespace: row.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut settings: ReviewSettings = settings_json
            .as_deref()
            .map(serde_json::from_str)
            .transpose()?
            .unwrap_or_default();
        settings.included_namespaces = included_namespaces
            .iter()
            .map(|selection| selection.namespace.clone())
            .collect();
        Ok(ProjectReviewSettings {
            source_locale: (!settings.source_locale.is_empty()).then_some(settings.source_locale),
            fallback_priority: settings.fallback_priority,
            preserve_existing_japanese: settings.preserve_existing_japanese,
            fill_missing_from_other_languages: settings.fill_missing_from_other_languages,
            setup_completed: setup_completed != 0,
            included_namespaces,
        })
    }

    /// Read the resolved Review settings saved for one mod unit.
    pub fn review_settings_for_unit(&self, mod_unit_id: i64) -> Result<ReviewSettings> {
        let connection = open_read_connection(&self.database_path)?;
        let settings_json: String = connection.query_row(
            "SELECT review_settings_json FROM mod_units WHERE id = ?1",
            [mod_unit_id],
            |row| row.get(0),
        )?;
        Ok(serde_json::from_str(&settings_json)?)
    }

    pub fn list_mod_units(&self) -> Result<Vec<ModUnitRecord>> {
        let connection = open_read_connection(&self.database_path)?;
        let mut statement = connection.prepare(
            "SELECT id, sort_index, source_filename, source_sha256, stored_path,
                    original_path, mod_id, name, version, loader, minecraft_version
             FROM mod_units ORDER BY sort_index, id",
        )?;
        statement
            .query_map([], |row| {
                let sort_index: i64 = row.get(1)?;
                Ok(ModUnitRecord {
                    id: row.get(0)?,
                    sort_index: usize::try_from(sort_index)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(1, sort_index))?,
                    source_filename: row.get(2)?,
                    source_sha256: row.get(3)?,
                    stored_path: row.get(4)?,
                    original_path: row.get(5)?,
                    mod_id: row.get(6)?,
                    name: row.get(7)?,
                    version: row.get(8)?,
                    loader: row.get(9)?,
                    minecraft_version: row.get(10)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn rename_project(&self, display_name: &str) -> Result<()> {
        let display_name = crate::project::normalize_project_name(display_name)?;
        let connection = open_database(&self.database_path)?;
        let changed = connection.execute(
            "UPDATE project_meta SET display_name = ?1, updated_at = ?2",
            params![display_name, now_rfc3339()?],
        )?;
        if changed == 0 {
            return Err(ProjectError::ProjectNotFound(self.database_path.clone()));
        }
        Ok(())
    }

    pub fn has_unrestored_jar_replacements(&self, mod_unit_id: i64) -> Result<bool> {
        let connection = open_read_connection(&self.database_path)?;
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM jar_replacements WHERE mod_unit_id = ?1 AND restored_at IS NULL",
            [mod_unit_id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    pub fn remove_mod_unit(&self, mod_unit_id: i64) -> Result<()> {
        let source_path = self.mod_unit_source_path(mod_unit_id)?;
        let mut connection = open_database(&self.database_path)?;
        let unit_count: i64 =
            connection.query_row("SELECT COUNT(*) FROM mod_units", [], |row| row.get(0))?;
        if unit_count <= 1 {
            return Err(ProjectError::LastModUnit);
        }

        // Move the JAR aside before committing the cascading DB deletion. A DB failure can then
        // restore the original path; after commit, failure to purge the staged file must not leave
        // the caller believing the mod still exists when its translations have already been deleted.
        let staged_path = crate::files::temporary_sibling(&source_path);
        let staged_source = match fs::rename(&source_path, &staged_path) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(error.into()),
        };
        if let Err(error) = remove_mod_unit(&mut connection, mod_unit_id, true) {
            drop(connection);
            if staged_source && let Err(restore_error) = fs::rename(&staged_path, &source_path) {
                tracing::error!(
                    source = %source_path.display(),
                    staged = %staged_path.display(),
                    %error,
                    %restore_error,
                    "stored mod JAR could not be restored after a failed database deletion"
                );
            }
            return Err(error);
        }
        drop(connection);
        if staged_source && let Err(error) = fs::remove_file(&staged_path) {
            // The DB commit is final and the original stored_path is gone, so report success
            // to let the active project and UI synchronize; the leftover is unreferenced.
            tracing::warn!(
                path = %staged_path.display(),
                %error,
                "deleted mod JAR could not be purged after database commit"
            );
        }
        Ok(())
    }

    pub fn list_jar_replacements(
        &self,
        include_restored: bool,
    ) -> Result<Vec<JarReplacementRecord>> {
        let connection = open_read_connection(&self.database_path)?;
        let sql = if include_restored {
            "SELECT id, mod_unit_id, target_path, backup_path, previous_sha256,
                    written_sha256, created_at, restored_at
             FROM jar_replacements ORDER BY created_at DESC, id DESC"
        } else {
            "SELECT id, mod_unit_id, target_path, backup_path, previous_sha256,
                    written_sha256, created_at, restored_at
             FROM jar_replacements WHERE restored_at IS NULL
             ORDER BY created_at DESC, id DESC"
        };
        let mut statement = connection.prepare(sql)?;
        statement
            .query_map([], jar_replacement_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn active_jar_replacement(
        &self,
        mod_unit_id: i64,
        target_path: &str,
    ) -> Result<Option<JarReplacementRecord>> {
        let connection = open_read_connection(&self.database_path)?;
        connection
            .query_row(
                "SELECT id, mod_unit_id, target_path, backup_path, previous_sha256,
                        written_sha256, created_at, restored_at
                 FROM jar_replacements
                 WHERE mod_unit_id = ?1 AND target_path = ?2 AND restored_at IS NULL",
                rusqlite::params![mod_unit_id, target_path],
                jar_replacement_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn mod_unit_source_path(&self, mod_unit_id: i64) -> Result<PathBuf> {
        let connection = open_read_connection(&self.database_path)?;
        let stored_path: String = connection.query_row(
            "SELECT stored_path FROM mod_units WHERE id = ?1",
            [mod_unit_id],
            |row| row.get(0),
        )?;
        Ok(self
            .database_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(stored_path))
    }

    pub fn project_metadata(&self) -> Result<ProjectMetadata> {
        let connection = open_read_connection(&self.database_path)?;
        let mods = {
            let mut statement = connection.prepare(
                "SELECT mod_unit_id, mod_id, name, version, loader, minecraft_version,
                        metadata_source FROM mods ORDER BY mod_unit_id, id",
            )?;
            statement
                .query_map([], |row| {
                    Ok(crate::ModRecord {
                        mod_unit_id: row.get(0)?,
                        mod_id: row.get(1)?,
                        name: row.get(2)?,
                        version: row.get(3)?,
                        loader: row.get(4)?,
                        minecraft_version: row.get(5)?,
                        metadata_source: row.get(6)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let namespaces = {
            let mut statement = connection.prepare(
                "SELECT mod_unit_id, namespace, container_path, included, entry_count
                 FROM namespaces ORDER BY mod_unit_id, namespace",
            )?;
            statement
                .query_map([], |row| {
                    let entry_count: i64 = row.get(4)?;
                    Ok(crate::NamespaceRecord {
                        mod_unit_id: row.get(0)?,
                        namespace: row.get(1)?,
                        container_path: row.get(2)?,
                        included: row.get::<_, i64>(3)? != 0,
                        entry_count: usize::try_from(entry_count).map_err(|_| {
                            rusqlite::Error::IntegralValueOutOfRange(4, entry_count)
                        })?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let nested_archives = {
            let mut statement = connection.prepare(
                "SELECT mod_unit_id, archive_path, mod_id, name, version, has_languages,
                        skipped_reason FROM nested_archives ORDER BY mod_unit_id, archive_path",
            )?;
            statement
                .query_map([], |row| {
                    Ok(crate::NestedArchiveRecord {
                        mod_unit_id: row.get(0)?,
                        archive_path: row.get(1)?,
                        mod_id: row.get(2)?,
                        name: row.get(3)?,
                        version: row.get(4)?,
                        has_languages: row.get::<_, i64>(5)? != 0,
                        skipped_reason: row.get(6)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let languages = {
            let mut statement = connection.prepare(
                "SELECT mod_unit_id, namespace, locale, format, container_path, archive_path,
                        entry_count, is_target FROM languages
                 ORDER BY mod_unit_id, namespace, container_path, archive_path, locale",
            )?;
            statement
                .query_map([], |row| {
                    let format: String = row.get(3)?;
                    let format = match format.as_str() {
                        "json" => LanguageFileFormat::Json,
                        "legacy_lang" => LanguageFileFormat::LegacyLang,
                        _ => {
                            return Err(rusqlite::Error::InvalidColumnType(
                                3,
                                "format".to_owned(),
                                rusqlite::types::Type::Text,
                            ));
                        }
                    };
                    let entry_count: i64 = row.get(6)?;
                    Ok(crate::LanguageRecord {
                        mod_unit_id: row.get(0)?,
                        namespace: row.get(1)?,
                        locale: row.get(2)?,
                        format,
                        container_path: row.get(4)?,
                        archive_path: row.get(5)?,
                        entry_count: usize::try_from(entry_count).map_err(|_| {
                            rusqlite::Error::IntegralValueOutOfRange(6, entry_count)
                        })?,
                        is_target: row.get::<_, i64>(7)? != 0,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        Ok(ProjectMetadata {
            mods,
            namespaces,
            nested_archives,
            languages,
        })
    }

    pub fn entry_summary(&self, mod_unit_id: Option<i64>) -> Result<ProjectEntrySummary> {
        let connection = open_read_connection(&self.database_path)?;
        let filter = if mod_unit_id.is_some() {
            "WHERE mod_unit_id = ?1"
        } else {
            ""
        };
        // 空白だけの値は翻訳未完了として、軽量スキャンとReviewの集計基準に揃える。
        let sql = format!(
            "SELECT COUNT(*),
                    COALESCE(SUM(CASE WHEN target_text IS NOT NULL
                        AND trim(target_text, {SQLITE_WHITESPACE_CHARACTERS}) <> ''
                        THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(EXISTS(SELECT 1 FROM entry_flags WHERE entry_id = translation_entries.id AND flag = 'needs_review')), 0),
                    COALESCE(SUM(CASE WHEN target_original_text IS NOT NULL
                        AND trim(target_original_text, {SQLITE_WHITESPACE_CHARACTERS}) <> ''
                        THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(EXISTS(SELECT 1 FROM entry_flags WHERE entry_id = translation_entries.id AND flag IN ('asset_missing', 'asset_ambiguous'))), 0)
             FROM translation_entries {filter}"
        );
        let (total, translated, needs_review, existing_japanese, unresolved_assets): (
            i64,
            i64,
            i64,
            i64,
            i64,
        ) = if let Some(mod_unit_id) = mod_unit_id {
            connection.query_row(&sql, [mod_unit_id], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })?
        } else {
            connection.query_row(&sql, [], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })?
        };
        let to_usize = |value| {
            usize::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, value))
        };
        Ok(ProjectEntrySummary {
            total: to_usize(total)?,
            translated: to_usize(translated)?,
            needs_review: to_usize(needs_review)?,
            existing_japanese: to_usize(existing_japanese)?,
            unresolved_assets: to_usize(unresolved_assets)?,
        })
    }

    pub fn asset_type_counts(&self, mod_unit_id: Option<i64>) -> Result<HashMap<String, usize>> {
        let connection = open_read_connection(&self.database_path)?;
        let mut counts = HashMap::new();
        {
            let mut statement = connection.prepare(
                "SELECT asset_type, SUM(asset_count) FROM asset_type_counts
                 WHERE (?1 IS NULL OR mod_unit_id = ?1) GROUP BY asset_type",
            )?;
            for record in statement.query_map([mod_unit_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })? {
                let (asset_type, count) = record?;
                let count = usize::try_from(count)
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(1, count))?;
                counts.insert(asset_type, count);
            }
        }

        // Projects created before aggregate counts were introduced still have their full asset
        // rows. Read those only for units without an aggregate so opening existing projects keeps
        // their preview summary while new projects avoid storing a second copy of archive paths.
        let mut statement = connection.prepare(
            "SELECT assets.asset_type, COUNT(*) FROM assets
             WHERE (?1 IS NULL OR assets.mod_unit_id = ?1)
               AND NOT EXISTS (
                   SELECT 1 FROM asset_type_counts
                   WHERE asset_type_counts.mod_unit_id = assets.mod_unit_id
               )
             GROUP BY assets.mod_unit_id, assets.asset_type",
        )?;
        for record in statement.query_map([mod_unit_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })? {
            let (asset_type, count) = record?;
            let count = usize::try_from(count)
                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(1, count))?;
            *counts.entry(asset_type).or_default() += count;
        }
        Ok(counts)
    }
}

fn list_glossary_terms(connection: &Connection) -> Result<Vec<GlossaryTerm>> {
    let mut statement = connection.prepare(
        "SELECT id, source_term, target_term, note, case_sensitive, created_at, updated_at
         FROM glossary_terms ORDER BY source_term COLLATE NOCASE, source_term, id",
    )?;
    statement
        .query_map([], |row| {
            Ok(GlossaryTerm {
                id: row.get(0)?,
                source_term: row.get(1)?,
                target_term: row.get(2)?,
                note: row.get(3)?,
                case_sensitive: row.get::<_, i64>(4)? != 0,
                created_at: row.get(5)?,
                updated_at: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

pub(crate) fn upsert_glossary_terms(
    connection: &mut Connection,
    terms: Vec<GlossaryTermInput>,
) -> Result<()> {
    let transaction = connection.transaction()?;
    for term in terms {
        let source_term = term.source_term.trim();
        let target_term = term.target_term.trim();
        if source_term.is_empty() || target_term.is_empty() {
            return Err(ProjectError::InvalidDatabaseValue(
                "glossary source and target terms must not be empty".to_owned(),
            ));
        }
        let now = now_rfc3339()?;
        let existing_id: Option<i64> = transaction
            .query_row(
                "SELECT id FROM glossary_terms
                 WHERE source_term = ?1
                    OR (?2 = 0 AND case_sensitive = 0 AND lower(source_term) = lower(?1))
                 ORDER BY CASE WHEN ?2 = 0 AND case_sensitive = 0 THEN 0 ELSE 1 END
                 LIMIT 1",
                rusqlite::params![source_term, term.case_sensitive as i64],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(id) = existing_id {
            transaction.execute(
                "UPDATE glossary_terms
                 SET source_term = ?1, target_term = ?2, note = ?3,
                     case_sensitive = ?4, updated_at = ?5
                 WHERE id = ?6",
                rusqlite::params![
                    source_term,
                    target_term,
                    term.note
                        .as_deref()
                        .map(str::trim)
                        .filter(|note| !note.is_empty()),
                    term.case_sensitive as i64,
                    now,
                    id
                ],
            )?;
        } else {
            transaction.execute(
                "INSERT INTO glossary_terms
                    (source_term, target_term, note, case_sensitive, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                rusqlite::params![
                    source_term,
                    target_term,
                    term.note
                        .as_deref()
                        .map(str::trim)
                        .filter(|note| !note.is_empty()),
                    term.case_sensitive as i64,
                    now
                ],
            )?;
        }
    }
    transaction.commit()?;
    Ok(())
}

pub(crate) fn update_glossary_term(
    connection: &mut Connection,
    id: i64,
    term: GlossaryTermInput,
) -> Result<()> {
    let source_term = term.source_term.trim();
    let target_term = term.target_term.trim();
    if source_term.is_empty() || target_term.is_empty() {
        return Err(ProjectError::InvalidDatabaseValue(
            "glossary source and target terms must not be empty".to_owned(),
        ));
    }

    let transaction = connection.transaction()?;
    let exists: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM glossary_terms WHERE id = ?1)",
        [id],
        |row| row.get(0),
    )?;
    if !exists {
        return Err(ProjectError::InvalidDatabaseValue(format!(
            "glossary term {id} was not found"
        )));
    }

    let conflicting_id: Option<i64> = transaction
        .query_row(
            "SELECT id FROM glossary_terms
             WHERE id <> ?1
               AND (source_term = ?2
                    OR (?3 = 0 AND case_sensitive = 0 AND lower(source_term) = lower(?2)))
             LIMIT 1",
            rusqlite::params![id, source_term, term.case_sensitive as i64],
            |row| row.get(0),
        )
        .optional()?;
    if conflicting_id.is_some() {
        return Err(ProjectError::InvalidDatabaseValue(
            "glossary source term already exists".to_owned(),
        ));
    }

    // Inline edits retain identity even when the source term changes; CSV imports keep using the
    // source-keyed upsert above because each imported row intentionally merges by source term.
    let changed = transaction.execute(
        "UPDATE glossary_terms
         SET source_term = ?1, target_term = ?2, note = ?3,
             case_sensitive = ?4, updated_at = ?5
         WHERE id = ?6",
        rusqlite::params![
            source_term,
            target_term,
            term.note
                .as_deref()
                .map(str::trim)
                .filter(|note| !note.is_empty()),
            term.case_sensitive as i64,
            now_rfc3339()?,
            id
        ],
    )?;
    if changed != 1 {
        return Err(ProjectError::InvalidDatabaseValue(format!(
            "glossary term {id} was not updated"
        )));
    }
    transaction.commit()?;
    Ok(())
}

pub(crate) fn delete_glossary_term(connection: &mut Connection, id: i64) -> Result<()> {
    connection.execute("DELETE FROM glossary_terms WHERE id = ?1", [id])?;
    Ok(())
}

fn jar_replacement_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<JarReplacementRecord> {
    Ok(JarReplacementRecord {
        id: row.get(0)?,
        mod_unit_id: row.get(1)?,
        target_path: row.get(2)?,
        backup_path: row.get(3)?,
        previous_sha256: row.get(4)?,
        written_sha256: row.get(5)?,
        created_at: row.get(6)?,
        restored_at: row.get(7)?,
    })
}

pub(crate) fn save_jar_replacement(
    connection: &mut Connection,
    replacement: JarReplacementRecord,
) -> Result<()> {
    let transaction = connection.transaction()?;
    let active_id = transaction
        .query_row(
            "SELECT id FROM jar_replacements
             WHERE mod_unit_id = ?1 AND target_path = ?2 AND restored_at IS NULL",
            rusqlite::params![replacement.mod_unit_id, replacement.target_path],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    if let Some(id) = active_id {
        // 再上書き時は初回の元JARバックアップとprevious hashを維持する。
        transaction.execute(
            "UPDATE jar_replacements SET written_sha256 = ?1
             WHERE id = ?2 AND restored_at IS NULL",
            rusqlite::params![replacement.written_sha256, id],
        )?;
    } else {
        transaction.execute(
            "INSERT INTO jar_replacements (
                mod_unit_id, target_path, backup_path, previous_sha256,
                written_sha256, created_at, restored_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
            rusqlite::params![
                replacement.mod_unit_id,
                replacement.target_path,
                replacement.backup_path,
                replacement.previous_sha256,
                replacement.written_sha256,
                replacement.created_at,
            ],
        )?;
    }
    transaction.commit()?;
    Ok(())
}

pub(crate) fn mark_jar_replacement_restored(
    connection: &mut Connection,
    id: i64,
    restored_at: &str,
) -> Result<()> {
    let changed = connection.execute(
        "UPDATE jar_replacements SET restored_at = ?1
         WHERE id = ?2 AND restored_at IS NULL",
        rusqlite::params![restored_at, id],
    )?;
    if changed == 0 {
        return Err(ProjectError::InvalidDatabaseValue(format!(
            "active jar_replacements.id={id} was not found"
        )));
    }
    Ok(())
}

pub(crate) fn save_analysis(
    connection: &mut Connection,
    mod_unit_id: i64,
    analysis: AnalysisData,
) -> Result<()> {
    let transaction = connection.transaction()?;
    transaction.execute(
        "DELETE FROM translation_entries WHERE mod_unit_id = ?1",
        [mod_unit_id],
    )?;
    transaction.execute("DELETE FROM assets WHERE mod_unit_id = ?1", [mod_unit_id])?;
    transaction.execute(
        "DELETE FROM languages WHERE mod_unit_id = ?1",
        [mod_unit_id],
    )?;
    transaction.execute(
        "DELETE FROM namespaces WHERE mod_unit_id = ?1",
        [mod_unit_id],
    )?;
    transaction.execute(
        "DELETE FROM nested_archives WHERE mod_unit_id = ?1",
        [mod_unit_id],
    )?;
    transaction.execute("DELETE FROM mods WHERE mod_unit_id = ?1", [mod_unit_id])?;

    let root_mod = analysis.mods.first().cloned();

    for record in analysis.mods {
        transaction.execute(
            "INSERT INTO mods (
                mod_unit_id, mod_id, name, version, loader, minecraft_version, metadata_source
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                mod_unit_id,
                record.mod_id,
                record.name,
                record.version,
                record.loader,
                record.minecraft_version,
                record.metadata_source
            ],
        )?;
    }
    for record in analysis.namespaces {
        transaction.execute(
            "INSERT INTO namespaces (
                mod_unit_id, namespace, container_path, included, entry_count
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                mod_unit_id,
                record.namespace,
                record.container_path,
                record.included,
                usize_to_i64(record.entry_count, "namespace.entry_count")?
            ],
        )?;
    }
    for record in analysis.nested_archives {
        transaction.execute(
            "INSERT INTO nested_archives (
                mod_unit_id, archive_path, mod_id, name, version, has_languages, skipped_reason
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                mod_unit_id,
                record.archive_path,
                record.mod_id,
                record.name,
                record.version,
                record.has_languages,
                record.skipped_reason
            ],
        )?;
    }
    for record in analysis.languages {
        let format = match record.format {
            LanguageFileFormat::Json => "json",
            LanguageFileFormat::LegacyLang => "legacy_lang",
        };
        transaction.execute(
            "INSERT INTO languages (
                mod_unit_id, namespace, locale, format, container_path, archive_path,
                entry_count, is_target
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                mod_unit_id,
                record.namespace,
                record.locale,
                format,
                record.container_path,
                record.archive_path,
                usize_to_i64(record.entry_count, "language.entry_count")?,
                record.is_target
            ],
        )?;
    }

    let now = now_rfc3339()?;
    // Create / Reviewは数千entryを一括保存する。executeのたびにSQLをprepareし直さないよう、
    // entry本体とflagのstatementをtransaction内で再利用する。
    let mut insert_entry = transaction.prepare_cached(
        "INSERT INTO translation_entries (
            mod_unit_id, namespace, translation_key, source_text, source_locale, target_text,
            target_original_text, translated_source_text, target_origin, sort_index, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
    )?;
    let mut insert_entry_flag = transaction
        .prepare_cached("INSERT OR IGNORE INTO entry_flags (entry_id, flag) VALUES (?1, ?2)")?;
    for entry in analysis.entries {
        let target_text = if entry.origin == yc_language::TranslationOrigin::Missing
            && entry.target_text.is_empty()
        {
            None
        } else {
            Some(entry.target_text.as_str())
        };
        let translated_source_text = entry.translated_source_text.as_deref().or_else(|| {
            (target_text.is_some())
                .then_some(entry.source_text.as_deref())
                .flatten()
        });
        insert_entry.execute(params![
            mod_unit_id,
            entry.namespace,
            entry.key,
            entry.source_text,
            entry.source_locale,
            target_text,
            entry.target_original_text,
            translated_source_text,
            origin_to_db(&entry.origin),
            usize_to_i64(entry.sort_index, "translation_entry.sort_index")?,
            now
        ])?;
        let entry_id = transaction.last_insert_rowid();
        for flag in entry.flags {
            insert_entry_flag.execute(params![entry_id, flag_to_db(&flag)])?;
        }
    }
    drop(insert_entry_flag);
    drop(insert_entry);
    let (unit_name, unit_mod_id, unit_version, unit_loader, unit_minecraft_version) = root_mod
        .map(|record| {
            (
                record.name,
                record.mod_id,
                record.version,
                record.loader,
                record.minecraft_version,
            )
        })
        .unwrap_or_default();
    let (unit_count, instance_path, fallback_filename, current_display_name, previous_unit_name): (
        i64,
        Option<String>,
        String,
        String,
        Option<String>,
    ) = transaction.query_row(
        "SELECT (SELECT COUNT(*) FROM mod_units), instance_path,
                (SELECT source_filename FROM mod_units WHERE id = ?1), display_name,
                (SELECT name FROM mod_units WHERE id = ?1)
         FROM project_meta LIMIT 1",
        [mod_unit_id],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        },
    )?;
    let multi_unit_name = instance_path
        .as_deref()
        .and_then(|path| Path::new(path).file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| format!("{unit_count} 個の mod"));
    let previous_automatic_name = if unit_count == 1 {
        previous_unit_name
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| fallback_filename.clone())
    } else {
        multi_unit_name.clone()
    };
    let next_automatic_name = if unit_count == 1 {
        unit_name
            .as_deref()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or(&fallback_filename)
            .to_owned()
    } else {
        multi_unit_name
    };
    transaction.execute(
        "UPDATE mod_units SET mod_id = ?1, name = ?2, version = ?3,
             loader = ?4, minecraft_version = ?5 WHERE id = ?6",
        params![
            unit_mod_id,
            unit_name,
            unit_version,
            unit_loader,
            unit_minecraft_version,
            mod_unit_id
        ],
    )?;
    // 既存スキーマでは自動名と手動名を分けて記録しないため、保存前の自動名と
    // 現在名が一致するときだけ追解析後のMod名へ更新し、ユーザーの改名を保護する。
    if current_display_name == previous_automatic_name {
        transaction.execute(
            "UPDATE project_meta SET display_name = ?1 WHERE id = (SELECT id FROM project_meta LIMIT 1)",
            [&next_automatic_name],
        )?;
    }
    update_project_timestamp(&transaction, &now)?;
    transaction.commit()?;
    tracing::info!("analysis records saved to Project database");
    Ok(())
}

pub(crate) fn save_asset_index(
    connection: &mut Connection,
    mod_unit_id: i64,
    index: ProjectAssetIndex,
) -> Result<()> {
    let ProjectAssetIndex { assets, matches } = index;
    let cleanup_started = Instant::now();
    let transaction = connection.transaction()?;
    transaction.execute("DELETE FROM assets WHERE mod_unit_id = ?1", [mod_unit_id])?;
    transaction.execute(
        "DELETE FROM asset_type_counts WHERE mod_unit_id = ?1",
        [mod_unit_id],
    )?;
    transaction.execute(
        "UPDATE translation_entries
         SET asset_kind = NULL, asset_id = NULL, asset_confidence = NULL
         WHERE mod_unit_id = ?1",
        [mod_unit_id],
    )?;
    transaction.execute(
        "DELETE FROM entry_flags WHERE flag IN ('asset_missing', 'asset_ambiguous')
         AND entry_id IN (SELECT id FROM translation_entries WHERE mod_unit_id = ?1)",
        [mod_unit_id],
    )?;
    tracing::info!(
        stage = "asset_index_database_cleanup",
        elapsed_ms = cleanup_started.elapsed().as_millis(),
        "Project asset database phase completed"
    );

    // The UI reads per-entry flags and aggregate asset-type counts; previews resolve assets from
    // the immutable project JARs. Duplicating every path and link in SQLite only inflates large projects.
    let assets_started = Instant::now();
    let asset_count = assets.len();
    let mut counts_by_type = HashMap::<String, usize>::new();
    {
        let mut insert_count = transaction.prepare_cached(
            "INSERT INTO asset_type_counts (mod_unit_id, asset_type, asset_count)
             VALUES (?1, ?2, ?3)",
        )?;
        for asset in assets {
            *counts_by_type.entry(asset.asset_type.clone()).or_default() += 1;
        }
        for (asset_type, count) in counts_by_type {
            let count = i64::try_from(count).map_err(|_| {
                ProjectError::InvalidDatabaseValue(format!(
                    "asset count is too large for type {asset_type}"
                ))
            })?;
            insert_count.execute(params![mod_unit_id, asset_type, count])?;
        }
    }
    tracing::info!(
        stage = "asset_index_database_assets",
        elapsed_ms = assets_started.elapsed().as_millis(),
        asset_count,
        "Project asset database phase completed"
    );

    // 各mappingごとにSELECTを行うとCreateの数千キーでSQLite round-tripが積み上がる。
    // 一度だけ全entry IDを読み、以後はメモリ上の複合キーで参照する。
    let entry_ids_started = Instant::now();
    let entry_ids = {
        let mut statement = transaction.prepare(
            "SELECT id, namespace, translation_key FROM translation_entries WHERE mod_unit_id = ?1",
        )?;
        statement
            .query_map([mod_unit_id], |row| {
                Ok((
                    (row.get::<_, String>(1)?, row.get::<_, String>(2)?),
                    row.get::<_, i64>(0)?,
                ))
            })?
            .collect::<rusqlite::Result<HashMap<_, _>>>()?
    };
    tracing::info!(
        stage = "asset_index_database_entry_ids",
        elapsed_ms = entry_ids_started.elapsed().as_millis(),
        entry_count = entry_ids.len(),
        "Project asset database phase completed"
    );
    let mappings_started = Instant::now();
    let mut update_entry = transaction.prepare_cached(
        "UPDATE translation_entries
         SET asset_kind = ?1, asset_id = ?2, asset_confidence = ?3
         WHERE id = ?4",
    )?;
    let mut insert_entry_flag = transaction
        .prepare_cached("INSERT OR IGNORE INTO entry_flags (entry_id, flag) VALUES (?1, ?2)")?;
    for mapping in &matches {
        let entry_id = entry_ids
            .get(&(mapping.namespace.clone(), mapping.key.clone()))
            .copied()
            .ok_or_else(|| {
                ProjectError::InvalidDatabaseValue(format!(
                    "asset mapping entry not found: {}:{}",
                    mapping.namespace, mapping.key
                ))
            })?;
        update_entry.execute(params![
            mapping.asset_kind,
            mapping.asset_id,
            mapping.confidence,
            entry_id
        ])?;
        if mapping.missing {
            insert_entry_flag.execute(params![entry_id, flag_to_db(&EntryFlag::AssetMissing)])?;
        }
        if mapping.ambiguous {
            insert_entry_flag.execute(params![entry_id, flag_to_db(&EntryFlag::AssetAmbiguous)])?;
        }
    }
    tracing::info!(
        stage = "asset_index_database_matches",
        elapsed_ms = mappings_started.elapsed().as_millis(),
        match_count = matches.len(),
        "Project asset database phase completed"
    );

    drop(insert_entry_flag);
    drop(update_entry);

    let commit_started = Instant::now();
    let now = now_rfc3339()?;
    update_project_timestamp(&transaction, &now)?;
    transaction.commit()?;
    tracing::info!(
        stage = "asset_index_database_commit",
        elapsed_ms = commit_started.elapsed().as_millis(),
        "Project asset database phase completed"
    );
    tracing::info!(asset_count, "Project asset index saved");
    Ok(())
}

pub(crate) fn save_review_settings(
    connection: &mut Connection,
    mod_unit_id: i64,
    settings: ReviewSettings,
    setup_completed: bool,
) -> Result<()> {
    let transaction = connection.transaction()?;
    let settings_json = serde_json::to_string(&settings)?;
    let now = now_rfc3339()?;
    transaction.execute(
        "UPDATE project_meta
         SET setup_completed = ?1, updated_at = ?2",
        params![setup_completed, now],
    )?;
    transaction.execute(
        "UPDATE mod_units SET review_settings_json = ?1 WHERE id = ?2",
        params![settings_json, mod_unit_id],
    )?;
    transaction.execute(
        "UPDATE namespaces SET included = 0 WHERE mod_unit_id = ?1",
        [mod_unit_id],
    )?;
    for namespace in &settings.included_namespaces {
        transaction.execute(
            "UPDATE namespaces SET included = 1 WHERE mod_unit_id = ?1 AND namespace = ?2",
            params![mod_unit_id, namespace],
        )?;
    }
    transaction.commit()?;
    tracing::debug!(setup_completed, "Project review settings saved");
    Ok(())
}

pub(crate) fn remove_mod_unit(
    connection: &mut Connection,
    mod_unit_id: i64,
    preserve_project_name: bool,
) -> Result<()> {
    let transaction = connection.transaction()?;
    let removed = transaction.execute("DELETE FROM mod_units WHERE id = ?1", [mod_unit_id])?;
    if removed == 0 {
        return Err(ProjectError::InvalidDatabaseValue(format!(
            "mod unit {mod_unit_id} does not exist"
        )));
    }
    let updated_at = now_rfc3339()?;
    if preserve_project_name {
        transaction.execute("UPDATE project_meta SET updated_at = ?1", [&updated_at])?;
    } else {
        // Import cleanup removes invalid source units before the user can rename the project,
        // so keep the automatically generated name in sync with the remaining sources.
        let (unit_count, instance_path): (i64, Option<String>) = transaction.query_row(
            "SELECT (SELECT COUNT(*) FROM mod_units), instance_path FROM project_meta LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let display_name = match unit_count {
            1 => transaction.query_row(
                "SELECT COALESCE(NULLIF(trim(name), ''), source_filename) FROM mod_units LIMIT 1",
                [],
                |row| row.get::<_, String>(0),
            )?,
            count => instance_path
                .as_deref()
                .and_then(|path| Path::new(path).file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| format!("{count} 個の mod")),
        };
        transaction.execute(
            "UPDATE project_meta SET display_name = ?1, updated_at = ?2",
            params![display_name, updated_at],
        )?;
    }
    transaction.commit()?;
    Ok(())
}

pub(crate) fn update_entries(connection: &mut Connection, updates: Vec<EntryUpdate>) -> Result<()> {
    let transaction = connection.transaction()?;
    let now = now_rfc3339()?;
    for update in updates {
        let source_text: Option<String> = transaction
            .query_row(
                "SELECT source_text FROM translation_entries WHERE id = ?1",
                [update.id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(ProjectError::EntryNotFound(update.id))?;
        let origin = match update.origin {
            crate::UpdateOrigin::Ai => "ai",
            crate::UpdateOrigin::Manual => "edited",
        };
        let changed = transaction.execute(
            "UPDATE translation_entries
             SET target_text = ?1, translated_source_text = ?2, target_origin = ?3, updated_at = ?4
             WHERE id = ?5",
            params![update.target_text, source_text, origin, now, update.id],
        )?;
        if changed == 0 {
            return Err(ProjectError::EntryNotFound(update.id));
        }
    }
    update_project_timestamp(&transaction, &now)?;
    transaction.commit()?;
    Ok(())
}

pub(crate) fn add_flag(connection: &mut Connection, entry_id: i64, flag: EntryFlag) -> Result<()> {
    let transaction = connection.transaction()?;
    insert_flag(&transaction, entry_id, &flag)?;
    let now = now_rfc3339()?;
    update_project_timestamp(&transaction, &now)?;
    transaction.commit()?;
    Ok(())
}

pub(crate) fn remove_flag(
    connection: &mut Connection,
    entry_id: i64,
    flag: EntryFlag,
) -> Result<()> {
    let transaction = connection.transaction()?;
    ensure_entry_exists(&transaction, entry_id)?;
    transaction.execute(
        "DELETE FROM entry_flags WHERE entry_id = ?1 AND flag = ?2",
        params![entry_id, flag_to_db(&flag)],
    )?;
    let now = now_rfc3339()?;
    update_project_timestamp(&transaction, &now)?;
    transaction.commit()?;
    Ok(())
}

fn list_entries(connection: &Connection, mod_unit_id: Option<i64>) -> Result<Vec<ProjectEntry>> {
    let raw_entries = {
        let mut statement = connection.prepare(
            "SELECT id, mod_unit_id, namespace, translation_key, source_text, source_locale,
                    target_text, target_original_text, translated_source_text, target_origin,
                    sort_index
             FROM translation_entries
             WHERE (?1 IS NULL OR mod_unit_id = ?1)
             ORDER BY mod_unit_id, sort_index, id",
        )?;
        statement
            .query_map([mod_unit_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, i64>(10)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };

    let mut indexes_by_id = HashMap::with_capacity(raw_entries.len());
    let mut entries = Vec::with_capacity(raw_entries.len());
    for (index, raw) in raw_entries.into_iter().enumerate() {
        let (
            id,
            mod_unit_id,
            namespace,
            key,
            source_text,
            source_locale,
            target_text,
            target_original_text,
            translated_source_text,
            origin,
            sort_index,
        ) = raw;
        entries.push(ProjectEntry {
            id,
            mod_unit_id,
            namespace,
            key,
            source_text,
            source_locale,
            target_text,
            target_original_text,
            translated_source_text,
            origin: origin_from_db(&origin)?,
            sort_index: i64_to_usize(sort_index, "translation_entry.sort_index")?,
            flags: Vec::new(),
        });
        indexes_by_id.insert(id, index);
    }

    let mut statement = connection.prepare(
        "SELECT entry_id, flag FROM entry_flags
         WHERE (?1 IS NULL OR entry_id IN (
             SELECT id FROM translation_entries WHERE mod_unit_id = ?1
         )) ORDER BY entry_id, flag",
    )?;
    let flags = statement
        .query_map([mod_unit_id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (entry_id, flag) in flags {
        let index = indexes_by_id.get(&entry_id).copied().ok_or_else(|| {
            ProjectError::InvalidDatabaseValue(format!("orphan entry flag for id {entry_id}"))
        })?;
        entries[index].flags.push(flag_from_db(&flag)?);
    }
    Ok(entries)
}

fn insert_flag(
    transaction: &rusqlite::Transaction<'_>,
    entry_id: i64,
    flag: &EntryFlag,
) -> Result<()> {
    transaction.execute(
        "INSERT OR IGNORE INTO entry_flags (entry_id, flag) VALUES (?1, ?2)",
        params![entry_id, flag_to_db(flag)],
    )?;
    Ok(())
}

fn ensure_entry_exists(transaction: &rusqlite::Transaction<'_>, entry_id: i64) -> Result<()> {
    let exists: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM translation_entries WHERE id = ?1)",
        [entry_id],
        |row| row.get(0),
    )?;
    if exists {
        Ok(())
    } else {
        Err(ProjectError::EntryNotFound(entry_id))
    }
}

fn update_project_timestamp(
    transaction: &rusqlite::Transaction<'_>,
    timestamp: &str,
) -> Result<()> {
    transaction.execute("UPDATE project_meta SET updated_at = ?1", [timestamp])?;
    Ok(())
}

fn usize_to_i64(value: usize, label: &str) -> Result<i64> {
    i64::try_from(value).map_err(|_| {
        ProjectError::InvalidDatabaseValue(format!("{label} exceeds SQLite INTEGER range"))
    })
}

fn i64_to_usize(value: i64, label: &str) -> Result<usize> {
    usize::try_from(value)
        .map_err(|_| ProjectError::InvalidDatabaseValue(format!("{label} is negative")))
}

fn now_rfc3339() -> Result<String> {
    Ok(OffsetDateTime::now_utc().format(&Rfc3339)?)
}
