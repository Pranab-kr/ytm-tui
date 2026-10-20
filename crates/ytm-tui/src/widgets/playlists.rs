//! Home, playlist, album, and artist rows. They all draw through the shared
//! two-line [`row`] renderer: a cover gutter, the title, then a detail line with
//! the count/year/subscribers on the right. Keeping them on the same renderer is
//! what stops the columns drifting between panes.

use crate::{
    app::AppState,
    theme::Theme,
    widgets::{art, row},
};
use ratatui::{Frame, layout::Rect};

/// Shown on playlists YouTube will not let us edit (FR-C).
const READONLY: &str = "read-only";

/// The home feed (FR-B6): shelf headings interleaved with their cards. Headings
/// scroll rather than stick, since a carousel's title only means something next to
/// its own cards. Each card carries a kind tag — one shelf mixes all four.
///
/// Headings are drawn by the shared renderer too, so they occupy the same two
/// lines as an item and navigation/click math stays in item units.
pub fn draw_home(f: &mut Frame, area: Rect, s: &AppState, t: &Theme, art: &mut art::ArtCache) {
    use crate::app::HomeRow;
    use ytm_core::HomeTarget;

    row::draw_list(f, area, s, t, art, s.home_rows.len(), |idx| {
        let Some(home_row) = s.home_rows.get(idx) else {
            return row::Row::default();
        };
        match home_row {
            HomeRow::Heading(title) => row::Row {
                title: title.clone(),
                heading: true,
                ..row::Row::default()
            },
            HomeRow::Item(i) => {
                let is_sel = idx == s.selected;
                // Only tracks can be multi-selected; a library card has no
                // video id to mark.
                let marked = match &i.target {
                    HomeTarget::Track(v) => s.marked.contains(v),
                    _ => false,
                };
                row::Row {
                    title: i.title.clone(),
                    subtitle: i.subtitle.clone(),
                    right: i.kind_label().to_owned(),
                    thumbnail: i.thumbnail_url.clone(),
                    marker: if marked {
                        "\u{2022} ".to_owned()
                    } else {
                        String::new()
                    },
                    accent: false,
                    marked: is_sel || marked,
                    heading: false,
                }
            }
        }
    });
}

pub fn draw_playlists(f: &mut Frame, area: Rect, s: &AppState, t: &Theme, art: &mut art::ArtCache) {
    // The filtered view, not `s.playlists`: the reducer counts and indexes this
    // list, so drawing the raw one makes `/` appear to do nothing and puts the
    // cursor on a different row than Enter acts on.
    let rows = s.visible_playlists();
    row::draw_list(f, area, s, t, art, rows.len(), |idx| {
        let Some(p) = rows.get(idx) else {
            return row::Row::default();
        };
        let count = match p.track_count {
            Some(n) => format!("{n} tracks"),
            None => "?".to_owned(),
        };
        row::Row {
            title: p.title.clone(),
            subtitle: format!("Playlist \u{2022} {count}"),
            right: if p.is_system {
                READONLY.to_owned()
            } else {
                String::new()
            },
            thumbnail: p.thumbnail_url.clone(),
            marked: idx == s.selected,
            ..row::Row::default()
        }
    });
}

pub fn draw_albums(f: &mut Frame, area: Rect, s: &AppState, t: &Theme, art: &mut art::ArtCache) {
    let rows = s.visible_albums();
    row::draw_list(f, area, s, t, art, rows.len(), |idx| {
        let Some(a) = rows.get(idx) else {
            return row::Row::default();
        };
        let artists = if a.artists.is_empty() {
            "Unknown artist".to_owned()
        } else {
            a.artists.join(", ")
        };
        row::Row {
            title: a.title.clone(),
            subtitle: artists,
            right: a.year.clone().unwrap_or_else(|| "\u{2014}".to_owned()),
            thumbnail: a.thumbnail_url.clone(),
            marked: idx == s.selected,
            ..row::Row::default()
        }
    });
}

pub fn draw_artists(f: &mut Frame, area: Rect, s: &AppState, t: &Theme, art: &mut art::ArtCache) {
    let rows = s.visible_artists();
    row::draw_list(f, area, s, t, art, rows.len(), |idx| {
        let Some(a) = rows.get(idx) else {
            return row::Row::default();
        };
        row::Row {
            title: a.name.clone(),
            subtitle: "Artist".to_owned(),
            right: a.subscribers.clone().unwrap_or_default(),
            thumbnail: a.thumbnail_url.clone(),
            marked: idx == s.selected,
            ..row::Row::default()
        }
    });
}

#[cfg(test)]
mod tests {
    use crate::{
        app::{AppState, Pane},
        theme::Theme,
    };
    use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};
    use ytm_core::{Album, AlbumId, Artist, ArtistId, Playlist};

    /// Render the whole app so the tests cover what the user sees.
    fn buffer_of(s: &AppState) -> Buffer {
        let mut t = Terminal::new(TestBackend::new(80, 20)).unwrap();
        let theme = Theme::default();
        t.draw(|f| {
            crate::render::render(
                f,
                s,
                &theme,
                &crate::keymap::KeyMap::default(),
                &mut crate::widgets::art::ArtCache::disabled(),
            )
        })
        .unwrap();
        t.backend().buffer().clone()
    }

    fn text_of(s: &AppState) -> String {
        buffer_of(s).content().iter().map(|c| c.symbol()).collect()
    }

    /// One screen line as a string, for column assertions.
    fn line_of(buf: &Buffer, y: u16) -> String {
        (0..buf.area.width)
            .map(|x| buf.cell((x, y)).unwrap().symbol())
            .collect()
    }

    /// The screen row where `render` starts the list: one below the pane heading.
    fn list_top() -> u16 {
        crate::render::list_top_for(false, false, false)
    }

    /// The screen column where the main pane starts, past the sidebar and rule.
    fn list_left() -> u16 {
        crate::render::SIDEBAR_WIDTH + 1
    }

    /// The visible text of one screen line from the start of the main pane.
    fn pane_line(buf: &Buffer, y: u16) -> String {
        line_of(buf, y)
            .chars()
            .skip(usize::from(list_left()))
            .collect()
    }

    #[test]
    fn playlists_show_title_and_track_count() {
        let s = AppState {
            pane: Pane::Playlists,
            playlists: vec![Playlist {
                track_count: Some(42),
                ..Playlist::stub("p1", "Deep Focus")
            }],
            ..Default::default()
        };
        let text = text_of(&s);
        assert!(text.contains("Deep Focus"));
        assert!(text.contains("42 tracks"), "track count missing");
        assert!(
            text.contains("Playlist \u{2022} 42 tracks"),
            "the count shares the second line with the row kind, got: {text}"
        );
    }

    #[test]
    fn a_playlist_with_unknown_count_renders_without_panicking() {
        let s = AppState {
            pane: Pane::Playlists,
            playlists: vec![Playlist {
                track_count: None,
                ..Playlist::stub("p1", "Mystery")
            }],
            ..Default::default()
        };
        let text = text_of(&s);
        assert!(text.contains("Mystery"));
        assert!(
            text.contains('?'),
            "an unknown count needs a visible placeholder, got: {text}"
        );
    }

    #[test]
    fn albums_show_title_artist_and_year() {
        let s = AppState {
            pane: Pane::Albums,
            albums: vec![Album {
                id: AlbumId::from("a1"),
                title: "Geogaddi".into(),
                artists: vec!["Boards of Canada".into()],
                year: Some("2002".into()),
                thumbnail_url: None,
            }],
            ..Default::default()
        };
        let text = text_of(&s);
        assert!(text.contains("Geogaddi"));
        assert!(text.contains("Boards of Canada"));
        assert!(text.contains("2002"), "album year missing");
    }

    #[test]
    fn artists_show_names_and_subscribers() {
        let s = AppState {
            pane: Pane::Artists,
            artists: vec![Artist {
                id: ArtistId::from("r1"),
                name: "Aphex Twin".into(),
                subscribers: Some("1.2M".into()),
                thumbnail_url: None,
            }],
            ..Default::default()
        };
        let text = text_of(&s);
        assert!(text.contains("Aphex Twin"));
        assert!(text.contains("1.2M"), "artist subscribers missing");
    }

    #[test]
    fn a_system_playlist_is_visually_marked_as_read_only() {
        let s = AppState {
            pane: Pane::Playlists,
            playlists: vec![Playlist {
                is_system: true,
                ..Playlist::stub("LM", "Your Likes")
            }],
            ..Default::default()
        };
        let text = text_of(&s);
        assert!(text.contains("Your Likes"));
        // The plan's version asserted only the title, which every other test
        // already covers — it could pass with no marker at all. FR-C: the user
        // must be able to see this one cannot be edited.
        assert!(
            text.contains("read-only"),
            "a system playlist needs a visible read-only marker"
        );
    }

    #[test]
    fn an_editable_playlist_is_not_marked_read_only() {
        // Guards the inverse, or "read-only" could be printed unconditionally.
        let s = AppState {
            pane: Pane::Playlists,
            playlists: vec![Playlist::stub("p1", "Deep Focus")],
            ..Default::default()
        };
        assert!(!text_of(&s).contains("read-only"));
    }

    #[test]
    fn empty_panes_say_something_rather_than_going_blank() {
        // Each pane names its own empty state now (via AppState::empty_message),
        // so assert on a word from the pane's specific hint rather than a shared
        // "Nothing here".
        for (pane, needle) in [
            (Pane::Playlists, "No playlists"),
            (Pane::Albums, "No saved albums"),
            (Pane::Artists, "No followed artists"),
        ] {
            let s = AppState {
                pane,
                ..Default::default()
            };
            assert!(
                text_of(&s).contains(needle),
                "empty {pane:?} pane should show {needle:?}"
            );
        }
    }

    #[test]
    fn home_headings_start_at_the_left_edge_while_items_get_the_cover_gutter() {
        use crate::app::HomeRow;
        use ytm_core::{HomeItem, HomeTarget, VideoId};

        let s = AppState {
            pane: Pane::Home,
            home_rows: vec![
                HomeRow::Heading("Quick picks".into()),
                HomeRow::Item(HomeItem {
                    title: "Song 1".into(),
                    subtitle: "Artist 1".into(),
                    target: HomeTarget::Track(VideoId::from("v1")),
                    thumbnail_url: None,
                }),
            ],
            selected: 1,
            ..Default::default()
        };
        let buf = buffer_of(&s);
        let top = list_top();
        let left = list_left();

        assert_eq!(
            buf.cell((left, top)).unwrap().symbol(),
            "Q",
            "a heading has no cover gutter: it starts at the pane's left edge"
        );
        assert!(
            pane_line(&buf, top).starts_with("Quick picks"),
            "heading text must not be pushed right, got {:?}",
            pane_line(&buf, top)
        );
        assert_eq!(
            buf.cell((left, top + 2)).unwrap().symbol(),
            "\u{266a}",
            "an item below the heading begins with the cover gutter"
        );
        assert!(
            pane_line(&buf, top + 2).contains("Song 1"),
            "the item's title follows the cover and mark gutters"
        );
    }

    #[test]
    fn a_home_heading_consumes_two_lines_so_items_sit_on_even_offsets() {
        use crate::app::HomeRow;
        use ytm_core::{HomeItem, HomeTarget, VideoId};

        let item = |title: &str, id: &str| {
            HomeRow::Item(HomeItem {
                title: title.into(),
                subtitle: "Artist".into(),
                target: HomeTarget::Track(VideoId::from(id)),
                thumbnail_url: None,
            })
        };
        let s = AppState {
            pane: Pane::Home,
            home_rows: vec![
                HomeRow::Heading("Quick picks".into()),
                item("Song 1", "v1"),
                item("Song 2", "v2"),
            ],
            selected: 1,
            ..Default::default()
        };
        let buf = buffer_of(&s);
        let top = list_top();
        let find_line = |needle: &str| {
            (0..buf.area.height)
                .find(|&y| line_of(&buf, y).contains(needle))
                .unwrap_or_else(|| panic!("{needle:?} is not on screen"))
        };

        assert_eq!(find_line("Quick picks"), top, "the heading starts the list");
        assert_eq!(
            find_line("Song 1"),
            top + 2,
            "the item after a heading starts two rows below it"
        );
        assert_eq!(
            find_line("Song 2"),
            top + 4,
            "every home row is exactly two lines tall"
        );
    }

    #[test]
    fn playlist_rows_start_with_a_cover_gutter_and_put_the_count_on_line_two() {
        let s = AppState {
            pane: Pane::Playlists,
            playlists: vec![Playlist {
                track_count: Some(42),
                ..Playlist::stub("p1", "Deep Focus")
            }],
            ..Default::default()
        };
        let buf = buffer_of(&s);
        let top = list_top();
        let left = list_left();

        assert_eq!(
            buf.cell((left, top)).unwrap().symbol(),
            "\u{266a}",
            "playlist rows need the cover gutter"
        );
        assert!(
            line_of(&buf, top).contains("Deep Focus"),
            "the title stays on the first line"
        );
        let second = line_of(&buf, top + 1);
        assert!(
            second.contains("Playlist \u{2022} 42 tracks"),
            "the count shares the second line, got {second:?}"
        );
    }

    #[test]
    fn album_rows_start_with_a_cover_gutter_and_put_metadata_on_line_two() {
        let s = AppState {
            pane: Pane::Albums,
            albums: vec![Album {
                id: AlbumId::from("a1"),
                title: "Geogaddi".into(),
                artists: vec!["Boards of Canada".into()],
                year: Some("2002".into()),
                thumbnail_url: None,
            }],
            ..Default::default()
        };
        let buf = buffer_of(&s);
        let top = list_top();
        let left = list_left();

        assert_eq!(
            buf.cell((left, top)).unwrap().symbol(),
            "\u{266a}",
            "album rows need the cover gutter"
        );
        assert!(
            line_of(&buf, top).contains("Geogaddi"),
            "the title stays on the first line"
        );
        let second = line_of(&buf, top + 1);
        assert!(
            second.contains("Boards of Canada"),
            "the artist belongs on the second line, got {second:?}"
        );
        assert!(
            second.contains("2002"),
            "the year belongs on the second line, got {second:?}"
        );
    }

    #[test]
    fn artist_rows_start_with_a_cover_gutter_and_put_metadata_on_line_two() {
        let s = AppState {
            pane: Pane::Artists,
            artists: vec![Artist {
                id: ArtistId::from("r1"),
                name: "Aphex Twin".into(),
                subscribers: Some("1.2M".into()),
                thumbnail_url: None,
            }],
            ..Default::default()
        };
        let buf = buffer_of(&s);
        let top = list_top();
        let left = list_left();

        assert_eq!(
            buf.cell((left, top)).unwrap().symbol(),
            "\u{266a}",
            "artist rows need the cover gutter"
        );
        assert!(
            line_of(&buf, top).contains("Aphex Twin"),
            "the name stays on the first line"
        );
        let second = line_of(&buf, top + 1);
        assert!(
            second.contains("Artist"),
            "the row kind belongs on the second line, got {second:?}"
        );
        assert!(
            second.contains("1.2M"),
            "the subscriber count belongs on the second line, got {second:?}"
        );
    }

    #[test]
    fn marked_home_tracks_render_with_bullet_and_highlighted_background() {
        use crate::app::HomeRow;
        use ytm_core::{HomeItem, HomeTarget, VideoId};

        let mut s = AppState {
            pane: Pane::Home,
            home_rows: vec![
                HomeRow::Heading("Quick picks".into()),
                HomeRow::Item(HomeItem {
                    title: "Song 1".into(),
                    subtitle: "Artist 1".into(),
                    target: HomeTarget::Track(VideoId::from("v1")),
                    thumbnail_url: None,
                }),
                HomeRow::Item(HomeItem {
                    title: "Song 2".into(),
                    subtitle: "Artist 2".into(),
                    target: HomeTarget::Track(VideoId::from("v2")),
                    thumbnail_url: None,
                }),
            ],
            selected: 2, // cursor on row 2 (Song 2), row 1 (Song 1) is marked
            ..Default::default()
        };
        s.marked.insert(VideoId::from("v1"));
        let theme = Theme::preset("tokyonight").unwrap();

        let backend = ratatui::backend::TestBackend::new(80, 10);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                super::draw_home(
                    f,
                    f.area(),
                    &s,
                    &theme,
                    &mut crate::widgets::art::ArtCache::disabled(),
                )
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        let rendered: String = buffer.content().iter().map(|c| c.symbol()).collect();
        assert!(rendered.contains('•'), "marked home track needs a bullet");
        // The heading occupies lines 0-1, so the marked Song 1 is on lines 2-3.
        assert_eq!(
            buffer.cell((5, 2)).unwrap().symbol(),
            "\u{2022}",
            "the bullet sits in the mark gutter after the cover"
        );
        assert_eq!(
            buffer.cell((0, 2)).unwrap().bg,
            theme.bg_sel,
            "marked home track needs background highlight"
        );
        assert_eq!(
            buffer.cell((0, 3)).unwrap().bg,
            theme.bg_sel,
            "the highlight covers the marked row's second line too"
        );
    }
}
