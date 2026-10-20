//! The play queue as two-line rows. The left gutter carries two meanings: the
//! multi-select bullet and the play position, with the bullet winning when a
//! row is marked. The shared layout lives in [`crate::widgets::row`].

use crate::{
    app::AppState,
    theme::Theme,
    widgets::{art, row},
};
use ratatui::{Frame, layout::Rect};

pub fn draw(f: &mut Frame, area: Rect, s: &AppState, t: &Theme, art: &mut art::ArtCache) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    // The filtered view, not `s.queue`: the reducer counts and indexes this, so
    // drawing the raw queue made `/` look broken here and put the cursor on a
    // different row than `x` or Enter acted on.
    let rows = s.visible_tracks();
    row::draw_list(f, area, s, t, art, rows.len(), |idx| {
        let Some(track) = rows.get(idx) else {
            return row::Row::default();
        };
        let is_sel = idx == s.selected;
        // Unfiltered, the visible index *is* the queue index, and it is the
        // only way to tell two copies of one song apart. Under a filter the
        // indices differ, so match on id and accept both rows highlighting.
        let is_current = if s.is_filtering() {
            s.queue_current
                .and_then(|c| s.queue.get(c))
                .is_some_and(|c| c.video_id == track.video_id)
        } else {
            s.queue_current == Some(idx)
        };
        let marked = s.marked.contains(&track.video_id);

        // The gutter shows one glyph and the mark wins it, so a `V` range reads
        // as a contiguous run of bullets. Play position survives in the accent
        // title, and the play glyph returns when the mark clears.
        let marker = if marked {
            "\u{2022} ".to_owned()
        } else if is_current {
            "\u{25b6} ".to_owned()
        } else {
            String::new()
        };
        let subtitle = match track.album.as_deref().filter(|a| !a.is_empty()) {
            Some(album) => format!("{} \u{2022} {album}", track.artist_display()),
            None => track.artist_display(),
        };

        row::Row {
            title: track.title.clone(),
            subtitle,
            right: track.duration.to_string(),
            thumbnail: track.thumbnail_url.clone(),
            marker,
            accent: is_current,
            marked: is_sel || marked,
            heading: false,
        }
    });
}

#[cfg(test)]
mod tests {
    use crate::{
        app::{AppState, Pane},
        theme::Theme,
    };
    use ratatui::{Terminal, backend::TestBackend};
    use ytm_core::Track;

    /// Render through the real entry point, so these cover what the user sees.
    fn text_of(s: &AppState) -> String {
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
        t.backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    fn two_entries() -> AppState {
        AppState {
            pane: Pane::Queue,
            queue: vec![Track::stub("v1", "First"), Track::stub("v2", "Second")],
            ..Default::default()
        }
    }

    #[test]
    fn queue_lists_its_tracks_in_order() {
        let text = text_of(&two_entries());
        assert!(text.contains("First"));
        assert!(text.contains("Second"));
        assert!(
            text.find("First") < text.find("Second"),
            "order must be preserved"
        );
    }

    #[test]
    fn queue_rows_start_with_a_cover_gutter() {
        assert!(
            text_of(&two_entries()).contains('\u{266a}'),
            "every queue row needs the cover gutter"
        );
    }

    #[test]
    fn queue_rows_put_artist_album_and_duration_on_the_second_line() {
        use ytm_core::TrackDuration;
        let s = AppState {
            pane: Pane::Queue,
            queue: vec![Track {
                title: "Roygbiv".into(),
                artists: vec!["Boards of Canada".into()],
                album: Some("Geogaddi".into()),
                duration: TrackDuration::from_secs(149),
                ..Track::stub("v1", "Roygbiv")
            }],
            ..Default::default()
        };
        let text = text_of(&s);
        assert!(
            text.contains("Boards of Canada \u{2022} Geogaddi"),
            "artist and album share the second line, got: {text}"
        );
        assert!(text.contains("2:29"));
    }

    #[test]
    fn the_current_queue_entry_is_marked() {
        let s = AppState {
            queue_current: Some(1),
            ..two_entries()
        };
        // FR-Q1: the user must be able to tell where they are.
        assert!(
            text_of(&s).contains("\u{25b6}"),
            "current entry needs a marker"
        );
    }

    #[test]
    fn a_marked_queue_entry_shows_the_multiselect_bullet() {
        // FR-C4/FR-Q3: `v`/`V` marks must be visible here as in the track list,
        // or the user cannot see what they selected before moving or removing it.
        // The state side already marked queue rows; only the widget missed them.
        let mut s = two_entries();
        s.marked.insert(ytm_core::VideoId::from("v2"));
        assert!(
            text_of(&s).contains('\u{2022}'),
            "a marked queue row needs the bullet"
        );
    }

    #[test]
    fn an_unmarked_queue_shows_no_bullet() {
        // Guards the test above: drawing the bullet on every row would pass it
        // while telling the user nothing.
        assert!(!text_of(&two_entries()).contains('\u{2022}'));
    }

    #[test]
    fn an_entry_that_is_not_current_gets_no_marker() {
        // Without this, printing the marker on every row would pass the test
        // above while telling the user nothing.
        let s = AppState {
            queue_current: None,
            ..two_entries()
        };
        assert!(!text_of(&s).contains("\u{25b6}"));
    }

    #[test]
    fn an_empty_queue_explains_itself() {
        let s = AppState {
            pane: Pane::Queue,
            ..Default::default()
        };
        assert!(text_of(&s).contains("Queue is empty"));
    }

    #[test]
    fn marked_queue_rows_render_with_highlighted_background() {
        let mut state = AppState {
            pane: Pane::Queue,
            queue: vec![Track::stub("v1", "First"), Track::stub("v2", "Second")],
            selected: 1, // cursor on row 1, row 0 is only marked
            ..Default::default()
        };
        state.marked.insert(ytm_core::VideoId::from("v1"));
        let theme = Theme::preset("tokyonight").unwrap();

        let backend = ratatui::backend::TestBackend::new(80, 10);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                super::draw(
                    f,
                    f.area(),
                    &state,
                    &theme,
                    &mut crate::widgets::art::ArtCache::disabled(),
                )
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        let rendered: String = buffer.content().iter().map(|c| c.symbol()).collect();
        assert!(rendered.contains('•'));
        let cell = buffer.cell((0, 0)).unwrap();
        assert_eq!(cell.bg, theme.bg_sel);
        assert_eq!(
            buffer.cell((0, 1)).unwrap().bg,
            theme.bg_sel,
            "the marked background covers the row's second line too"
        );
    }

    #[test]
    fn queue_render_survives_out_of_bounds_selected() {
        let state = AppState {
            pane: Pane::Queue,
            queue: (0..48)
                .map(|i| Track::stub(&format!("t{i}"), &format!("Track {i}")))
                .collect(),
            selected: 68,
            scroll_offset: 40,
            ..Default::default()
        };
        let theme = Theme::default();

        let backend = ratatui::backend::TestBackend::new(80, 20);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                super::draw(
                    f,
                    f.area(),
                    &state,
                    &theme,
                    &mut crate::widgets::art::ArtCache::disabled(),
                )
            })
            .unwrap();
    }
}
