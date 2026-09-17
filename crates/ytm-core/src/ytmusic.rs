//! The live MusicSource. Wraps ytmapi-rs; the only file besides mapping.rs that
//! calls it.

use crate::{mapping, model::*, source::*};
use ytmapi_rs::YtMusic;
use ytmapi_rs::auth::noauth::NoAuthToken;
use ytmapi_rs::auth::{AuthToken, BrowserToken, LoggedIn};
use ytmapi_rs::common::{ApiOutcome, YoutubeID};
use ytmapi_rs::parse::SearchResultPlaylist;
use ytmapi_rs::query::playlist::PrivacyStatus;
use ytmapi_rs::query::search::{FilteredSearch, SearchQuery, SongsFilter};
use ytmapi_rs::query::{CreatePlaylistQuery, EditPlaylistQuery};

/// Generic over the token type, though cookie auth is the only one left: the
/// generic is what keeps `MusicSource` free of any `ytmapi-rs` type, and it costs
/// nothing to keep.
pub struct YtMusicSource<A: AuthToken> {
    api: YtMusic<A>,
}

impl YtMusicSource<NoAuthToken> {
    pub async fn unauthenticated() -> Result<Self, SourceError> {
        let api = YtMusic::new_unauthenticated().await.map_err(classify)?;
        Ok(Self { api })
    }
}

impl YtMusicSource<BrowserToken> {
    /// Cookie fallback (FR-A5).
    pub async fn from_cookie_file(path: impl AsRef<std::path::Path>) -> Result<Self, SourceError> {
        let api = YtMusic::from_cookie_file(path).await.map_err(classify)?;
        Ok(Self { api })
    }
}

/// Upstream errors are opaque strings; classify them into our variants so the
/// UI can show a sentence (NFR-9). Refine the substrings against real failures.
fn classify(e: ytmapi_rs::Error) -> SourceError {
    let kind = e.into_kind();
    if matches!(kind, ytmapi_rs::error::ErrorKind::ApiStatusFailed) {
        return SourceError::AlreadyInPlaylist;
    }
    let s = kind.to_string();
    let l = s.to_lowercase();
    if l.contains("401") || l.contains("unauthor") {
        SourceError::NotAuthenticated
    } else if l.contains("429")
        || l.contains("rate")
        || l.contains("automated queries")
        || l.contains("we're sorry")
        || (l.contains("expected value") && (l.contains("line 1") || l.contains("line: 1")))
    {
        SourceError::RateLimited
    } else if l.contains("404") || l.contains("not found") {
        SourceError::NotFound(s)
    } else if l.contains("parse") || l.contains("navigation") {
        SourceError::Parse(s)
    } else if l.contains("status_failed") {
        SourceError::AlreadyInPlaylist
    } else {
        SourceError::Network(s)
    }
}

/// Run upstream's typed parse over a response we already hold, so we can inspect
/// the JSON first — upstream's parsers fail on an empty library section instead
/// of yielding an empty list. `parse_into` runs on JSON in hand: no extra request.
fn parse_json<Q, O>(query: &Q, json: String) -> Result<O, SourceError>
where
    O: ytmapi_rs::parse::ParseFrom<Q>,
{
    if let Some(error) = classify_raw_api_error(&json) {
        return Err(error);
    }
    let value: ytmapi_rs::json::Json =
        serde_json::from_str(&json).map_err(|e| SourceError::Parse(e.to_string()))?;
    ytmapi_rs::parse::ProcessedResult {
        query,
        source: json,
        json: value,
    }
    .parse_into()
    .map_err(classify)
}

fn classify_raw_api_error(json: &str) -> Option<SourceError> {
    let trimmed = json.trim_start();
    if trimmed.starts_with('<')
        || trimmed.contains("automated queries")
        || trimmed.contains("We're sorry")
        || trimmed.contains("we're sorry")
    {
        return Some(SourceError::RateLimited);
    }
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let error = value.get("error")?;
    let code = error.get("code").and_then(serde_json::Value::as_i64);
    let message = error
        .get("message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("YouTube API error");
    Some(match code {
        Some(401) => SourceError::NotAuthenticated,
        Some(429) => SourceError::RateLimited,
        _ => SourceError::Other(format!("YouTube API error ({message})")),
    })
}

/// The one song-search path, shared by the authenticated and the guest source:
/// `ytmapi-rs` 0.3.3's typed `search_songs` reads a UGC `artist • 43K views •
/// 3:26` byline as an album and aborts the whole response (PROGRESS.md 2026-09-02).
async fn song_search_from_raw<A: AuthToken>(
    api: &YtMusic<A>,
    query: &str,
) -> Result<Vec<Track>, SourceError> {
    // The query owns its text (Cow::Owned), so it can be 'static — pinned so the
    // turbofish below has a concrete type; `impl Borrow<Q>` cannot infer it.
    let q: SearchQuery<'static, FilteredSearch<SongsFilter>> = SearchQuery::from(query.to_owned());
    let json = api
        .raw_json_query::<SearchQuery<'static, FilteredSearch<SongsFilter>>>(&q)
        .await
        .map_err(classify)?;
    if let Some(error) = classify_raw_api_error(&json) {
        return Err(error);
    }
    Ok(crate::search_raw::tracks_from_raw(&json))
}

/// YouTube answers a mutation with an outcome rather than an HTTP error, so a
/// silent `Failure` would look like success to the UI.
fn check_outcome(o: ApiOutcome) -> Result<(), SourceError> {
    match o {
        ApiOutcome::Success => Ok(()),
        ApiOutcome::Failure => Err(SourceError::Other("YouTube rejected the change".to_owned())),
    }
}

fn to_privacy_status(p: Privacy) -> PrivacyStatus {
    match p {
        Privacy::Public => PrivacyStatus::Public,
        Privacy::Private => PrivacyStatus::Private,
        Privacy::Unlisted => PrivacyStatus::Unlisted,
    }
}

/// YouTube Music's home feed (FR-B6). `ytmapi-rs` 0.3.3 has no home query, but
/// `Query`/`PostQuery` are user-implementable, so `browseId: FEmusic_home` needs
/// no fork (verified live 2026-08-31). Raw output; `home_feed` does the parsing.
#[derive(Debug)]
pub struct HomeRaw;

impl ytmapi_rs::parse::ParseFrom<GetHomeQuery> for HomeRaw {
    fn parse_from(_: ytmapi_rs::parse::ProcessedResult<GetHomeQuery>) -> ytmapi_rs::Result<Self> {
        // Never called: we always go through `raw_json_query`.
        Ok(HomeRaw)
    }
}

/// The `FEmusic_home` browse query.
#[derive(Debug, Clone)]
pub struct GetHomeQuery;

impl<A: LoggedIn> ytmapi_rs::query::Query<A> for GetHomeQuery {
    type Output = HomeRaw;
    type Method = ytmapi_rs::query::PostMethod;
}

impl ytmapi_rs::query::PostQuery for GetHomeQuery {
    fn header(&self) -> serde_json::Map<String, serde_json::Value> {
        serde_json::Map::from_iter([("browseId".to_string(), serde_json::json!("FEmusic_home"))])
    }
    fn params(&self) -> Vec<(&str, std::borrow::Cow<'_, str>)> {
        vec![]
    }
    fn path(&self) -> &str {
        "browse"
    }
}

/// The next page of the home feed — the useful one. Measured live: page 1 is
/// "Listen again" / "From your library" / "Listen together", while page 2 holds
/// "Quick picks", "Covers and remixes", "Heard in Shorts" — what the web leads with.
#[derive(Debug, Clone)]
pub struct GetHomeContinuationQuery(pub String);

impl ytmapi_rs::parse::ParseFrom<GetHomeContinuationQuery> for HomeRaw {
    fn parse_from(
        _: ytmapi_rs::parse::ProcessedResult<GetHomeContinuationQuery>,
    ) -> ytmapi_rs::Result<Self> {
        Ok(HomeRaw)
    }
}

impl<A: LoggedIn> ytmapi_rs::query::Query<A> for GetHomeContinuationQuery {
    type Output = HomeRaw;
    type Method = ytmapi_rs::query::PostMethod;
}

impl ytmapi_rs::query::PostQuery for GetHomeContinuationQuery {
    fn header(&self) -> serde_json::Map<String, serde_json::Value> {
        serde_json::Map::from_iter([("browseId".to_string(), serde_json::json!("FEmusic_home"))])
    }
    fn params(&self) -> Vec<(&str, std::borrow::Cow<'_, str>)> {
        vec![
            ("continuation", std::borrow::Cow::from(self.0.as_str())),
            ("type", std::borrow::Cow::from("next")),
        ]
    }
    fn path(&self) -> &str {
        "browse"
    }
}

/// How many home-feed pages to walk. Page 1 carries none of the shelves the web
/// player leads with, and three pages is where the returns flatten — each page is
/// a round trip the user waits on. Measured live 2026-08-31.
const HOME_PAGES: usize = 3;

/// Which song list to show for an artist: the full playlist, or the page preview.
/// The shelf is a ~5-row preview with no thumbnail and no duration; its
/// `browse_id` playlist has both, so it wins when it arrived with as many rows.
fn better_artist_tracks(preview: Vec<Track>, full: Result<Vec<Track>, SourceError>) -> Vec<Track> {
    match full {
        // `>=` rather than `>`: at equal length the playlist rows are still the
        // better ones, because they carry art and durations the shelf does not.
        Ok(rows) if rows.len() >= preview.len() && !rows.is_empty() => rows,
        Ok(_) => preview,
        Err(e) => {
            // Not a toast: the preview rows are on screen and playable, so this
            // is a degraded result, not a failure the user must act on.
            tracing::debug!(error = %e, "artist songs playlist unavailable; keeping the page preview");
            preview
        }
    }
}

macro_rules! impl_feed {
    ($token:ty) => {
        impl YtMusicSource<$token> {
            /// Walk the home feed, following continuations (FR-B6). A first-page
            /// error propagates — YouTube is unreachable. A later page failing
            /// just stops the walk rather than losing the shelves in hand.
            async fn feed_shelves(&self) -> Result<Vec<HomeShelf>, SourceError> {
                let first = self
                    .api
                    .raw_json_query::<GetHomeQuery>(&GetHomeQuery)
                    .await
                    .map_err(classify)?;
                if let Some(error) = classify_raw_api_error(&first) {
                    return Err(error);
                }
                let mut shelves = crate::home_feed::shelves_from_raw(&first);
                let mut token = crate::home_feed::continuation_token(&first);

                for _ in 1..HOME_PAGES {
                    let Some(t) = token.take() else { break };
                    let q = GetHomeContinuationQuery(t);
                    let Ok(page) = self
                        .api
                        .raw_json_query::<GetHomeContinuationQuery>(&q)
                        .await
                    else {
                        break;
                    };
                    shelves.extend(crate::home_feed::shelves_from_raw(&page));
                    token = crate::home_feed::continuation_token(&page);
                }
                Ok(shelves)
            }
        }
    };
}

impl_feed!(BrowserToken);

/// One `MusicSource` impl per concrete token type, not one `impl<A: LoggedIn>`:
/// `AuthToken::headers` returns an opaque `impl IntoIterator` with no `Send`
/// bound, so the `BoxFut` cast needs auto-trait leakage from a concrete `A`.
macro_rules! impl_music_source {
    ($token:ty) => {
        impl MusicSource for YtMusicSource<$token> {
            fn library_playlists(&self) -> BoxFut<'_, Vec<Playlist>> {
                Box::pin(async move {
                    let raw = self.api.get_library_playlists().await.map_err(classify)?;
                    Ok(raw.iter().map(mapping::playlist_from_library).collect())
                })
            }

            fn library_songs(&self) -> BoxFut<'_, Vec<Track>> {
                Box::pin(async move {
                    let raw = self.api.get_library_songs().await.map_err(classify)?;
                    Ok(raw.iter().map(mapping::track_from_table_list).collect())
                })
            }

            fn library_albums(&self) -> BoxFut<'_, Vec<Album>> {
                Box::pin(async move {
                    // Raw JSON, not `get_library_albums`: with no saved albums
                    // YouTube sends a `messageRenderer` where the parser demands
                    // a `gridRenderer`, and that reached the user as a toast (FR-B3).
                    let query = ytmapi_rs::query::GetLibraryAlbumsQuery::default();
                    let json = self
                        .api
                        .raw_json_query::<ytmapi_rs::query::GetLibraryAlbumsQuery>(&query)
                        .await
                        .map_err(classify)?;
                    if crate::library_raw::is_empty_library(&json) {
                        return Ok(Vec::new());
                    }
                    let raw: Vec<ytmapi_rs::parse::SearchResultAlbum> = parse_json(&query, json)?;
                    Ok(raw.iter().map(mapping::album_from_search).collect())
                })
            }

            fn library_artists(&self) -> BoxFut<'_, Vec<Artist>> {
                Box::pin(async move {
                    // Same empty-library hazard as albums: an artist list that
                    // empties would fail to parse rather than render empty.
                    let query = ytmapi_rs::query::GetLibraryArtistsQuery::default();
                    let json = self
                        .api
                        .raw_json_query::<ytmapi_rs::query::GetLibraryArtistsQuery>(&query)
                        .await
                        .map_err(classify)?;
                    if crate::library_raw::is_empty_library(&json) {
                        return Ok(Vec::new());
                    }
                    let raw: Vec<ytmapi_rs::parse::LibraryArtist> = parse_json(&query, json)?;
                    Ok(raw.iter().map(mapping::artist_from_library).collect())
                })
            }

            fn home_shelves(&self) -> BoxFut<'_, Vec<HomeShelf>> {
                Box::pin(async move { self.feed_shelves().await })
            }

            fn recommended_albums(&self) -> BoxFut<'_, Vec<Album>> {
                Box::pin(async move {
                    // Most accounts save no albums, so the library pane is a dead
                    // end. The feed's album cards fill it instead.
                    Ok(crate::home_feed::albums_from_shelves(
                        &self.feed_shelves().await?,
                    ))
                })
            }

            fn artist_tracks(&self, id: ArtistId) -> BoxFut<'_, Vec<Track>> {
                Box::pin(async move {
                    let raw = self
                        .api
                        .get_artist(ytmapi_rs::common::ArtistChannelID::from_raw(id.as_str()))
                        .await
                        .map_err(classify)?;
                    // `top_releases.songs` is the artist page's song shelf. An
                    // artist with no shelf yields an empty list rather than an
                    // error — nothing is broken, there is just nothing to play.
                    let Some(songs) = raw.top_releases.songs else {
                        return Ok(Vec::new());
                    };

                    // That shelf is the web UI's ~5-row preview above "Show all",
                    // and `ArtistSong` carries no thumbnail or duration, so those
                    // rows render bare. One request on `browse_id` buys both.
                    let preview: Vec<Track> = songs
                        .results
                        .iter()
                        .map(mapping::track_from_artist_song)
                        .collect();
                    let full = self
                        .playlist_tracks(PlaylistId::from(songs.browse_id.get_raw()))
                        .await;
                    // A failed or empty follow-up keeps the preview: five playable
                    // rows beat an error for something the user can already see.
                    Ok(better_artist_tracks(preview, full))
                })
            }

            fn album_tracks(&self, id: AlbumId) -> BoxFut<'_, Vec<Track>> {
                Box::pin(async move {
                    let raw = self
                        .api
                        .get_album(ytmapi_rs::common::AlbumID::from_raw(id.as_str()))
                        .await
                        .map_err(classify)?;
                    Ok(mapping::tracks_from_album(&raw))
                })
            }

            fn playlist_tracks(&self, id: PlaylistId) -> BoxFut<'_, Vec<Track>> {
                Box::pin(async move {
                    // One request, parsed twice — no extra round trip: upstream's
                    // typed parse for track data, ours for `setVideoId`, which
                    // 0.3.3 discards but removal needs (FR-C5). Browse: VL form.
                    let browse_id = id.browse_form();
                    let query = ytmapi_rs::query::GetPlaylistTracksQuery::new(
                        ytmapi_rs::common::PlaylistID::from_raw(&browse_id),
                    );
                    // Turbofished: `impl Borrow<Q>` cannot infer Q from a reference.
                    let json = self
                        .api
                        .raw_json_query::<ytmapi_rs::query::GetPlaylistTracksQuery>(&query)
                        .await
                        .map_err(classify)?;

                    if let Some(error) = classify_raw_api_error(&json) {
                        return Err(error);
                    }

                    let value: ytmapi_rs::json::Json = serde_json::from_str(&json)
                        .map_err(|e| SourceError::Parse(e.to_string()))?;
                    let items: Vec<ytmapi_rs::parse::PlaylistItem> =
                        ytmapi_rs::parse::ProcessedResult {
                            query: &query,
                            source: json.clone(),
                            json: value,
                        }
                        .parse_into()
                        .map_err(classify)?;

                    // Episodes (podcasts) are out of scope and map to None.
                    let tracks: Vec<Track> = items
                        .iter()
                        .filter_map(mapping::track_from_playlist_item)
                        .collect();
                    // Paired by videoId, not position: upstream returned 83
                    // tracks for an 85-row shelf, so it drops rows internally
                    // and nothing positional can line up. Measured live.
                    let rows = crate::playlist_raw::entry_ids_from_raw(&json);
                    Ok(crate::playlist_raw::attach_entry_ids(tracks, &rows))
                })
            }

            fn playlist_details(&self, id: PlaylistId) -> BoxFut<'_, Playlist> {
                Box::pin(async move {
                    // Browse endpoint: VL-prefixed form.
                    let browse_id = id.browse_form();
                    let raw = self
                        .api
                        .get_playlist_details(ytmapi_rs::common::PlaylistID::from_raw(&browse_id))
                        .await
                        .map_err(classify)?;
                    Ok(mapping::playlist_from_details(&raw))
                })
            }

            fn search_songs(&self, query: String) -> BoxFut<'_, Vec<Track>> {
                Box::pin(async move { song_search_from_raw(&self.api, &query).await })
            }

            fn search_albums(&self, query: String) -> BoxFut<'_, Vec<Album>> {
                Box::pin(async move {
                    let raw = self
                        .api
                        .search_albums(query.as_str())
                        .await
                        .map_err(classify)?;
                    Ok(raw.iter().map(mapping::album_from_search).collect())
                })
            }

            fn search_artists(&self, query: String) -> BoxFut<'_, Vec<Artist>> {
                Box::pin(async move {
                    let raw = self
                        .api
                        .search_artists(query.as_str())
                        .await
                        .map_err(classify)?;
                    Ok(raw.iter().map(mapping::artist_from_search).collect())
                })
            }

            fn search_playlists(&self, query: String) -> BoxFut<'_, Vec<Playlist>> {
                Box::pin(async move {
                    let raw = self
                        .api
                        .search_playlists(query.as_str())
                        .await
                        .map_err(classify)?;
                    // Podcast results are out of scope.
                    Ok(raw
                        .iter()
                        .filter_map(|p| match p {
                            SearchResultPlaylist::Featured(f) => {
                                Some(mapping::playlist_from_search_featured(f))
                            }
                            SearchResultPlaylist::Community(c) => {
                                Some(mapping::playlist_from_search_community(c))
                            }
                            _ => None,
                        })
                        .collect())
                })
            }

            fn create_playlist(
                &self,
                title: String,
                description: Option<String>,
                privacy: Privacy,
            ) -> BoxFut<'_, PlaylistId> {
                Box::pin(async move {
                    let query = CreatePlaylistQuery::new(
                        title.as_str(),
                        description.as_deref(),
                        to_privacy_status(privacy),
                    );
                    let id = self.api.create_playlist(query).await.map_err(classify)?;
                    Ok(PlaylistId(id.get_raw().to_owned()))
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
                    if mapping::is_system_playlist(id.as_str()) {
                        return Err(SourceError::NotEditable(id.to_string()));
                    }
                    // Mutation endpoint: bare form. The VL form is answered with
                    // 400 INVALID_ARGUMENT.
                    let raw_id = id.mutation_form();
                    let pid = ytmapi_rs::common::PlaylistID::from_raw(&raw_id);
                    // EditPlaylistQuery is built per-change upstream; issue one call per
                    // field the caller actually wants changed.
                    if let Some(t) = new_title.as_deref() {
                        let mut q = EditPlaylistQuery::new_title(pid.clone(), t);
                        if let Some(d) = new_description.as_deref() {
                            q = q.with_new_description(d);
                        }
                        check_outcome(self.api.edit_playlist(q).await.map_err(classify)?)?;
                    } else if let Some(d) = new_description.as_deref() {
                        let q = EditPlaylistQuery::new_description(pid.clone(), d);
                        check_outcome(self.api.edit_playlist(q).await.map_err(classify)?)?;
                    }
                    if let Some(p) = new_privacy {
                        let q = EditPlaylistQuery::new_privacy_status(pid, to_privacy_status(p));
                        check_outcome(self.api.edit_playlist(q).await.map_err(classify)?)?;
                    }
                    Ok(())
                })
            }

            fn delete_playlist(&self, id: PlaylistId) -> BoxFut<'_, ()> {
                Box::pin(async move {
                    if mapping::is_system_playlist(id.as_str()) {
                        return Err(SourceError::NotEditable(id.to_string()));
                    }
                    // Mutation endpoint: bare form.
                    let raw_id = id.mutation_form();
                    self.api
                        .delete_playlist(ytmapi_rs::common::PlaylistID::from_raw(&raw_id))
                        .await
                        .map_err(classify)
                })
            }

            fn add_tracks(&self, id: PlaylistId, videos: Vec<VideoId>) -> BoxFut<'_, ()> {
                Box::pin(async move {
                    if mapping::is_system_playlist(id.as_str()) {
                        return Err(SourceError::NotEditable(id.to_string()));
                    }
                    let ids: Vec<_> = videos
                        .iter()
                        .map(|v| ytmapi_rs::common::VideoID::from_raw(v.as_str()))
                        .collect();
                    // Mutation endpoint: bare form.
                    let raw_id = id.mutation_form();
                    self.api
                        .add_video_items_to_playlist(
                            ytmapi_rs::common::PlaylistID::from_raw(&raw_id),
                            ids,
                        )
                        .await
                        .map_err(classify)?;
                    Ok(())
                })
            }

            /// Needs `SetVideoId`, not `VideoId` — see the doc comment on `SetVideoId`.
            fn remove_tracks(&self, id: PlaylistId, entries: Vec<SetVideoId>) -> BoxFut<'_, ()> {
                Box::pin(async move {
                    // Refuse before the API call: a system playlist can never be edited.
                    if mapping::is_system_playlist(id.as_str()) {
                        return Err(SourceError::NotEditable(id.to_string()));
                    }
                    let ids: Vec<_> = entries
                        .iter()
                        .map(|s| ytmapi_rs::common::SetVideoID::from_raw(s.as_str()))
                        .collect();
                    // Mutation endpoint: bare form.
                    let raw_id = id.mutation_form();
                    self.api
                        .remove_playlist_items(
                            ytmapi_rs::common::PlaylistID::from_raw(&raw_id),
                            ids,
                        )
                        .await
                        .map_err(classify)
                })
            }
        }
    };
}

impl_music_source!(BrowserToken);

/// Public browsing works with the visitor token; account methods deliberately
/// fail locally so callers get a clear error instead of probing private APIs.
impl MusicSource for YtMusicSource<NoAuthToken> {
    fn is_authenticated(&self) -> bool {
        false
    }

    fn library_playlists(&self) -> BoxFut<'_, Vec<Playlist>> {
        Box::pin(async { Err(SourceError::NotAuthenticated) })
    }
    fn library_songs(&self) -> BoxFut<'_, Vec<Track>> {
        Box::pin(async { Err(SourceError::NotAuthenticated) })
    }
    fn library_albums(&self) -> BoxFut<'_, Vec<Album>> {
        Box::pin(async { Err(SourceError::NotAuthenticated) })
    }
    fn library_artists(&self) -> BoxFut<'_, Vec<Artist>> {
        Box::pin(async { Err(SourceError::NotAuthenticated) })
    }
    fn home_shelves(&self) -> BoxFut<'_, Vec<HomeShelf>> {
        Box::pin(async { Err(SourceError::NotAuthenticated) })
    }
    fn recommended_albums(&self) -> BoxFut<'_, Vec<Album>> {
        Box::pin(async { Err(SourceError::NotAuthenticated) })
    }

    fn artist_tracks(&self, id: ArtistId) -> BoxFut<'_, Vec<Track>> {
        Box::pin(async move {
            let raw = self
                .api
                .get_artist(ytmapi_rs::common::ArtistChannelID::from_raw(id.as_str()))
                .await
                .map_err(classify)?;
            let Some(songs) = raw.top_releases.songs else {
                return Ok(Vec::new());
            };
            let preview = songs
                .results
                .iter()
                .map(mapping::track_from_artist_song)
                .collect();
            Ok(preview)
        })
    }

    fn album_tracks(&self, id: AlbumId) -> BoxFut<'_, Vec<Track>> {
        Box::pin(async move {
            // Album pages are public browses, like playlists and artists.
            let raw = self
                .api
                .get_album(ytmapi_rs::common::AlbumID::from_raw(id.as_str()))
                .await
                .map_err(classify)?;
            Ok(mapping::tracks_from_album(&raw))
        })
    }

    fn playlist_tracks(&self, id: PlaylistId) -> BoxFut<'_, Vec<Track>> {
        Box::pin(async move {
            let browse_id = id.browse_form();
            let query = ytmapi_rs::query::GetPlaylistTracksQuery::new(
                ytmapi_rs::common::PlaylistID::from_raw(&browse_id),
            );
            let json = self
                .api
                .raw_json_query::<ytmapi_rs::query::GetPlaylistTracksQuery>(&query)
                .await
                .map_err(classify)?;
            if let Some(error) = classify_raw_api_error(&json) {
                return Err(error);
            }
            let value =
                serde_json::from_str(&json).map_err(|e| SourceError::Parse(e.to_string()))?;
            let items: Vec<ytmapi_rs::parse::PlaylistItem> = ytmapi_rs::parse::ProcessedResult {
                query: &query,
                source: json.clone(),
                json: value,
            }
            .parse_into()
            .map_err(classify)?;
            let tracks = items
                .iter()
                .filter_map(mapping::track_from_playlist_item)
                .collect();
            let rows = crate::playlist_raw::entry_ids_from_raw(&json);
            Ok(crate::playlist_raw::attach_entry_ids(tracks, &rows))
        })
    }

    fn playlist_details(&self, id: PlaylistId) -> BoxFut<'_, Playlist> {
        Box::pin(async move {
            let browse_id = id.browse_form();
            let raw = self
                .api
                .get_playlist_details(ytmapi_rs::common::PlaylistID::from_raw(&browse_id))
                .await
                .map_err(classify)?;
            Ok(mapping::playlist_from_details(&raw))
        })
    }

    fn search_songs(&self, query: String) -> BoxFut<'_, Vec<Track>> {
        Box::pin(async move { song_search_from_raw(&self.api, &query).await })
    }
    fn search_albums(&self, query: String) -> BoxFut<'_, Vec<Album>> {
        Box::pin(async move {
            let raw = self
                .api
                .search_albums(query.as_str())
                .await
                .map_err(classify)?;
            Ok(raw.iter().map(mapping::album_from_search).collect())
        })
    }
    fn search_artists(&self, query: String) -> BoxFut<'_, Vec<Artist>> {
        Box::pin(async move {
            let raw = self
                .api
                .search_artists(query.as_str())
                .await
                .map_err(classify)?;
            Ok(raw.iter().map(mapping::artist_from_search).collect())
        })
    }
    fn search_playlists(&self, query: String) -> BoxFut<'_, Vec<Playlist>> {
        Box::pin(async move {
            let raw = self
                .api
                .search_playlists(query.as_str())
                .await
                .map_err(classify)?;
            Ok(raw
                .iter()
                .filter_map(|p| match p {
                    SearchResultPlaylist::Featured(f) => {
                        Some(mapping::playlist_from_search_featured(f))
                    }
                    SearchResultPlaylist::Community(c) => {
                        Some(mapping::playlist_from_search_community(c))
                    }
                    _ => None,
                })
                .collect())
        })
    }

    fn create_playlist(&self, _: String, _: Option<String>, _: Privacy) -> BoxFut<'_, PlaylistId> {
        Box::pin(async { Err(SourceError::NotAuthenticated) })
    }
    fn edit_playlist(
        &self,
        _: PlaylistId,
        _: Option<String>,
        _: Option<String>,
        _: Option<Privacy>,
    ) -> BoxFut<'_, ()> {
        Box::pin(async { Err(SourceError::NotAuthenticated) })
    }
    fn delete_playlist(&self, _: PlaylistId) -> BoxFut<'_, ()> {
        Box::pin(async { Err(SourceError::NotAuthenticated) })
    }
    fn add_tracks(&self, _: PlaylistId, _: Vec<VideoId>) -> BoxFut<'_, ()> {
        Box::pin(async { Err(SourceError::NotAuthenticated) })
    }
    fn remove_tracks(&self, _: PlaylistId, _: Vec<SetVideoId>) -> BoxFut<'_, ()> {
        Box::pin(async { Err(SourceError::NotAuthenticated) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracks(n: usize, art: bool) -> Vec<Track> {
        (0..n)
            .map(|i| Track {
                thumbnail_url: art.then(|| "https://example/a.jpg".to_owned()),
                duration: TrackDuration::from_secs(if art { 180 } else { 0 }),
                ..Track::stub(&format!("v{i}"), "T")
            })
            .collect()
    }

    #[tokio::test]
    #[ignore = "constructs the real upstream guest visitor token"]
    async fn guest_source_reports_not_authenticated() {
        let source = YtMusicSource::unauthenticated().await.unwrap();
        assert!(!source.is_authenticated());
    }

    #[tokio::test]
    #[ignore = "constructs the real upstream guest visitor token"]
    async fn guest_account_methods_return_not_authenticated() {
        let source = YtMusicSource::unauthenticated().await.unwrap();
        for error in [
            source.library_playlists().await.unwrap_err(),
            source.library_songs().await.unwrap_err(),
            source.library_albums().await.unwrap_err(),
            source.library_artists().await.unwrap_err(),
            source.home_shelves().await.unwrap_err(),
            source.recommended_albums().await.unwrap_err(),
            source
                .create_playlist("x".into(), None, Privacy::Private)
                .await
                .unwrap_err(),
            source
                .edit_playlist(PlaylistId::from("PLx"), None, None, None)
                .await
                .unwrap_err(),
            source
                .delete_playlist(PlaylistId::from("PLx"))
                .await
                .unwrap_err(),
            source
                .add_tracks(PlaylistId::from("PLx"), vec![])
                .await
                .unwrap_err(),
            source
                .remove_tracks(PlaylistId::from("PLx"), vec![])
                .await
                .unwrap_err(),
        ] {
            assert!(matches!(error, SourceError::NotAuthenticated));
        }
    }

    #[tokio::test]
    #[ignore = "live InnerTube guest-search gate; run by hand"]
    async fn guest_song_search_returns_tracks() {
        // Exercises the real guest path: search_songs now runs raw_json_query
        // through search_raw, not upstream's typed parser. "freak" is the query
        // whose UGC view-count rows used to abort the whole response.
        let source = YtMusicSource::unauthenticated()
            .await
            .expect("guest handshake succeeds");
        let tracks = source
            .search_songs("freak".into())
            .await
            .expect("guest search succeeds");
        assert!(!tracks.is_empty(), "guest search returned no tracks");
    }

    #[test]
    fn song_search_error_bodies_become_classified_source_errors() {
        let unauthorized =
            classify_raw_api_error("{\"error\":{\"code\":401,\"message\":\"expired\"}}");
        assert!(matches!(unauthorized, Some(SourceError::NotAuthenticated)));

        let rate_limited =
            classify_raw_api_error("{\"error\":{\"code\":429,\"message\":\"quota\"}}");
        assert!(matches!(rate_limited, Some(SourceError::RateLimited)));

        assert!(classify_raw_api_error("{\"contents\":{}}").is_none());
    }

    #[test]
    fn html_bot_check_bodies_become_rate_limited() {
        let html = "<html><head><title>Sorry...</title></head><body><h1>We're sorry...</h1><p>... but your computer or network may be sending automated queries.</p></body></html>";
        assert!(matches!(
            classify_raw_api_error(html),
            Some(SourceError::RateLimited)
        ));

        let query = ytmapi_rs::query::GetLibraryAlbumsQuery::default();
        let err =
            parse_json::<_, Vec<ytmapi_rs::parse::SearchResultAlbum>>(&query, html.to_string())
                .unwrap_err();
        assert!(matches!(err, SourceError::RateLimited));
    }

    #[test]
    fn duplicate_add_playlist_status_failed_becomes_already_in_playlist() {
        let json = r#"{"status": "STATUS_FAILED"}"#;
        let query = ytmapi_rs::query::playlist::AddPlaylistItemsQuery::new_from_videos(
            ytmapi_rs::common::PlaylistID::from_raw("PLtest"),
            vec![ytmapi_rs::common::VideoID::from_raw("test_video")],
            ytmapi_rs::query::playlist::DuplicateHandlingMode::default(),
        );
        let err = parse_json::<_, Vec<ytmapi_rs::parse::AddPlaylistItem>>(&query, json.to_string())
            .unwrap_err();
        assert!(
            matches!(err, SourceError::AlreadyInPlaylist),
            "expected AlreadyInPlaylist, got {err:?}"
        );
        assert_eq!(err.to_string(), "already in playlist");
    }

    #[test]
    fn the_typed_parser_the_bug_was_in_aborts_where_our_raw_parser_recovers() {
        // The scrubbed fixture is exactly the shape upstream could not parse, so
        // its typed search must still reject it — if this stops failing, the
        // fixture no longer reproduces the 2026-09-02 bug. Ours keeps 3 songs.
        let json = include_str!("../tests/fixtures/search_songs_ugc.json");
        let query = SearchQuery::<FilteredSearch<SongsFilter>>::from("freak");
        let typed =
            parse_json::<_, Vec<ytmapi_rs::parse::SearchResultSong>>(&query, json.to_owned());
        assert!(
            typed.is_err(),
            "upstream mistakes the UGC view-count byline for an album and aborts the response"
        );
        assert_eq!(
            crate::search_raw::tracks_from_raw(json).len(),
            3,
            "our raw parser keeps the usable songs the typed parser erased"
        );
    }

    #[test]
    fn the_full_playlist_replaces_the_five_row_preview() {
        // The owner saw only five songs per artist, each with no art and 0:00 —
        // that shelf is the web UI's preview above "Show all".
        let got = better_artist_tracks(tracks(5, false), Ok(tracks(40, true)));
        assert_eq!(got.len(), 40, "the full list must win");
        assert!(
            got[0].thumbnail_url.is_some(),
            "playlist entries carry the album art the shelf lacks"
        );
        assert_ne!(
            got[0].duration,
            TrackDuration::from_secs(0),
            "and a real duration"
        );
    }

    #[test]
    fn a_failed_follow_up_keeps_the_preview() {
        // Five playable rows beat an error for rows already on screen.
        let got = better_artist_tracks(tracks(5, false), Err(SourceError::RateLimited));
        assert_eq!(got.len(), 5);
    }

    #[test]
    fn an_empty_follow_up_keeps_the_preview() {
        // A playlist that parsed to nothing must not blank a shelf that had rows.
        let got = better_artist_tracks(tracks(5, false), Ok(Vec::new()));
        assert_eq!(got.len(), 5);
    }

    #[test]
    fn a_shorter_follow_up_keeps_the_preview() {
        // Fewer rows than the preview means the follow-up lost something; the
        // longer list is the safer answer.
        let got = better_artist_tracks(tracks(5, false), Ok(tracks(2, true)));
        assert_eq!(got.len(), 5);
    }

    #[test]
    fn an_artist_with_no_shelf_stays_empty() {
        assert!(better_artist_tracks(Vec::new(), Ok(Vec::new())).is_empty());
    }
}
