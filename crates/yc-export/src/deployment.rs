use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    ExportError, ExportInput, atomic_write_file, create_temporary_output,
    modified_jar::{export_modified_jar_validated, validate_modified_jar_input},
};

static BACKUP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InPlaceReplaceReport {
    pub backup_path: PathBuf,
    pub previous_sha256: String,
    pub written_sha256: String,
}

/// Copy a completed resource pack into an instance's resourcepacks directory.
pub fn install_resource_pack(
    pack_zip: &Path,
    instance_dir: &Path,
    overwrite: bool,
) -> Result<PathBuf, ExportError> {
    let file_name = pack_zip
        .file_name()
        .ok_or_else(|| ExportError::InvalidOutputPath(pack_zip.to_path_buf()))?;
    let resourcepacks_dir = instance_dir.join("resourcepacks");
    let destination = resourcepacks_dir.join(file_name);
    install_resource_pack_to(pack_zip, &destination, overwrite)
}

/// Copy a completed resource pack atomically to its selected destination.
pub fn install_resource_pack_to(
    pack_zip: &Path,
    destination: &Path,
    overwrite: bool,
) -> Result<PathBuf, ExportError> {
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    atomic_write_file(
        destination,
        overwrite,
        |output| {
            let mut source = File::open(pack_zip)?;
            io::copy(&mut source, output)?;
            Ok(())
        },
        |_| Ok(()),
    )?;
    Ok(destination.to_path_buf())
}

/// Infer an instance directory from a selected mods directory.
pub fn instance_dir_from_mods_dir(mods_dir: &Path) -> Option<PathBuf> {
    let parent = mods_dir.parent()?;
    if parent.as_os_str().is_empty() {
        Some(PathBuf::from("."))
    } else {
        Some(parent.to_path_buf())
    }
}

/// Check the common files that identify a Minecraft instance directory.
pub fn looks_like_instance_dir(directory: &Path) -> bool {
    directory.join("mods").is_dir()
        && (directory.join("options.txt").is_file() || directory.join("saves").is_dir())
}

/// Replace a mod JAR from the project's original copy while retaining a backup.
pub fn replace_jar_in_place(
    input: &ExportInput<'_>,
    source_jar: &Path,
    target_jar: &Path,
    backup_dir: &Path,
) -> Result<InPlaceReplaceReport, ExportError> {
    if source_jar.exists()
        && target_jar.exists()
        && fs::canonicalize(source_jar)? == fs::canonicalize(target_jar)?
    {
        return Err(ExportError::SourceAndOutputSame(source_jar.to_path_buf()));
    }
    // Reject signed or otherwise invalid source JARs before creating a backup as a side effect.
    validate_modified_jar_input(input, source_jar)?;

    let backup_path = backup_target_jar(target_jar, backup_dir)?;
    let previous_sha256 = sha256_hex(&backup_path)?;

    let mut temporary = create_temporary_output(target_jar)?;
    drop(temporary.file.take());
    export_modified_jar_validated(input, source_jar, &temporary.path, true)?;
    let written_sha256 = sha256_hex(&temporary.path)?;
    fs::rename(&temporary.path, target_jar)
        .map_err(|source| target_file_error(source, target_jar))?;
    temporary.file = None;

    tracing::info!(
        source = %source_jar.display(),
        target = %target_jar.display(),
        backup = %backup_path.display(),
        "mod JAR replaced in place"
    );
    Ok(InPlaceReplaceReport {
        backup_path,
        previous_sha256,
        written_sha256,
    })
}

/// Replace a previously modified JAR while retaining its first, original backup.
pub fn replace_jar_in_place_with_backup(
    input: &ExportInput<'_>,
    source_jar: &Path,
    target_jar: &Path,
    backup_path: &Path,
) -> Result<InPlaceReplaceReport, ExportError> {
    if source_jar.exists()
        && target_jar.exists()
        && fs::canonicalize(source_jar)? == fs::canonicalize(target_jar)?
    {
        return Err(ExportError::SourceAndOutputSame(source_jar.to_path_buf()));
    }
    if !backup_path.is_file() {
        return Err(ExportError::Io(io::Error::new(
            io::ErrorKind::NotFound,
            "the original JAR backup is missing",
        )));
    }
    if fs::canonicalize(backup_path)? == fs::canonicalize(target_jar)? {
        return Err(ExportError::SourceAndOutputSame(target_jar.to_path_buf()));
    }
    validate_modified_jar_input(input, source_jar)?;

    let previous_sha256 = sha256_hex(target_jar)?;
    let mut temporary = create_temporary_output(target_jar)?;
    drop(temporary.file.take());
    export_modified_jar_validated(input, source_jar, &temporary.path, true)?;
    let written_sha256 = sha256_hex(&temporary.path)?;
    fs::rename(&temporary.path, target_jar)
        .map_err(|source| target_file_error(source, target_jar))?;
    temporary.file = None;

    tracing::info!(
        source = %source_jar.display(),
        target = %target_jar.display(),
        backup = %backup_path.display(),
        "mod JAR replaced using its original backup"
    );
    Ok(InPlaceReplaceReport {
        backup_path: backup_path.to_path_buf(),
        previous_sha256,
        written_sha256,
    })
}

/// Restore a JAR using an atomic sibling-file replacement.
pub fn restore_jar_from_backup(backup_path: &Path, target_jar: &Path) -> Result<(), ExportError> {
    atomic_write_file(
        target_jar,
        true,
        |output| {
            let mut backup = File::open(backup_path)?;
            io::copy(&mut backup, output)?;
            Ok(())
        },
        |_| Ok(()),
    )
    .map_err(|error| match error {
        ExportError::RenameOutput { source, .. } => target_file_error(source, target_jar),
        other => other,
    })
}

fn backup_target_jar(target_jar: &Path, backup_dir: &Path) -> Result<PathBuf, ExportError> {
    let file_name = target_jar
        .file_name()
        .ok_or_else(|| ExportError::InvalidOutputPath(target_jar.to_path_buf()))?;
    fs::create_dir_all(backup_dir)?;
    let mut target =
        File::open(target_jar).map_err(|source| target_file_error(source, target_jar))?;

    for _ in 0..100 {
        let elapsed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let sequence = BACKUP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut backup_name = OsString::from(file_name);
        backup_name.push(format!(
            ".{}.{:09}-{sequence}.bak",
            elapsed.as_secs(),
            elapsed.subsec_nanos()
        ));
        let backup_path = backup_dir.join(backup_name);
        let mut backup = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&backup_path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => return Err(ExportError::Io(source)),
        };
        let copied = io::copy(&mut target, &mut backup)
            .and_then(|_| backup.sync_all())
            .map(|_| ());
        if let Err(source) = copied {
            drop(backup);
            let _ = fs::remove_file(&backup_path);
            return Err(target_file_error(source, target_jar));
        }
        return Ok(backup_path);
    }

    Err(ExportError::Io(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique JAR backup filename",
    )))
}

fn sha256_hex(path: &Path) -> Result<String, ExportError> {
    let digest = yc_archive::sha256_file(path)?;
    let mut output = String::with_capacity(digest.len() * 2);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in digest {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    Ok(output)
}

fn target_file_error(source: io::Error, target: &Path) -> ExportError {
    if is_target_locked_error(&source) {
        ExportError::TargetLocked(target.to_path_buf())
    } else {
        ExportError::Io(source)
    }
}

fn is_target_locked_error(source: &io::Error) -> bool {
    #[cfg(windows)]
    {
        matches!(source.raw_os_error(), Some(5) | Some(32) | Some(33))
    }
    #[cfg(not(windows))]
    {
        let _ = source;
        false
    }
}
