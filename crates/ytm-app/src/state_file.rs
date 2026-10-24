//! Runtime state that survives a restart but is not configuration.
//!
//! Separate from `config.toml` on purpose: that file is hand-maintained and
//! `ytm-tui config` promises never to overwrite it. This one is ours to rewrite.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct RuntimeState {
    /// Last volume, 0-100. `None` means fall back to `playback.volume`.
    pub volume: Option<u8>,
}

pub fn default_path() -> PathBuf {
    crate::config::paths::cache_dir().join("state.toml")
}

/// Never fails: this file is disposable, and a bad one must not block startup.
pub fn load(path: &Path) -> RuntimeState {
    let Ok(text) = std::fs::read_to_string(path) else {
        return RuntimeState::default();
    };
    let mut s: RuntimeState = match toml::from_str(&text) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "ignoring an unreadable state file");
            RuntimeState::default()
        }
    };
    // A hand-edited 200 must not become the starting volume.
    if s.volume.is_some_and(|v| v > 100) {
        s.volume = None;
    }
    s
}

pub fn save(path: &Path, s: &RuntimeState) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = toml::to_string(s).map_err(std::io::Error::other)?;
    std::fs::write(path, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(name)
    }

    #[test]
    fn a_missing_state_file_is_defaults_not_an_error() {
        // Runtime state is disposable. A first run has no file, and that is
        // normal, not a startup failure.
        let p = tmp("ytm-state-absent.toml");
        let _ = std::fs::remove_file(&p);
        assert_eq!(load(&p).volume, None);
    }

    #[test]
    fn a_corrupt_state_file_is_defaults_not_an_error() {
        let p = tmp("ytm-state-corrupt.toml");
        std::fs::write(&p, "this is not toml {{{").unwrap();
        assert_eq!(load(&p).volume, None, "a bad file must not break startup");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn a_saved_volume_round_trips() {
        let p = tmp("ytm-state-roundtrip.toml");
        let _ = std::fs::remove_file(&p);
        save(&p, &RuntimeState { volume: Some(42) }).unwrap();
        assert_eq!(load(&p).volume, Some(42));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn an_out_of_range_volume_in_the_file_is_ignored() {
        // u8 parses up to 255; the player clamps to 100. A hand-edited 200 must
        // not become the starting volume.
        let p = tmp("ytm-state-range.toml");
        std::fs::write(&p, "volume = 200\n").unwrap();
        assert_eq!(load(&p).volume, None, "out of range is treated as absent");
        let _ = std::fs::remove_file(&p);
    }
}
