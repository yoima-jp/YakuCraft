#[tauri::command]
pub fn automatic_updates_supported() -> bool {
    #[cfg(target_os = "windows")]
    {
        true
    }

    #[cfg(target_os = "linux")]
    {
        std::env::var_os("APPIMAGE").is_some()
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        false
    }
}
