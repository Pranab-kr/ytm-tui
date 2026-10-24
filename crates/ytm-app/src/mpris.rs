//! OS media-key and now-playing integration (FR-U6). Entirely optional:
//! if the platform bus is unavailable, the app runs exactly as before.

use souvlaki::{MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, PlatformConfig};
use ytm_player::player::PlayerCommand;

pub fn to_command(ev: MediaControlEvent) -> Option<PlayerCommand> {
    Some(match ev {
        MediaControlEvent::Toggle => PlayerCommand::TogglePause,
        MediaControlEvent::Play => PlayerCommand::Resume,
        MediaControlEvent::Pause => PlayerCommand::Pause,
        MediaControlEvent::Next => PlayerCommand::Next,
        MediaControlEvent::Previous => PlayerCommand::Previous,
        MediaControlEvent::Stop => PlayerCommand::Stop,
        // souvlaki documents this as "intended to be 0.0-1.0, but other values
        // are also accepted", so clamp before casting — a negative would wrap.
        MediaControlEvent::SetVolume(v) => {
            PlayerCommand::SetVolume((v.clamp(0.0, 1.0) * 100.0).round() as u8)
        }
        MediaControlEvent::SetPosition(souvlaki::MediaPosition(d)) => {
            PlayerCommand::SeekAbsolute(d.as_secs())
        }
        MediaControlEvent::SeekBy(souvlaki::SeekDirection::Forward, d) => {
            PlayerCommand::SeekRelative((d.as_secs_f64().round() as i64).max(1))
        }
        MediaControlEvent::SeekBy(souvlaki::SeekDirection::Backward, d) => {
            PlayerCommand::SeekRelative(-((d.as_secs_f64().round() as i64).max(1)))
        }
        MediaControlEvent::Seek(souvlaki::SeekDirection::Forward) => PlayerCommand::SeekRelative(5),
        MediaControlEvent::Seek(souvlaki::SeekDirection::Backward) => {
            PlayerCommand::SeekRelative(-5)
        }
        _ => return None,
    })
}

/// Owned metadata, so it is testable without a live bus. `souvlaki::MediaMetadata`
/// borrows every field, which cannot outlive a function building it from `AppState`;
/// owning it here is what makes the mapping a pure function.
pub struct OwnedMetadata {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub cover_url: Option<String>,
    pub duration: Option<std::time::Duration>,
}

pub fn metadata_of(s: &ytm_tui::app::AppState) -> OwnedMetadata {
    match &s.now_playing {
        None => OwnedMetadata {
            title: None,
            artist: None,
            album: None,
            cover_url: None,
            duration: None,
        },
        Some(t) => {
            let dur = if s.duration.as_secs() > 0 {
                s.duration
            } else {
                t.duration
            };
            OwnedMetadata {
                title: Some(t.title.clone()),
                artist: Some(t.artist_display()),
                album: t.album.clone(),
                cover_url: t.thumbnail_url.clone(),
                duration: if dur.as_secs() > 0 {
                    Some(std::time::Duration::from_secs(dur.as_secs()))
                } else {
                    None
                },
            }
        }
    }
}

/// MPRIS playback status from our own state. Kept separate from metadata
/// because the two change at different times: status on every pause, metadata
/// only on a track change.
pub fn playback_of(s: &ytm_tui::app::AppState) -> MediaPlayback {
    use ytm_player::player::PlaybackState as P;
    let progress = Some(souvlaki::MediaPosition(std::time::Duration::from_secs(
        s.position.as_secs(),
    )));
    match s.playback {
        P::Playing => MediaPlayback::Playing { progress },
        P::Paused => MediaPlayback::Paused { progress },
        // Loading is resolving the URL/buffering; report Paused at 0 so desktop widgets
        // keep metadata visible without advancing their seekbar ahead of real audio.
        P::Loading => MediaPlayback::Paused {
            progress: Some(souvlaki::MediaPosition(std::time::Duration::ZERO)),
        },
        P::Stopped => MediaPlayback::Stopped,
    }
}

/// Push only the current track metadata to the OS.
pub fn update_metadata(controls: &mut MediaControls, s: &ytm_tui::app::AppState) {
    let m = metadata_of(s);
    if let Err(e) = controls.set_metadata(MediaMetadata {
        title: m.title.as_deref(),
        album: m.album.as_deref(),
        artist: m.artist.as_deref(),
        cover_url: m.cover_url.as_deref(),
        duration: m.duration,
    }) {
        tracing::debug!(error = ?e, "could not set MPRIS metadata");
    }
}

/// Push only the current playback status/position to the OS.
pub fn update_playback(controls: &mut MediaControls, s: &ytm_tui::app::AppState) {
    if let Err(e) = controls.set_playback(playback_of(s)) {
        tracing::debug!(error = ?e, "could not set MPRIS playback status");
    }
}

/// Push volume change to the OS.
pub fn update_volume(controls: &mut MediaControls, volume: u8) {
    if let Err(e) = controls.set_volume(volume as f64 / 100.0) {
        tracing::debug!(error = ?e, "could not set MPRIS volume");
    }
}

/// Push the current track and playback status to the OS.
pub fn update(controls: &mut MediaControls, s: &ytm_tui::app::AppState) {
    update_metadata(controls, s);
    update_playback(controls, s);
}

/// Returns `None` when the platform has no media-control bus — not an error. The
/// handler runs on souvlaki's own thread, so it may only send: it has no access to
/// `AppState`, which the event loop owns.
pub fn attach(tx: tokio::sync::mpsc::UnboundedSender<PlayerCommand>) -> Option<MediaControls> {
    let config = PlatformConfig {
        dbus_name: "ytm_tui",
        display_name: "ytm-tui",
        hwnd: None,
    };
    let mut controls = match MediaControls::new(config) {
        Ok(c) => c,
        Err(e) => {
            tracing::info!(error = ?e, "no media-control bus, continuing without media keys");
            return None;
        }
    };
    if let Err(e) = controls.attach(move |ev| {
        if let Some(cmd) = to_command(ev) {
            let _ = tx.send(cmd);
        }
    }) {
        tracing::info!(error = ?e, "could not attach media controls");
        return None;
    }
    tracing::info!("MPRIS media controls attached");
    Some(controls)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_key_events_map_to_player_commands() {
        use souvlaki::MediaControlEvent as E;
        assert!(matches!(
            to_command(E::Toggle),
            Some(PlayerCommand::TogglePause)
        ));
        assert!(matches!(to_command(E::Play), Some(PlayerCommand::Resume)));
        assert!(matches!(to_command(E::Pause), Some(PlayerCommand::Pause)));
        assert!(matches!(to_command(E::Next), Some(PlayerCommand::Next)));
        assert!(matches!(
            to_command(E::Previous),
            Some(PlayerCommand::Previous)
        ));
        assert!(matches!(to_command(E::Stop), Some(PlayerCommand::Stop)));
    }

    #[test]
    fn unsupported_events_are_ignored_rather_than_mapped_wrongly() {
        use souvlaki::MediaControlEvent as E;
        assert!(to_command(E::Raise).is_none());
    }

    #[test]
    fn seeking_and_position_events_map_to_player_commands() {
        use souvlaki::{MediaControlEvent as E, MediaPosition, SeekDirection};
        use std::time::Duration;

        assert!(matches!(
            to_command(E::SetPosition(MediaPosition(Duration::from_secs(42)))),
            Some(PlayerCommand::SeekAbsolute(42))
        ));
        assert!(matches!(
            to_command(E::SeekBy(SeekDirection::Forward, Duration::from_secs(10))),
            Some(PlayerCommand::SeekRelative(10))
        ));
        assert!(matches!(
            to_command(E::SeekBy(SeekDirection::Backward, Duration::from_secs(10))),
            Some(PlayerCommand::SeekRelative(-10))
        ));
        assert!(matches!(
            to_command(E::Seek(SeekDirection::Forward)),
            Some(PlayerCommand::SeekRelative(5))
        ));
        assert!(matches!(
            to_command(E::Seek(SeekDirection::Backward)),
            Some(PlayerCommand::SeekRelative(-5))
        ));
    }

    #[test]
    fn metadata_is_empty_when_nothing_is_playing() {
        let s = ytm_tui::app::AppState::default();
        let m = metadata_of(&s);
        assert!(m.title.is_none());
    }

    #[test]
    fn metadata_carries_title_artist_and_duration() {
        let s = ytm_tui::app::AppState {
            now_playing: Some(ytm_core::Track::stub("v1", "Roygbiv")),
            duration: ytm_core::TrackDuration::from_secs(149),
            ..Default::default()
        };
        let m = metadata_of(&s);
        assert_eq!(m.title.as_deref(), Some("Roygbiv"));
        assert_eq!(m.duration.map(|d| d.as_secs()), Some(149));
    }

    #[test]
    fn metadata_falls_back_to_track_duration_if_appstate_duration_is_zero() {
        let mut track = ytm_core::Track::stub("v1", "Roygbiv");
        track.duration = ytm_core::TrackDuration::from_secs(173);
        let s = ytm_tui::app::AppState {
            now_playing: Some(track),
            duration: ytm_core::TrackDuration::from_secs(0),
            ..Default::default()
        };
        let m = metadata_of(&s);
        assert_eq!(m.duration.map(|d| d.as_secs()), Some(173));
    }

    #[test]
    fn playback_status_follows_our_own_state() {
        use ytm_player::player::PlaybackState as P;
        let at = |p: P| ytm_tui::app::AppState {
            playback: p,
            ..Default::default()
        };
        assert!(matches!(
            playback_of(&at(P::Playing)),
            MediaPlayback::Playing { .. }
        ));
        assert!(matches!(
            playback_of(&at(P::Paused)),
            MediaPlayback::Paused { .. }
        ));
        assert!(matches!(
            playback_of(&at(P::Stopped)),
            MediaPlayback::Stopped
        ));
        // Loading is resolving URL/buffering; report Paused so widgets keep metadata without drifting.
        assert!(matches!(
            playback_of(&at(P::Loading)),
            MediaPlayback::Paused { .. }
        ));
    }

    #[test]
    fn playback_status_carries_the_current_position() {
        let s = ytm_tui::app::AppState {
            playback: ytm_player::player::PlaybackState::Playing,
            position: ytm_core::TrackDuration::from_secs(42),
            ..Default::default()
        };
        match playback_of(&s) {
            MediaPlayback::Playing {
                progress: Some(souvlaki::MediaPosition(d)),
            } => assert_eq!(d.as_secs(), 42),
            other => panic!("expected a position, got {other:?}"),
        }
    }

    #[test]
    fn volume_from_the_bus_is_clamped_before_it_reaches_the_player() {
        // souvlaki documents SetVolume as "intended 0.0-1.0, other values also
        // accepted". A negative would wrap when cast to u8.
        use souvlaki::MediaControlEvent as E;
        assert!(matches!(
            to_command(E::SetVolume(0.5)),
            Some(PlayerCommand::SetVolume(50))
        ));
        assert!(matches!(
            to_command(E::SetVolume(4.0)),
            Some(PlayerCommand::SetVolume(100))
        ));
        assert!(matches!(
            to_command(E::SetVolume(-1.0)),
            Some(PlayerCommand::SetVolume(0))
        ));
    }

    #[test]
    fn metadata_includes_the_cover_url_when_the_track_has_one() {
        let s = ytm_tui::app::AppState {
            now_playing: Some(ytm_core::Track {
                thumbnail_url: Some("https://example.com/a.jpg".into()),
                ..ytm_core::Track::stub("v1", "T")
            }),
            ..Default::default()
        };
        assert_eq!(
            metadata_of(&s).cover_url.as_deref(),
            Some("https://example.com/a.jpg")
        );
    }

    #[test]
    fn metadata_joins_multiple_artists_the_way_the_ui_does() {
        let s = ytm_tui::app::AppState {
            now_playing: Some(ytm_core::Track {
                artists: vec!["Boards of Canada".into(), "Autechre".into()],
                ..ytm_core::Track::stub("v1", "T")
            }),
            ..Default::default()
        };
        assert_eq!(
            metadata_of(&s).artist.as_deref(),
            Some("Boards of Canada, Autechre")
        );
    }
}
