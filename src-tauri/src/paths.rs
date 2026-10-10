#[cfg(debug_assertions)]
use std::env;
use std::io;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub data_dir: PathBuf,
    pub settings_file: PathBuf,
    pub recent_file: PathBuf,
    pub logs_dir: PathBuf,
    pub projects_dir: PathBuf,
}

impl AppPaths {
    pub fn create(data_dir: PathBuf) -> io::Result<Self> {
        // 実データを汚さず再起動後の保存を確かめるため、専用の環境変数はdebugビルドでだけ有効にする。
        #[cfg(debug_assertions)]
        let data_dir = env::var_os("YAKUCRAFT_DATA_DIR")
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .unwrap_or(data_dir);

        let settings_file = data_dir.join("settings.json");
        let recent_file = data_dir.join("recent.json");
        let logs_dir = data_dir.join("logs");
        let projects_dir = data_dir.join("projects");

        std::fs::create_dir_all(&data_dir)?;
        std::fs::create_dir_all(&logs_dir)?;
        std::fs::create_dir_all(&projects_dir)?;

        Ok(Self {
            data_dir,
            settings_file,
            recent_file,
            logs_dir,
            projects_dir,
        })
    }
}
