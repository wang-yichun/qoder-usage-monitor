use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

struct DiagnoseState {
    file: Mutex<File>,
}

static STATE: OnceLock<DiagnoseState> = OnceLock::new();

pub fn init() -> Result<PathBuf, String> {
    let path = std::env::temp_dir().join("qoder-usage.log");
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|error| format!("无法写入诊断日志 {}: {error}", path.display()))?;
    let _ = STATE.set(DiagnoseState {
        file: Mutex::new(file),
    });
    Ok(path)
}

pub fn log(message: impl AsRef<str>) {
    let Some(state) = STATE.get() else {
        return;
    };
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let line = format!("[{seconds}] {}\n", message.as_ref());
    if let Ok(mut file) = state.file.lock() {
        let _ = file.write_all(line.as_bytes());
        let _ = file.flush();
    }
}

pub fn log_error(context: &str, error: impl std::fmt::Display) {
    log(format!("{context}: {error}"));
}
