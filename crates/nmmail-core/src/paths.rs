//! Per-user data locations.
//!
//! Windows: `%LOCALAPPDATA%\NMMail`, macOS: `~/Library/Application Support/NMMail`,
//! Linux: `$XDG_DATA_HOME/nmmail`. Overridable with `NMMAIL_DATA_DIR` (tests, portable mode).

use std::path::PathBuf;

pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("NMMAIL_DATA_DIR") {
        return PathBuf::from(dir);
    }
    let base = dirs::data_local_dir().unwrap_or_else(std::env::temp_dir);
    if cfg!(any(target_os = "windows", target_os = "macos")) {
        base.join("NMMail")
    } else {
        base.join("nmmail")
    }
}

pub fn db_path() -> PathBuf {
    data_dir().join("nmmail.db")
}

pub fn log_dir() -> PathBuf {
    data_dir().join("logs")
}

pub fn attachments_tmp_dir() -> PathBuf {
    std::env::temp_dir().join("NMMail")
}

/// Name of the IPC endpoint (named pipe on Windows, socket path elsewhere).
pub fn ipc_endpoint() -> String {
    #[cfg(windows)]
    {
        let user = std::env::var("USERNAME").unwrap_or_else(|_| "user".into());
        format!(r"\\.\pipe\nmmail-{user}")
    }
    #[cfg(not(windows))]
    {
        let dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|_| cfg!(target_os = "linux"))
            .unwrap_or_else(data_dir);
        dir.join("nmmail.sock").to_string_lossy().into_owned()
    }
}
