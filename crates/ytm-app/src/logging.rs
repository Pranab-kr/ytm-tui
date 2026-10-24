//! File-only logging. Nothing may reach stdout/stderr — it corrupts the TUI frame.

use std::path::Path;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

/// Initialize file logging. Hold the returned guard for the whole process;
/// dropping it stops the writer thread and loses buffered lines.
pub fn init(dir: &Path) -> std::io::Result<WorkerGuard> {
    std::fs::create_dir_all(dir)?;
    let appender = tracing_appender::rolling::daily(dir, "ytm-tui.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let filter = EnvFilter::try_from_env("YTM_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    // try_init rather than init so a second call in tests does not panic.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_ansi(false)
        .try_init();
    Ok(guard)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_creates_log_file_in_given_dir() {
        let dir = std::env::temp_dir().join(format!("ytmlog{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let guard = init(&dir).expect("init should succeed");
        tracing::info!("hello from test");
        drop(guard);
        let entries: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert!(
            !entries.is_empty(),
            "expected a log file to be created in {dir:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
