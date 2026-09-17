//! The event loop. Owns AppState; nothing here may block on I/O (NFR-2).

use crate::mpris;
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io::Stdout;
use std::sync::Arc;
use tokio::sync::mpsc;
use ytm_core::MusicSource;
use ytm_player::player::{Player, PlayerCommand};
use ytm_tui::{
    app::{AppState, ConfirmAction, Focus, Modal, Pane, PromptAction, ToastKind},
    event::{AppEvent, InputAction},
    keymap::KeyMap,
    mutation::Mutation,
    search_state::SearchDebounce,
    theme::Theme,
};

/// What a config reload produced. Rebuilt together: a `[keys]` change, a
/// `[ui] theme` change, and a `[behaviour]` change all land in the same file.
pub struct ReloadedConfig {
    pub keymap: KeyMap,
    pub theme: Theme,
    pub theme_name: String,
    pub behaviour: crate::config::BehaviourConfig,
    pub custom_theme: Option<Theme>,
    pub theme_file: Option<std::path::PathBuf>,
    pub auto_reload_theme: bool,
}

/// Suspend the TUI, open `$EDITOR` on the config, reload on exit (`,`). A parse
/// error is returned rather than applied, so a typo cannot reset the live keymap
/// and theme. Raw mode and the alternate screen are released around the child.
pub fn edit_config_in_editor(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    path: &std::path::Path,
) -> color_eyre::Result<Option<ReloadedConfig>> {
    use crossterm::{
        execute,
        terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
    };

    let Some(editor) = editor_command() else {
        return Err(color_eyre::eyre::eyre!(
            "set $EDITOR (or $VISUAL) to edit the config from here"
        ));
    };

    // Written on demand so the editor always opens on something real, and the
    // user gets the documented defaults rather than an empty buffer.
    if !path.exists() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, crate::config::EXAMPLE_TOML)?;
    }
    let before = std::fs::read_to_string(path).unwrap_or_default();

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        crossterm::event::DisableMouseCapture,
        LeaveAlternateScreen,
        crossterm::cursor::Show
    )?;

    // Split so `EDITOR="code -w"` works, not just a bare binary name.
    let mut parts = editor.split_whitespace();
    let bin = parts.next().unwrap_or("vi");
    let status = std::process::Command::new(bin)
        .args(parts)
        .arg(path)
        .status();

    // Restore the TUI before reporting anything: an error surfaces as a toast,
    // which needs the alternate screen back.
    enable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        EnterAlternateScreen,
        crossterm::event::EnableMouseCapture,
        crossterm::cursor::Hide
    )?;
    terminal.clear()?;

    let status = status?;
    if !status.success() {
        return Err(color_eyre::eyre::eyre!("{bin} exited with {status}"));
    }
    let after = std::fs::read_to_string(path)?;
    let reloaded = reload_config(&after)?;
    if after == before && reloaded.theme_file.is_none() {
        return Ok(None);
    }
    Ok(Some(reloaded))
}

/// Rebuild the keymap and theme from config text. Separate from the editor so
/// it is testable without spawning a process.
pub fn reload_config(text: &str) -> color_eyre::Result<ReloadedConfig> {
    let cfg = crate::config::Config::from_toml_str(text)?;
    let keys_toml = toml::to_string(&cfg.keys)?;
    let keymap = KeyMap::from_toml_str_with(&keys_toml, cfg.ui.vim_keys)?;
    let (theme, theme_name) = crate::config::resolve_theme(&cfg)?;
    let custom_theme = if theme_name == "custom" {
        Some(theme)
    } else {
        None
    };
    let theme_file = cfg
        .ui
        .theme_file
        .as_deref()
        .map(crate::config::expand_tilde);
    Ok(ReloadedConfig {
        keymap,
        theme,
        theme_name,
        behaviour: cfg.behaviour,
        custom_theme,
        theme_file,
        auto_reload_theme: cfg.ui.auto_reload_theme,
    })
}

/// Check if a theme file on disk has been modified and can be parsed as a valid theme.
/// Returns `Some(new_theme)` if the file modified time changed and the file parsed cleanly.
/// If unchanged or if parsing fails (e.g. while being written), returns `None`.
pub fn check_theme_file_update(
    path: &std::path::Path,
    last_mtime: &mut Option<std::time::SystemTime>,
) -> Option<Theme> {
    let mtime = std::fs::metadata(path).ok()?.modified().ok()?;
    if last_mtime.as_ref() == Some(&mtime) {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    let theme = Theme::from_toml_str(&text).ok()?;
    *last_mtime = Some(mtime);
    Some(theme)
}

/// `$VISUAL` first, then `$EDITOR` — the conventional order.
pub fn editor_command() -> Option<String> {
    for key in ["VISUAL", "EDITOR"] {
        if let Ok(v) = std::env::var(key)
            && !v.trim().is_empty()
        {
            return Some(v);
        }
    }
    None
}

/// What a guest is told at startup, and again if a cookie turns out to be dead.
pub const GUEST_NOTICE: &str =
    "guest mode — search and queue are available; set auth.cookie_file for your library";

/// Settle the UI against what the source can actually do. The handshake is the
/// only moment an *expired* cookie shows: YouTube answers one with HTTP 200, so
/// `run_tui` had already opened a library pane that would stay forever empty.
fn source_ready(state: &mut AppState, authenticated: bool) {
    state.guest = !authenticated;
    if !state.guest || !state.pane.requires_auth() {
        return;
    }
    // Cleared before the pane changes: these came from the cache, and they are
    // another account's rows as far as this session is concerned (FR-G9).
    state.playlists.clear();
    state.tracks.clear();
    state.albums.clear();
    state.artists.clear();
    state.home_rows.clear();
    state.open_playlist = None;
    state.pane = Pane::Search;
    state.sidebar_selected = 5;
    state.focus = Focus::SearchInput;
    state.push_toast(ToastKind::Info, GUEST_NOTICE, state.elapsed_ms);
}

/// Start background work, declining with a toast if the source is not built yet
/// — replaying it later would fire a request the user moved on from. Opening a
/// playlist shows its cached rows first; nothing read them back before.
#[allow(clippy::too_many_arguments)]
fn try_spawn(
    task: Task,
    source: &Option<Arc<dyn MusicSource>>,
    tx: &mpsc::UnboundedSender<AppEvent>,
    state: &mut AppState,
    cache: Option<&ytm_core::cache::Cache>,
    storage: Option<&ytm_player::storage::AudioStorageManager>,
    cookie_file: Option<&std::path::Path>,
    cache_writer: Option<&std::sync::mpsc::Sender<CacheWork>>,
) {
    if let Task::LoadDownloads = &task {
        if let Some(cache) = cache {
            match cache.get_downloaded_tracks() {
                Ok(v) => {
                    let tracks = v.into_iter().map(|dt| dt.to_track()).collect();
                    let _ = tx.send(AppEvent::DownloadedTracksLoaded(tracks));
                }
                Err(e) => {
                    tracing::warn!(error = %e, "could not read downloaded tracks");
                    state.loading = false;
                }
            }
        } else {
            state.loading = false;
        }
        return;
    }
    if let Task::Download(tracks) = task {
        if let Some(storage) = storage.cloned() {
            let cookie = cookie_file.map(|p| p.to_path_buf());
            let tx = tx.clone();
            let writer = cache_writer.cloned();
            tokio::task::spawn_blocking(move || {
                for track in tracks {
                    let dest = storage
                        .download_dir()
                        .join(format!("{}.opus", track.video_id.as_str()));
                    match storage.download_track_sync(&track.video_id, &dest, cookie.as_deref()) {
                        Ok(saved_path) => {
                            let file_size = saved_path.metadata().map(|m| m.len()).unwrap_or(0);
                            let dt = ytm_core::DownloadedTrack {
                                video_id: track.video_id.clone(),
                                title: track.title.clone(),
                                artists: track.artists.clone(),
                                album: track.album.clone(),
                                duration_secs: track.duration.as_secs(),
                                thumbnail_url: track.thumbnail_url.clone(),
                                file_path: saved_path.to_string_lossy().into_owned(),
                                file_size_bytes: file_size,
                                downloaded_at: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_secs())
                                    .unwrap_or(0),
                            };
                            if let Some(w) = &writer {
                                let _ = w.send(CacheWork::SaveDownloaded(dt));
                            }
                            let _ = tx.send(AppEvent::DownloadedTrackSaved(track));
                        }
                        Err(e) => {
                            let _ = tx.send(AppEvent::Error(format!(
                                "Download failed for \"{}\": {}",
                                track.title, e
                            )));
                        }
                    }
                }
            });
        } else {
            state.push_toast(
                ToastKind::Error,
                "Download failed: storage not configured",
                state.elapsed_ms,
            );
        }
        return;
    }
    if let Task::DeleteDownloads(tracks) = task {
        if let Some(storage) = storage.cloned() {
            let writer = cache_writer.cloned();
            tokio::task::spawn_blocking(move || {
                for track in tracks {
                    for ext in [".opus", ".webm", ".m4a", ".mp3", ""] {
                        let p = storage
                            .download_dir()
                            .join(format!("{}{ext}", track.video_id.as_str()));
                        if p.is_file() {
                            let _ = std::fs::remove_file(p);
                        }
                    }
                    if let Some(w) = &writer {
                        let _ = w.send(CacheWork::RemoveDownloaded(track.video_id.clone()));
                    }
                }
            });
        }
        return;
    }
    if let Task::EnqueuePlaylist {
        id,
        title,
        play_next,
    } = &task
    {
        let play_next = *play_next;
        let title = title.clone();
        let id = id.clone();
        if let Some(cache) = cache
            && let Ok(tracks) = cache.load_playlist_tracks(&id)
            && !tracks.is_empty()
        {
            state.loading = false;
            let where_to = if play_next {
                "playing next"
            } else {
                "added to queue"
            };
            let toast = match tracks.len() {
                1 => format!("{where_to}: {title} (1 track)"),
                n => format!("{where_to}: {title} ({n} tracks)"),
            };
            let _ = tx.send(AppEvent::EnqueueTracks {
                tracks,
                play_next,
                toast: Some(toast),
            });
            return;
        }
        let Some(src) = source.clone() else {
            state.loading = false;
            state.push_toast(ToastKind::Info, "still connecting…", state.elapsed_ms);
            return;
        };
        let tx = tx.clone();
        let writer = cache_writer.cloned();
        tokio::spawn(async move {
            match src.playlist_tracks(id.clone()).await {
                Ok(tracks) => {
                    if tracks.is_empty() {
                        let _ = tx.send(AppEvent::Error(format!("playlist \"{title}\" is empty")));
                        return;
                    }
                    if let Some(w) = &writer {
                        let _ = w.send(CacheWork::PlaylistTracks {
                            id: id.clone(),
                            tracks: tracks.clone(),
                        });
                    }
                    let where_to = if play_next {
                        "playing next"
                    } else {
                        "added to queue"
                    };
                    let toast = match tracks.len() {
                        1 => format!("{where_to}: {title} (1 track)"),
                        n => format!("{where_to}: {title} ({n} tracks)"),
                    };
                    let _ = tx.send(AppEvent::EnqueueTracks {
                        tracks,
                        play_next,
                        toast: Some(toast),
                    });
                }
                Err(e) => {
                    let _ = tx.send(AppEvent::Error(format!("could not load playlist: {e}")));
                }
            }
        });
        return;
    }
    if let Task::EnqueueAlbum {
        id,
        title,
        play_next,
    } = &task
    {
        let play_next = *play_next;
        let title = title.clone();
        let id = id.clone();
        let Some(src) = source.clone() else {
            state.loading = false;
            state.push_toast(ToastKind::Info, "still connecting…", state.elapsed_ms);
            return;
        };
        let tx = tx.clone();
        tokio::spawn(async move {
            match src.album_tracks(id).await {
                Ok(tracks) => {
                    if tracks.is_empty() {
                        let _ = tx.send(AppEvent::Error(format!("album \"{title}\" is empty")));
                        return;
                    }
                    let where_to = if play_next {
                        "playing next"
                    } else {
                        "added to queue"
                    };
                    let toast = match tracks.len() {
                        1 => format!("{where_to}: {title} (1 track)"),
                        n => format!("{where_to}: {title} ({n} tracks)"),
                    };
                    let _ = tx.send(AppEvent::EnqueueTracks {
                        tracks,
                        play_next,
                        toast: Some(toast),
                    });
                }
                Err(e) => {
                    let _ = tx.send(AppEvent::Error(format!("could not load album: {e}")));
                }
            }
        });
        return;
    }
    if let Task::OpenPlaylist(id) = &task
        && let Some(cache) = cache
    {
        preload_playlist_tracks(cache, state, id);
    }
    match source {
        Some(src) => spawn_task(task, src.clone(), tx.clone()),
        None => state.push_toast(ToastKind::Info, "still connecting…", state.elapsed_ms),
    }
}

/// How close two clicks must be to count as a double-click. 400ms is the common
/// desktop default: shorter reads a deliberate double-click as two singles,
/// longer lets two unrelated clicks play something.
const DOUBLE_CLICK_MS: u64 = 400;

/// Work the loop should start in the background as a result of an input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Task {
    /// The home feed's recommendation shelves (FR-B6).
    LoadHome,
    LoadPlaylists,
    LoadSongs,
    LoadAlbums,
    LoadArtists,
    OpenPlaylist(ytm_core::PlaylistId),
    EnqueuePlaylist {
        id: ytm_core::PlaylistId,
        title: String,
        play_next: bool,
    },
    EnqueueAlbum {
        id: ytm_core::AlbumId,
        title: String,
        play_next: bool,
    },
    /// An artist's top tracks (FR-B7). Carries the name so the heading can be
    /// set without looking it up — an artist opened from search is not in
    /// `artists`, so there would be nothing to look up.
    OpenArtist {
        id: ytm_core::ArtistId,
        name: String,
    },
    /// An album's songs. Carries the title for the heading, like `OpenArtist`.
    OpenAlbum {
        id: ytm_core::AlbumId,
        name: String,
    },
    Search(String),
    SearchArtists(String),
    /// Refresh the downloaded tracks list from SQLite.
    LoadDownloads,
    /// A server-side edit, carrying the token of the optimistic change it
    /// settles. Same spawn path as a read so the loop keeps one.
    Mutate {
        token: u64,
        task: MutationTask,
    },
    /// Download track(s) for offline listening.
    Download(Vec<ytm_core::Track>),
    /// Delete downloaded track(s) from disk and local index.
    DeleteDownloads(Vec<ytm_core::Track>),
}

/// Background work that changes server state. Separate from `Task` because these
/// carry a mutation token: the response has to name the optimistic edit it
/// settles, or a late failure reverts whichever edit was newest (FR-C6).
#[derive(Debug, Clone, PartialEq, Eq)]
// Delete is wired in Task 31, AddTracks/RemoveTracks in Tasks 31-32. Declared
// now because `run_mutation` handles all five, and a partial enum would mean
// touching its match again for every task.
#[allow(dead_code)]
pub enum MutationTask {
    Create {
        title: String,
        description: Option<String>,
        privacy: ytm_core::Privacy,
    },
    Rename {
        id: ytm_core::PlaylistId,
        title: String,
    },
    Delete {
        id: ytm_core::PlaylistId,
    },
    AddTracks {
        id: ytm_core::PlaylistId,
        videos: Vec<ytm_core::VideoId>,
    },
    RemoveTracks {
        id: ytm_core::PlaylistId,
        entries: Vec<ytm_core::SetVideoId>,
    },
}

/// Perform one mutation and report the outcome, tagged with its token.
pub async fn run_mutation(
    token: u64,
    task: MutationTask,
    source: Arc<dyn MusicSource>,
) -> AppEvent {
    let (result, ok_msg): (
        Result<Option<ytm_core::PlaylistId>, ytm_core::SourceError>,
        &str,
    ) = match task {
        MutationTask::Create {
            title,
            description,
            privacy,
        } => (
            source
                .create_playlist(title, description, privacy)
                .await
                .map(Some),
            "playlist created",
        ),
        MutationTask::Rename { id, title } => (
            source
                .edit_playlist(id, Some(title), None, None)
                .await
                .map(|_| None),
            "playlist renamed",
        ),
        MutationTask::Delete { id } => (
            source.delete_playlist(id).await.map(|_| None),
            "playlist deleted",
        ),
        MutationTask::AddTracks { id, videos } => (
            source.add_tracks(id, videos).await.map(|_| None),
            "added to playlist",
        ),
        MutationTask::RemoveTracks { id, entries } => (
            source.remove_tracks(id, entries).await.map(|_| None),
            "removed from playlist",
        ),
    };

    match result {
        Ok(real_id) => AppEvent::MutationOk {
            token,
            real_id,
            message: ok_msg.to_owned(),
        },
        Err(e) => AppEvent::MutationFailed {
            token,
            message: e.to_string(),
        },
    }
}

/// A temp id for an optimistic row, replaced by the server's on commit. Prefixed
/// so a leaked one is obvious in a log rather than looking like a real id.
fn temp_playlist_id(token: u64) -> ytm_core::PlaylistId {
    ytm_core::PlaylistId::from(format!("ytm-tui-temp-{token}").as_str())
}

/// Apply an open prompt and return the edit plus the API call it needs.
/// Validation happens here, not in the modal: an empty name is refused before any
/// optimistic row appears, so there is nothing to roll back.
pub fn submit_prompt(state: &mut AppState) -> Option<(u64, MutationTask)> {
    let Some(Modal::Prompt { value, action, .. }) = state.modal.clone() else {
        return None;
    };
    let title = value.trim().to_owned();
    if title.is_empty() {
        state.push_toast(ToastKind::Error, "a name is required", state.elapsed_ms);
        return None;
    }
    state.modal = None;

    match action {
        PromptAction::CreatePlaylist => {
            // The id is a placeholder until MutationOk brings the real one.
            // Peek rather than take, so the id names the token that settles it.
            let temp = ytm_core::Playlist {
                title: title.clone(),
                ..ytm_core::Playlist::stub(
                    temp_playlist_id(state.pending.peek_token()).as_str(),
                    &title,
                )
            };
            let token = state.begin_mutation(Mutation::CreatePlaylist { temp });
            Some((
                token,
                MutationTask::Create {
                    title,
                    description: None,
                    privacy: ytm_core::Privacy::Private,
                },
            ))
        }
        PromptAction::RenamePlaylist(id) => {
            let previous = state.playlists.iter().find(|p| p.id == id)?.title.clone();
            let token = state.begin_mutation(Mutation::RenamePlaylist {
                id: id.clone(),
                previous,
                next: title.clone(),
            });
            Some((token, MutationTask::Rename { id, title }))
        }
    }
}

/// Open the create prompt. Always allowed — it depends on no selection.
pub fn open_create_prompt(state: &mut AppState) {
    state.modal = Some(Modal::Prompt {
        title: "New playlist name".to_owned(),
        value: String::new(),
        action: PromptAction::CreatePlaylist,
    });
}

/// Open the rename prompt for the selected playlist, pre-filled with its title.
/// Refuses a system playlist before any API call: FR-C2 does not apply to them and
/// YouTube rejects the edit, so an optimistic rename would snap back.
pub fn open_rename_prompt(state: &mut AppState) -> Option<ytm_core::PlaylistId> {
    let p = state.selected_playlist()?;
    if p.is_system {
        let msg = format!("\"{}\" cannot be renamed", p.title);
        state.push_toast(ToastKind::Error, &msg, state.elapsed_ms);
        return None;
    }
    let (id, title) = (p.id.clone(), p.title.clone());
    state.modal = Some(Modal::Prompt {
        title: "Rename playlist".to_owned(),
        value: title,
        action: PromptAction::RenamePlaylist(id.clone()),
    });
    Some(id)
}

/// Ask before deleting (FR-C3). Refuses system playlists outright.
pub fn open_delete_confirm(state: &mut AppState) {
    let Some(p) = state.selected_playlist() else {
        return;
    };
    if p.is_system {
        // No confirmation for an action that cannot succeed: asking "are you
        // sure?" about something impossible only wastes a keystroke.
        let msg = format!(
            "\"{}\" is managed by YouTube Music and cannot be deleted",
            p.title
        );
        state.push_toast(ToastKind::Error, &msg, state.elapsed_ms);
        return;
    }
    state.modal = Some(Modal::Confirm {
        text: format!("Delete \"{}\"? This cannot be undone.", p.title),
        action: ConfirmAction::DeletePlaylist(p.id),
    });
}

/// The user pressed `y`. Apply optimistically and hand back the work to do.
pub fn confirm_action(state: &mut AppState) -> Option<(u64, MutationTask)> {
    let Some(Modal::Confirm { action, .. }) = state.modal.take() else {
        return None;
    };
    match action {
        // Handled in `dispatch_input`'s modal branch: a quit is not a mutation
        // and has nothing to roll back, so it never reaches here.
        ConfirmAction::Quit => None,
        ConfirmAction::DeletePlaylist(id) => {
            let index = state.playlists.iter().position(|p| p.id == id)?;
            let snapshot = state.playlists[index].clone();
            let token = state.begin_mutation(Mutation::DeletePlaylist {
                id: id.clone(),
                index,
                snapshot,
            });
            Some((token, MutationTask::Delete { id }))
        }
        ConfirmAction::RemoveTracks { playlist, entries } => {
            // Indices come from the list as it stands, so rollback can put each
            // track back where it was.
            let removed: Vec<(usize, ytm_core::Track)> = state
                .tracks
                .iter()
                .enumerate()
                .filter(|(_, t)| {
                    t.set_video_id
                        .as_ref()
                        .is_some_and(|sv| entries.contains(sv))
                })
                .map(|(i, t)| (i, t.clone()))
                .collect();
            let token = state.begin_mutation(Mutation::RemoveTracks {
                playlist: playlist.clone(),
                removed,
            });
            Some((
                token,
                MutationTask::RemoveTracks {
                    id: playlist,
                    entries,
                },
            ))
        }
    }
}

/// Marked tracks if any, otherwise the selected one (FR-C4). Ordered by the rows
/// on screen, not by `marked`'s iteration order: it is a `HashSet`, so returning
/// it directly sent a marked run to the API scrambled.
pub fn targets_for_add(state: &AppState) -> Vec<ytm_core::VideoId> {
    if !state.marked.is_empty() {
        return state
            .track_rows()
            .iter()
            .map(|t| t.video_id.clone())
            .filter(|id| state.marked.contains(id))
            .collect();
    }
    state
        .selected_track()
        .map(|t| vec![t.video_id.clone()])
        .unwrap_or_default()
}

/// Full tracks for a queue action: the marked rows, else the selected one.
/// Tracks rather than ids because the player queues `Track`s, and re-looking them
/// up by id would be a second source of truth.
fn queue_targets(state: &AppState) -> Vec<ytm_core::Track> {
    if !state.marked.is_empty() {
        return state
            .track_rows()
            .iter()
            .filter(|t| state.marked.contains(&t.video_id))
            .cloned()
            .collect();
    }
    state.selected_track().into_iter().collect()
}

/// What the toast says. Names a single track; counts a selection, because
/// naming only the first of twelve reads as a bug.
fn enqueue_message(tracks: &[ytm_core::Track], next: bool) -> String {
    let where_to = if next {
        "playing next"
    } else {
        "added to queue"
    };
    match tracks {
        [one] => format!("{}: {}", where_to, one.title),
        many => format!("{} {} tracks", where_to, many.len()),
    }
}

/// Open the target-playlist picker for the marked or selected tracks. Only
/// editable playlists are offered: YouTube rejects an add to a system playlist,
/// so listing one would be offering an action that cannot work.
pub fn open_add_to_playlist(state: &mut AppState) {
    let targets = targets_for_add(state);
    if targets.is_empty() {
        state.push_toast(ToastKind::Error, "nothing selected", state.elapsed_ms);
        return;
    }
    let choices: Vec<(ytm_core::PlaylistId, String)> = state
        .playlists
        .iter()
        .filter(|p| !p.is_system)
        .map(|p| (p.id.clone(), p.title.clone()))
        .collect();
    if choices.is_empty() {
        state.push_toast(
            ToastKind::Error,
            "no editable playlist to add to — create one with N",
            state.elapsed_ms,
        );
        return;
    }
    state.modal = Some(Modal::PickPlaylist {
        targets,
        choices,
        selected: 0,
    });
}

/// The user picked a playlist. Hand back the add to run. No optimistic row to
/// show — the tracks go to a playlist that need not be the one on screen, so
/// `Mutation::AddTracks` records the edit for the toast and nothing else changes.
pub fn submit_pick(state: &mut AppState) -> Option<(u64, MutationTask)> {
    let Some(Modal::PickPlaylist {
        targets,
        choices,
        selected,
    }) = state.modal.take()
    else {
        return None;
    };
    let (id, _) = choices.get(selected)?.clone();
    let token = state.begin_mutation(Mutation::AddTracks {
        playlist: id.clone(),
        count: targets.len(),
    });
    // The marks were the input to this action; leaving them set would make the
    // next `A` silently repeat it.
    state.clear_marks();
    Some((
        token,
        MutationTask::AddTracks {
            id,
            videos: targets,
        },
    ))
}

/// Confirm before removing (FR-C5). Playlist reads do carry `set_video_id` now —
/// `playlist_raw` extracts what `ytmapi-rs` 0.3.3 drops — so the refusal is the
/// fallback for when extraction finds nothing rather than the normal path.
pub fn open_remove_confirm(state: &mut AppState) {
    let Some(playlist) = state.open_playlist.clone() else {
        state.push_toast(ToastKind::Error, "open a playlist first", state.elapsed_ms);
        return;
    };

    let targets = targets_for_add(state);
    let entries: Vec<ytm_core::SetVideoId> = state
        .tracks
        .iter()
        .filter(|t| targets.contains(&t.video_id))
        .filter_map(|t| t.set_video_id.clone())
        .collect();

    if entries.is_empty() {
        state.push_toast(
            ToastKind::Error,
            "these tracks cannot be removed — try refreshing the playlist",
            state.elapsed_ms,
        );
        return;
    }

    state.modal = Some(Modal::Confirm {
        text: format!("Remove {} track(s) from this playlist?", entries.len()),
        action: ConfirmAction::RemoveTracks { playlist, entries },
    });
}

/// What a Home card opens. Tracks play in place; a playlist, artist, or album
/// descends into its own pane first — loading rows while Home still draws the
/// feed lands them invisibly, which is what made these rows look dead.
fn open_home_item(state: &mut AppState, item: &ytm_core::HomeItem) -> Option<Task> {
    match item.target.clone() {
        ytm_core::HomeTarget::Playlist(id) => {
            state.goto_source(2);
            start(state, Task::OpenPlaylist(id))
        }
        ytm_core::HomeTarget::Artist(id) => {
            let name = item.title.clone();
            state.goto_source(5);
            start(state, Task::OpenArtist { id, name })
        }
        ytm_core::HomeTarget::Album(id) => {
            let name = item.title.clone();
            state.goto_source(4);
            start(state, Task::OpenAlbum { id, name })
        }
        ytm_core::HomeTarget::Track(_) => None,
    }
}

/// Translate one input action into player commands, state changes, and
/// background work. Pure with respect to I/O — that is what makes it testable.
pub fn dispatch_input(
    action: InputAction,
    state: &mut AppState,
    player: &impl Player,
    behaviour: &crate::config::BehaviourConfig,
) -> Option<Task> {
    use InputAction as A;

    // A modal owns the keyboard, transport included. Enter on a prompt is the
    // one action the reducer cannot finish, because submitting means an API
    // call — everything else is a state transition.
    if state.modal.is_some() {
        if action == A::Confirm && matches!(state.modal, Some(Modal::Prompt { .. })) {
            let (token, task) = submit_prompt(state)?;
            return Some(Task::Mutate { token, task });
        }
        if action == A::Confirm && matches!(state.modal, Some(Modal::PickPlaylist { .. })) {
            let (token, task) = submit_pick(state)?;
            return Some(Task::Mutate { token, task });
        }
        if matches!(state.modal, Some(Modal::Confirm { .. })) {
            // Quit settles no server edit, so it never reaches `confirm_action`,
            // which returns a MutationTask. `q` confirms as well as `y`: it is
            // the key the user just pressed, and making it mean "no" is a trap.
            if matches!(
                state.modal,
                Some(Modal::Confirm {
                    action: ConfirmAction::Quit,
                    ..
                })
            ) {
                match action {
                    A::Char('y') | A::Char('Y') | A::Confirm | A::Quit => {
                        state.modal = None;
                        state.should_quit = true;
                        return None;
                    }
                    A::Char('n') | A::Char('N') => {
                        state.modal = None;
                        return None;
                    }
                    _ => {}
                }
            }
            match action {
                // `y`/`n` are not keymap bindings: they mean nothing outside a
                // confirm, and binding them globally would shadow real keys.
                A::Char('y') | A::Char('Y') | A::Confirm => {
                    let (token, task) = confirm_action(state)?;
                    return Some(Task::Mutate { token, task });
                }
                A::Char('n') | A::Char('N') => {
                    state.modal = None;
                    return None;
                }
                _ => {}
            }
        }
        state.apply(AppEvent::Input(action));
        return None;
    }

    // Playlist editing needs an account. Refused here rather than deeper down so
    // no modal opens for an edit that could never be sent — the `x` arm below is
    // queue-local and must keep working, so it is excluded by pane.
    let account_action = match &action {
        A::AddToPlaylist | A::CreatePlaylist | A::RenamePlaylist | A::DeletePlaylist => true,
        A::RemoveFromPlaylist => state.pane != Pane::Queue && state.pane != Pane::Downloads,
        _ => false,
    };
    if state.guest && account_action {
        state.guest_refusal("playlist editing");
        return None;
    }

    // Transport actions belong to the player; everything else to the state.
    match action {
        // Diverted before the fall-through, which is what sets `should_quit`.
        A::Quit if behaviour.confirm_on_quit => {
            state.modal = Some(Modal::Confirm {
                text: "Quit ytm-tui? (y/n)".to_owned(),
                action: ConfirmAction::Quit,
            });
        }
        A::TogglePause => {
            send(player, PlayerCommand::TogglePause);
        }
        A::NextTrack => {
            send(player, PlayerCommand::Next);
        }
        A::PrevTrack => {
            send(player, PlayerCommand::Previous);
        }
        A::SeekForward => {
            send(
                player,
                PlayerCommand::SeekRelative(behaviour.seek_step_secs),
            );
        }
        A::SeekBack => {
            send(
                player,
                PlayerCommand::SeekRelative(-behaviour.seek_step_secs),
            );
        }
        A::VolumeUp | A::VolumeDown => {
            let delta = if action == A::VolumeUp {
                behaviour.volume_step
            } else {
                -behaviour.volume_step
            };
            // Set it locally too: the bar should move on the next frame rather
            // than waiting for the actor's VolumeChanged to come back.
            state.volume = ytm_player::player::clamp_volume(state.volume as i64 + delta);
            send(player, PlayerCommand::SetVolume(state.volume));
        }
        A::ToggleMute => {
            state.muted = !state.muted;
            send(player, PlayerCommand::ToggleMute);
        }
        A::ToggleShuffle => {
            state.shuffle = !state.shuffle;
            send(player, PlayerCommand::SetShuffle(state.shuffle));
        }
        A::CycleRepeat => {
            state.repeat = state.repeat.next();
            send(player, PlayerCommand::SetRepeat(state.repeat));
        }
        A::Confirm => {
            // In the queue, Enter plays the row that is already there. Going
            // through PlayNow (which inserts) put a second copy of a finished
            // track in beside the first.
            if state.pane == Pane::Queue {
                // `selected` counts visible rows; `JumpTo` takes a queue index.
                // Translate the filtered row, and send nothing when it is past the
                // end rather than indexing past the queue in the actor.
                if let Some(i) = state.queue_index_of_selected() {
                    send(player, PlayerCommand::JumpTo(i));
                }
                return None;
            }
            // In the playlist list, Enter opens; on a track, Enter plays.
            if let Some(p) = state.selected_playlist() {
                return start(state, Task::OpenPlaylist(p.id.clone()));
            }
            // An artist row opens their top tracks (FR-B7). Before this the
            // Artists pane was a dead end: names on screen, and Enter did
            // nothing at all.
            if let Some(a) = state.selected_artist() {
                return start(
                    state,
                    Task::OpenArtist {
                        id: a.id.clone(),
                        name: a.name.clone(),
                    },
                );
            }
            // An album row opens its songs, the same as a playlist row. Before
            // this the Albums pane was a dead end in the same way.
            if let Some(a) = state.selected_album() {
                return start(
                    state,
                    Task::OpenAlbum {
                        id: a.id.clone(),
                        name: a.title.clone(),
                    },
                );
            }
            // A home card does whatever its kind implies: a track plays, and a
            // playlist, artist, or album opens in its own pane. One carousel
            // holds all of them, so the row decides, not the pane.
            if state.pane == Pane::Home
                && let Some(item) = state.selected_home_item().cloned()
                && let Some(task) = open_home_item(state, &item)
            {
                return Some(task);
            }
            if let Some(t) = state.selected_track() {
                send(player, PlayerCommand::PlayNow(t));
            }
        }
        // Both honour a marked selection, so `V` over a run then `a` queues the
        // whole range rather than only the row under the cursor.
        A::AddToQueue | A::PlayNext => {
            if let Some(p) = state.selected_playlist() {
                return start(
                    state,
                    Task::EnqueuePlaylist {
                        id: p.id.clone(),
                        title: p.title.clone(),
                        play_next: action == A::PlayNext,
                    },
                );
            }
            if let Some(a) = state.selected_album() {
                return start(
                    state,
                    Task::EnqueueAlbum {
                        id: a.id.clone(),
                        title: a.title.clone(),
                        play_next: action == A::PlayNext,
                    },
                );
            }
            let tracks = queue_targets(state);
            if tracks.is_empty() {
                return None;
            }
            let msg = enqueue_message(&tracks, action == A::PlayNext);
            let cmd = if action == A::PlayNext {
                PlayerCommand::EnqueueNext(tracks)
            } else {
                PlayerCommand::EnqueueBack(tracks)
            };
            send(player, cmd);
            // FR-U3: without this, `a` outside the queue pane gives no sign it
            // worked — the queue is not on screen to show the new row.
            state.push_toast(ToastKind::Success, &msg, state.elapsed_ms);
            state.clear_marks();
        }
        // Queue edits are local to this pane; the actor owns queue truth and
        // answers with `QueueChanged`. `x` removes from a playlist elsewhere, and
        // server-side track reordering is out of scope.
        A::RemoveFromPlaylist if state.pane == Pane::Queue => {
            // Marked rows if any, else the cursor — `V` then `x` clears the range.
            // Remove highest-index first because each removal shifts what follows;
            // the cursor path uses the same visible-to-queue translation as Enter.
            let mut targets: Vec<usize> = if state.marked.is_empty() {
                state.queue_index_of_selected().into_iter().collect()
            } else {
                state
                    .queue
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| state.marked.contains(&t.video_id))
                    .map(|(i, _)| i)
                    .collect()
            };
            if !targets.is_empty() {
                let min_target = *targets.iter().min().unwrap();
                let remaining_count = state.queue.len().saturating_sub(targets.len());
                state.selected = min_target.min(remaining_count.saturating_sub(1));
                state.scroll_offset = state.scroll_offset.min(state.selected);
            }
            targets.sort_unstable_by(|a, b| b.cmp(a));
            for idx in targets {
                if idx < state.queue.len() {
                    send(player, PlayerCommand::RemoveFromQueue(idx));
                }
            }
            state.clear_marks();
        }
        A::RemoveFromPlaylist if state.pane == Pane::Downloads => {
            let targets: Vec<ytm_core::Track> = if state.marked.is_empty() {
                state.selected_track().into_iter().collect()
            } else {
                state
                    .downloaded_tracks
                    .iter()
                    .filter(|t| state.marked.contains(&t.video_id))
                    .cloned()
                    .collect()
            };
            if !targets.is_empty() {
                let min_target = if state.marked.is_empty() {
                    state.selected
                } else {
                    state
                        .downloaded_tracks
                        .iter()
                        .enumerate()
                        .filter(|(_, t)| state.marked.contains(&t.video_id))
                        .map(|(i, _)| i)
                        .min()
                        .unwrap_or(0)
                };
                let target_ids: std::collections::HashSet<_> =
                    targets.iter().map(|t| t.video_id.clone()).collect();
                state
                    .downloaded_tracks
                    .retain(|t| !target_ids.contains(&t.video_id));
                state.selected = min_target.min(state.downloaded_tracks.len().saturating_sub(1));
                state.clamp_selection();
                let msg = match targets.as_slice() {
                    [one] => format!("Deleted \"{}\" from downloads", one.title),
                    many => format!("Deleted {} tracks from downloads", many.len()),
                };
                state.push_toast(ToastKind::Success, &msg, state.elapsed_ms);
                state.clear_marks();
                return Some(Task::DeleteDownloads(targets));
            }
        }
        A::MoveEntryUp | A::MoveEntryDown if state.pane == Pane::Queue => {
            let down = action == A::MoveEntryDown;
            // Marked rows move as one block, the way `x` removes them as one —
            // without this a `V` range could be selected and deleted but never
            // reordered. Matched by video id, so the set is right under a filter.
            let mut targets: Vec<usize> = if state.marked.is_empty() {
                state.queue_index_of_selected().into_iter().collect()
            } else {
                state
                    .queue
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| state.marked.contains(&t.video_id))
                    .map(|(i, _)| i)
                    .collect()
            };
            // Out of range at either end: the actor would index past the queue.
            // Checked across the whole block, because sliding only the entries
            // with room left would squash the range together.
            let at_edge = if down {
                targets.iter().any(|i| i + 1 >= state.queue.len())
            } else {
                targets.contains(&0)
            };
            if !targets.is_empty() && !at_edge {
                // Highest index first going down, lowest first going up: each
                // move shifts everything past it, so the other order would drag
                // the block apart one entry at a time.
                targets.sort_unstable();
                if down {
                    targets.reverse();
                }
                for from in &targets {
                    let to = if down { from + 1 } else { from - 1 };
                    send(player, PlayerCommand::MoveInQueue { from: *from, to });
                }
                // Follow the entry, not the row, or a held key walks the selection
                // back over the track it just moved. The marks are video ids, so
                // they ride along and `J` can be held.
                let follows = state.marked.is_empty()
                    || state
                        .queue
                        .get(state.selected)
                        .is_some_and(|t| state.marked.contains(&t.video_id));
                if follows {
                    state.selected = if down {
                        state.selected + 1
                    } else {
                        state.selected.saturating_sub(1)
                    };
                }
                // The range has become a block move, so stop extending it: a
                // later `j` would otherwise recompute the marks from an anchor
                // that no longer points at the row it was set on.
                state.visual_anchor = None;
                state.marks_before_visual.clear();
            }
        }
        A::MoveEntryUp | A::MoveEntryDown => {}
        A::ClearQueue if state.pane == Pane::Queue => {
            send(player, PlayerCommand::ClearQueue);
            state.selected = 0;
            state.scroll_offset = 0;
            state.clear_marks();
        }
        A::ClearQueue => {}
        A::Download => {
            let tracks = queue_targets(state);
            if tracks.is_empty() {
                state.push_toast(
                    ToastKind::Error,
                    "nothing selected to download",
                    state.elapsed_ms,
                );
                return None;
            }
            let msg = match tracks.as_slice() {
                [one] => format!("Downloading \"{}\"…", one.title),
                many => format!("Downloading {} tracks…", many.len()),
            };
            state.push_toast(ToastKind::Info, &msg, state.elapsed_ms);
            state.clear_marks();
            return Some(Task::Download(tracks));
        }
        A::AddToPlaylist => open_add_to_playlist(state),
        A::RemoveFromPlaylist => open_remove_confirm(state),
        A::ToggleMark => state.toggle_mark(),
        A::CreatePlaylist => open_create_prompt(state),
        A::DeletePlaylist => open_delete_confirm(state),
        A::RenamePlaylist => {
            open_rename_prompt(state);
        }
        A::Refresh => {
            return start(state, pane_task(state.pane)?);
        }
        // The forward half of the h/l pair, here rather than in the reducer because
        // opening needs a fetch. Only descends from the playlist list: on a track
        // pane it would make a navigation key start audio, and Enter is that key.
        A::Right => {
            if let Some(p) = state.selected_playlist() {
                return start(state, Task::OpenPlaylist(p.id.clone()));
            }
            // An artist row descends into their tracks, the same as Enter. The
            // reducer's Right arm assumed the loop did this and nothing here
            // checked, so `l` on an artist silently did nothing.
            if let Some(a) = state.selected_artist() {
                return start(
                    state,
                    Task::OpenArtist {
                        id: a.id.clone(),
                        name: a.name.clone(),
                    },
                );
            }
            // An album row descends into its songs, like the other two lists.
            if let Some(a) = state.selected_album() {
                return start(
                    state,
                    Task::OpenAlbum {
                        id: a.id.clone(),
                        name: a.title.clone(),
                    },
                );
            }
            // A home card opens the same thing `Enter` would: the row decides.
            if state.pane == Pane::Home
                && let Some(item) = state.selected_home_item().cloned()
                && let Some(task) = open_home_item(state, &item)
            {
                return Some(task);
            }
            state.apply(AppEvent::Input(A::Right));
        }
        // A number key switches pane, so the new pane needs its rows.
        A::GoTo(n) => {
            state.apply(AppEvent::Input(A::GoTo(n)));
            if let Some(task) = pane_task(state.pane) {
                return start(state, task);
            }
        }
        // Everything else is a state transition.
        other => state.apply(AppEvent::Input(other)),
    }
    None
}

/// Move the selection to whatever was clicked, returning a fetch if the click
/// changed pane. Selection only — a misplaced click that starts audio is worse
/// than one costing a keypress, and Enter or `a` is one key away.
fn handle_click(
    col: u16,
    row: u16,
    area: ratatui::layout::Rect,
    state: &mut AppState,
    player: &impl Player,
) -> Option<Task> {
    use ytm_tui::render::{ClickTarget, click_target};

    match click_target(
        area,
        col,
        row,
        state.search_row_visible(),
        state.filter_row_visible(),
        state.column_header_visible(),
    ) {
        ClickTarget::Source(i) => {
            // The sidebar draws PANE_ORDER from its first row, so the index maps
            // straight onto a source. Out of range is a click below the last
            // entry: ignored rather than clamped to whatever sits at the end.
            let n = u8::try_from(i + 1).ok()?;
            if usize::from(n) > ytm_tui::app::PANE_ORDER.len() {
                return None;
            }
            state.apply(AppEvent::Input(InputAction::GoTo(n)));
            pane_task(state.pane)
        }
        ClickTarget::Row(offset) => {
            // The offset is from the top of the *visible window*, so the scroll
            // position has to be added back or every click after scrolling would
            // select a row near the top of the list.
            let idx = state.scroll_offset + offset;
            if idx < state.list_len() {
                state.selected = idx;
                state.focus = ytm_tui::app::Focus::Main;
                state.refresh_visual_marks();
            }
            None
        }
        // Seeking is the one click that is not selection: the bar *is* a position.
        // `nowplaying` owns the geometry, so a wider flags column cannot make a
        // click land on a different second than the pointer.
        ClickTarget::Progress(col) => {
            let (_, np) = ytm_tui::render::body_and_nowplaying(area);
            let secs = ytm_tui::widgets::nowplaying::seek_target_secs(
                np.width as usize,
                col as usize,
                state,
            )?;
            send(player, PlayerCommand::SeekAbsolute(secs));
            None
        }
        ClickTarget::Nothing => None,
    }
}

/// The fetch a pane needs to fill itself, or `None` when it has nothing to load.
/// Queue is local state owned by the actor and Search waits for a query — a fetch
/// for either would be a request the user never made.
fn pane_task(pane: Pane) -> Option<Task> {
    Some(match pane {
        Pane::Home => Task::LoadHome,
        Pane::Playlists => Task::LoadPlaylists,
        Pane::Songs => Task::LoadSongs,
        Pane::Albums => Task::LoadAlbums,
        Pane::Artists => Task::LoadArtists,
        Pane::Downloads => Task::LoadDownloads,
        Pane::Search | Pane::Queue => return None,
    })
}

/// Record the current query on every keystroke, restarting the debounce timer.
/// Reads the query from `AppState` rather than taking the character, so the buffer
/// stays owned by the reducer. Keystrokes outside the search pane are ignored.
pub fn note_search_input(d: &mut SearchDebounce, state: &AppState, now_ms: u64) {
    // The Artists pane has its own search field, so typing there must debounce
    // too — otherwise the query built up and nothing was ever sent.
    if state.pane == Pane::Search || (state.pane == Pane::Artists && state.artist_search_active) {
        d.note_input(&state.search_query, now_ms);
    }
}

/// Call on every tick. Returns a search to run once typing has settled.
pub fn search_tick(d: &mut SearchDebounce, state: &mut AppState, now_ms: u64) -> Option<Task> {
    let q = d.should_fire(now_ms)?;
    // Which search fired depends on where the field is: the Artists pane's own
    // field looks for artists, and sending its query to `search_songs` would
    // fill the artist list with tracks.
    let task = if state.pane == Pane::Artists && state.artist_search_active {
        Task::SearchArtists(q)
    } else {
        Task::Search(q)
    };
    start(state, task)
}

/// A dropped command means the actor thread is gone. There is nothing useful to
/// do about it from here, so log it rather than unwrapping into a panic that
/// would take the terminal down with it.
fn send(player: &impl Player, cmd: PlayerCommand) {
    if let Err(e) = player.send(cmd) {
        tracing::error!(error = %e, "player command dropped");
    }
}

/// Run until the user quits. Four event sources, one owner of state. Nothing here
/// awaits network or audio work: every slow thing goes to `spawn_task` or the
/// player actor and returns as an `AppEvent` (NFR-2), so every arm is cheap.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    mut state: AppState,
    // Not built yet on purpose: awaiting cookie validation before the loop froze
    // the app for seconds with the cached frame up, and buffered keys then all
    // fired at once. The loop awaits this concurrently with input.
    source_fut: impl std::future::Future<Output = color_eyre::Result<Arc<dyn MusicSource>>>,
    player: impl Player,
    mut player_events: mpsc::UnboundedReceiver<ytm_player::player::PlayerEvent>,
    mut keymap: KeyMap,
    mut theme: Theme,
    tick_ms: u64,
    mut behaviour: crate::config::BehaviourConfig,
    cookie_auth: bool,
    cache_reader: Option<ytm_core::cache::Cache>,
    cache_writer: Option<std::sync::mpsc::Sender<CacheWork>>,
    mut art: ytm_tui::widgets::art::ArtCache,
    mut media: Option<souvlaki::MediaControls>,
    // `config_path` is where `,` opens an editor and what a reload re-reads;
    // `theme_name` tracks where `t` is in the preset cycle.
    mut media_keys: mpsc::UnboundedReceiver<PlayerCommand>,
    config_path: std::path::PathBuf,
    mut theme_name: String,
    mut custom_theme: Option<Theme>,
    mut theme_file: Option<std::path::PathBuf>,
    mut auto_reload_theme: bool,
    storage: Option<ytm_player::storage::AudioStorageManager>,
    cookie_file: Option<std::path::PathBuf>,
) -> color_eyre::Result<u8> {
    use crossterm::event::{Event as CtEvent, EventStream, KeyEventKind, MouseEventKind};
    use futures::StreamExt;

    // App-internal events (results of background work).
    let (app_tx, mut app_rx) = mpsc::unbounded_channel::<AppEvent>();
    let mut term_events = EventStream::new();
    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(tick_ms.max(1)));
    let started = std::time::Instant::now();
    let mut debounce = SearchDebounce::default();
    let mut last_theme_mtime = theme_file
        .as_ref()
        .and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok());
    let mut last_theme_check_ms: u64 = 0;
    const THEME_CHECK_INTERVAL_MS: u64 = 1000;
    // `zz` spans two key presses, so the prefix has to live across iterations.
    let mut pending = ytm_tui::keymap::Pending::default();
    // Last left click, for double-click detection: (row index, millis). Crossterm
    // reports no double-click event of its own, so two clicks on the same row
    // inside the window are what makes one.
    let mut last_click: Option<(usize, u64)> = None;

    // `main` has already drawn the cached frame — building the source needs a
    // network round trip, and NFR-1 will not survive doing that first. Redrawing
    // here is cheap and keeps `run` correct when called with a cold cache.
    terminal.draw(|f| ytm_tui::render::render(f, &state, &theme, &keymap, &mut art))?;
    // Loading from the first frame: the source is still being built, and the
    // spinner is the honest signal that something is in flight.
    state.loading = true;

    // `None` until the source is ready. Every key still works meanwhile —
    // navigation, the queue, playback of anything cached — and the actions needing
    // the network are skipped, not queued to fire after the user moved on.
    let mut source: Option<Arc<dyn MusicSource>> = None;
    let mut source_fut = std::pin::pin!(source_fut);
    let mut pending_first_fetch = true;

    loop {
        tokio::select! {
            // Terminal input
            Some(Ok(ev)) = term_events.next() => {
                match ev {
                    // The wheel scrolls the focused list (FR-U8). Clicks are not
                    // bound on purpose — capturing them would cost the user the
                    // terminal's own text selection.
                    CtEvent::Mouse(m) => {
                        use crossterm::event::MouseButton;
                        match m.kind {
                            MouseEventKind::ScrollDown => {
                                state.apply(AppEvent::Input(InputAction::ScrollDown));
                            }
                            MouseEventKind::ScrollUp => {
                                state.apply(AppEvent::Input(InputAction::ScrollUp));
                            }
                            // Left click selects — a sidebar entry switches source,
                            // a list row moves the cursor. Deliberately does not
                            // play: `a`/Enter are one key away once selected.
                            MouseEventKind::Down(MouseButton::Left) if state.modal.is_none() => {
                                let before = state.selected;
                                let task = handle_click(
                                    m.column,
                                    m.row,
                                    terminal.size()?.into(),
                                    &mut state,
                                    &player,
                                );
                                if let Some(task) = task {
                                    if let Some(t) = start(&mut state, task) {
                                        try_spawn(
                                            t,
                                            &source,
                                            &app_tx,
                                            &mut state,
                                            cache_reader.as_ref(),
                                            storage.as_ref(),
                                            cookie_file.as_deref(),
                                            cache_writer.as_ref(),
                                        );
                                    }
                                    // A sidebar click is not part of a double-click
                                    // on a row.
                                    last_click = None;
                                } else {
                                    // Second click on the same row activates it,
                                    // via `dispatch_input` so a playlist or artist
                                    // row opens instead of playing a non-track.
                                    let now = state.elapsed_ms;
                                    let same = last_click
                                        .is_some_and(|(row, at)| {
                                            row == state.selected
                                                && now.saturating_sub(at) <= DOUBLE_CLICK_MS
                                        });
                                    if same && state.selected == before {
                                        if let Some(t) = dispatch_input(InputAction::Confirm, &mut state, &player, &behaviour) {
                                            try_spawn(
                                                t,
                                                &source,
                                                &app_tx,
                                                &mut state,
                                                cache_reader.as_ref(),
                                                storage.as_ref(),
                                                cookie_file.as_deref(),
                                                cache_writer.as_ref(),
                                            );
                                        }
                                        last_click = None;
                                    } else {
                                        last_click = Some((state.selected, now));
                                    }
                                }
                            }
                            // Right click queues the row under the pointer, which is
                            // the one action worth a mouse shortcut.
                            MouseEventKind::Down(MouseButton::Right) if state.modal.is_none() => {
                                if handle_click(
                                    m.column,
                                    m.row,
                                    terminal.size()?.into(),
                                    &mut state,
                                    &player,
                                )
                                .is_none()
                                    && let Some(t) = state.selected_track()
                                {
                                    let msg = enqueue_message(std::slice::from_ref(&t), false);
                                    send(&player, PlayerCommand::EnqueueBack(vec![t]));
                                    state.push_toast(ToastKind::Success, &msg, state.elapsed_ms);
                                }
                            }
                            _ => {}
                        }
                    }
                    CtEvent::Key(k) if k.kind == KeyEventKind::Press => {
                        // `input_focus`, not `focus`: an open prompt is a text field, so
                        // letters must resolve to Char(c) rather than commands.
                        let (action, next) =
                            keymap.resolve_chord(k, state.input_focus(), pending);
                        pending = next;
                        if let Some(a) = action {
                            // Both of these own resources `dispatch_input` cannot
                            // reach: the live theme, and the terminal itself.
                            match a {
                                InputAction::CycleTheme if state.modal.is_none() => {
                                    theme_name = ytm_tui::theme::Theme::next_preset_with_custom(
                                        &theme_name,
                                        custom_theme.is_some(),
                                    )
                                    .to_owned();
                                    theme = if theme_name == "custom" {
                                        custom_theme.unwrap_or_default()
                                    } else {
                                        ytm_tui::theme::Theme::preset(&theme_name)
                                            .unwrap_or_default()
                                    };
                                    state.push_toast(
                                        ToastKind::Info,
                                        &format!("theme: {theme_name}"),
                                        state.elapsed_ms,
                                    );
                                }
                                InputAction::EditConfig if state.modal.is_none() => {
                                    match edit_config_in_editor(terminal, &config_path) {
                                        Ok(Some(reloaded)) => {
                                            keymap = reloaded.keymap;
                                            theme = reloaded.theme;
                                            theme_name = reloaded.theme_name;
                                            behaviour = reloaded.behaviour;
                                            custom_theme = reloaded.custom_theme;
                                            theme_file = reloaded.theme_file;
                                            auto_reload_theme = reloaded.auto_reload_theme;
                                            last_theme_mtime = theme_file
                                                .as_ref()
                                                .and_then(|p| std::fs::metadata(p).ok())
                                                .and_then(|m| m.modified().ok());
                                            state.push_toast(
                                                ToastKind::Success,
                                                "config reloaded",
                                                state.elapsed_ms,
                                            );
                                        }
                                        // Unchanged file, or no editor to open.
                                        Ok(None) => {}
                                        Err(e) => state.push_toast(
                                            ToastKind::Error,
                                            &format!("config not reloaded: {e}"),
                                            state.elapsed_ms,
                                        ),
                                    }
                                    // The editor painted over the frame.
                                    terminal.clear()?;
                                }
                                _ => {
                                    if let Some(task) = dispatch_input(a, &mut state, &player, &behaviour) {
                                        try_spawn(
                                            task,
                                            &source,
                                            &app_tx,
                                            &mut state,
                                            cache_reader.as_ref(),
                                            storage.as_ref(),
                                            cookie_file.as_deref(),
                                            cache_writer.as_ref(),
                                        );
                                    }
                                }
                            }
                            // Restart the debounce timer, so the search fires
                            // from the tick arm once typing stops.
                            state.elapsed_ms = started.elapsed().as_millis() as u64;
                            note_search_input(&mut debounce, &state, state.elapsed_ms);
                        }
                    }
                    CtEvent::Resize(_, _) => state.apply(AppEvent::Resize),
                    _ => {}
                }
            }

            // Player actor
            Some(pe) = player_events.recv() => {
                // Metadata on a track change, status on a state change. Not on
                // every Progress event: that is 4Hz of D-Bus traffic for a
                // position the desktop widget interpolates itself.
                let notify = matches!(
                    pe,
                    ytm_player::player::PlayerEvent::TrackChanged(_)
                        | ytm_player::player::PlayerEvent::StateChanged(_)
                );
                state.apply(AppEvent::Player(pe));
                if notify && let Some(c) = media.as_mut() {
                    mpris::update(c, &state);
                }
            }

            // OS media keys. The handler runs on souvlaki's thread and can only
            // send, so the command is forwarded from here where the player lives.
            Some(cmd) = media_keys.recv() => {
                tracing::debug!(?cmd, "media key");
                send(&player, cmd);
            }

            // Background work results
            Some(ae) = app_rx.recv() => {
                if let Some(hint) = empty_library_hint(cookie_auth, &ae) {
                    state.push_toast(ToastKind::Info, &hint, state.elapsed_ms);
                }
                // A DELETE plus one INSERT per track, then a commit that fsyncs:
                // 44 ms for a 10k-song library, measured, with no key processed
                // and no frame drawn. The writer thread owns the connection.
                if let Some(tx) = cache_writer.as_ref()
                    && let Some(work) = cache_work(&ae)
                {
                    let _ = tx.send(work);
                }
                // Art lives in the cache, not in state: protocol objects are
                // not comparable or cloneable, so a pure reducer cannot hold them.
                if let AppEvent::ArtFailed { url } = &ae {
                    art.mark_failed(url);
                }
                if let AppEvent::ArtLoaded { url, image } = ae {
                    art.insert(&url, *image);
                    continue;
                }
                if let AppEvent::EnqueueTracks {
                    tracks,
                    play_next,
                    toast,
                } = ae
                {
                    state.loading = false;
                    let cmd = if play_next {
                        PlayerCommand::EnqueueNext(tracks)
                    } else {
                        PlayerCommand::EnqueueBack(tracks)
                    };
                    send(&player, cmd);
                    if let Some(msg) = toast {
                        state.push_toast(ToastKind::Success, &msg, state.elapsed_ms);
                    }
                    continue;
                }
                state.apply(ae);
            }

            // The source finishing its handshake. `pending_first_fetch` keeps this
            // arm from being polled again once it has completed — a completed
            // future must not be awaited a second time.
            built = &mut source_fut, if pending_first_fetch => {
                pending_first_fetch = false;
                match built {
                    Ok(src) => {
                        // Before `pane_task`: a guest on a library pane is moved
                        // to Search first, so the task started is Search's and
                        // not a library fetch that can only fail.
                        source_ready(&mut state, src.is_authenticated());
                        // The starting pane's rows, now that there is something to
                        // fetch them with.
                        if let Some(task) = pane_task(state.pane) {
                            spawn_task(task, src.clone(), app_tx.clone());
                        } else {
                            state.loading = false;
                        }
                        source = Some(src);
                    }
                    Err(e) => {
                        // Not fatal any more: the TUI is already up and usable for
                        // anything cached, so an auth failure is a message rather
                        // than an exit that dumps a report over a live screen.
                        state.loading = false;
                        state.push_toast(
                            ToastKind::Error,
                            &format!("could not sign in: {e}"),
                            state.elapsed_ms,
                        );
                        tracing::error!(error = %e, "source unavailable");
                    }
                }
            }

            // Render tick
            _ = ticker.tick() => {
                let now_ms = started.elapsed().as_millis() as u64;
                state.elapsed_ms = now_ms;
                state.apply(AppEvent::Tick);
                if auto_reload_theme
                    && let Some(path) = &theme_file
                    && now_ms.saturating_sub(last_theme_check_ms) >= THEME_CHECK_INTERVAL_MS
                {
                    last_theme_check_ms = now_ms;
                    if let Some(new_theme) = check_theme_file_update(path, &mut last_theme_mtime) {
                        custom_theme = Some(new_theme);
                        if theme_name == "custom" {
                            theme = new_theme;
                            state.push_toast(
                                ToastKind::Info,
                                "theme: custom (reloaded)",
                                state.elapsed_ms,
                            );
                        }
                    }
                }
                if let Some(task) = search_tick(&mut debounce, &mut state, now_ms) {
                    try_spawn(
                        task,
                        &source,
                        &app_tx,
                        &mut state,
                        cache_reader.as_ref(),
                        storage.as_ref(),
                        cookie_file.as_deref(),
                        cache_writer.as_ref(),
                    );
                }
                if art.is_enabled()
                    && let Some(url) = art_tick(&mut art, &state)
                {
                    spawn_art_fetch(url, app_tx.clone());
                }
            }
        }

        // Set before drawing: the reducer needs the row count for paging and
        // `zz`, and only the frame knows it.
        state.viewport_rows = ytm_tui::render::list_rows_for(
            terminal.size()?.into(),
            state.search_row_visible(),
            state.filter_row_visible(),
            state.column_header_visible(),
        );
        terminal.draw(|f| ytm_tui::render::render(f, &state, &theme, &keymap, &mut art))?;

        if state.should_quit {
            send(&player, PlayerCommand::Shutdown);
            // The level the user left at, for `state.toml`.
            return Ok(state.volume);
        }
    }
}

/// Run one unit of work off-thread and post the result back.
fn spawn_task(task: Task, source: Arc<dyn MusicSource>, tx: mpsc::UnboundedSender<AppEvent>) {
    tokio::spawn(async move {
        // Logged both ways: nothing about a background fetch is visible on
        // screen, so the log is the only place to see one succeed or fail.
        tracing::debug!(?task, "background task started");
        let started = std::time::Instant::now();
        let ev = match task {
            Task::LoadHome => match source.home_shelves().await {
                Ok(v) => AppEvent::HomeLoaded(v),
                Err(e) => AppEvent::Error(e.to_string()),
            },
            Task::OpenArtist { id, name } => match source.artist_tracks(id.clone()).await {
                Ok(tracks) => AppEvent::ArtistTracksLoaded { id, name, tracks },
                Err(e) => AppEvent::Error(e.to_string()),
            },
            Task::OpenAlbum { id, name } => match source.album_tracks(id.clone()).await {
                Ok(tracks) => AppEvent::AlbumTracksLoaded { id, name, tracks },
                Err(e) => AppEvent::Error(e.to_string()),
            },
            Task::LoadPlaylists => match source.library_playlists().await {
                Ok(v) => AppEvent::PlaylistsLoaded(v),
                Err(e) => AppEvent::Error(e.to_string()),
            },
            Task::LoadSongs => match source.library_songs().await {
                Ok(v) => AppEvent::LibrarySongsLoaded(v),
                Err(e) => AppEvent::Error(e.to_string()),
            },
            Task::LoadAlbums => match source.library_albums().await {
                Ok(v) => AppEvent::AlbumsLoaded(v),
                Err(e) => AppEvent::Error(e.to_string()),
            },
            Task::LoadArtists => match source.library_artists().await {
                Ok(v) => AppEvent::ArtistsLoaded(v),
                Err(e) => AppEvent::Error(e.to_string()),
            },
            Task::OpenPlaylist(id) => match source.playlist_tracks(id.clone()).await {
                Ok(tracks) => AppEvent::PlaylistTracksLoaded { id, tracks },
                Err(e) => AppEvent::Error(e.to_string()),
            },
            Task::Search(q) => match source.search_songs(q.clone()).await {
                Ok(tracks) => AppEvent::SearchResults { query: q, tracks },
                Err(e) => AppEvent::Error(e.to_string()),
            },
            Task::SearchArtists(q) => match source.search_artists(q.clone()).await {
                Ok(artists) => AppEvent::ArtistSearchResults { query: q, artists },
                Err(e) => AppEvent::Error(e.to_string()),
            },
            // Failures come back as MutationFailed, not Error: the token has to
            // survive so `rollback` reverts the right edit.
            Task::Mutate { token, task } => run_mutation(token, task, source.clone()).await,
            Task::LoadDownloads
            | Task::Download(_)
            | Task::DeleteDownloads(_)
            | Task::EnqueuePlaylist { .. }
            | Task::EnqueueAlbum { .. } => return,
        };
        match &ev {
            AppEvent::Error(m) => tracing::warn!(error = %m, "background task failed"),
            other => tracing::info!(
                event = event_name(other),
                rows = event_rows(other),
                ms = started.elapsed().as_millis() as u64,
                "background task finished"
            ),
        }
        let _ = tx.send(ev);
    });
}

/// Log-friendly names, so a load is legible in the log without a Debug dump of
/// every track.
fn event_name(ev: &AppEvent) -> &'static str {
    match ev {
        AppEvent::PlaylistsLoaded(_) => "playlists",
        AppEvent::LibrarySongsLoaded(_) => "songs",
        AppEvent::DownloadedTracksLoaded(_) => "downloaded_tracks",
        AppEvent::DownloadedTrackSaved(_) => "download_saved",
        AppEvent::AlbumsLoaded(_) => "albums",
        AppEvent::ArtistsLoaded(_) => "artists",
        AppEvent::PlaylistTracksLoaded { .. } => "playlist_tracks",
        AppEvent::SearchResults { .. } => "search",
        // Named rather than left as "other": the home feed walks several pages
        // and is the slowest fetch in the app, so an unlabelled multi-second
        // entry is the one you most want to identify in a log.
        AppEvent::HomeLoaded(_) => "home",
        AppEvent::ArtistTracksLoaded { .. } => "artist_tracks",
        AppEvent::AlbumTracksLoaded { .. } => "album_tracks",
        AppEvent::ArtistSearchResults { .. } => "artist_search",
        _ => "other",
    }
}

fn event_rows(ev: &AppEvent) -> usize {
    match ev {
        AppEvent::PlaylistsLoaded(v) => v.len(),
        AppEvent::LibrarySongsLoaded(v) => v.len(),
        AppEvent::DownloadedTracksLoaded(v) => v.len(),
        AppEvent::DownloadedTrackSaved(_) => 1,
        AppEvent::AlbumsLoaded(v) => v.len(),
        AppEvent::ArtistsLoaded(v) => v.len(),
        AppEvent::PlaylistTracksLoaded { tracks, .. } => tracks.len(),
        AppEvent::SearchResults { tracks, .. } => tracks.len(),
        // Shelves, not cards: the shelf count is what says whether the
        // multi-page walk actually reached page 2.
        AppEvent::HomeLoaded(v) => v.len(),
        AppEvent::ArtistTracksLoaded { tracks, .. } => tracks.len(),
        AppEvent::AlbumTracksLoaded { tracks, .. } => tracks.len(),
        AppEvent::ArtistSearchResults { artists, .. } => artists.len(),
        _ => 0,
    }
}

/// A library call that succeeded with zero rows, under cookie auth, is the only
/// signal an expired cookie gives (FR-A6): InnerTube answers one with HTTP 200 and
/// a signed-out page, so "empty" and "expired" are indistinguishable from here.
pub fn empty_library_hint(cookie_auth: bool, ev: &AppEvent) -> Option<String> {
    if !cookie_auth {
        return None;
    }
    let empty = match ev {
        AppEvent::PlaylistsLoaded(v) => v.is_empty(),
        AppEvent::LibrarySongsLoaded(v) => v.is_empty(),
        AppEvent::AlbumsLoaded(v) => v.is_empty(),
        AppEvent::ArtistsLoaded(v) => v.is_empty(),
        _ => false,
    };
    empty.then(|| {
        "library came back empty — if that is wrong, the cookie expired; re-export it".to_owned()
    })
}

/// Fill state from the cache before the first frame (NFR-1). A local SQLite read
/// is cheap enough to do before the draw; the network refresh overwrites it.
/// Failures are logged — the cache is disposable and must never block startup.
pub fn preload_from_cache(cache: &ytm_core::cache::Cache, state: &mut AppState) {
    match cache.load_playlists() {
        Ok(v) if !v.is_empty() => state.playlists = v,
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "could not read cached playlists"),
    }
    match cache.load_library_songs() {
        Ok(v) if !v.is_empty() => state.tracks = v,
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "could not read cached songs"),
    }
    match cache.get_downloaded_tracks() {
        Ok(v) => state.downloaded_tracks = v.into_iter().map(|dt| dt.to_track()).collect(),
        Err(e) => tracing::warn!(error = %e, "could not read downloaded tracks"),
    }
}

/// Show a playlist's cached tracks while the fetch is in flight. A miss leaves
/// the pane alone rather than blanking it. A hit also sets `open_playlist` —
/// filling `tracks` alone would leave the playlist list on screen.
pub fn preload_playlist_tracks(
    cache: &ytm_core::cache::Cache,
    state: &mut AppState,
    id: &ytm_core::PlaylistId,
) {
    match cache.load_playlist_tracks(id) {
        Ok(v) if !v.is_empty() => {
            state.open_playlist = Some(id.clone());
            state.tracks = v;
            // The cursor was on a playlist row and would otherwise index a
            // shorter track list, or scroll to a window with no cursor in it.
            state.selected = 0;
            state.scroll_offset = 0;
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "could not read cached playlist tracks"),
    }
}

/// Persist non-empty library responses for the next cold start. Empty responses
/// are skipped because an expired cookie answers HTTP 200 with zero rows and
/// would wipe a good cache; search, albums, and artists are not cached.
fn cache_work(ev: &AppEvent) -> Option<CacheWork> {
    match ev {
        AppEvent::PlaylistsLoaded(v) if !v.is_empty() => Some(CacheWork::Playlists(v.clone())),
        AppEvent::LibrarySongsLoaded(v) if !v.is_empty() => {
            Some(CacheWork::LibrarySongs(v.clone()))
        }
        // A playlist genuinely can be empty, and its rows are keyed by id, so
        // there is no wipe-the-library risk in writing that through.
        AppEvent::PlaylistTracksLoaded { id, tracks } => Some(CacheWork::PlaylistTracks {
            id: id.clone(),
            tracks: tracks.clone(),
        }),
        _ => None,
    }
}

/// A pending cache write, owned so it can cross a thread boundary.
pub enum CacheWork {
    Playlists(Vec<ytm_core::Playlist>),
    LibrarySongs(Vec<ytm_core::Track>),
    PlaylistTracks {
        id: ytm_core::PlaylistId,
        tracks: Vec<ytm_core::Track>,
    },
    SaveDownloaded(ytm_core::DownloadedTrack),
    RemoveDownloaded(ytm_core::VideoId),
}

impl CacheWork {
    fn run(self, c: &ytm_core::cache::Cache) -> Result<(), ytm_core::cache::CacheError> {
        match self {
            Self::Playlists(v) => c.save_playlists(&v),
            Self::LibrarySongs(v) => c.save_library_songs(&v),
            Self::PlaylistTracks { id, tracks } => c.save_playlist_tracks(&id, &tracks),
            Self::SaveDownloaded(track) => c.save_downloaded_track(&track),
            Self::RemoveDownloaded(id) => c.remove_downloaded_track(&id),
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Self::Playlists(_) => "playlists",
            Self::LibrarySongs(_) => "songs",
            Self::PlaylistTracks { .. } => "playlist_tracks",
            Self::SaveDownloaded(_) => "save_downloaded",
            Self::RemoveDownloaded(_) => "remove_downloaded",
        }
    }
}

/// The cache lives on its own thread; the loop only ever sends. Keeps rusqlite's
/// non-`Sync` `Connection` in one place — an `Arc<Cache>` will not compile, and a
/// `Mutex` on the event loop would put the stall back under contention.
pub fn spawn_cache_writer(cache: ytm_core::cache::Cache) -> std::sync::mpsc::Sender<CacheWork> {
    let (tx, rx) = std::sync::mpsc::channel::<CacheWork>();
    std::thread::Builder::new()
        .name("ytm-cache".into())
        .spawn(move || {
            while let Ok(work) = rx.recv() {
                let name = work.name();
                if let Err(e) = work.run(&cache) {
                    tracing::warn!(error = %e, event = name, "could not write to the cache");
                }
            }
        })
        .expect("spawning the cache writer");
    tx
}

/// The URL to fetch art for, if any, exactly once per URL. From the tick arm, not
/// `TrackChanged`, so a track whose art failed or that started before the picker
/// finished probing still gets one attempt; `should_fetch` keeps that cheap.
fn art_tick(art: &mut ytm_tui::widgets::art::ArtCache, state: &AppState) -> Option<String> {
    let url = state.art_url()?;
    art.should_fetch(&url).then_some(url)
}

/// Fetch and decode one thumbnail off the UI thread (NFR-2). Decoding is CPU work,
/// so it goes to `spawn_blocking`. Every failure path posts `ArtFailed`, which is
/// what stops the URL being retried on every tick; a silent drop retries forever.
fn spawn_art_fetch(url: String, tx: mpsc::UnboundedSender<AppEvent>) {
    tokio::spawn(async move {
        let ev = match fetch_art(&url).await {
            Ok(image) => AppEvent::ArtLoaded {
                url: url.clone(),
                image: Box::new(image),
            },
            Err(e) => {
                // Debug level, not warn: a missing thumbnail is normal and
                // FR-U5 says the UI is complete without art.
                tracing::debug!(url = %url, error = %e, "album art unavailable");
                AppEvent::ArtFailed { url: url.clone() }
            }
        };
        let _ = tx.send(ev);
    });
}

async fn fetch_art(
    url: &str,
) -> Result<image::DynamicImage, Box<dyn std::error::Error + Send + Sync>> {
    let bytes = reqwest::get(url).await?.error_for_status()?.bytes().await?;
    // Decode is CPU-bound; keep it off the async workers.
    let image = tokio::task::spawn_blocking(move || {
        image::ImageReader::new(std::io::Cursor::new(bytes))
            .with_guessed_format()?
            .decode()
    })
    .await??;
    Ok(image)
}

/// Mark the spinner before handing work off, so FR-U4 holds for the whole
/// round trip rather than starting when the answer arrives.
fn start(state: &mut AppState, task: Task) -> Option<Task> {
    state.loading = true;
    Some(task)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use ytm_core::mock::MockSource;
    // `Player` itself arrives via `use super::*`.
    use ytm_player::{mock::MockPlayer, player::PlayerCommand};
    use ytm_tui::{
        app::{AppState, Focus, Pane},
        event::{AppEvent, InputAction},
    };

    fn deps() -> (Arc<MockSource>, Arc<MockPlayer>) {
        let (p, _rx) = MockPlayer::new();
        (Arc::new(MockSource::new()), Arc::new(p))
    }

    /// Default steps, so the many `dispatch_input` calls below need no struct.
    fn beh() -> crate::config::BehaviourConfig {
        crate::config::BehaviourConfig::default()
    }

    #[test]
    fn quit_is_immediate_when_confirm_on_quit_is_off() {
        // The long-standing default: q quits. Must not regress.
        let (_src, player) = deps();
        let mut s = AppState::default();
        let b = crate::config::BehaviourConfig {
            confirm_on_quit: false,
            ..beh()
        };
        dispatch_input(InputAction::Quit, &mut s, &*player, &b);
        assert!(s.should_quit, "q must still quit immediately by default");
        assert!(s.modal.is_none(), "and must not open a modal");
    }

    #[test]
    fn quit_opens_a_confirm_when_configured() {
        let (_src, player) = deps();
        let mut s = AppState::default();
        let b = crate::config::BehaviourConfig {
            confirm_on_quit: true,
            ..beh()
        };
        dispatch_input(InputAction::Quit, &mut s, &*player, &b);
        assert!(!s.should_quit, "the confirm must not have quit yet");
        assert!(
            matches!(
                s.modal,
                Some(Modal::Confirm {
                    action: ConfirmAction::Quit,
                    ..
                })
            ),
            "expected a Quit confirm, got {:?}",
            s.modal
        );
    }

    #[test]
    fn ctrl_c_bypasses_the_confirm_because_it_is_the_escape_hatch() {
        // keymap.rs resolves Ctrl+C to ForceQuit precisely so a confirm cannot
        // shadow it: "escapes everything" has to keep meaning that.
        let (_src, player) = deps();
        let b = crate::config::BehaviourConfig {
            confirm_on_quit: true,
            ..beh()
        };
        let mut s = AppState::default();
        dispatch_input(InputAction::ForceQuit, &mut s, &*player, &b);
        assert!(s.should_quit, "Ctrl+C must quit even with confirm_on_quit");
        assert!(s.modal.is_none(), "and must not leave a modal behind");
    }

    #[test]
    fn y_confirms_the_quit_and_n_cancels_it() {
        let (_src, player) = deps();
        let b = crate::config::BehaviourConfig {
            confirm_on_quit: true,
            ..beh()
        };

        let mut yes = AppState::default();
        dispatch_input(InputAction::Quit, &mut yes, &*player, &b);
        dispatch_input(InputAction::Char('y'), &mut yes, &*player, &b);
        assert!(yes.should_quit, "y must quit");
        assert!(yes.modal.is_none(), "and close the modal");

        let mut no = AppState::default();
        dispatch_input(InputAction::Quit, &mut no, &*player, &b);
        dispatch_input(InputAction::Char('n'), &mut no, &*player, &b);
        assert!(!no.should_quit, "n must not quit");
        assert!(no.modal.is_none(), "and must close the modal");
    }

    #[test]
    fn a_second_q_inside_the_confirm_also_quits() {
        // q is the key the user just pressed; making it mean "no" would be a
        // trap. Enter and y already confirm, so q should too.
        let (_src, player) = deps();
        let b = crate::config::BehaviourConfig {
            confirm_on_quit: true,
            ..beh()
        };
        let mut s = AppState::default();
        dispatch_input(InputAction::Quit, &mut s, &*player, &b);
        dispatch_input(InputAction::Quit, &mut s, &*player, &b);
        assert!(s.should_quit, "qq must quit");
    }

    #[test]
    fn seek_uses_the_configured_step_rather_than_a_hardcoded_five() {
        let (_src, player) = deps();
        let mut s = AppState::default();
        let b = crate::config::BehaviourConfig {
            seek_step_secs: 30,
            ..beh()
        };
        dispatch_input(InputAction::SeekForward, &mut s, &*player, &b);
        dispatch_input(InputAction::SeekBack, &mut s, &*player, &b);
        assert!(
            matches!(
                player.commands().as_slice(),
                [
                    PlayerCommand::SeekRelative(30),
                    PlayerCommand::SeekRelative(-30)
                ]
            ),
            "behaviour.seek_step_secs must reach the player, got {:?}",
            player.commands()
        );
    }

    #[test]
    fn volume_uses_the_configured_step_rather_than_a_hardcoded_five() {
        let (_src, player) = deps();
        let mut s = AppState {
            volume: 50,
            ..Default::default()
        };
        let b = crate::config::BehaviourConfig {
            volume_step: 10,
            ..beh()
        };
        dispatch_input(InputAction::VolumeUp, &mut s, &*player, &b);
        assert_eq!(s.volume, 60, "the bar must move by the configured step");
        dispatch_input(InputAction::VolumeDown, &mut s, &*player, &b);
        dispatch_input(InputAction::VolumeDown, &mut s, &*player, &b);
        assert_eq!(s.volume, 40);
        assert!(
            matches!(player.commands().last(), Some(PlayerCommand::SetVolume(40))),
            "the player must be told the same level, got {:?}",
            player.commands()
        );
    }

    #[test]
    fn a_configured_step_still_clamps_at_the_ends() {
        // A large step must not wrap or panic: clamp_volume owns the bounds.
        let (_src, player) = deps();
        let b = crate::config::BehaviourConfig {
            volume_step: 90,
            ..beh()
        };
        let mut s = AppState {
            volume: 50,
            ..Default::default()
        };
        dispatch_input(InputAction::VolumeUp, &mut s, &*player, &b);
        assert_eq!(s.volume, 100);
        dispatch_input(InputAction::VolumeDown, &mut s, &*player, &b);
        assert_eq!(s.volume, 10);
        dispatch_input(InputAction::VolumeDown, &mut s, &*player, &b);
        assert_eq!(s.volume, 0, "must clamp, not wrap");
    }

    #[test]
    fn a_reload_carries_the_new_behaviour_values() {
        // Editing behaviour with `,` must apply without a restart, the same way
        // keys and the theme already do.
        let r = reload_config("[behaviour]\nseek_step_secs = 15\nvolume_step = 3")
            .expect("valid config");
        assert_eq!(r.behaviour.seek_step_secs, 15);
        assert_eq!(r.behaviour.volume_step, 3);
    }

    #[test]
    fn unauthenticated_source_ready_redirects_locked_pane_to_search() {
        let mut state = AppState {
            pane: Pane::Playlists,
            sidebar_selected: 1,
            playlists: vec![ytm_core::Playlist::stub("p1", "Cached")],
            ..Default::default()
        };
        source_ready(&mut state, false);
        assert!(state.guest);
        assert_eq!(state.pane, Pane::Search);
        assert_eq!(state.sidebar_selected, 5);
        assert_eq!(state.focus, Focus::SearchInput);
        assert!(
            state.playlists.is_empty(),
            "cached account rows must not survive into guest mode"
        );
    }

    #[test]
    fn authenticated_source_ready_preserves_selected_pane() {
        let mut state = AppState {
            pane: Pane::Playlists,
            sidebar_selected: 1,
            ..Default::default()
        };
        source_ready(&mut state, true);
        assert!(!state.guest);
        assert_eq!(state.pane, Pane::Playlists);
    }

    #[test]
    fn a_guest_already_on_search_is_not_told_twice() {
        // `run_tui` shows the notice when it starts a guest on Search; repeating
        // it here would stack two identical toasts on the first frame.
        let mut state = AppState {
            pane: Pane::Search,
            sidebar_selected: 5,
            ..Default::default()
        };
        source_ready(&mut state, false);
        assert!(state.guest);
        assert!(state.toasts.is_empty());
    }

    #[test]
    fn guest_account_actions_show_a_toast_without_opening_a_modal() {
        let (_source, player) = deps();
        for action in [
            InputAction::AddToPlaylist,
            InputAction::CreatePlaylist,
            InputAction::RenamePlaylist,
            InputAction::DeletePlaylist,
            InputAction::RemoveFromPlaylist,
        ] {
            let mut state = AppState {
                guest: true,
                pane: Pane::Playlists,
                ..Default::default()
            };
            assert!(dispatch_input(action.clone(), &mut state, &*player, &beh()).is_none());
            assert!(state.modal.is_none(), "{action:?} must not open a modal");
            assert_eq!(state.toasts.len(), 1, "{action:?} must explain itself");
        }
    }

    #[test]
    fn guest_queue_removal_still_reaches_the_player() {
        let (_source, player) = deps();
        let mut state = AppState {
            guest: true,
            pane: Pane::Queue,
            queue: vec![ytm_core::Track::stub("v1", "Song")],
            ..Default::default()
        };
        dispatch_input(
            InputAction::RemoveFromPlaylist,
            &mut state,
            &*player,
            &beh(),
        );
        assert!(matches!(
            player.commands().as_slice(),
            [PlayerCommand::RemoveFromQueue(0)]
        ));
    }

    #[test]
    fn toggle_pause_reaches_the_player_not_the_state() {
        let (_src, player) = deps();
        let mut s = AppState::default();
        dispatch_input(InputAction::TogglePause, &mut s, &*player, &beh());
        assert!(matches!(player.commands()[0], PlayerCommand::TogglePause));
    }

    #[test]
    fn volume_up_clamps_at_one_hundred() {
        let (_src, player) = deps();
        let mut s = AppState {
            volume: 97,
            ..Default::default()
        };
        dispatch_input(InputAction::VolumeUp, &mut s, &*player, &beh());
        match player.commands()[0] {
            PlayerCommand::SetVolume(v) => assert_eq!(v, 100),
            ref o => panic!("expected SetVolume, got {o:?}"),
        }
    }

    #[test]
    fn volume_down_clamps_at_zero() {
        let (_src, player) = deps();
        let mut s = AppState {
            volume: 2,
            ..Default::default()
        };
        dispatch_input(InputAction::VolumeDown, &mut s, &*player, &beh());
        match player.commands()[0] {
            PlayerCommand::SetVolume(v) => assert_eq!(v, 0),
            ref o => panic!("expected SetVolume, got {o:?}"),
        }
    }

    #[test]
    fn cycle_repeat_advances_the_mode() {
        let (_src, player) = deps();
        let mut s = AppState::default();
        dispatch_input(InputAction::CycleRepeat, &mut s, &*player, &beh());
        match player.commands()[0] {
            PlayerCommand::SetRepeat(m) => {
                assert_eq!(m, ytm_player::player::RepeatMode::One)
            }
            ref o => panic!("expected SetRepeat, got {o:?}"),
        }
    }

    #[test]
    fn confirm_on_a_selected_track_plays_it() {
        let (_src, player) = deps();
        let mut s = AppState {
            pane: Pane::Songs,
            tracks: vec![ytm_core::Track::stub("v7", "Song")],
            selected: 0,
            ..Default::default()
        };
        dispatch_input(InputAction::Confirm, &mut s, &*player, &beh());
        match &player.commands()[0] {
            PlayerCommand::PlayNow(t) => assert_eq!(t.video_id.as_str(), "v7"),
            o => panic!("expected PlayNow, got {o:?}"),
        }
    }

    #[test]
    fn confirm_with_an_empty_list_sends_nothing() {
        let (_src, player) = deps();
        let mut s = AppState {
            pane: Pane::Songs,
            ..Default::default()
        };
        dispatch_input(InputAction::Confirm, &mut s, &*player, &beh());
        assert!(
            player.commands().is_empty(),
            "must not play a track that does not exist"
        );
    }

    #[test]
    fn add_to_queue_enqueues_the_selected_track() {
        let (_src, player) = deps();
        let mut s = AppState {
            pane: Pane::Songs,
            tracks: vec![ytm_core::Track::stub("v1", "A")],
            ..Default::default()
        };
        dispatch_input(InputAction::AddToQueue, &mut s, &*player, &beh());
        assert!(matches!(
            player.commands()[0],
            PlayerCommand::EnqueueBack(_)
        ));
    }

    #[test]
    fn an_empty_library_under_cookie_auth_hints_at_an_expired_cookie() {
        // An expired cookie is NOT an auth error: InnerTube answers HTTP 200
        // with a signed-out page, so the library parses as zero rows. Without
        // this hint the only symptom is an empty list. See PROGRESS.md.
        let ev = AppEvent::PlaylistsLoaded(vec![]);
        let hint = empty_library_hint(true, &ev).expect("cookie auth must hint");
        assert!(hint.contains("re-export"), "got {hint:?}");
    }

    #[test]
    fn a_populated_library_never_hints() {
        let ev = AppEvent::PlaylistsLoaded(vec![ytm_core::Playlist::stub("p1", "Focus")]);
        assert!(empty_library_hint(true, &ev).is_none());
    }

    #[test]
    fn oauth_auth_does_not_get_the_cookie_hint() {
        // A genuinely empty account under OAuth has a different diagnosis;
        // telling the user to re-export cookies they do not use is noise.
        let ev = AppEvent::PlaylistsLoaded(vec![]);
        assert!(empty_library_hint(false, &ev).is_none());
    }

    #[test]
    fn typing_a_query_fires_one_search_after_the_pause_not_per_keystroke() {
        // FR-S2: three keystrokes must cost one request, not three.
        let (_src, player) = deps();
        let mut d = ytm_tui::search_state::SearchDebounce::new(300);
        let mut s = AppState {
            pane: Pane::Search,
            focus: Focus::SearchInput,
            ..Default::default()
        };

        let mut fired = Vec::new();
        for (i, c) in "boa".chars().enumerate() {
            let now = 1000 + i as u64 * 50;
            dispatch_input(InputAction::Char(c), &mut s, &*player, &beh());
            note_search_input(&mut d, &s, now);
            // A tick between keystrokes is too soon to fire.
            if let Some(t) = search_tick(&mut d, &mut s, now + 10) {
                fired.push(t);
            }
        }
        assert!(fired.is_empty(), "fired mid-typing: {fired:?}");

        let task = search_tick(&mut d, &mut s, 1500).expect("must fire after the pause");
        assert_eq!(task, Task::Search("boa".into()));
        assert!(s.loading, "the spinner must show while the search runs");
    }

    #[test]
    fn a_settled_query_does_not_fire_again_on_every_tick() {
        let (_src, player) = deps();
        let mut d = ytm_tui::search_state::SearchDebounce::new(300);
        let mut s = AppState {
            pane: Pane::Search,
            focus: Focus::SearchInput,
            ..Default::default()
        };
        dispatch_input(InputAction::Char('x'), &mut s, &*player, &beh());
        note_search_input(&mut d, &s, 1000);
        assert!(search_tick(&mut d, &mut s, 1400).is_some());
        for now in [1500, 1600, 5000] {
            assert_eq!(search_tick(&mut d, &mut s, now), None, "re-fired at {now}");
        }
    }

    #[test]
    fn keystrokes_outside_the_search_pane_never_schedule_a_search() {
        let (_src, player) = deps();
        let mut d = ytm_tui::search_state::SearchDebounce::new(300);
        let mut s = AppState {
            pane: Pane::Songs,
            focus: Focus::Main,
            ..Default::default()
        };
        dispatch_input(InputAction::Down, &mut s, &*player, &beh());
        note_search_input(&mut d, &s, 1000);
        assert_eq!(search_tick(&mut d, &mut s, 2000), None);
    }

    fn queue_of_three() -> AppState {
        AppState {
            pane: Pane::Queue,
            queue: vec![
                ytm_core::Track::stub("v1", "A"),
                ytm_core::Track::stub("v2", "B"),
                ytm_core::Track::stub("v3", "C"),
            ],
            focus: Focus::Main,
            selected: 1,
            ..Default::default()
        }
    }

    #[test]
    fn x_in_the_queue_removes_the_selected_entry() {
        let (_src, player) = deps();
        let mut s = queue_of_three();
        dispatch_input(InputAction::RemoveFromPlaylist, &mut s, &*player, &beh());
        match player.commands()[0] {
            PlayerCommand::RemoveFromQueue(i) => assert_eq!(i, 1),
            ref o => panic!("expected RemoveFromQueue, got {o:?}"),
        }
    }

    /// A queue with a filter on, matching entries 1 and 3 only.
    fn filtered_queue_of_four() -> AppState {
        AppState {
            pane: Pane::Queue,
            focus: Focus::Main,
            queue: vec![
                ytm_core::Track::stub("q0", "We Don't Talk Anymore"),
                ytm_core::Track::stub("q1", "blue"),
                ytm_core::Track::stub("q2", "Something Else"),
                ytm_core::Track::stub("q3", "I'm Good (Blue)"),
            ],
            filter: "blu".into(),
            selected: 0,
            ..Default::default()
        }
    }

    #[test]
    fn enter_on_a_filtered_row_plays_that_row_not_the_queues_nth() {
        // The owner's report: filter to "blu", press Enter on the first match, and
        // the queue's *first* track played instead. `selected` counts visible rows;
        // JumpTo takes a queue index.
        let (_src, player) = deps();
        let mut s = filtered_queue_of_four();
        dispatch_input(InputAction::Confirm, &mut s, &*player, &beh());
        match player.commands()[0] {
            PlayerCommand::JumpTo(i) => assert_eq!(i, 1, "visible row 0 is queue 1"),
            ref o => panic!("expected JumpTo, got {o:?}"),
        }
    }

    #[test]
    fn enter_on_the_second_filtered_row_plays_the_right_entry() {
        // "if the 2nd song hover and sel, the queue current song plays next one".
        let (_src, player) = deps();
        let mut s = AppState {
            selected: 1,
            ..filtered_queue_of_four()
        };
        dispatch_input(InputAction::Confirm, &mut s, &*player, &beh());
        match player.commands()[0] {
            PlayerCommand::JumpTo(i) => assert_eq!(i, 3, "visible row 1 is queue 3"),
            ref o => panic!("expected JumpTo, got {o:?}"),
        }
    }

    #[test]
    fn enter_past_the_last_filtered_row_sends_nothing() {
        // Two matches, so row 2 does not exist. A command here would index past
        // the queue in the actor.
        let (_src, player) = deps();
        let mut s = AppState {
            selected: 2,
            ..filtered_queue_of_four()
        };
        dispatch_input(InputAction::Confirm, &mut s, &*player, &beh());
        assert!(player.commands().is_empty());
    }

    #[test]
    fn x_on_a_filtered_row_removes_that_row() {
        // Same translation bug: unmarked `x` passed the visible index straight
        // through, so it deleted whatever sat at that spot in the full queue.
        let (_src, player) = deps();
        let mut s = filtered_queue_of_four();
        dispatch_input(InputAction::RemoveFromPlaylist, &mut s, &*player, &beh());
        match player.commands()[0] {
            PlayerCommand::RemoveFromQueue(i) => assert_eq!(i, 1),
            ref o => panic!("expected RemoveFromQueue, got {o:?}"),
        }
    }

    #[test]
    fn moving_a_filtered_row_moves_that_entry() {
        // J/K had the same bug. Marked ranges never did — they match by video id
        // against the real queue — so this only ever bit the single-row case.
        let (_src, player) = deps();
        let mut s = filtered_queue_of_four();
        dispatch_input(InputAction::MoveEntryDown, &mut s, &*player, &beh());
        assert_eq!(moves(&player), vec![(1, 2)], "queue 1 moves to 2");
    }

    #[test]
    fn removing_a_queue_entry_does_not_mutate_the_local_queue() {
        // The actor owns queue truth and answers with QueueChanged. Editing
        // `state.queue` here would show a row count the player disagrees with.
        let (_src, player) = deps();
        let mut s = queue_of_three();
        dispatch_input(InputAction::RemoveFromPlaylist, &mut s, &*player, &beh());
        assert_eq!(s.queue.len(), 3, "the view must wait for QueueChanged");
    }

    #[test]
    fn x_on_marked_range_in_queue_adjusts_selection_and_clears_marks() {
        let (_src, player) = deps();
        let tracks: Vec<ytm_core::Track> = (0..88)
            .map(|i| ytm_core::Track::stub(&format!("v{i}"), &format!("Track {i}")))
            .collect();
        let mut s = AppState {
            pane: Pane::Queue,
            queue: tracks,
            selected: 79,
            scroll_offset: 40,
            ..Default::default()
        };
        // Mark rows 40..80 (40 tracks)
        for i in 40..80 {
            s.marked.insert(ytm_core::VideoId::from(format!("v{i}")));
        }
        s.visual_anchor = Some(40);

        dispatch_input(InputAction::RemoveFromPlaylist, &mut s, &*player, &beh());

        assert_eq!(player.commands().len(), 40);
        assert!(s.marked.is_empty());
        assert_eq!(s.visual_anchor, None);
        assert_eq!(s.selected, 40);
        assert!(s.scroll_offset <= 40);
    }

    #[test]
    fn x_outside_the_queue_does_not_touch_the_queue() {
        // In a playlist, `x` means remove-from-playlist (Task 32), not dequeue.
        let (_src, player) = deps();
        let mut s = AppState {
            pane: Pane::Songs,
            tracks: vec![ytm_core::Track::stub("v1", "A")],
            ..Default::default()
        };
        dispatch_input(InputAction::RemoveFromPlaylist, &mut s, &*player, &beh());
        assert!(player.commands().is_empty());
    }

    #[test]
    fn moving_an_entry_down_swaps_it_with_the_next_one() {
        let (_src, player) = deps();
        let mut s = queue_of_three();
        dispatch_input(InputAction::MoveEntryDown, &mut s, &*player, &beh());
        match player.commands()[0] {
            PlayerCommand::MoveInQueue { from, to } => assert_eq!((from, to), (1, 2)),
            ref o => panic!("expected MoveInQueue, got {o:?}"),
        }
        assert_eq!(s.selected, 2, "the selection follows the entry it moved");
    }

    #[test]
    fn moving_an_entry_up_swaps_it_with_the_previous_one() {
        let (_src, player) = deps();
        let mut s = queue_of_three();
        dispatch_input(InputAction::MoveEntryUp, &mut s, &*player, &beh());
        match player.commands()[0] {
            PlayerCommand::MoveInQueue { from, to } => assert_eq!((from, to), (1, 0)),
            ref o => panic!("expected MoveInQueue, got {o:?}"),
        }
        assert_eq!(s.selected, 0);
    }

    #[test]
    fn an_entry_cannot_be_moved_off_either_end() {
        let (_src, player) = deps();
        let mut top = AppState {
            selected: 0,
            ..queue_of_three()
        };
        dispatch_input(InputAction::MoveEntryUp, &mut top, &*player, &beh());
        let mut bottom = AppState {
            selected: 2,
            ..queue_of_three()
        };
        dispatch_input(InputAction::MoveEntryDown, &mut bottom, &*player, &beh());
        assert!(
            player.commands().is_empty(),
            "an out-of-range move would panic the actor"
        );
        assert_eq!(top.selected, 0);
        assert_eq!(bottom.selected, 2);
    }

    /// The reorder commands a dispatch produced, in the order the actor sees them.
    fn moves(player: &MockPlayer) -> Vec<(usize, usize)> {
        player
            .commands()
            .iter()
            .filter_map(|c| match c {
                PlayerCommand::MoveInQueue { from, to } => Some((*from, *to)),
                _ => None,
            })
            .collect()
    }

    fn queue_of_four_with_middle_marked() -> AppState {
        let mut s = AppState {
            pane: Pane::Queue,
            queue: vec![
                ytm_core::Track::stub("v1", "A"),
                ytm_core::Track::stub("v2", "B"),
                ytm_core::Track::stub("v3", "C"),
                ytm_core::Track::stub("v4", "D"),
            ],
            focus: Focus::Main,
            selected: 1,
            ..Default::default()
        };
        s.marked.insert(ytm_core::VideoId::from("v2"));
        s.marked.insert(ytm_core::VideoId::from("v3"));
        s
    }

    #[test]
    fn a_marked_block_moves_down_together() {
        // FR-Q3 with FR-C4: `J` moved only the cursor row, so a `V` range could be
        // selected and removed but never reordered. Highest index first — each move
        // shifts what follows, so ascending order drags the block apart.
        let (_src, player) = deps();
        let mut s = queue_of_four_with_middle_marked();
        dispatch_input(InputAction::MoveEntryDown, &mut s, &*player, &beh());
        assert_eq!(moves(&player), vec![(2, 3), (1, 2)]);
        assert_eq!(s.selected, 2, "the cursor follows the block");
    }

    #[test]
    fn a_marked_block_moves_up_together() {
        let (_src, player) = deps();
        let mut s = queue_of_four_with_middle_marked();
        dispatch_input(InputAction::MoveEntryUp, &mut s, &*player, &beh());
        // Lowest index first going up, for the same reason reversed.
        assert_eq!(moves(&player), vec![(1, 0), (2, 1)]);
        assert_eq!(s.selected, 0);
    }

    #[test]
    fn a_marked_block_stops_at_the_ends_instead_of_collapsing() {
        // Moving a block that already touches an end would slide only the
        // entries that can move and squash the range together.
        let (_src, player) = deps();
        let mut top = queue_of_four_with_middle_marked();
        top.marked.insert(ytm_core::VideoId::from("v1"));
        dispatch_input(InputAction::MoveEntryUp, &mut top, &*player, &beh());

        let mut bottom = queue_of_four_with_middle_marked();
        bottom.marked.insert(ytm_core::VideoId::from("v4"));
        dispatch_input(InputAction::MoveEntryDown, &mut bottom, &*player, &beh());

        assert!(
            moves(&player).is_empty(),
            "a block against the edge must not move at all"
        );
    }

    /// Replay a dispatch's reorder commands through the real queue. The pairs alone
    /// do not prove the result: each `MoveInQueue` is a remove then an insert, so
    /// every command shifts the indices the next one means.
    fn order_after(action: InputAction, state: &mut AppState) -> Vec<String> {
        let (_src, player) = deps();
        let mut q = ytm_player::queue::Queue::default();
        q.push_back(state.queue.clone());
        dispatch_input(action, state, &*player, &beh());
        for (from, to) in moves(&player) {
            q.move_item(from, to);
        }
        q.tracks()
            .iter()
            .map(|t| t.video_id.as_str().to_owned())
            .collect()
    }

    #[test]
    fn a_block_moved_down_stays_contiguous_in_the_real_queue() {
        let mut s = queue_of_four_with_middle_marked();
        assert_eq!(
            order_after(InputAction::MoveEntryDown, &mut s),
            vec!["v1", "v4", "v2", "v3"],
            "v2+v3 must slide past v4 as one block, still adjacent and in order"
        );
    }

    #[test]
    fn a_block_moved_up_stays_contiguous_in_the_real_queue() {
        let mut s = queue_of_four_with_middle_marked();
        assert_eq!(
            order_after(InputAction::MoveEntryUp, &mut s),
            vec!["v2", "v3", "v1", "v4"]
        );
    }

    #[test]
    fn a_split_selection_moves_each_run_without_swallowing_a_gap() {
        // Marks need not be contiguous — `v` on two distant rows is legal. Each
        // marked entry moves one step; the unmarked row between them stays put.
        let mut s = queue_of_four_with_middle_marked();
        s.marked.remove(&ytm_core::VideoId::from("v3"));
        s.marked.insert(ytm_core::VideoId::from("v4"));
        // v2 and v4 marked, v4 is last, so the block is against the bottom.
        let (_src, player) = deps();
        dispatch_input(InputAction::MoveEntryDown, &mut s, &*player, &beh());
        assert!(
            moves(&player).is_empty(),
            "one marked entry at the end pins the whole selection"
        );
    }

    #[test]
    fn a_moved_block_keeps_its_marks_so_the_key_repeats() {
        // Marks are video ids, so they follow the entries through the reorder.
        // Clearing them here would make `J` move the block once and then start
        // moving the single cursor row instead.
        let (_src, player) = deps();
        let mut s = queue_of_four_with_middle_marked();
        dispatch_input(InputAction::MoveEntryDown, &mut s, &*player, &beh());
        assert!(s.marked.contains(&ytm_core::VideoId::from("v2")));
        assert!(s.marked.contains(&ytm_core::VideoId::from("v3")));
    }

    #[test]
    fn reordering_outside_the_queue_is_ignored() {
        let (_src, player) = deps();
        let mut s = AppState {
            pane: Pane::Songs,
            tracks: vec![
                ytm_core::Track::stub("v1", "A"),
                ytm_core::Track::stub("v2", "B"),
            ],
            ..Default::default()
        };
        dispatch_input(InputAction::MoveEntryDown, &mut s, &*player, &beh());
        assert!(
            player.commands().is_empty(),
            "there is no server-side track order to change (out of scope)"
        );
    }

    #[test]
    fn the_clear_binding_empties_the_queue() {
        let (_src, player) = deps();
        let mut s = queue_of_three();
        dispatch_input(InputAction::ClearQueue, &mut s, &*player, &beh());
        assert!(matches!(player.commands()[0], PlayerCommand::ClearQueue));
    }

    #[test]
    fn clearing_resets_the_selection_so_it_cannot_dangle() {
        let (_src, player) = deps();
        let mut s = queue_of_three();
        dispatch_input(InputAction::ClearQueue, &mut s, &*player, &beh());
        assert_eq!(s.selected, 0);
    }

    #[tokio::test]
    async fn creating_a_playlist_calls_the_source_and_commits() {
        let src = Arc::new(MockSource::new());
        let mut s = AppState {
            modal: Some(ytm_tui::app::Modal::Prompt {
                title: "Name".into(),
                value: "Road Trip".into(),
                action: ytm_tui::app::PromptAction::CreatePlaylist,
            }),
            ..Default::default()
        };

        let (token, task) = submit_prompt(&mut s).expect("a prompt submission yields a mutation");
        assert_eq!(
            s.playlists.len(),
            1,
            "FR-C6: optimistic row appears at once"
        );
        assert!(s.modal.is_none(), "the modal closes on submit");

        let ev = run_mutation(token, task, src.clone()).await;
        assert!(
            src.calls().iter().any(|c| c.starts_with("create_playlist")),
            "got {:?}",
            src.calls()
        );
        match ev {
            AppEvent::MutationOk {
                token: t, real_id, ..
            } => {
                assert_eq!(t, token);
                assert!(real_id.is_some(), "commit needs the real id to swap in");
            }
            other => panic!("expected MutationOk, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_failed_create_yields_mutation_failed_with_the_same_token() {
        let src = Arc::new(MockSource::new());
        src.fail_next(ytm_core::SourceError::RateLimited);
        let ev = run_mutation(
            7,
            MutationTask::Create {
                title: "X".into(),
                description: None,
                privacy: ytm_core::Privacy::Private,
            },
            src,
        )
        .await;
        match ev {
            AppEvent::MutationFailed { token, message } => {
                assert_eq!(token, 7);
                assert!(message.contains("too many requests"), "got: {message}");
            }
            other => panic!("expected MutationFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn renaming_sends_the_new_title() {
        let src =
            Arc::new(MockSource::new().with_playlists(vec![ytm_core::Playlist::stub("p1", "Old")]));
        let _ = run_mutation(
            1,
            MutationTask::Rename {
                id: "p1".into(),
                title: "New".into(),
            },
            src.clone(),
        )
        .await;
        assert!(src.calls().iter().any(|c| c.starts_with("edit_playlist")));
        assert_eq!(src.library_playlists().await.unwrap()[0].title, "New");
    }

    #[test]
    fn renaming_a_system_playlist_is_refused_before_any_api_call() {
        let mut s = AppState {
            pane: Pane::Playlists,
            playlists: vec![ytm_core::Playlist {
                is_system: true,
                ..ytm_core::Playlist::stub("LM", "Your Likes")
            }],
            selected: 0,
            ..Default::default()
        };
        assert!(open_rename_prompt(&mut s).is_none(), "must refuse");
        assert_eq!(s.toasts.len(), 1, "and say why");
        assert!(
            s.modal.is_none(),
            "no prompt for a playlist that cannot change"
        );
    }

    #[test]
    fn renaming_an_editable_playlist_prefills_its_current_title() {
        // An empty field would make rename feel like create.
        let mut s = AppState {
            pane: Pane::Playlists,
            playlists: vec![ytm_core::Playlist::stub("p1", "Focus")],
            selected: 0,
            ..Default::default()
        };
        assert!(open_rename_prompt(&mut s).is_some());
        match &s.modal {
            Some(ytm_tui::app::Modal::Prompt { value, .. }) => assert_eq!(value, "Focus"),
            other => panic!("expected a prompt, got {other:?}"),
        }
    }

    #[test]
    fn submitting_an_empty_name_is_refused() {
        let mut s = AppState {
            modal: Some(ytm_tui::app::Modal::Prompt {
                title: "Name".into(),
                value: "   ".into(),
                action: ytm_tui::app::PromptAction::CreatePlaylist,
            }),
            ..Default::default()
        };
        assert!(submit_prompt(&mut s).is_none());
        assert!(
            s.playlists.is_empty(),
            "no optimistic row for an invalid name"
        );
        assert_eq!(s.toasts.len(), 1, "and say why");
    }

    #[test]
    fn a_rejected_rename_puts_the_old_title_back() {
        // The whole point of the mutation log: the row reverts, not the list.
        let mut s = AppState {
            pane: Pane::Playlists,
            playlists: vec![ytm_core::Playlist::stub("p1", "Focus")],
            selected: 0,
            modal: Some(ytm_tui::app::Modal::Prompt {
                title: "Rename".into(),
                value: "Deep Focus".into(),
                action: ytm_tui::app::PromptAction::RenamePlaylist("p1".into()),
            }),
            ..Default::default()
        };
        let (token, _) = submit_prompt(&mut s).expect("rename must start");
        assert_eq!(s.playlists[0].title, "Deep Focus");
        s.apply(AppEvent::MutationFailed {
            token,
            message: "rejected".into(),
        });
        assert_eq!(s.playlists[0].title, "Focus");
    }

    #[test]
    fn n_opens_a_create_prompt_and_r_a_rename_prompt() {
        let (_src, player) = deps();
        let mut s = AppState {
            pane: Pane::Playlists,
            playlists: vec![ytm_core::Playlist::stub("p1", "Focus")],
            selected: 0,
            ..Default::default()
        };
        dispatch_input(InputAction::CreatePlaylist, &mut s, &*player, &beh());
        assert!(matches!(s.modal, Some(ytm_tui::app::Modal::Prompt { .. })));
        s.modal = None;
        dispatch_input(InputAction::RenamePlaylist, &mut s, &*player, &beh());
        assert!(matches!(s.modal, Some(ytm_tui::app::Modal::Prompt { .. })));
        assert!(player.commands().is_empty(), "neither touches the player");
    }

    #[test]
    fn pressing_delete_opens_a_confirmation_rather_than_deleting() {
        // FR-C3: destructive actions are never one keystroke.
        let mut s = AppState {
            pane: Pane::Playlists,
            playlists: vec![ytm_core::Playlist::stub("p1", "Focus")],
            selected: 0,
            ..Default::default()
        };
        open_delete_confirm(&mut s);
        assert!(matches!(s.modal, Some(Modal::Confirm { .. })));
        assert_eq!(s.playlists.len(), 1, "nothing is removed until confirmed");
    }

    #[test]
    fn the_confirmation_names_the_playlist() {
        let mut s = AppState {
            pane: Pane::Playlists,
            playlists: vec![ytm_core::Playlist::stub("p1", "Focus")],
            ..Default::default()
        };
        open_delete_confirm(&mut s);
        match &s.modal {
            Some(Modal::Confirm { text, .. }) => {
                assert!(text.contains("Focus"), "got: {text}")
            }
            other => panic!("expected a confirm, got {other:?}"),
        }
    }

    #[test]
    fn confirming_removes_the_row_and_returns_the_task() {
        let mut s = AppState {
            pane: Pane::Playlists,
            playlists: vec![ytm_core::Playlist::stub("p1", "Focus")],
            ..Default::default()
        };
        open_delete_confirm(&mut s);
        let (_token, task) = confirm_action(&mut s).expect("confirm yields work");
        assert!(matches!(task, MutationTask::Delete { .. }));
        assert!(s.playlists.is_empty(), "optimistic removal");
        assert!(s.modal.is_none());
    }

    #[test]
    fn a_failed_delete_puts_the_playlist_back() {
        let mut s = AppState {
            pane: Pane::Playlists,
            playlists: vec![
                ytm_core::Playlist::stub("p1", "A"),
                ytm_core::Playlist::stub("p2", "B"),
            ],
            selected: 1,
            ..Default::default()
        };
        open_delete_confirm(&mut s);
        let (token, _) = confirm_action(&mut s).unwrap();
        assert_eq!(s.playlists.len(), 1);
        s.rollback(token);
        assert_eq!(s.playlists.len(), 2);
        assert_eq!(s.playlists[1].title, "B", "restored at its original index");
    }

    #[test]
    fn a_system_playlist_cannot_be_deleted() {
        let mut s = AppState {
            pane: Pane::Playlists,
            playlists: vec![ytm_core::Playlist {
                is_system: true,
                ..ytm_core::Playlist::stub("LM", "Your Likes")
            }],
            ..Default::default()
        };
        open_delete_confirm(&mut s);
        assert!(
            s.modal.is_none(),
            "no confirmation for an impossible action"
        );
        assert_eq!(s.toasts.len(), 1, "explain why instead");
    }

    #[test]
    fn declining_the_confirmation_changes_nothing() {
        let mut s = AppState {
            pane: Pane::Playlists,
            playlists: vec![ytm_core::Playlist::stub("p1", "Focus")],
            ..Default::default()
        };
        open_delete_confirm(&mut s);
        s.apply(AppEvent::Input(InputAction::Cancel));
        assert!(s.modal.is_none());
        assert_eq!(s.playlists.len(), 1);
        assert!(s.pending.is_empty());
    }

    #[test]
    fn y_confirms_a_delete_and_n_declines_it() {
        // Nothing else proves a user can actually answer the box: `y` and `n`
        // are not in the keymap, so the loop has to resolve them.
        let (_src, player) = deps();
        let mut yes = AppState {
            pane: Pane::Playlists,
            playlists: vec![ytm_core::Playlist::stub("p1", "Focus")],
            ..Default::default()
        };
        open_delete_confirm(&mut yes);
        let task = dispatch_input(InputAction::Char('y'), &mut yes, &*player, &beh());
        assert!(
            matches!(
                task,
                Some(Task::Mutate {
                    task: MutationTask::Delete { .. },
                    ..
                })
            ),
            "got {task:?}"
        );
        assert!(yes.playlists.is_empty());

        let mut no = AppState {
            playlists: vec![ytm_core::Playlist::stub("p1", "Focus")],
            ..Default::default()
        };
        open_delete_confirm(&mut no);
        assert!(dispatch_input(InputAction::Char('n'), &mut no, &*player, &beh()).is_none());
        assert!(no.modal.is_none(), "n closes the box");
        assert_eq!(no.playlists.len(), 1, "and deletes nothing");
    }

    #[test]
    fn d_on_a_playlist_opens_the_delete_confirmation() {
        let (_src, player) = deps();
        let mut s = AppState {
            pane: Pane::Playlists,
            playlists: vec![ytm_core::Playlist::stub("p1", "Focus")],
            ..Default::default()
        };
        dispatch_input(InputAction::DeletePlaylist, &mut s, &*player, &beh());
        assert!(matches!(s.modal, Some(Modal::Confirm { .. })));
    }

    fn playlist_track(v: &str, sv: &str, title: &str) -> ytm_core::Track {
        ytm_core::Track {
            set_video_id: Some(ytm_core::SetVideoId::from(sv)),
            ..ytm_core::Track::stub(v, title)
        }
    }

    fn open_playlist_with(tracks: Vec<ytm_core::Track>) -> AppState {
        AppState {
            pane: Pane::Playlists,
            open_playlist: Some("p1".into()),
            tracks,
            ..Default::default()
        }
    }

    #[test]
    fn with_no_marks_the_target_is_the_selected_track() {
        let s = AppState {
            pane: Pane::Songs,
            tracks: vec![
                ytm_core::Track::stub("v1", "A"),
                ytm_core::Track::stub("v2", "B"),
            ],
            selected: 1,
            ..Default::default()
        };
        assert_eq!(targets_for_add(&s), vec![ytm_core::VideoId::from("v2")]);
    }

    #[test]
    fn marked_tracks_take_precedence_over_the_selection() {
        // FR-C4: multi-select.
        let mut s = AppState {
            pane: Pane::Songs,
            tracks: vec![
                ytm_core::Track::stub("v1", "A"),
                ytm_core::Track::stub("v2", "B"),
                ytm_core::Track::stub("v3", "C"),
            ],
            selected: 0,
            ..Default::default()
        };
        s.marked.insert(ytm_core::VideoId::from("v2"));
        s.marked.insert(ytm_core::VideoId::from("v3"));
        let mut got = targets_for_add(&s);
        got.sort();
        assert_eq!(
            got,
            vec![ytm_core::VideoId::from("v2"), ytm_core::VideoId::from("v3")]
        );
    }

    #[test]
    fn an_empty_list_yields_no_targets() {
        let s = AppState::default();
        assert!(targets_for_add(&s).is_empty());
    }

    #[test]
    fn toggle_mark_adds_then_removes() {
        let mut s = AppState {
            pane: Pane::Songs,
            tracks: vec![ytm_core::Track::stub("v1", "A")],
            ..Default::default()
        };
        s.toggle_mark();
        assert!(s.marked.contains(&ytm_core::VideoId::from("v1")));
        s.toggle_mark();
        assert!(s.marked.is_empty());
    }

    #[test]
    fn removing_requires_an_open_playlist() {
        let mut s = AppState {
            pane: Pane::Songs, // library songs, not a playlist
            tracks: vec![playlist_track("v1", "sv1", "A")],
            ..Default::default()
        };
        open_remove_confirm(&mut s);
        assert!(s.modal.is_none(), "there is no playlist to remove from");
        assert_eq!(s.toasts.len(), 1);
    }

    #[test]
    fn removing_a_track_without_a_set_video_id_is_refused() {
        // Without SetVideoId the API cannot identify the entry.
        let mut s = open_playlist_with(vec![ytm_core::Track::stub("v1", "A")]);
        open_remove_confirm(&mut s);
        assert!(s.modal.is_none());
        assert_eq!(s.toasts.len(), 1, "explain rather than fail silently");
    }

    #[test]
    fn removing_opens_a_confirmation_naming_the_count() {
        let mut s = open_playlist_with(vec![
            playlist_track("v1", "sv1", "A"),
            playlist_track("v2", "sv2", "B"),
        ]);
        s.marked.insert(ytm_core::VideoId::from("v1"));
        s.marked.insert(ytm_core::VideoId::from("v2"));
        open_remove_confirm(&mut s);
        match &s.modal {
            Some(Modal::Confirm { text, .. }) => {
                assert!(text.contains('2'), "got: {text}")
            }
            other => panic!("expected a confirm, got {other:?}"),
        }
    }

    #[test]
    fn confirming_a_removal_drops_the_rows_and_can_be_undone() {
        let mut s = open_playlist_with(vec![
            playlist_track("v1", "sv1", "A"),
            playlist_track("v2", "sv2", "B"),
            playlist_track("v3", "sv3", "C"),
        ]);
        s.selected = 1;
        open_remove_confirm(&mut s);
        let (token, task) = confirm_action(&mut s).expect("confirm yields work");
        assert!(matches!(task, MutationTask::RemoveTracks { .. }));
        assert_eq!(s.tracks.len(), 2);
        s.rollback(token);
        let titles: Vec<_> = s.tracks.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, ["A", "B", "C"], "restored in order");
    }

    #[test]
    fn adding_with_no_editable_playlist_says_so_instead_of_opening_an_empty_picker() {
        let mut s = AppState {
            pane: Pane::Songs,
            tracks: vec![ytm_core::Track::stub("v1", "A")],
            playlists: vec![ytm_core::Playlist {
                is_system: true,
                ..ytm_core::Playlist::stub("LM", "Your Likes")
            }],
            ..Default::default()
        };
        open_add_to_playlist(&mut s);
        assert!(s.modal.is_none());
        assert_eq!(s.toasts.len(), 1);
    }

    #[test]
    fn the_picker_lists_only_editable_playlists() {
        let mut s = AppState {
            pane: Pane::Songs,
            tracks: vec![ytm_core::Track::stub("v1", "A")],
            playlists: vec![
                ytm_core::Playlist {
                    is_system: true,
                    ..ytm_core::Playlist::stub("LM", "Your Likes")
                },
                ytm_core::Playlist::stub("p1", "Focus"),
            ],
            ..Default::default()
        };
        open_add_to_playlist(&mut s);
        match &s.modal {
            Some(Modal::PickPlaylist {
                choices, targets, ..
            }) => {
                assert_eq!(choices.len(), 1, "a system playlist cannot be added to");
                assert_eq!(choices[0].1, "Focus");
                assert_eq!(targets.len(), 1);
            }
            other => panic!("expected a picker, got {other:?}"),
        }
    }

    #[test]
    fn picking_a_playlist_spawns_the_add_and_clears_the_marks() {
        let (_src, player) = deps();
        let mut s = AppState {
            pane: Pane::Songs,
            tracks: vec![
                ytm_core::Track::stub("v1", "A"),
                ytm_core::Track::stub("v2", "B"),
            ],
            playlists: vec![ytm_core::Playlist::stub("p1", "Focus")],
            ..Default::default()
        };
        s.marked.insert(ytm_core::VideoId::from("v1"));
        s.marked.insert(ytm_core::VideoId::from("v2"));
        open_add_to_playlist(&mut s);
        let task = dispatch_input(InputAction::Confirm, &mut s, &*player, &beh());
        match task {
            Some(Task::Mutate {
                task: MutationTask::AddTracks { id, videos },
                ..
            }) => {
                assert_eq!(id, ytm_core::PlaylistId::from("p1"));
                assert_eq!(videos.len(), 2);
            }
            other => panic!("expected an AddTracks mutation, got {other:?}"),
        }
        assert!(s.modal.is_none());
        assert!(s.marked.is_empty(), "marks are consumed by the action");
    }

    #[test]
    fn the_picker_moves_through_its_choices() {
        let mut s = AppState {
            pane: Pane::Songs,
            tracks: vec![ytm_core::Track::stub("v1", "A")],
            playlists: vec![
                ytm_core::Playlist::stub("p1", "One"),
                ytm_core::Playlist::stub("p2", "Two"),
            ],
            ..Default::default()
        };
        open_add_to_playlist(&mut s);
        s.apply(AppEvent::Input(InputAction::Down));
        match &s.modal {
            Some(Modal::PickPlaylist { selected, .. }) => assert_eq!(*selected, 1),
            other => panic!("expected a picker, got {other:?}"),
        }
        // And it must not run off the end.
        s.apply(AppEvent::Input(InputAction::Down));
        match &s.modal {
            Some(Modal::PickPlaylist { selected, .. }) => assert_eq!(*selected, 1),
            other => panic!("expected a picker, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn adding_tracks_calls_the_source_with_every_video_id() {
        let src = Arc::new(
            MockSource::new().with_playlists(vec![ytm_core::Playlist::stub("p1", "Target")]),
        );
        let _ = run_mutation(
            1,
            MutationTask::AddTracks {
                id: "p1".into(),
                videos: vec![ytm_core::VideoId::from("v1"), ytm_core::VideoId::from("v2")],
            },
            src.clone(),
        )
        .await;
        assert!(
            src.calls().iter().any(|c| c == "add_tracks(p1,2)"),
            "got {:?}",
            src.calls()
        );
    }

    #[tokio::test]
    async fn adding_duplicate_track_fails_with_already_in_playlist_message() {
        let src = Arc::new(MockSource::new());
        src.fail_next(ytm_core::SourceError::AlreadyInPlaylist);
        let event = run_mutation(
            1,
            MutationTask::AddTracks {
                id: "p1".into(),
                videos: vec![ytm_core::VideoId::from("v1")],
            },
            src,
        )
        .await;
        match event {
            AppEvent::MutationFailed { token, message } => {
                assert_eq!(token, 1);
                assert_eq!(message, "already in playlist");
            }
            other => panic!("expected MutationFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn removing_tracks_sends_every_set_video_id() {
        let src = Arc::new(MockSource::new());
        let _ = run_mutation(
            1,
            MutationTask::RemoveTracks {
                id: "p1".into(),
                entries: vec![
                    ytm_core::SetVideoId::from("sv1"),
                    ytm_core::SetVideoId::from("sv2"),
                ],
            },
            src.clone(),
        )
        .await;
        assert!(
            src.calls().iter().any(|c| c == "remove_tracks(p1,2)"),
            "got {:?}",
            src.calls()
        );
    }

    #[test]
    fn navigation_actions_do_not_reach_the_player() {
        let (_src, player) = deps();
        let mut s = AppState {
            pane: Pane::Songs,
            tracks: vec![
                ytm_core::Track::stub("a", "A"),
                ytm_core::Track::stub("b", "B"),
            ],
            ..Default::default()
        };
        dispatch_input(InputAction::Down, &mut s, &*player, &beh());
        assert!(player.commands().is_empty());
        assert_eq!(s.selected, 1, "state handles navigation");
    }

    #[test]
    fn a_warm_cache_fills_state_before_any_network_call() {
        // NFR-1: the first frame must have content without awaiting the network.
        let cache = ytm_core::cache::Cache::open_in_memory().unwrap();
        cache
            .save_playlists(&[ytm_core::Playlist::stub("p1", "Focus")])
            .unwrap();
        cache
            .save_library_songs(&[ytm_core::Track::stub("v1", "Song")])
            .unwrap();
        let mut s = AppState::default();
        preload_from_cache(&cache, &mut s);
        assert_eq!(s.playlists.len(), 1);
        assert_eq!(s.playlists[0].title, "Focus");
        assert_eq!(s.tracks.len(), 1);
    }

    #[test]
    fn a_cold_cache_leaves_state_untouched_and_does_not_fail() {
        let cache = ytm_core::cache::Cache::open_in_memory().unwrap();
        let mut s = AppState::default();
        preload_from_cache(&cache, &mut s);
        assert!(s.playlists.is_empty());
        assert!(s.tracks.is_empty());
    }

    /// The write path the loop takes, minus the thread: `cache_work` decides
    /// what to persist and `CacheWork::run` performs it, so a test that skips
    /// `cache_work` would not prove the loop writes anything.
    fn write_through(cache: &ytm_core::cache::Cache, ev: &AppEvent) {
        if let Some(work) = cache_work(ev) {
            work.run(cache).unwrap();
        }
    }

    #[test]
    fn a_playlists_event_is_written_through_to_the_cache() {
        let cache = ytm_core::cache::Cache::open_in_memory().unwrap();
        write_through(
            &cache,
            &AppEvent::PlaylistsLoaded(vec![ytm_core::Playlist::stub("p1", "Focus")]),
        );
        assert_eq!(cache.load_playlists().unwrap()[0].title, "Focus");
    }

    #[test]
    fn playlist_tracks_are_written_through_under_their_playlist_id() {
        let cache = ytm_core::cache::Cache::open_in_memory().unwrap();
        let id = ytm_core::PlaylistId::from("p1");
        write_through(
            &cache,
            &AppEvent::PlaylistTracksLoaded {
                id: id.clone(),
                tracks: vec![ytm_core::Track::stub("v1", "First")],
            },
        );
        assert_eq!(cache.load_playlist_tracks(&id).unwrap()[0].title, "First");
        assert!(
            cache.load_library_songs().unwrap().is_empty(),
            "playlist rows must not leak into the library songs"
        );
    }

    #[test]
    fn an_empty_library_response_does_not_wipe_a_good_cache() {
        // An expired cookie answers HTTP 200 with zero rows. Writing that
        // through would turn a one-off auth lapse into a lost cache, so the
        // next cold start would have nothing to show.
        let cache = ytm_core::cache::Cache::open_in_memory().unwrap();
        cache
            .save_playlists(&[ytm_core::Playlist::stub("p1", "Focus")])
            .unwrap();
        write_through(&cache, &AppEvent::PlaylistsLoaded(vec![]));
        assert_eq!(cache.load_playlists().unwrap().len(), 1);
    }

    #[test]
    fn search_results_are_not_cached() {
        // Search is not library state; caching it would show stale matches for
        // a query the user has not typed yet.
        let cache = ytm_core::cache::Cache::open_in_memory().unwrap();
        write_through(
            &cache,
            &AppEvent::SearchResults {
                query: "q".into(),
                tracks: vec![ytm_core::Track::stub("v1", "Hit")],
            },
        );
        assert!(cache.load_library_songs().unwrap().is_empty());
    }

    #[test]
    fn opening_a_cached_playlist_shows_its_tracks_before_the_fetch_returns() {
        // save_playlist_tracks has always written these rows; nothing read them
        // back, so a cold start showed an empty list until the network answered.
        let cache = ytm_core::cache::Cache::open_in_memory().unwrap();
        let id = ytm_core::PlaylistId::from("p1");
        cache
            .save_playlist_tracks(&id, &[ytm_core::Track::stub("v1", "cached title")])
            .unwrap();

        let mut state = AppState {
            pane: Pane::Playlists,
            playlists: vec![ytm_core::Playlist::stub("p1", "One")],
            // The cursor is on the playlist row, which may be past the end of
            // the shorter track list it is about to show.
            selected: 0,
            ..Default::default()
        };
        preload_playlist_tracks(&cache, &mut state, &id);
        assert_eq!(state.tracks.len(), 1, "cached rows must be shown at once");
        assert_eq!(state.tracks[0].title, "cached title");
        // Filling `tracks` is not enough: the Playlists pane draws the playlist
        // list until `open_playlist` is set, so the rows would be invisible.
        assert_eq!(
            state.selected_track().map(|t| t.title),
            Some("cached title".to_owned()),
            "the pane must actually be showing the cached rows"
        );
    }

    #[test]
    fn an_uncached_playlist_leaves_the_list_untouched() {
        // Must not blank out whatever is on screen just because this id is new.
        let cache = ytm_core::cache::Cache::open_in_memory().unwrap();
        let mut state = AppState {
            tracks: vec![ytm_core::Track::stub("old", "still here")],
            ..Default::default()
        };
        preload_playlist_tracks(
            &cache,
            &mut state,
            &ytm_core::PlaylistId::from("never-seen"),
        );
        assert_eq!(
            state.tracks.len(),
            1,
            "a cache miss must leave the list alone, not clear it"
        );
        assert_eq!(state.tracks[0].title, "still here");
        assert!(
            state.open_playlist.is_none(),
            "a miss must not descend into a playlist with nothing to show"
        );
    }

    #[test]
    fn the_spawn_path_itself_shows_the_cached_rows() {
        // The reducer-level test above proves the function; this proves the loop
        // reaches it. Every previous bug of this shape was a correct function
        // nothing called.
        let cache = ytm_core::cache::Cache::open_in_memory().unwrap();
        let id = ytm_core::PlaylistId::from("p1");
        cache
            .save_playlist_tracks(&id, &[ytm_core::Track::stub("v1", "cached title")])
            .unwrap();
        let mut state = AppState::default();
        let (tx, _rx) = mpsc::unbounded_channel();

        // No source yet: the toast path, which is also when a cold pane is most
        // likely to be looked at.
        try_spawn(
            Task::OpenPlaylist(id),
            &None,
            &tx,
            &mut state,
            Some(&cache),
            None,
            None,
            None,
        );
        assert_eq!(state.tracks.len(), 1, "try_spawn must run the preload");
        assert_eq!(state.tracks[0].title, "cached title");
    }

    #[test]
    fn the_cache_writer_thread_performs_the_write_the_loop_sends_it() {
        // The loop only ever sends; if the thread did not run the work, every
        // write would silently vanish and only a cold start would reveal it.
        let dir = std::env::temp_dir().join(format!("ytm-writer{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("c.db");

        let tx = spawn_cache_writer(ytm_core::cache::Cache::open(&path).unwrap());
        let work = cache_work(&AppEvent::PlaylistsLoaded(vec![ytm_core::Playlist::stub(
            "p1", "Focus",
        )]))
        .expect("a non-empty playlists event is worth persisting");
        tx.send(work).unwrap();
        // Dropping the sender ends the thread's recv loop after it drains.
        drop(tx);

        // Reopened rather than shared: the writer owns its connection.
        let mut seen = Vec::new();
        for _ in 0..50 {
            std::thread::sleep(std::time::Duration::from_millis(20));
            seen = ytm_core::cache::Cache::open(&path)
                .unwrap()
                .load_playlists()
                .unwrap();
            if !seen.is_empty() {
                break;
            }
        }
        assert_eq!(seen.len(), 1, "the writer thread must perform the write");
        assert_eq!(seen[0].title, "Focus");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn art_is_fetched_once_for_the_playing_track() {
        let mut art = ytm_tui::widgets::art::ArtCache::disabled();
        let s = AppState {
            now_playing: Some(ytm_core::Track {
                thumbnail_url: Some("https://example.com/a.jpg".into()),
                ..ytm_core::Track::stub("v1", "T")
            }),
            ..Default::default()
        };
        assert_eq!(
            art_tick(&mut art, &s).as_deref(),
            Some("https://example.com/a.jpg")
        );
        assert!(
            art_tick(&mut art, &s).is_none(),
            "a second tick must not refetch the same URL"
        );
    }

    #[test]
    fn no_art_is_fetched_when_nothing_is_playing() {
        let mut art = ytm_tui::widgets::art::ArtCache::disabled();
        assert!(art_tick(&mut art, &AppState::default()).is_none());
    }

    #[test]
    fn a_track_without_a_thumbnail_fetches_nothing() {
        let mut art = ytm_tui::widgets::art::ArtCache::disabled();
        let s = AppState {
            now_playing: Some(ytm_core::Track::stub("v1", "T")),
            ..Default::default()
        };
        assert!(art_tick(&mut art, &s).is_none());
    }

    #[test]
    fn a_failed_art_url_is_not_refetched() {
        let mut art = ytm_tui::widgets::art::ArtCache::disabled();
        let s = AppState {
            now_playing: Some(ytm_core::Track {
                thumbnail_url: Some("https://example.com/dead.jpg".into()),
                ..ytm_core::Track::stub("v1", "T")
            }),
            ..Default::default()
        };
        assert!(art_tick(&mut art, &s).is_some());
        art.mark_failed("https://example.com/dead.jpg");
        assert!(
            art_tick(&mut art, &s).is_none(),
            "a dead thumbnail must not be retried every tick"
        );
    }

    #[test]
    fn right_on_a_playlist_opens_it_like_enter() {
        // The forward half of the h/l pair: `l` descends into a playlist, `h`
        // comes back out. Opening is a network task, so it lives here rather
        // than in the reducer.
        let (_src, player) = deps();
        let mut s = AppState {
            pane: Pane::Playlists,
            playlists: vec![ytm_core::Playlist::stub("p1", "Focus")],
            focus: Focus::Main,
            ..Default::default()
        };
        let task = dispatch_input(InputAction::Right, &mut s, &*player, &beh());
        assert_eq!(task, Some(Task::OpenPlaylist("p1".into())));
    }

    #[test]
    fn right_on_an_artist_opens_their_tracks_like_enter() {
        // `l` on a playlist row opened it, but on an artist row it did nothing: the
        // reducer says "the loop turns this into the fetch" and the loop only
        // checked for a playlist, so the h/l pair was half-missing.
        let (_src, player) = deps();
        let mut s = AppState {
            pane: Pane::Artists,
            artists: vec![ytm_core::Artist {
                id: ytm_core::ArtistId::from("UC1"),
                name: "Sabrina Carpenter".into(),
                subscribers: None,
                thumbnail_url: None,
            }],
            focus: Focus::Main,
            ..Default::default()
        };
        let task = dispatch_input(InputAction::Right, &mut s, &*player, &beh());
        assert_eq!(
            task,
            Some(Task::OpenArtist {
                id: ytm_core::ArtistId::from("UC1"),
                name: "Sabrina Carpenter".into(),
            })
        );
    }

    #[test]
    fn right_inside_an_open_artist_does_not_reopen_them() {
        // Their tracks are already on screen, so `l` must not refetch.
        let (_src, player) = deps();
        let mut s = AppState {
            pane: Pane::Artists,
            open_artist: Some((ytm_core::ArtistId::from("UC1"), "Someone".into())),
            artist_tracks: vec![ytm_core::Track::stub("v1", "T")],
            focus: Focus::Main,
            ..Default::default()
        };
        assert_eq!(
            dispatch_input(InputAction::Right, &mut s, &*player, &beh()),
            None
        );
    }

    #[test]
    fn right_inside_an_open_playlist_does_not_reopen_it() {
        // Nothing to descend into, so `l` must not fire a redundant fetch.
        let (_src, player) = deps();
        let mut s = AppState {
            pane: Pane::Playlists,
            playlists: vec![ytm_core::Playlist::stub("p1", "Focus")],
            open_playlist: Some("p1".into()),
            tracks: vec![ytm_core::Track::stub("v1", "T")],
            focus: Focus::Main,
            ..Default::default()
        };
        assert!(dispatch_input(InputAction::Right, &mut s, &*player, &beh()).is_none());
    }

    fn home_state(row: ytm_tui::app::HomeRow) -> AppState {
        AppState {
            pane: Pane::Home,
            home_rows: vec![row],
            focus: Focus::Main,
            ..Default::default()
        }
    }

    fn home_item(title: &str, target: ytm_core::HomeTarget) -> ytm_tui::app::HomeRow {
        ytm_tui::app::HomeRow::Item(ytm_core::HomeItem {
            title: title.into(),
            subtitle: String::new(),
            target,
            thumbnail_url: None,
        })
    }

    #[test]
    fn enter_on_a_home_playlist_opens_it_in_the_playlists_pane() {
        // The fetch used to land while the pane still showed the feed, so the
        // rows arrived invisibly and Enter looked dead.
        let (_src, player) = deps();
        let mut s = home_state(home_item(
            "Recap",
            ytm_core::HomeTarget::Playlist("PL1".into()),
        ));
        let task = dispatch_input(InputAction::Confirm, &mut s, &*player, &beh());
        assert_eq!(task, Some(Task::OpenPlaylist("PL1".into())));
        assert_eq!(
            s.pane,
            Pane::Playlists,
            "the playlist's tracks render in the Playlists pane, not Home"
        );
    }

    #[test]
    fn enter_on_a_home_artist_opens_them_in_the_artists_pane() {
        // Same invisible-landing bug as playlists: the tracks arrived but Home
        // kept drawing the feed over them.
        let (_src, player) = deps();
        let mut s = home_state(home_item(
            "Someone",
            ytm_core::HomeTarget::Artist("UC1".into()),
        ));
        let task = dispatch_input(InputAction::Confirm, &mut s, &*player, &beh());
        assert_eq!(
            task,
            Some(Task::OpenArtist {
                id: "UC1".into(),
                name: "Someone".into(),
            })
        );
        assert_eq!(s.pane, Pane::Artists);
    }

    #[test]
    fn enter_on_a_home_album_opens_it_in_the_albums_pane() {
        // Album rows used to return None outright: "Albums for you" was a list
        // nothing could open.
        let (_src, player) = deps();
        let mut s = home_state(home_item(
            "Blue Eyes",
            ytm_core::HomeTarget::Album("MPREb_1".into()),
        ));
        let task = dispatch_input(InputAction::Confirm, &mut s, &*player, &beh());
        assert_eq!(
            task,
            Some(Task::OpenAlbum {
                id: "MPREb_1".into(),
                name: "Blue Eyes".into(),
            })
        );
        assert_eq!(s.pane, Pane::Albums);
    }

    #[test]
    fn enter_on_a_home_track_still_plays_it() {
        // The pane switch above must not steal plain tracks: they play in place.
        let (_src, player) = deps();
        let mut s = home_state(home_item("Blow", ytm_core::HomeTarget::Track("v1".into())));
        let task = dispatch_input(InputAction::Confirm, &mut s, &*player, &beh());
        assert!(task.is_none(), "a track plays via the player, not a task");
        assert_eq!(s.pane, Pane::Home);
        assert_eq!(
            player.commands().len(),
            1,
            "one PlayNow must reach the player"
        );
    }

    #[test]
    fn enter_on_an_album_row_opens_its_songs() {
        // The Albums pane was a dead end: names on screen, Enter doing nothing.
        let (_src, player) = deps();
        let mut s = AppState {
            pane: Pane::Albums,
            albums: vec![ytm_core::Album {
                id: "MPREb_1".into(),
                title: "Blue Eyes".into(),
                artists: vec!["Honey".into()],
                year: None,
                thumbnail_url: None,
            }],
            focus: Focus::Main,
            ..Default::default()
        };
        assert_eq!(
            dispatch_input(InputAction::Confirm, &mut s, &*player, &beh()),
            Some(Task::OpenAlbum {
                id: "MPREb_1".into(),
                name: "Blue Eyes".into(),
            })
        );
    }

    #[test]
    fn right_on_an_album_row_opens_it_like_enter() {
        // `l` is the forward half of the h/l pair everywhere else; albums must
        // not be the one list where it does nothing.
        let (_src, player) = deps();
        let mut s = AppState {
            pane: Pane::Albums,
            albums: vec![ytm_core::Album {
                id: "MPREb_1".into(),
                title: "Blue Eyes".into(),
                artists: vec!["Honey".into()],
                year: None,
                thumbnail_url: None,
            }],
            focus: Focus::Main,
            ..Default::default()
        };
        assert_eq!(
            dispatch_input(InputAction::Right, &mut s, &*player, &beh()),
            Some(Task::OpenAlbum {
                id: "MPREb_1".into(),
                name: "Blue Eyes".into(),
            })
        );
    }

    #[test]
    fn right_inside_an_open_album_does_not_reopen_it() {
        // The songs are already on screen, so `l` must not refetch.
        let (_src, player) = deps();
        let mut s = AppState {
            pane: Pane::Albums,
            open_album: Some(("MPREb_1".into(), "Blue Eyes".into())),
            album_tracks: vec![ytm_core::Track::stub("v1", "T")],
            focus: Focus::Main,
            ..Default::default()
        };
        assert!(dispatch_input(InputAction::Right, &mut s, &*player, &beh()).is_none());
    }

    #[test]
    fn right_on_a_track_pane_does_not_play_anything() {
        // `l` is navigation, not Enter. Playing on a focus change would be a
        // nasty surprise.
        let (_src, player) = deps();
        let mut s = AppState {
            pane: Pane::Songs,
            tracks: vec![ytm_core::Track::stub("v1", "T")],
            focus: Focus::Main,
            ..Default::default()
        };
        let task = dispatch_input(InputAction::Right, &mut s, &*player, &beh());
        assert!(task.is_none());
        assert!(
            player.commands().is_empty(),
            "nothing should reach the player"
        );
    }

    #[test]
    fn a_number_key_switching_to_a_pane_loads_it() {
        // Pressing 4 for Albums must fetch albums, or the pane sits empty.
        // Home is source 1, so the sources after it all shifted by one.
        let (_src, player) = deps();
        let mut s = AppState::default();
        let task = dispatch_input(InputAction::GoTo(4), &mut s, &*player, &beh());
        assert_eq!(task, Some(Task::LoadAlbums));
        assert_eq!(s.pane, Pane::Albums);
    }

    #[test]
    fn a_number_key_for_the_queue_needs_no_fetch() {
        // The queue is local state owned by the actor; there is nothing to load.
        let (_src, player) = deps();
        let mut s = AppState::default();
        assert!(dispatch_input(InputAction::GoTo(7), &mut s, &*player, &beh()).is_none());
        assert_eq!(s.pane, Pane::Queue);
    }

    #[test]
    fn marked_targets_come_out_in_row_order() {
        // `marked` is a HashSet, so iterating it scrambles the order. The user
        // selected a run of rows and expects them added in that order.
        let mut s = AppState {
            pane: Pane::Songs,
            tracks: (0..12)
                .map(|i| ytm_core::Track::stub(&format!("v{i:02}"), "T"))
                .collect(),
            ..Default::default()
        };
        for t in s.tracks.clone() {
            s.marked.insert(t.video_id.clone());
        }
        let got: Vec<String> = targets_for_add(&s).iter().map(|v| v.0.clone()).collect();
        let want: Vec<String> = s.tracks.iter().map(|t| t.video_id.0.clone()).collect();
        assert_eq!(got, want, "targets must follow the on-screen order");
    }

    #[test]
    fn a_visual_range_reaches_add_to_playlist_through_the_keymap() {
        // End to end for `V`: the keymap must produce the action, the range must
        // mark, and the picker must carry every row. Feeding actions in directly
        // would prove the reducer works while nothing could reach it.
        let (_src, player) = deps();
        let km = ytm_tui::keymap::KeyMap::default();
        let mut s = AppState {
            pane: Pane::Songs,
            focus: ytm_tui::app::Focus::Main,
            tracks: (0..5)
                .map(|i| ytm_core::Track::stub(&format!("v{i}"), "T"))
                .collect(),
            playlists: vec![ytm_core::Playlist::stub("p1", "Focus")],
            ..Default::default()
        };
        let press = |s: &mut AppState, c: char| {
            use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
            let a = km
                .resolve(
                    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
                    s.input_focus(),
                )
                .unwrap_or_else(|| panic!("{c:?} is unbound"));
            dispatch_input(a, s, &*player, &beh())
        };
        press(&mut s, 'V');
        press(&mut s, 'j');
        press(&mut s, 'j');
        assert_eq!(s.marked.len(), 3, "V then j j must mark three rows");
        press(&mut s, 'A');
        match dispatch_input(InputAction::Confirm, &mut s, &*player, &beh()) {
            Some(Task::Mutate {
                task: MutationTask::AddTracks { videos, .. },
                ..
            }) => {
                let ids: Vec<String> = videos.iter().map(|v| v.0.clone()).collect();
                assert_eq!(ids, vec!["v0", "v1", "v2"], "in order, all three");
            }
            other => panic!("expected AddTracks, got {other:?}"),
        }
    }

    #[test]
    fn hand_marks_and_a_visual_range_add_up_through_the_keymap() {
        // The owner's question: does marking rows by hand still work alongside
        // the new range selection? The union must reach the API.
        let (_src, player) = deps();
        let km = ytm_tui::keymap::KeyMap::default();
        let mut s = AppState {
            pane: Pane::Songs,
            focus: ytm_tui::app::Focus::Main,
            tracks: (0..6)
                .map(|i| ytm_core::Track::stub(&format!("v{i}"), "T"))
                .collect(),
            playlists: vec![ytm_core::Playlist::stub("p1", "Focus")],
            ..Default::default()
        };
        let press = |s: &mut AppState, c: char| {
            use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
            let a = km
                .resolve(
                    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
                    s.input_focus(),
                )
                .unwrap();
            dispatch_input(a, s, &*player, &beh())
        };
        press(&mut s, 'v'); // hand-mark v0
        press(&mut s, 'j');
        press(&mut s, 'j');
        press(&mut s, 'V'); // range from v2
        press(&mut s, 'j'); // ..v3
        press(&mut s, 'A');
        match dispatch_input(InputAction::Confirm, &mut s, &*player, &beh()) {
            Some(Task::Mutate {
                task: MutationTask::AddTracks { videos, .. },
                ..
            }) => {
                let ids: Vec<String> = videos.iter().map(|v| v.0.clone()).collect();
                assert_eq!(ids, vec!["v0", "v2", "v3"]);
            }
            other => panic!("expected AddTracks, got {other:?}"),
        }
    }

    #[test]
    fn a_visual_range_removes_every_selected_track_from_a_playlist() {
        // The other bulk action `V` feeds: the confirm must name all of them.
        let mut s = open_playlist_with(vec![
            playlist_track("v1", "sv1", "A"),
            playlist_track("v2", "sv2", "B"),
            playlist_track("v3", "sv3", "C"),
            playlist_track("v4", "sv4", "D"),
        ]);
        s.apply(AppEvent::Input(InputAction::ToggleVisual));
        s.apply(AppEvent::Input(InputAction::Down));
        s.apply(AppEvent::Input(InputAction::Down));
        open_remove_confirm(&mut s);
        match &s.modal {
            Some(Modal::Confirm {
                action: ConfirmAction::RemoveTracks { entries, .. },
                ..
            }) => assert_eq!(entries.len(), 3, "all three rows of the range"),
            other => panic!("expected a remove confirm, got {other:?}"),
        }
    }

    #[test]
    fn enter_on_an_album_row_does_not_play_an_invisible_song() {
        let (_src, player) = deps();
        let mut s = AppState {
            pane: Pane::Songs,
            focus: ytm_tui::app::Focus::Main,
            tracks: vec![ytm_core::Track::stub("v0", "Left over from Songs")],
            ..Default::default()
        };
        // 4 = Albums (Home is source 1). The songs stay in `tracks`; the pane
        // now shows albums.
        s.apply(AppEvent::Input(InputAction::GoTo(4)));
        assert_eq!(s.pane, Pane::Albums);
        dispatch_input(InputAction::Confirm, &mut s, &*player, &beh());
        let log = player.commands();
        assert!(
            log.is_empty(),
            "Enter on an album row started audio: {log:?}"
        );
    }

    #[test]
    fn add_to_queue_on_an_album_row_queues_nothing() {
        let (_src, player) = deps();
        let mut s = AppState {
            pane: Pane::Songs,
            focus: ytm_tui::app::Focus::Main,
            tracks: vec![ytm_core::Track::stub("v0", "Left over from Songs")],
            ..Default::default()
        };
        s.apply(AppEvent::Input(InputAction::GoTo(4)));
        dispatch_input(InputAction::AddToQueue, &mut s, &*player, &beh());
        let log = player.commands();
        assert!(log.is_empty(), "`a` on an album row queued a song: {log:?}");
    }

    #[test]
    fn adding_to_the_queue_confirms_with_a_toast() {
        // FR-U3: `a` is otherwise silent unless the queue pane happens to be
        // open, so the user cannot tell it worked.
        let (_src, player) = deps();
        let mut s = AppState {
            pane: Pane::Songs,
            tracks: vec![ytm_core::Track::stub("v1", "Roygbiv")],
            ..Default::default()
        };
        dispatch_input(InputAction::AddToQueue, &mut s, &*player, &beh());
        assert_eq!(s.toasts.len(), 1, "adding must be acknowledged");
        assert_eq!(s.toasts[0].kind, ToastKind::Success);
        assert!(
            s.toasts[0].text.contains("Roygbiv"),
            "name what was added, got: {}",
            s.toasts[0].text
        );
    }

    #[test]
    fn play_next_confirms_with_its_own_wording() {
        // `e` and `a` do different things, so one shared message would mislead.
        let (_src, player) = deps();
        let mut s = AppState {
            pane: Pane::Songs,
            tracks: vec![ytm_core::Track::stub("v1", "Roygbiv")],
            ..Default::default()
        };
        dispatch_input(InputAction::PlayNext, &mut s, &*player, &beh());
        assert_eq!(s.toasts.len(), 1);
        assert!(
            s.toasts[0].text.to_lowercase().contains("next"),
            "got: {}",
            s.toasts[0].text
        );
    }

    #[test]
    fn adding_a_whole_marked_selection_reports_the_count() {
        // A range of 12 tracks naming only the first would read as a bug.
        let (_src, player) = deps();
        let mut s = AppState {
            pane: Pane::Songs,
            tracks: (0..3)
                .map(|i| ytm_core::Track::stub(&format!("v{i}"), "T"))
                .collect(),
            ..Default::default()
        };
        for t in s.tracks.clone() {
            s.marked.insert(t.video_id.clone());
        }
        dispatch_input(InputAction::AddToQueue, &mut s, &*player, &beh());
        assert!(s.toasts[0].text.contains('3'), "got: {}", s.toasts[0].text);
        match player.commands().first() {
            Some(PlayerCommand::EnqueueBack(ts)) => {
                assert_eq!(ts.len(), 3, "all marked tracks must be queued")
            }
            other => panic!("expected EnqueueBack, got {other:?}"),
        }
    }

    #[test]
    fn adding_with_an_empty_list_says_nothing_rather_than_lying() {
        let (_src, player) = deps();
        let mut s = AppState {
            pane: Pane::Songs,
            ..Default::default()
        };
        dispatch_input(InputAction::AddToQueue, &mut s, &*player, &beh());
        assert!(s.toasts.is_empty(), "nothing was added, so say nothing");
        assert!(player.commands().is_empty());
    }

    #[test]
    fn the_bundled_example_config_parses() {
        // `,` writes this file when the user has no config yet, so an invalid
        // example would hand them a config that refuses to load.
        let c = crate::config::Config::from_toml_str(crate::config::EXAMPLE_TOML)
            .expect("config.example.toml must parse");
        // Every value in it is commented out or a real default, so it must be
        // indistinguishable from no config at all.
        let d = crate::config::Config::default();
        assert_eq!(c.playback.volume, d.playback.volume);
        assert_eq!(c.behaviour.seek_step_secs, d.behaviour.seek_step_secs);
        assert_eq!(c.behaviour.volume_step, d.behaviour.volume_step);
        assert_eq!(c.ui.tick_ms, d.ui.tick_ms);
        assert_eq!(c.ui.theme, d.ui.theme);
        assert_eq!(c.ui.auto_reload_theme, d.ui.auto_reload_theme);
    }

    #[test]
    fn every_commented_keybinding_in_the_example_names_a_real_action() {
        // A typo'd action name in the example is silently ignored by the keymap,
        // so the user would rebind a key and see nothing happen.
        let mut checked = 0;
        for line in crate::config::EXAMPLE_TOML.lines() {
            let l = line.trim().trim_start_matches('#').trim();
            let Some((name, _)) = l.split_once(" = ") else {
                continue;
            };
            // Only the [keys] block uses quoted single-char values.
            if !l.contains('"') || name.contains('.') {
                continue;
            }
            if KeyMap::action_names().contains(&name) {
                checked += 1;
            }
        }
        assert!(
            checked >= 25,
            "expected the example to document the bindings, matched {checked}"
        );
    }

    #[test]
    fn reloading_applies_a_rebound_key_and_a_new_theme() {
        let r = reload_config(
            r#"
            [ui]
            theme = "gruvbox"
            [keys]
            toggle_visual = "z"
            "#,
        )
        .expect("valid config must reload");
        assert_eq!(r.theme_name, "gruvbox");
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        assert_eq!(
            r.keymap.resolve(
                KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE),
                ytm_tui::app::Focus::Main
            ),
            Some(InputAction::ToggleVisual)
        );
    }

    #[test]
    fn reloading_a_broken_config_is_an_error_rather_than_a_reset() {
        // The running keymap and theme must survive a typo: resetting to
        // defaults mid-session would be worse than refusing.
        assert!(reload_config("[ui]\ntheme = \"no-such-theme\"").is_err());
        assert!(reload_config("this is not toml").is_err());
        assert!(reload_config("[behaviour]\nvolume_step = 0").is_err());
    }

    #[test]
    fn reloading_config_includes_custom_theme_and_auto_reload_flag() {
        let dir = std::env::temp_dir().join(format!("ytm-test-theme-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let theme_path = dir.join("theme.toml");
        std::fs::write(&theme_path, "accent = \"#112233\"").unwrap();

        let toml = format!(
            "[ui]\ntheme_file = {:?}\nauto_reload_theme = true",
            theme_path.to_str().unwrap()
        );
        let r = reload_config(&toml).expect("valid config with theme_file must reload");
        assert_eq!(r.theme_name, "custom");
        assert!(r.custom_theme.is_some());
        assert_eq!(r.theme_file, Some(theme_path.clone()));
        assert!(r.auto_reload_theme);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn theme_file_update_detects_changes_and_ignores_unchanged_or_invalid() {
        let dir = std::env::temp_dir().join(format!("ytm-theme-check-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("theme.toml");
        std::fs::write(&path, "accent = \"#112233\"").unwrap();

        let mut last_mtime = None;
        let t1 = check_theme_file_update(&path, &mut last_mtime);
        assert!(t1.is_some());
        assert_eq!(
            t1.unwrap().accent,
            ratatui::style::Color::Rgb(0x11, 0x22, 0x33)
        );

        // Second check without modification returns None.
        let t2 = check_theme_file_update(&path, &mut last_mtime);
        assert!(t2.is_none());

        // Partial / invalid write returns None without panicking.
        std::fs::write(&path, "invalid toml {]").unwrap();
        let t3 = check_theme_file_update(&path, &mut last_mtime);
        assert!(t3.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pressing_add_to_queue_on_playlist_row_dispatches_enqueue_playlist() {
        let (_src, player) = deps();
        let mut state = AppState {
            pane: Pane::Playlists,
            open_playlist: None,
            selected: 0,
            playlists: vec![ytm_core::Playlist {
                track_count: Some(2),
                ..ytm_core::Playlist::stub("p1", "Test Playlist")
            }],
            ..Default::default()
        };

        let task = dispatch_input(InputAction::AddToQueue, &mut state, &*player, &beh());

        assert_eq!(
            task,
            Some(Task::EnqueuePlaylist {
                id: ytm_core::PlaylistId::from("p1"),
                title: "Test Playlist".to_owned(),
                play_next: false,
            })
        );
        assert!(state.loading);
    }

    #[test]
    fn pressing_play_next_on_playlist_row_dispatches_enqueue_playlist_next() {
        let (_src, player) = deps();
        let mut state = AppState {
            pane: Pane::Playlists,
            open_playlist: None,
            selected: 0,
            playlists: vec![ytm_core::Playlist {
                track_count: Some(2),
                ..ytm_core::Playlist::stub("p1", "Test Playlist")
            }],
            ..Default::default()
        };

        let task = dispatch_input(InputAction::PlayNext, &mut state, &*player, &beh());

        assert_eq!(
            task,
            Some(Task::EnqueuePlaylist {
                id: ytm_core::PlaylistId::from("p1"),
                title: "Test Playlist".to_owned(),
                play_next: true,
            })
        );
        assert!(state.loading);
    }

    #[test]
    fn try_spawn_enqueue_playlist_from_cache() {
        let dir = std::env::temp_dir().join(format!("ytm-playlist-cache-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let cache = ytm_core::cache::Cache::open(&dir.join("c.db")).unwrap();
        let pid = ytm_core::PlaylistId::from("p1");
        cache
            .save_playlist_tracks(&pid, &[ytm_core::Track::stub("t1", "Track 1")])
            .unwrap();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut state = AppState {
            loading: true,
            ..Default::default()
        };

        try_spawn(
            Task::EnqueuePlaylist {
                id: pid,
                title: "Test Playlist".to_owned(),
                play_next: false,
            },
            &None,
            &tx,
            &mut state,
            Some(&cache),
            None,
            None,
            None,
        );

        assert!(!state.loading);
        let ev = rx.try_recv().unwrap();
        match ev {
            AppEvent::EnqueueTracks {
                tracks,
                play_next,
                toast,
            } => {
                assert_eq!(tracks.len(), 1);
                assert!(!play_next);
                assert!(toast.unwrap().contains("Test Playlist"));
            }
            other => panic!("expected EnqueueTracks, got {:?}", other),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn try_spawn_enqueue_playlist_from_source() {
        let (src, _player) = deps();
        let src = Arc::new(
            Arc::into_inner(src)
                .unwrap()
                .with_tracks(vec![ytm_core::Track::stub("t1", "Track 1")]),
        );
        let pid = ytm_core::PlaylistId::from("p1");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut state = AppState {
            loading: true,
            ..Default::default()
        };

        try_spawn(
            Task::EnqueuePlaylist {
                id: pid,
                title: "Test Playlist".to_owned(),
                play_next: true,
            },
            &Some(src),
            &tx,
            &mut state,
            None,
            None,
            None,
            None,
        );

        let ev = tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv())
            .await
            .unwrap()
            .unwrap();
        match ev {
            AppEvent::EnqueueTracks {
                tracks,
                play_next,
                toast,
            } => {
                assert_eq!(tracks.len(), 1);
                assert!(play_next);
                assert!(toast.unwrap().contains("Test Playlist"));
            }
            other => panic!("expected EnqueueTracks, got {:?}", other),
        }
    }

    #[test]
    fn pressing_d_dispatches_download_job_with_toast() {
        let (_src, player) = deps();
        let mut state = AppState {
            tracks: vec![ytm_core::Track::stub("dl_track", "Download Track")],
            selected: 0,
            pane: Pane::Songs,
            ..Default::default()
        };

        let task = dispatch_input(InputAction::Download, &mut state, &*player, &beh());

        assert!(state.toasts.iter().any(|t| t.text.contains("Downloading")));
        assert!(matches!(task, Some(Task::Download(_))));
    }

    #[test]
    fn pressing_d_with_marked_tracks_dispatches_all_marked_tracks() {
        let (_src, player) = deps();
        let mut state = AppState {
            tracks: vec![
                ytm_core::Track::stub("t1", "Track 1"),
                ytm_core::Track::stub("t2", "Track 2"),
                ytm_core::Track::stub("t3", "Track 3"),
            ],
            selected: 0,
            pane: Pane::Songs,
            ..Default::default()
        };
        state.marked.insert(ytm_core::VideoId::from("t1"));
        state.marked.insert(ytm_core::VideoId::from("t3"));

        let task = dispatch_input(InputAction::Download, &mut state, &*player, &beh());

        assert!(
            state
                .toasts
                .iter()
                .any(|t| t.text.contains("Downloading 2 tracks"))
        );
        assert!(state.marked.is_empty(), "marks must be cleared");
        match task {
            Some(Task::Download(tracks)) => {
                assert_eq!(tracks.len(), 2);
                assert_eq!(tracks[0].video_id.as_str(), "t1");
                assert_eq!(tracks[1].video_id.as_str(), "t3");
            }
            other => panic!("expected Download task, got {:?}", other),
        }
    }

    #[test]
    fn pressing_x_on_downloads_pane_removes_track_and_returns_delete_task() {
        let (_src, player) = deps();
        let mut state = AppState {
            downloaded_tracks: vec![
                ytm_core::Track::stub("t1", "Track 1"),
                ytm_core::Track::stub("t2", "Track 2"),
            ],
            selected: 0,
            pane: Pane::Downloads,
            ..Default::default()
        };

        let task = dispatch_input(
            InputAction::RemoveFromPlaylist,
            &mut state,
            &*player,
            &beh(),
        );

        assert_eq!(state.downloaded_tracks.len(), 1);
        assert_eq!(state.downloaded_tracks[0].video_id.as_str(), "t2");
        assert!(
            state
                .toasts
                .iter()
                .any(|t| t.text.contains("Deleted \"Track 1\" from downloads"))
        );
        match task {
            Some(Task::DeleteDownloads(tracks)) => {
                assert_eq!(tracks.len(), 1);
                assert_eq!(tracks[0].video_id.as_str(), "t1");
            }
            other => panic!("expected DeleteDownloads task, got {:?}", other),
        }
    }

    #[test]
    fn preload_from_cache_populates_downloaded_tracks() {
        let dir = std::env::temp_dir().join(format!("ytm-dl-preload-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cache = ytm_core::cache::Cache::open(&dir.join("cache.db")).unwrap();

        let dt = ytm_core::DownloadedTrack {
            video_id: ytm_core::VideoId::from("dl1"),
            title: "Downloaded Title".into(),
            artists: vec!["Artist".into()],
            album: None,
            duration_secs: 180,
            thumbnail_url: None,
            file_path: "/tmp/dl1.opus".into(),
            file_size_bytes: 4096,
            downloaded_at: 1000,
        };
        cache.save_downloaded_track(&dt).unwrap();

        let mut state = AppState::default();
        preload_from_cache(&cache, &mut state);

        assert_eq!(state.downloaded_tracks.len(), 1);
        assert_eq!(state.downloaded_tracks[0].video_id.as_str(), "dl1");
        assert_eq!(state.downloaded_tracks[0].title, "Downloaded Title");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_writer_handles_save_and_remove_downloaded() {
        let dir = std::env::temp_dir().join(format!("ytm-dl-writer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cache = ytm_core::cache::Cache::open(&dir.join("cache.db")).unwrap();

        let dt = ytm_core::DownloadedTrack {
            video_id: ytm_core::VideoId::from("w1"),
            title: "Writer Test".into(),
            artists: vec!["Artist".into()],
            album: None,
            duration_secs: 200,
            thumbnail_url: None,
            file_path: "/tmp/w1.opus".into(),
            file_size_bytes: 1024,
            downloaded_at: 2000,
        };

        CacheWork::SaveDownloaded(dt).run(&cache).unwrap();
        assert!(
            cache
                .is_track_downloaded(&ytm_core::VideoId::from("w1"))
                .unwrap()
        );

        CacheWork::RemoveDownloaded(ytm_core::VideoId::from("w1"))
            .run(&cache)
            .unwrap();
        assert!(
            !cache
                .is_track_downloaded(&ytm_core::VideoId::from("w1"))
                .unwrap()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
