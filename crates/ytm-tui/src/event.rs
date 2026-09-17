//! Everything that can change the app. The event loop's only vocabulary.

use ytm_core::{Album, AlbumId, Artist, Playlist, PlaylistId, Track};
use ytm_player::player::PlayerEvent;

/// A key press already resolved through the keymap into an intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputAction {
    Quit,
    /// Ctrl+C. Bypasses `behaviour.confirm_on_quit`: the keymap treats this
    /// as the escape hatch that nothing may shadow, a confirm included.
    ForceQuit,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    Left,
    Right,
    Confirm,
    Cancel,
    NextPane,
    PrevPane,
    GoTo(u8),
    TogglePause,
    NextTrack,
    PrevTrack,
    SeekForward,
    SeekBack,
    VolumeUp,
    VolumeDown,
    ToggleMute,
    ToggleShuffle,
    CycleRepeat,
    OpenSearch,
    /// Start a live filter over the rows on screen (`/`). Local, no request.
    OpenFilter,
    /// Scroll the list without moving the cursor off it (mouse wheel).
    ScrollUp,
    ScrollDown,
    /// Centre the selected row in the viewport (`zz`, from vim).
    CenterOnCursor,
    /// Select and reveal the currently playing row.
    FocusCurrent,
    OpenQueue,
    OpenHelp,
    AddToQueue,
    PlayNext,
    /// Reorder the selected queue entry (FR-Q3). Queue-only.
    MoveEntryUp,
    MoveEntryDown,
    ClearQueue,
    CreatePlaylist,
    RenamePlaylist,
    DeletePlaylist,
    RemoveFromPlaylist,
    AddToPlaylist,
    Refresh,
    ToggleMark,
    /// Start/stop a range selection anchored at the current row (visual mode).
    ToggleVisual,
    /// Cycle to the next built-in theme.
    CycleTheme,
    /// Open config.toml in $EDITOR, reloading keys and theme on exit.
    EditConfig,
    /// Download track(s) for offline listening.
    Download,
    Char(char),
    Backspace,
    /// Delete the word before the cursor (Ctrl+W).
    DeleteWordBack,
    /// Move the cursor a word at a time (Ctrl+Left / Ctrl+Right).
    WordLeft,
    WordRight,
    /// Move the cursor one character (Left/Right inside a text field).
    CharLeft,
    CharRight,
    /// Jump to the start/end of the line (Ctrl+A / Ctrl+E).
    LineStart,
    LineEnd,
}

#[derive(Debug)]
pub enum AppEvent {
    Input(InputAction),
    Player(PlayerEvent),
    Tick,
    Resize,

    PlaylistsLoaded(Vec<Playlist>),
    LibrarySongsLoaded(Vec<Track>),
    DownloadedTracksLoaded(Vec<Track>),
    DownloadedTrackSaved(Track),
    AlbumsLoaded(Vec<Album>),
    ArtistsLoaded(Vec<Artist>),
    PlaylistTracksLoaded {
        id: PlaylistId,
        tracks: Vec<Track>,
    },
    /// Artists found by searching, replacing the library list (FR-B7).
    ArtistSearchResults {
        query: String,
        artists: Vec<Artist>,
    },
    /// The home feed's shelves (FR-B6).
    HomeLoaded(Vec<ytm_core::HomeShelf>),
    /// An artist's top tracks, with the name for the heading (FR-B7).
    ArtistTracksLoaded {
        id: ytm_core::ArtistId,
        name: String,
        tracks: Vec<Track>,
    },
    /// An album's songs, with the title for the heading.
    AlbumTracksLoaded {
        id: AlbumId,
        name: String,
        tracks: Vec<Track>,
    },
    SearchResults {
        query: String,
        tracks: Vec<Track>,
    },

    /// Album art bytes arrived and decoded. Carries the URL so a late response
    /// for a track that is no longer playing can be cached without being drawn.
    ArtLoaded {
        url: String,
        image: Box<image::DynamicImage>,
    },
    /// Art could not be fetched or decoded. Not shown to the user: FR-U5 makes
    /// art optional, and a toast per missing thumbnail would be noise.
    ArtFailed {
        url: String,
    },

    /// A mutation succeeded server-side; `token` matches the optimistic edit.
    /// `real_id` is the server's id for a create, which replaces the temp one.
    MutationOk {
        token: u64,
        real_id: Option<PlaylistId>,
        message: String,
    },
    /// A mutation failed; roll back the edit tagged with `token`.
    MutationFailed {
        token: u64,
        message: String,
    },

    Error(String),
    LoginNeeded {
        user_code: String,
        url: String,
    },
    LoginComplete,
}
