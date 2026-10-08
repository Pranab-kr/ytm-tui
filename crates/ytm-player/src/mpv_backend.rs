//! Thin wrapper over libmpv2. Audio-only, no window, no OSD.
//!
//! `wait_event` BLOCKS, so this type must only ever be driven from a dedicated
//! OS thread — never from the tokio runtime (NFR-2).

use crate::player::PlayerError;
use libmpv2::{Mpv, events::Event};

pub struct MpvHandle {
    pub mpv: Mpv,
}

impl MpvHandle {
    pub fn new() -> Result<Self, PlayerError> {
        let mpv = Mpv::with_initializer(|init| {
            // Audio only — without these mpv opens a video window.
            init.set_property("vid", "no")?;
            init.set_property("video", "no")?;
            init.set_property("osc", false)?;
            init.set_property("input-default-bindings", false)?;
            init.set_property("terminal", false)?;
            init.set_property("ytdl", false)?;
            init.set_property("user-agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/149.0.0.0 Safari/537.36")?;
            // Bigger cache: streaming URLs stutter on the default.
            init.set_property("cache", "yes")?;
            init.set_property("demuxer-max-bytes", "32MiB")?;
            Ok(())
        })
        .map_err(|e| PlayerError::MpvUnavailable(e.to_string()))?;

        Ok(Self { mpv })
    }

    /// Replace whatever is playing with this URL and unpause playback.
    pub fn load(&self, url: &str) -> Result<(), PlayerError> {
        self.mpv
            .command("loadfile", &[url, "replace"])
            .map_err(|e| PlayerError::MpvUnavailable(e.to_string()))?;
        let _ = self.set_pause(false);
        Ok(())
    }

    pub fn set_pause(&self, paused: bool) -> Result<(), PlayerError> {
        self.mpv
            .set_property("pause", paused)
            .map_err(|e| PlayerError::MpvUnavailable(e.to_string()))
    }

    pub fn set_volume(&self, v: u8) -> Result<(), PlayerError> {
        self.mpv
            .set_property("volume", v as i64)
            .map_err(|e| PlayerError::MpvUnavailable(e.to_string()))
    }

    pub fn seek_absolute(&self, secs: u64) -> Result<(), PlayerError> {
        self.mpv
            .command("seek", &[&secs.to_string(), "absolute"])
            .map_err(|e| PlayerError::MpvUnavailable(e.to_string()))
    }

    pub fn stop(&self) -> Result<(), PlayerError> {
        self.mpv
            .command("stop", &[])
            .map_err(|e| PlayerError::MpvUnavailable(e.to_string()))
    }

    /// Seconds elapsed; `None` before playback starts.
    pub fn position(&self) -> Option<u64> {
        self.mpv
            .get_property::<f64>("time-pos")
            .ok()
            .map(|f| f.max(0.0) as u64)
    }

    pub fn duration(&self) -> Option<u64> {
        self.mpv
            .get_property::<f64>("duration")
            .ok()
            .map(|f| f.max(0.0) as u64)
    }

    /// Blocking. Only call from the actor thread.
    pub fn poll_event(&self, timeout_secs: f64) -> Option<Result<Event<'_>, libmpv2::Error>> {
        self.mpv.wait_event(timeout_secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "needs libmpv and an audio device; run manually with --ignored"]
    fn mpv_initializes_with_video_disabled() {
        let h = MpvHandle::new().expect("libmpv should initialize");
        // Audio-only: video must be off or mpv tries to open a window.
        let vid: String = h.mpv.get_property("vid").unwrap();
        assert_eq!(vid, "no");
    }

    #[test]
    fn missing_libmpv_produces_an_install_hint() {
        // NFR: never panic when a system dep is absent.
        let e = PlayerError::MpvUnavailable("not found".into());
        assert!(e.to_string().contains("install libmpv"), "got: {e}");
    }
}
