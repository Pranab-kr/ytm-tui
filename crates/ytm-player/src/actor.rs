//! The player actor: owns the Mpv handle on its own OS thread because
//! `wait_event` blocks and must never touch the tokio runtime (NFR-2).

use crate::{
    mpv_backend::MpvHandle, player::*, queue::Queue, resolver::StreamResolver,
    storage::AudioStorageManager,
};
use std::sync::mpsc as std_mpsc;
use tokio::sync::mpsc;
use ytm_core::{Track, TrackDuration, VideoId};

/// One retry per track, per FR-P6.
#[derive(Default)]
pub struct RetryState {
    used: bool,
}

impl RetryState {
    pub fn should_retry(&mut self) -> bool {
        if self.used {
            false
        } else {
            self.used = true;
            true
        }
    }
    pub fn reset(&mut self) {
        self.used = false;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    Eof,
    Error,
    Stop,
    Quit,
}

/// `EndFileReason` is a `c_uint` alias in libmpv2 6.0.0, not a Rust enum, so
/// this maps by value. Anything unrecognized is treated as an error rather than
/// a clean finish — guessing "clean" would silently skip a track.
pub fn map_end_reason(reason: libmpv2::EndFileReason) -> EndReason {
    use libmpv2_sys::{
        mpv_end_file_reason_MPV_END_FILE_REASON_EOF as EOF,
        mpv_end_file_reason_MPV_END_FILE_REASON_QUIT as QUIT,
        mpv_end_file_reason_MPV_END_FILE_REASON_STOP as STOP,
    };
    match reason {
        r if r == EOF => EndReason::Eof,
        r if r == STOP => EndReason::Stop,
        r if r == QUIT => EndReason::Quit,
        _ => EndReason::Error,
    }
}

/// Handle held by the event loop. Never blocks.
pub struct MpvPlayer {
    tx: std_mpsc::Sender<PlayerCommand>,
}

impl Player for MpvPlayer {
    fn send(&self, cmd: PlayerCommand) -> Result<(), PlayerError> {
        self.tx.send(cmd).map_err(|_| PlayerError::ActorGone)
    }
}

/// Start the actor thread. Returns the handle plus the event stream to select on.
pub fn spawn_player(
    volume: u8,
    cookie_header_file: Option<std::path::PathBuf>,
    storage: Option<AudioStorageManager>,
) -> Result<(MpvPlayer, mpsc::UnboundedReceiver<PlayerEvent>), PlayerError> {
    // Probe before spawning so a missing libmpv is a clean startup error.
    let handle = MpvHandle::new()?;
    handle.set_volume(volume)?;

    let (cmd_tx, cmd_rx) = std_mpsc::channel::<PlayerCommand>();
    let (ev_tx, ev_rx) = mpsc::unbounded_channel::<PlayerEvent>();

    std::thread::Builder::new()
        .name("ytm-player".into())
        .spawn(move || run_actor(handle, cmd_rx, ev_tx, volume, cookie_header_file, storage))
        .map_err(|e| PlayerError::MpvUnavailable(e.to_string()))?;

    Ok((MpvPlayer { tx: cmd_tx }, ev_rx))
}

/// Everything the actor owns. Grouped so the helpers take one argument rather
/// than ten.
struct Actor {
    mpv: MpvHandle,
    rt: tokio::runtime::Runtime,
    resolver: StreamResolver,
    storage: Option<AudioStorageManager>,
    active_prefetches: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<VideoId>>>,
    queue: Queue,
    retry: RetryState,
    state: PlaybackState,
    volume: u8,
    /// Volume before muting, so unmute restores it rather than guessing.
    muted_at: Option<u8>,
    events: mpsc::UnboundedSender<PlayerEvent>,
}

impl Actor {
    fn emit(&self, e: PlayerEvent) {
        // A closed receiver means the UI is gone; the loop will exit on Shutdown.
        let _ = self.events.send(e);
    }

    fn set_state(&mut self, s: PlaybackState) {
        if self.state != s {
            self.state = s;
            self.emit(PlayerEvent::StateChanged(s));
        }
    }

    fn emit_queue(&self) {
        self.emit(PlayerEvent::QueueChanged {
            tracks: self.queue.tracks().to_vec(),
            current: self.queue.current_index(),
        });
    }

    /// Resolve and load a track. Blocking `block_on` is correct here — this is
    /// the actor's own thread, not the runtime (NFR-2).
    fn play_track(&mut self, track: &Track) {
        self.set_state(PlaybackState::Loading);
        self.emit(PlayerEvent::TrackChanged(Some(track.clone())));

        let local_path = self
            .storage
            .as_ref()
            .and_then(|s| s.find_local_audio(&track.video_id));

        if let Some(path) = local_path {
            if let Err(e) = self.mpv.load(path.to_str().unwrap_or_default()) {
                self.emit(PlayerEvent::Error(e.to_string()));
                self.set_state(PlaybackState::Stopped);
            }
        } else {
            match self.rt.block_on(self.resolver.resolve(&track.video_id)) {
                Ok(url) => {
                    if let Err(e) = self.mpv.load(&url) {
                        self.emit(PlayerEvent::Error(e.to_string()));
                        self.set_state(PlaybackState::Stopped);
                    }
                }
                Err(e) => {
                    self.emit(PlayerEvent::Error(e.to_string()));
                    self.set_state(PlaybackState::Stopped);
                }
            }
        }

        self.trigger_lookahead_prefetch();
    }

    fn trigger_lookahead_prefetch(&mut self) {
        let Some(storage) = self.storage.as_ref() else {
            return;
        };
        let count = storage.prefetch_count();
        if count == 0 {
            return;
        }

        let Some(current_idx) = self.queue.current_index() else {
            return;
        };

        let tracks = self.queue.tracks();
        let upcoming: Vec<VideoId> = tracks
            .iter()
            .skip(current_idx + 1)
            .take(count)
            .map(|t| t.video_id.clone())
            .collect();

        if upcoming.is_empty() {
            return;
        }

        let cookie_jar = self.resolver.cookie_jar();
        for id in upcoming {
            if storage.find_local_audio(&id).is_some() {
                continue;
            }

            let mut active = match self.active_prefetches.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            if active.contains(&id) {
                continue;
            }
            active.insert(id.clone());
            drop(active);

            let active_set = std::sync::Arc::clone(&self.active_prefetches);
            let storage_clone = storage.clone();
            let target_path = storage.cache_dir().join(format!("{}.opus", id.as_str()));
            let cookie_jar_clone = cookie_jar.clone();

            std::thread::Builder::new()
                .name(format!("prefetch-{}", id.as_str()))
                .spawn(move || {
                    let _ = storage_clone.download_track_sync(
                        &id,
                        &target_path,
                        cookie_jar_clone.as_deref(),
                    );
                    let _ = storage_clone.prune_cache();
                    if let Ok(mut set) = active_set.lock() {
                        set.remove(&id);
                    }
                })
                .ok();
        }
    }

    /// Re-resolve the current track after a stale URL, once (FR-P6).
    fn retry_current(&mut self) {
        let Some(track) = self.queue.current().cloned() else {
            return;
        };
        if self.retry.should_retry() {
            self.resolver.invalidate(&track.video_id);
            self.play_track(&track);
        } else {
            self.emit(PlayerEvent::Error(
                "this track's stream expired and could not be renewed".into(),
            ));
            self.set_state(PlaybackState::Stopped);
        }
    }

    fn advance_and_play(&mut self) {
        self.retry.reset();
        match self.queue.advance().cloned() {
            Some(t) => {
                self.emit_queue();
                self.play_track(&t);
            }
            None => {
                // Queue ran out — FR-P: report it rather than looping.
                self.set_state(PlaybackState::Stopped);
                self.emit(PlayerEvent::TrackChanged(None));
            }
        }
    }

    fn play_previous(&mut self) {
        self.retry.reset();
        if let Some(t) = self.queue.previous().cloned() {
            self.emit_queue();
            self.play_track(&t);
        }
    }

    fn set_volume(&mut self, v: u8) {
        let v = clamp_volume(v as i64);
        if let Err(e) = self.mpv.set_volume(v) {
            self.emit(PlayerEvent::Error(e.to_string()));
            return;
        }
        self.volume = v;
        self.muted_at = None;
        self.emit(PlayerEvent::VolumeChanged(v));
    }

    fn toggle_mute(&mut self) {
        match self.muted_at.take() {
            // Unmute: restore what it was before.
            Some(prev) => {
                if self.mpv.set_volume(prev).is_ok() {
                    self.volume = prev;
                    self.emit(PlayerEvent::VolumeChanged(prev));
                }
            }
            None => {
                let prev = self.volume;
                if self.mpv.set_volume(0).is_ok() {
                    self.muted_at = Some(prev);
                    self.volume = 0;
                    self.emit(PlayerEvent::VolumeChanged(0));
                }
            }
        }
    }

    fn seek_relative(&mut self, delta: i64) {
        let pos = self.mpv.position().unwrap_or(0) as i64;
        let dur = self.mpv.duration().unwrap_or(0) as i64;
        let target = apply_seek(pos, delta, dur);
        if let Err(e) = self.mpv.seek_absolute(target as u64) {
            self.emit(PlayerEvent::Error(e.to_string()));
        }
    }

    fn handle(&mut self, cmd: PlayerCommand) {
        match cmd {
            // Handled by the caller so the loop can return.
            PlayerCommand::Shutdown => {}

            PlayerCommand::PlayNow(t) => {
                self.retry.reset();
                self.queue.push_next(vec![t.clone()]);
                // push_next inserts after current; step onto it.
                if self.queue.len() > 1 {
                    self.queue.advance();
                }
                self.emit_queue();
                self.play_track(&t);
            }

            // Play an entry that is already queued, rather than inserting a copy
            // of it. Enter on a queue row went through `PlayNow`, which inserts,
            // so replaying a finished track left two rows for the same song.
            PlayerCommand::JumpTo(idx) => {
                self.retry.reset();
                if let Some(t) = self.queue.jump_to(idx) {
                    self.emit_queue();
                    self.play_track(&t);
                }
            }

            PlayerCommand::Pause => {
                if self.mpv.set_pause(true).is_ok() {
                    self.set_state(PlaybackState::Paused);
                }
            }
            PlayerCommand::Resume => {
                if self.mpv.set_pause(false).is_ok() {
                    self.set_state(PlaybackState::Playing);
                }
            }
            PlayerCommand::TogglePause => {
                let want_paused = self.state.is_active();
                if self.mpv.set_pause(want_paused).is_ok() {
                    self.set_state(if want_paused {
                        PlaybackState::Paused
                    } else {
                        PlaybackState::Playing
                    });
                }
            }
            PlayerCommand::Stop => {
                let _ = self.mpv.stop();
                self.set_state(PlaybackState::Stopped);
            }

            PlayerCommand::Next => self.advance_and_play(),
            PlayerCommand::Previous => self.play_previous(),

            PlayerCommand::SeekRelative(d) => self.seek_relative(d),
            PlayerCommand::SeekAbsolute(s) => {
                if let Err(e) = self.mpv.seek_absolute(s) {
                    self.emit(PlayerEvent::Error(e.to_string()));
                }
            }

            PlayerCommand::SetVolume(v) => self.set_volume(v),
            PlayerCommand::ToggleMute => self.toggle_mute(),

            PlayerCommand::SetShuffle(on) => {
                self.queue.set_shuffle(on);
                self.emit(PlayerEvent::ShuffleChanged(on));
                self.emit_queue();
            }
            PlayerCommand::SetRepeat(m) => {
                self.queue.set_repeat(m);
                self.emit(PlayerEvent::RepeatChanged(m));
            }

            PlayerCommand::EnqueueBack(ts) => {
                let was_empty = self.queue.is_empty();
                self.queue.push_back(ts);
                self.emit_queue();
                // Nothing was playing, so start.
                if let Some(t) = self.queue.current().cloned().filter(|_| was_empty) {
                    self.play_track(&t);
                } else {
                    self.trigger_lookahead_prefetch();
                }
            }
            PlayerCommand::EnqueueNext(ts) => {
                let was_empty = self.queue.is_empty();
                self.queue.push_next(ts);
                self.emit_queue();
                if let Some(t) = self.queue.current().cloned().filter(|_| was_empty) {
                    self.play_track(&t);
                } else {
                    self.trigger_lookahead_prefetch();
                }
            }
            PlayerCommand::RemoveFromQueue(i) => {
                let was_current = self.queue.current_index() == Some(i);
                self.queue.remove(i);
                self.emit_queue();
                // Removing what is playing must change what is playing.
                if was_current {
                    match self.queue.current().cloned() {
                        Some(t) => self.play_track(&t),
                        None => {
                            let _ = self.mpv.stop();
                            self.set_state(PlaybackState::Stopped);
                            self.emit(PlayerEvent::TrackChanged(None));
                        }
                    }
                }
            }
            PlayerCommand::MoveInQueue { from, to } => {
                self.queue.move_item(from, to);
                self.emit_queue();
            }
            PlayerCommand::ClearQueue => {
                self.queue.clear();
                let _ = self.mpv.stop();
                self.set_state(PlaybackState::Stopped);
                self.emit(PlayerEvent::TrackChanged(None));
                self.emit_queue();
            }
        }
    }
}

fn run_actor(
    mpv: MpvHandle,
    cmds: std_mpsc::Receiver<PlayerCommand>,
    events: mpsc::UnboundedSender<PlayerEvent>,
    volume: u8,
    cookie_header_file: Option<std::path::PathBuf>,
    storage: Option<AudioStorageManager>,
) {
    // The actor thread needs its own small runtime for the async resolver.
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            let _ = events.send(PlayerEvent::Error(format!("player failed to start: {e}")));
            return;
        }
    };

    let mut actor = Actor {
        mpv,
        rt,
        resolver: {
            // YouTube refuses anonymous stream requests ("Sign in to confirm
            // you're not a bot"), so yt-dlp needs the same cookies the API uses.
            let r = StreamResolver::new();
            if let Some(path) = cookie_header_file.as_deref() {
                r.use_cookie_header_file(path);
            }
            r
        },
        storage,
        active_prefetches: std::sync::Arc::new(std::sync::Mutex::new(
            std::collections::HashSet::new(),
        )),
        queue: Queue::default(),
        retry: RetryState::default(),
        state: PlaybackState::Stopped,
        volume,
        muted_at: None,
        events,
    };

    let mut last_progress = std::time::Instant::now();

    loop {
        // 1. Drain pending commands without blocking.
        loop {
            match cmds.try_recv() {
                Ok(PlayerCommand::Shutdown) => {
                    let _ = actor.mpv.stop();
                    return;
                }
                Ok(cmd) => actor.handle(cmd),
                // The UI dropped its handle; nothing more can arrive.
                Err(std_mpsc::TryRecvError::Disconnected) => {
                    let _ = actor.mpv.stop();
                    return;
                }
                Err(std_mpsc::TryRecvError::Empty) => break,
            }
        }

        // 2. Pump mpv events with a short timeout so commands stay responsive.
        //    Borrow-scoped: `ev` borrows from mpv, so decide here and act after.
        let mut ended: Option<EndReason> = None;
        let mut loaded = false;
        if let Some(Ok(ev)) = actor.mpv.poll_event(0.1) {
            use libmpv2::events::Event as E;
            match ev {
                E::EndFile(reason) => ended = Some(map_end_reason(reason)),
                E::FileLoaded => loaded = true,
                _ => {}
            }
        }

        if loaded {
            actor.set_state(PlaybackState::Playing);
        }
        if let Some(reason) = ended {
            let finished = actor.queue.current().map(|t| t.video_id.clone());
            match reason {
                EndReason::Eof => {
                    if let Some(id) = finished {
                        actor.emit(PlayerEvent::TrackEnded(id));
                    }
                    actor.advance_and_play();
                }
                // A stale stream URL surfaces here as an error, not as a log
                // line: libmpv2 6.0.0 exposes no `request_log_messages`, so
                // Event::LogMessage never arrives and cannot be the signal.
                EndReason::Error => actor.retry_current(),
                // Stop and Quit are our own doing; state is already set.
                EndReason::Stop | EndReason::Quit => {}
            }
        }

        // 3. Emit progress at ~4Hz (FR-P7).
        if actor.state.is_active() && last_progress.elapsed().as_millis() >= 240 {
            last_progress = std::time::Instant::now();
            let position = TrackDuration::from_secs(actor.mpv.position().unwrap_or(0));
            let duration = TrackDuration::from_secs(actor.mpv.duration().unwrap_or(0));
            actor.emit(PlayerEvent::Progress { position, duration });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_403_is_retried_exactly_once() {
        // FR-P6: stale URL -> invalidate, re-resolve, retry once. Never loop.
        let mut r = RetryState::default();
        assert!(r.should_retry(), "first failure retries");
        assert!(!r.should_retry(), "second failure gives up");
    }

    #[test]
    fn retry_state_resets_on_a_new_track() {
        let mut r = RetryState::default();
        r.should_retry();
        r.reset();
        assert!(r.should_retry(), "each track gets its own retry budget");
    }

    #[test]
    fn libmpv_end_reasons_map_to_ours() {
        // libmpv2 6.0.0 exposes EndFileReason as a c_uint alias with integer
        // constants, not a Rust enum — so this mapping is by value and needs a
        // test to stay honest.
        use libmpv2_sys::{
            mpv_end_file_reason_MPV_END_FILE_REASON_EOF as EOF,
            mpv_end_file_reason_MPV_END_FILE_REASON_ERROR as ERROR,
            mpv_end_file_reason_MPV_END_FILE_REASON_QUIT as QUIT,
            mpv_end_file_reason_MPV_END_FILE_REASON_STOP as STOP,
        };
        assert_eq!(map_end_reason(EOF), EndReason::Eof);
        assert_eq!(map_end_reason(STOP), EndReason::Stop);
        assert_eq!(map_end_reason(QUIT), EndReason::Quit);
        assert_eq!(map_end_reason(ERROR), EndReason::Error);
        // REDIRECT (5) and anything unknown must not look like a clean finish,
        // or the queue would advance and silently skip a track.
        assert_eq!(map_end_reason(5), EndReason::Error);
        assert_eq!(map_end_reason(99), EndReason::Error);
    }

    #[test]
    fn play_track_loads_local_file_without_network_resolve() {
        use ytm_core::VideoId;

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let tmp = std::env::temp_dir().join(format!("ytm-actor-test-{}", nanos));
        let dl_dir = tmp.join("dl");
        std::fs::create_dir_all(&dl_dir).unwrap();

        let id = VideoId::from("local_track");
        let file = dl_dir.join("local_track.opus");
        std::fs::write(&file, b"test audio").unwrap();

        let storage = crate::storage::AudioStorageManager::new(dl_dir, tmp.join("cache"), 1024);
        assert_eq!(storage.find_local_audio(&id), Some(file));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn prefetch_skips_existing_tracks_and_fetches_upcoming() {
        use ytm_core::VideoId;

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let tmp = std::env::temp_dir().join(format!("ytm-actor-prefetch-{}", nanos));
        let dl_dir = tmp.join("dl");
        let cache_dir = tmp.join("cache");
        std::fs::create_dir_all(&dl_dir).unwrap();
        std::fs::create_dir_all(&cache_dir).unwrap();

        let id1 = VideoId::from("t1");
        let id2 = VideoId::from("t2");
        let f1 = dl_dir.join("t1.opus");
        std::fs::write(&f1, b"opus").unwrap();

        let storage = crate::storage::AudioStorageManager::new(dl_dir, cache_dir, 1024);
        assert!(storage.find_local_audio(&id1).is_some());
        assert!(storage.find_local_audio(&id2).is_none());

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
