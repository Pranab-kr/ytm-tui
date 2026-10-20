//! Tracks as two-line rows: cover, title, then artist/album and duration.
//! The shared layout lives in [`crate::widgets::row`], so this widget only
//! decides what each row says.

use crate::{
    app::AppState,
    theme::Theme,
    widgets::{art, row},
};
use ratatui::{Frame, layout::Rect};

/// The slice of rows to draw, keeping `selected` visible. Returns a half-open
/// range; `offset` is the previous scroll position, used as a starting guess so
/// the list does not jump while the selection stays in the viewport.
pub fn visible_window(selected: usize, offset: usize, height: usize, len: usize) -> (usize, usize) {
    if len == 0 || height == 0 {
        return (0, 0);
    }
    let selected = selected.min(len.saturating_sub(1));
    let mut start = offset.min(len.saturating_sub(1));
    if selected < start {
        start = selected;
    }
    if selected >= start + height {
        start = selected + 1 - height;
    }
    let end = (start + height).min(len);
    (start.min(end), end)
}

pub fn draw(f: &mut Frame, area: Rect, s: &AppState, t: &Theme, art: &mut art::ArtCache) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let rows = s.visible_tracks();
    row::draw_list(f, area, s, t, art, rows.len(), |idx| {
        let Some(track) = rows.get(idx) else {
            return row::Row::default();
        };
        let is_sel = idx == s.selected;
        let is_now = s
            .now_playing
            .as_ref()
            .is_some_and(|n| n.video_id == track.video_id);
        let marked = s.marked.contains(&track.video_id);
        let subtitle = match track.album.as_deref().filter(|a| !a.is_empty()) {
            Some(album) => format!("{} \u{2022} {album}", track.artist_display()),
            None => track.artist_display(),
        };

        row::Row {
            title: track.title.clone(),
            subtitle,
            right: track.duration.to_string(),
            thumbnail: track.thumbnail_url.clone(),
            marker: if marked {
                "\u{2022} ".to_owned()
            } else {
                String::new()
            },
            accent: is_now,
            // `draw_list` paints both lines for a marked row; the closure also
            // includes selection so a selected unmarked row is highlighted.
            marked: is_sel || marked,
            heading: false,
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Render through the real entry point so the test covers the layout the
    /// user actually sees, not just this widget in isolation.
    fn buffer_text(s: &crate::app::AppState) -> String {
        use ratatui::{Terminal, backend::TestBackend};
        let mut t = Terminal::new(TestBackend::new(80, 20)).unwrap();
        let theme = crate::theme::Theme::default();
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

    #[test]
    fn window_shows_the_top_when_selection_is_near_the_start() {
        assert_eq!(visible_window(0, 0, 10, 100), (0, 10));
        assert_eq!(visible_window(5, 0, 10, 100), (0, 10));
    }

    #[test]
    fn window_scrolls_when_the_selection_passes_the_bottom() {
        // selection 12 with a 10-row viewport must bring row 12 into view
        let (start, end) = visible_window(12, 0, 10, 100);
        assert!(
            start <= 12 && 12 < end,
            "selection must be visible, got {start}..{end}"
        );
    }

    #[test]
    fn window_never_exceeds_the_item_count() {
        let (start, end) = visible_window(2, 0, 10, 3);
        assert_eq!((start, end), (0, 3));
    }

    #[test]
    fn window_is_empty_for_an_empty_list() {
        assert_eq!(visible_window(0, 0, 10, 0), (0, 0));
    }

    #[test]
    fn window_handles_a_zero_height_viewport() {
        assert_eq!(visible_window(0, 0, 0, 50), (0, 0));
    }

    #[test]
    fn window_handles_out_of_bounds_selection() {
        let (start, end) = visible_window(68, 40, 20, 48);
        assert!(start <= end, "start {start} must be <= end {end}");
        assert!(end <= 48, "end {end} must be <= len 48");

        let (start, end) = visible_window(49, 47, 1, 48);
        assert!(start <= end, "start {start} must be <= end {end}");
        assert!(end <= 48, "end {end} must be <= len 48");
    }

    #[test]
    fn rows_show_title_artist_and_duration() {
        use crate::app::{AppState, Pane};
        use ytm_core::{Track, TrackDuration};

        let s = AppState {
            pane: Pane::Songs,
            tracks: vec![Track {
                title: "Roygbiv".into(),
                artists: vec!["Boards of Canada".into()],
                duration: TrackDuration::from_secs(149),
                ..Track::stub("v1", "Roygbiv")
            }],
            ..Default::default()
        };

        let text = buffer_text(&s);
        assert!(text.contains("Roygbiv"));
        assert!(text.contains("Boards of Canada"));
        assert!(text.contains("2:29"));
    }

    #[test]
    fn the_second_line_holds_artist_album_and_duration() {
        use crate::app::{AppState, Pane};
        use ytm_core::{Track, TrackDuration};

        let s = AppState {
            pane: Pane::Songs,
            tracks: vec![Track {
                title: "Roygbiv".into(),
                artists: vec!["Boards of Canada".into()],
                album: Some("Geogaddi".into()),
                duration: TrackDuration::from_secs(149),
                ..Track::stub("v1", "Roygbiv")
            }],
            ..Default::default()
        };

        let text = buffer_text(&s);
        assert!(
            text.contains("Boards of Canada \u{2022} Geogaddi"),
            "artist and album share the second line, got: {text}"
        );
        assert!(text.contains("2:29"), "duration is beside the artist");
    }

    #[test]
    fn track_rows_start_with_a_cover_gutter() {
        use crate::app::{AppState, Pane};
        use ytm_core::Track;

        let s = AppState {
            pane: Pane::Songs,
            tracks: vec![Track::stub("v1", "Roygbiv")],
            ..Default::default()
        };
        assert!(
            buffer_text(&s).contains('\u{266a}'),
            "a missing cover must keep the gutter visible"
        );
    }

    #[test]
    fn the_two_line_row_replaced_the_column_header() {
        use crate::app::{AppState, Pane};
        use ytm_core::Track;
        let s = AppState {
            pane: Pane::Songs,
            tracks: vec![Track::stub("v1", "Roygbiv")],
            ..Default::default()
        };
        let text = buffer_text(&s);
        assert!(!text.contains("Title"), "no separate header labels remain");
        assert!(!text.contains("Time"));
        // The row still renders.
        assert!(text.contains("Roygbiv"));
    }

    #[test]
    fn an_empty_pane_shows_a_message_not_a_blank_area() {
        use crate::app::{AppState, Pane};
        let s = AppState {
            pane: Pane::Songs,
            ..Default::default()
        };
        // The Fav pane names its own empty state rather than a flat "Nothing
        // here yet" — the message comes from `AppState::empty_message`.
        assert!(
            buffer_text(&s).contains("No liked songs"),
            "empty states must say something specific to the pane"
        );
    }

    #[test]
    fn marked_rows_render_with_highlighted_background() {
        use crate::app::{AppState, Pane};
        use crate::theme::Theme;
        use ytm_core::{Track, VideoId};

        let mut state = AppState {
            pane: Pane::Songs,
            tracks: vec![Track::stub("t1", "Song 1"), Track::stub("t2", "Song 2")],
            selected: 1, // cursor on row 1, row 0 is only marked
            ..Default::default()
        };
        state.marked.insert(VideoId::from("t1"));
        let theme = Theme::preset("tokyonight").unwrap();

        let backend = ratatui::backend::TestBackend::new(80, 10);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw(
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
}
