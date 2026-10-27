use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use uuid::Uuid;

use crate::{AppSettings, ProjectError, RecentProjects, Result};

pub fn load_settings(path: impl AsRef<Path>) -> Result<AppSettings> {
    load_json_or_default(path.as_ref())
}

pub fn save_settings(path: impl AsRef<Path>, settings: &AppSettings) -> Result<()> {
    atomic_write_json(path.as_ref(), settings)
}

pub fn load_recent_projects(path: impl AsRef<Path>) -> Result<RecentProjects> {
    load_json_or_default(path.as_ref())
}

pub fn save_recent_projects(path: impl AsRef<Path>, recent: &RecentProjects) -> Result<()> {
    atomic_write_json(path.as_ref(), recent)
}

fn load_json_or_default<T>(path: &Path) -> Result<T>
where
    T: serde::de::DeserializeOwned + Default,
{
    match fs::read(path) {
        Ok(contents) => {
            serde_json::from_slice(&contents).map_err(|source| ProjectError::JsonFile {
                path: path.to_owned(),
                source,
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(error) => Err(ProjectError::Io(error)),
    }
}

fn atomic_write_json(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    let contents = serde_json::to_vec_pretty(value)?;
    atomic_write(path, &contents)
}

pub(crate) fn atomic_write(path: &Path, contents: &[u8]) -> Result<()> {
    if let Some(parent) = nonempty_parent(path) {
        fs::create_dir_all(parent)?;
    }
    let temporary_path = temporary_sibling(path);
    let write_result = (|| -> Result<()> {
        let mut temporary = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary_path)?;
        temporary.write_all(contents)?;
        temporary.sync_all()?;
        drop(temporary);
        replace_file(&temporary_path, path)?;
        sync_parent(path)?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }
    write_result
}

pub(crate) fn temporary_sibling(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("file"));
    let mut temporary_name = file_name.to_os_string();
    temporary_name.push(format!(".tmp-{}", Uuid::new_v4()));
    path.with_file_name(temporary_name)
}

pub(crate) fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;

        const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
        const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

        let source_wide = source
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let destination_wide = destination
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        // 同じディレクトリの一時ファイルを原子的に置換し、既存JSONやbackupを途中状態にしない。
        let succeeded = unsafe {
            MoveFileExW(
                source_wide.as_ptr(),
                destination_wide.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if succeeded == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        fs::rename(source, destination)
    }
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn MoveFileExW(existing_file_name: *const u16, new_file_name: *const u16, flags: u32) -> i32;
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> Result<()> {
    use std::fs::File;

    if let Some(parent) = nonempty_parent(path) {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> Result<()> {
    Ok(())
}

fn nonempty_parent(path: &Path) -> Option<&Path> {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
}
