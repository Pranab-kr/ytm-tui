//! ratatui widgets. Each one draws into a `Rect` and reads `AppState`; none of
//! them mutate it.
pub mod art;
pub mod help;
pub mod modal;
pub mod nowplaying;
pub mod playlists;
pub mod queue;
pub mod row;
pub mod search;
pub mod sidebar;
pub mod toast;
pub mod tracklist;
