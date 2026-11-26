//! Per-session log files, so a failed session can be investigated after the fact.
//!
//! macOS: ~/Library/Logs/rdpvncdesk/  Windows/Linux: <local data dir>/rdpvncdesk/logs/
//! One file per host, overwritten by each new session (a numbered one while another session to
//! that host is still running). Passwords are never logged.

use std::fs::File;
use std::path::PathBuf;

pub fn dir() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        directories::BaseDirs::new().map(|d| d.home_dir().join("Library/Logs/rdpvncdesk"))
    } else {
        directories::ProjectDirs::from("", "", "rdpvncdesk").map(|d| d.data_local_dir().join("logs"))
    }
}

/// Creates (overwriting) the log file for a session to `destination`: only the last session
/// to each host is kept, so logs don't pile up.
pub fn create(destination: &str) -> Option<(File, PathBuf)> {
    let dir = dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    let host: String = destination
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' })
        .take(64)
        .collect();
    // A second session to the same host while the first is still running must not overwrite
    // its log (issue #3): each session holds a lock on its file; a locked one is skipped.
    for n in 1..=9 {
        let name = if n == 1 {
            format!("last-session-{host}.log")
        } else {
            format!("last-session-{host}-{n}.log")
        };
        let path = dir.join(name);
        let Ok(file) = std::fs::OpenOptions::new().write(true).create(true).truncate(false).open(&path)
        else {
            continue;
        };
        if file.try_lock().is_ok() {
            file.set_len(0).ok()?;
            return Some((file, path));
        }
    }
    None
}
