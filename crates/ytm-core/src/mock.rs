//! In-memory MusicSource for tests. No network, deterministic, records calls.

use crate::{model::*, source::*};
use std::sync::Mutex;

#[derive(Default)]
struct Inner {
    playlists: Vec<Playlist>,
    tracks: Vec<Track>,
    albums: Vec<Album>,
    artists: Vec<Artist>,
    shelves: Vec<HomeShelf>,
    artist_tracks: Vec<Track>,
    album_tracks: Vec<Track>,
    calls: Vec<String>,
    fail_next: Option<SourceError>,
    next_id: u32,
    authenticated: bool,
}

#[derive(Default)]
pub struct MockSource {
    inner: Mutex<Inner>,
}

impl MockSource {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                authenticated: true,
                ..Default::default()
            }),
        }
    }

    pub fn guest() -> Self {
        Self::default()
    }

    pub fn with_playlists(self, p: Vec<Playlist>) -> Self {
        self.inner.lock().unwrap().playlists = p;
        self
    }

    pub fn with_tracks(self, t: Vec<Track>) -> Self {
        self.inner.lock().unwrap().tracks = t;
        self
    }

    pub fn with_album_tracks(self, t: Vec<Track>) -> Self {
        self.inner.lock().unwrap().album_tracks = t;
        self
    }

    /// The next call — whichever it is — returns this error, once.
    pub fn fail_next(&self, e: SourceError) {
        self.inner.lock().unwrap().fail_next = Some(e);
    }

    pub fn calls(&self) -> Vec<String> {
        self.inner.lock().unwrap().calls.clone()
    }

    fn record(&self, what: impl Into<String>) -> Result<(), SourceError> {
        let mut g = self.inner.lock().unwrap();
        g.calls.push(what.into());
        match g.fail_next.take() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

/// Bodies are one-liners over the mutex; a macro keeps this readable.
macro_rules! mock_read {
    ($name:ident, $ret:ty, $field:ident) => {
        fn $name(&self) -> BoxFut<'_, Vec<$ret>> {
            Box::pin(async move {
                self.record(stringify!($name))?;
                Ok(self.inner.lock().unwrap().$field.clone())
            })
        }
    };
}

impl MusicSource for MockSource {
    fn is_authenticated(&self) -> bool {
        self.inner.lock().unwrap().authenticated
    }

    mock_read!(library_playlists, Playlist, playlists);
    mock_read!(library_songs, Track, tracks);
    mock_read!(library_albums, Album, albums);
    mock_read!(library_artists, Artist, artists);
    mock_read!(home_shelves, HomeShelf, shelves);

    fn recommended_albums(&self) -> BoxFut<'_, Vec<Album>> {
        Box::pin(async move {
            self.record("recommended_albums")?;
            Ok(self.inner.lock().unwrap().albums.clone())
        })
    }

    fn artist_tracks(&self, id: ArtistId) -> BoxFut<'_, Vec<Track>> {
        Box::pin(async move {
            self.record(format!("artist_tracks({id})"))?;
            Ok(self.inner.lock().unwrap().artist_tracks.clone())
        })
    }

    fn album_tracks(&self, id: AlbumId) -> BoxFut<'_, Vec<Track>> {
        Box::pin(async move {
            self.record(format!("album_tracks({id})"))?;
            Ok(self.inner.lock().unwrap().album_tracks.clone())
        })
    }

    fn playlist_tracks(&self, id: PlaylistId) -> BoxFut<'_, Vec<Track>> {
        Box::pin(async move {
            self.record(format!("playlist_tracks({id})"))?;
            Ok(self.inner.lock().unwrap().tracks.clone())
        })
    }

    fn playlist_details(&self, id: PlaylistId) -> BoxFut<'_, Playlist> {
        Box::pin(async move {
            self.record(format!("playlist_details({id})"))?;
            self.inner
                .lock()
                .unwrap()
                .playlists
                .iter()
                .find(|p| p.id == id)
                .cloned()
                .ok_or_else(|| SourceError::NotFound(id.to_string()))
        })
    }

    fn search_songs(&self, q: String) -> BoxFut<'_, Vec<Track>> {
        Box::pin(async move {
            self.record(format!("search_songs({q})"))?;
            let g = self.inner.lock().unwrap();
            Ok(g.tracks
                .iter()
                .filter(|t| t.title.to_lowercase().contains(&q.to_lowercase()))
                .cloned()
                .collect())
        })
    }

    fn search_albums(&self, q: String) -> BoxFut<'_, Vec<Album>> {
        Box::pin(async move {
            self.record(format!("search_albums({q})"))?;
            Ok(self.inner.lock().unwrap().albums.clone())
        })
    }

    fn search_artists(&self, q: String) -> BoxFut<'_, Vec<Artist>> {
        Box::pin(async move {
            self.record(format!("search_artists({q})"))?;
            Ok(self.inner.lock().unwrap().artists.clone())
        })
    }

    fn search_playlists(&self, q: String) -> BoxFut<'_, Vec<Playlist>> {
        Box::pin(async move {
            self.record(format!("search_playlists({q})"))?;
            Ok(self.inner.lock().unwrap().playlists.clone())
        })
    }

    fn create_playlist(
        &self,
        title: String,
        description: Option<String>,
        privacy: Privacy,
    ) -> BoxFut<'_, PlaylistId> {
        Box::pin(async move {
            self.record(format!("create_playlist({title})"))?;
            let mut g = self.inner.lock().unwrap();
            g.next_id += 1;
            let id = PlaylistId(format!("mock-pl-{}", g.next_id));
            g.playlists.push(Playlist {
                id: id.clone(),
                title,
                description,
                privacy,
                track_count: Some(0),
                thumbnail_url: None,
                is_system: false,
            });
            Ok(id)
        })
    }

    fn edit_playlist(
        &self,
        id: PlaylistId,
        new_title: Option<String>,
        new_description: Option<String>,
        new_privacy: Option<Privacy>,
    ) -> BoxFut<'_, ()> {
        Box::pin(async move {
            self.record(format!("edit_playlist({id})"))?;
            let mut g = self.inner.lock().unwrap();
            let Some(p) = g.playlists.iter_mut().find(|p| p.id == id) else {
                return Err(SourceError::NotFound(id.to_string()));
            };
            if let Some(t) = new_title {
                p.title = t;
            }
            if let Some(d) = new_description {
                p.description = Some(d);
            }
            if let Some(v) = new_privacy {
                p.privacy = v;
            }
            Ok(())
        })
    }

    fn delete_playlist(&self, id: PlaylistId) -> BoxFut<'_, ()> {
        Box::pin(async move {
            self.record(format!("delete_playlist({id})"))?;
            self.inner.lock().unwrap().playlists.retain(|p| p.id != id);
            Ok(())
        })
    }

    fn add_tracks(&self, id: PlaylistId, videos: Vec<VideoId>) -> BoxFut<'_, ()> {
        Box::pin(async move {
            self.record(format!("add_tracks({id},{})", videos.len()))?;
            Ok(())
        })
    }

    fn remove_tracks(&self, id: PlaylistId, entries: Vec<SetVideoId>) -> BoxFut<'_, ()> {
        Box::pin(async move {
            self.record(format!("remove_tracks({id},{})", entries.len()))?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_source_is_authenticated_unless_configured_as_guest() {
        assert!(MockSource::new().is_authenticated());
        assert!(!MockSource::guest().is_authenticated());
    }

    #[tokio::test]
    async fn returns_seeded_playlists() {
        let m = MockSource::new().with_playlists(vec![Playlist::stub("p1", "Focus")]);
        let got = m.library_playlists().await.unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].title, "Focus");
    }

    #[tokio::test]
    async fn returns_seeded_album_tracks() {
        let m = MockSource::new().with_album_tracks(vec![Track::stub("v9", "Album Song")]);
        let got = m.album_tracks(AlbumId::from("MPREb_1")).await.unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].title, "Album Song");
        assert_eq!(m.calls(), vec!["album_tracks(MPREb_1)"]);
    }

    #[tokio::test]
    async fn records_calls_in_order() {
        let m = MockSource::new();
        let _ = m.library_playlists().await;
        let _ = m.delete_playlist("p1".into()).await;
        assert_eq!(m.calls(), vec!["library_playlists", "delete_playlist(p1)"]);
    }

    #[tokio::test]
    async fn fail_next_makes_exactly_one_call_fail() {
        let m = MockSource::new().with_playlists(vec![Playlist::stub("p1", "Focus")]);
        m.fail_next(SourceError::RateLimited);
        assert!(m.library_playlists().await.is_err());
        assert!(
            m.library_playlists().await.is_ok(),
            "only the next call should fail"
        );
    }

    #[tokio::test]
    async fn create_playlist_appends_and_returns_new_id() {
        let m = MockSource::new();
        let id = m
            .create_playlist("New".into(), None, Privacy::Private)
            .await
            .unwrap();
        let all = m.library_playlists().await.unwrap();
        assert!(all.iter().any(|p| p.id == id && p.title == "New"));
    }
}
