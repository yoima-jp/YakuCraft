use std::{fs, path::Path, time::Duration};

use rusqlite::{Connection, MAIN_DB, OpenFlags};

use crate::{ProjectError, Result, files};

mod embedded_migrations {
    include!(concat!(env!("OUT_DIR"), "/migrations.rs"));
}

pub(crate) fn open_database(path: &Path) -> Result<Connection> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let existed_before_open = path.exists();
    let mut connection = Connection::open(path)?;
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    let journal_mode = connection.query_row("PRAGMA journal_mode = WAL", [], |row| {
        row.get::<_, String>(0)
    })?;
    if !journal_mode.eq_ignore_ascii_case("wal") {
        return Err(ProjectError::InvalidDatabaseValue(format!(
            "journal_mode={journal_mode}"
        )));
    }
    migrate(&mut connection, path, existed_before_open)?;
    Ok(connection)
}

pub(crate) fn open_read_connection(path: &Path) -> Result<Connection> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    Ok(connection)
}

fn migrate(
    connection: &mut Connection,
    database_path: &Path,
    existed_before_open: bool,
) -> Result<()> {
    let current_version_raw: i64 =
        connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let maximum_version = embedded_migrations::MIGRATIONS.len();
    let current_version = usize::try_from(current_version_raw).unwrap_or(usize::MAX);
    if current_version_raw < 0 || current_version > maximum_version {
        return Err(ProjectError::UnsupportedSchemaVersion {
            current: u32::try_from(current_version_raw).unwrap_or(u32::MAX),
            maximum: maximum_version,
        });
    }
    if current_version == maximum_version {
        let transaction = connection.transaction()?;
        transaction.execute(
            "UPDATE project_meta SET schema_version = ?1 WHERE schema_version != ?1",
            [current_version as i64],
        )?;
        transaction.commit()?;
        return Ok(());
    }

    //既存DBの唯一の復旧点を、DDLを開始する前に確定させる。
    if existed_before_open {
        backup_database(connection, database_path)?;
    }

    let transaction = connection.transaction()?;
    for (index, (filename, sql)) in embedded_migrations::MIGRATIONS
        .iter()
        .enumerate()
        .skip(current_version)
    {
        transaction
            .execute_batch(sql)
            .map_err(|source| ProjectError::Migration {
                migration: (*filename).to_owned(),
                source,
            })?;
        transaction.pragma_update(None, "user_version", (index + 1) as i64)?;
    }
    let final_version = maximum_version as i64;
    transaction.execute(
        "UPDATE project_meta SET schema_version = ?1",
        [final_version],
    )?;
    transaction.commit()?;
    tracing::info!(schema_version = final_version, "Project database migrated");
    Ok(())
}

fn backup_database(connection: &Connection, database_path: &Path) -> Result<()> {
    let backup_path = database_path.with_file_name("project.sqlite.backup");
    let temporary_path = files::temporary_sibling(&backup_path);
    let backup_result = (|| -> Result<()> {
        connection.backup(MAIN_DB, &temporary_path, None)?;
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&temporary_path)?
            .sync_all()?;
        files::replace_file(&temporary_path, &backup_path)?;
        Ok(())
    })();
    if backup_result.is_err() {
        let _ = fs::remove_file(temporary_path);
    }
    backup_result
}
