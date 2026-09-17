//! Playback: the `Player` trait, queue logic, stream resolution, and the mpv backend.
pub mod actor;
#[cfg(any(test, feature = "mock"))]
pub mod mock;
pub mod mpv_backend;
pub mod player;
pub mod queue;
pub mod resolver;
pub mod storage;
