use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender},
    thread::{self, JoinHandle},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

use crate::{ProjectError, Result, files};

const DEFAULT_STALE_AFTER: Duration = Duration::from_secs(5 * 60);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const LOCK_GUARD_FILE_NAME: &str = "project.lock.guard";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LockRecord {
    pub pid: u32,
    pub updated_at: String,
    pub token: Uuid,
}

pub struct ProjectLock {
    lock_path: PathBuf,
    record: LockRecord,
    stop: Sender<()>,
    heartbeat: Option<JoinHandle<()>>,
}

impl ProjectLock {
    pub fn acquire(project_directory: impl AsRef<Path>) -> Result<Self> {
        Self::acquire_with_stale_after(project_directory, DEFAULT_STALE_AFTER)
    }

    pub fn acquire_with_stale_after(
        project_directory: impl AsRef<Path>,
        stale_after: Duration,
    ) -> Result<Self> {
        let project_directory = project_directory.as_ref();
        if !project_directory.is_dir() {
            return Err(ProjectError::ProjectNotFound(
                project_directory.to_path_buf(),
            ));
        }
        let lock_path = project_directory.join("project.lock");
        // stale判定から削除・再作成までを全プロセスで直列化し、確認したlock以外を消さない。
        let _guard = acquire_lock_guard(project_directory)?;

        loop {
            let record = LockRecord {
                pid: std::process::id(),
                updated_at: now_rfc3339()?,
                token: Uuid::new_v4(),
            };
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(mut file) => {
                    let encoded = serde_json::to_vec(&record)?;
                    if let Err(error) = file.write_all(&encoded).and_then(|()| file.sync_all()) {
                        let _ = fs::remove_file(&lock_path);
                        return Err(ProjectError::Io(error));
                    }
                    drop(file);
                    return Ok(Self::start_heartbeat(lock_path, record));
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let existing = read_lock_record(&lock_path)?;
                    if !lock_is_stale(&existing, stale_after)? {
                        return Err(ProjectError::ProjectAlreadyOpen);
                    }
                    match fs::remove_file(&lock_path) {
                        Ok(()) => continue,
                        Err(remove_error)
                            if remove_error.kind() == std::io::ErrorKind::NotFound =>
                        {
                            continue;
                        }
                        Err(remove_error) => return Err(ProjectError::Io(remove_error)),
                    }
                }
                Err(error) => return Err(ProjectError::Io(error)),
            }
        }
    }

    pub fn record(&self) -> &LockRecord {
        &self.record
    }

    pub fn lock_path(&self) -> &Path {
        &self.lock_path
    }

    pub(crate) fn stop_heartbeat(&mut self) {
        let _ = self.stop.send(());
        if let Some(heartbeat) = self.heartbeat.take()
            && heartbeat.join().is_err()
        {
            tracing::error!(path = %self.lock_path.display(), "Project lock heartbeat thread panicked");
        }
    }

    pub(crate) fn retarget_after_directory_move(&mut self, project_directory: &Path) {
        debug_assert!(self.heartbeat.is_none());
        self.lock_path = project_directory.join("project.lock");
    }

    fn start_heartbeat(lock_path: PathBuf, record: LockRecord) -> Self {
        let (stop, receiver) = mpsc::channel();
        let heartbeat_path = lock_path.clone();
        let heartbeat_record = record.clone();
        let heartbeat =
            thread::spawn(move || heartbeat_loop(heartbeat_path, heartbeat_record, receiver));
        Self {
            lock_path,
            record,
            stop,
            heartbeat: Some(heartbeat),
        }
    }
}

impl Drop for ProjectLock {
    fn drop(&mut self) {
        self.stop_heartbeat();
        let Some(project_directory) = self.lock_path.parent() else {
            return;
        };
        match acquire_lock_guard(project_directory) {
            Ok(_guard)
                if read_lock_record(&self.lock_path)
                    .is_ok_and(|record| record.token == self.record.token) =>
            {
                if let Err(error) = fs::remove_file(&self.lock_path)
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    tracing::warn!(path = %self.lock_path.display(), %error, "failed to remove Project lock");
                }
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(path = %self.lock_path.display(), %error, "failed to lock Project drop guard");
            }
        }
    }
}

fn acquire_lock_guard(project_directory: &Path) -> Result<File> {
    //このguardは削除しない。削除すると待機中のhandleと新handleが別inodeをロックしてしまう。
    let guard_path = project_directory.join(LOCK_GUARD_FILE_NAME);
    let guard = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(guard_path)?;
    guard.lock()?;
    Ok(guard)
}

fn heartbeat_loop(lock_path: PathBuf, mut record: LockRecord, stop: Receiver<()>) {
    loop {
        match stop.recv_timeout(HEARTBEAT_INTERVAL) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {}
        }

        let Some(project_directory) = lock_path.parent() else {
            return;
        };
        let _guard = match acquire_lock_guard(project_directory) {
            Ok(guard) => guard,
            Err(error) => {
                tracing::warn!(path = %lock_path.display(), %error, "failed to lock Project heartbeat guard");
                continue;
            }
        };
        match read_lock_record(&lock_path) {
            Ok(current) if current.token == record.token => {
                match now_rfc3339() {
                    Ok(updated_at) => record.updated_at = updated_at,
                    Err(error) => {
                        tracing::warn!(%error, "failed to timestamp Project lock heartbeat");
                        continue;
                    }
                }
                match serde_json::to_vec(&record) {
                    Ok(encoded) => {
                        if let Err(error) = files::atomic_write(&lock_path, &encoded) {
                            tracing::warn!(path = %lock_path.display(), %error, "failed to update Project lock heartbeat");
                        }
                    }
                    Err(error) => tracing::warn!(%error, "failed to encode Project lock heartbeat"),
                }
            }
            Ok(_) => return,
            Err(error) => {
                tracing::warn!(path = %lock_path.display(), %error, "failed to read Project lock heartbeat")
            }
        }
    }
}

fn read_lock_record(path: &Path) -> Result<LockRecord> {
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(|_| ProjectError::InvalidLockRecord)
}

fn lock_is_stale(record: &LockRecord, stale_after: Duration) -> Result<bool> {
    if !process_is_alive(record.pid) {
        return Ok(true);
    }
    let updated_at = OffsetDateTime::parse(&record.updated_at, &Rfc3339)?;
    let age = OffsetDateTime::now_utc() - updated_at;
    Ok(age.whole_seconds() >= stale_after.as_secs() as i64)
}

fn now_rfc3339() -> Result<String> {
    Ok(OffsetDateTime::now_utc().format(&Rfc3339)?)
}

#[cfg(windows)]
fn process_is_alive(pid: u32) -> bool {
    use std::ffi::c_void;

    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const STILL_ACTIVE: u32 = 259;
    const ERROR_ACCESS_DENIED: u32 = 5;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit_handle: i32, process_id: u32) -> *mut c_void;
        fn GetExitCodeProcess(process: *mut c_void, exit_code: *mut u32) -> i32;
        fn CloseHandle(object: *mut c_void) -> i32;
        fn GetLastError() -> u32;
    }

    // WindowsではPIDを直接列挙する標準Rust APIがないため、限定クエリ権限でOpenProcessし、終了コードを調べる。
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return unsafe { GetLastError() } == ERROR_ACCESS_DENIED;
    }
    let mut exit_code = 0;
    let alive =
        unsafe { GetExitCodeProcess(process, &mut exit_code) != 0 && exit_code == STILL_ACTIVE };
    let _ = unsafe { CloseHandle(process) };
    alive
}

#[cfg(target_os = "linux")]
fn process_is_alive(pid: u32) -> bool {
    Path::new("/proc").join(pid.to_string()).exists()
}

#[cfg(all(unix, not(target_os = "linux")))]
fn process_is_alive(pid: u32) -> bool {
    use std::process::Command;

    Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "pid="])
        .output()
        .is_ok_and(|output| output.status.success() && !output.stdout.is_empty())
}

#[cfg(not(any(windows, unix)))]
fn process_is_alive(_pid: u32) -> bool {
    true
}
