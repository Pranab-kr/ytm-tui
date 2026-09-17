//! One row per track: mark, title, artist, duration. Columns are sized by
//! display width so CJK titles keep the grid intact (spec §6).

use crate::{app::AppState, theme::Theme, util::text::pad_to_width};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, Paragraph},
};

/// Right-hand duration column, wide enough for `1:02:03`.
const DURATION_WIDTH: usize = 7;
/// The multi-select bullet plus its trailing space.
const MARK_WIDTH: usize = 2;
/// Title takes six tenths of what is left; the artist gets the rest.
const TITLE_SHARE: usize = 6;

/// The slice of rows to draw, keeping `selected` visible. Returns a half-open
/// range; `offset` is the previous scroll position, used as a starting guess so
/// the list does not jump while the selection stays in the viewport.
pub fn visible_window(selected: usize, offset: usize, height: usize, len: usize) -> (usize, usize) {
    if len == 0 || height == 0 {
        return (0, 0);
    }
    let mut start = offset.min(len.saturating_sub(1));
    if selected < start {
        start = selected;
    }
    if selected >= start + height {
        start = selected + 1 - height;
    }
    let end = (start + height).min(len);
    (start, end)
}

/// A dim `Title / Artist / Time` header, aligned to the same columns the rows
/// use so it labels the grid rather than floating over it. The widths come from
/// the same constants `draw` uses — a second copy drifts off its columns.
pub fn draw_column_header(f: &mut Frame, area: Rect, t: &Theme) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let w = area.width as usize;
    let text_w = w.saturating_sub(DURATION_WIDTH + MARK_WIDTH);
    let title_w = (text_w * TITLE_SHARE) / 10;
    let artist_w = text_w.saturating_sub(title_w);

    let style = Style::default().fg(t.fg_dim).add_modifier(Modifier::BOLD);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            // The mark gutter is empty in the header; the labels start at the
            // title column so they line up with the row text below.
            Span::styled(" ".repeat(MARK_WIDTH), style),
            Span::styled(pad_to_width("Title", title_w), style),
            Span::styled(pad_to_width("Artist", artist_w), style),
            Span::styled(format!("{:>DURATION_WIDTH$}", "Time"), style),
        ])),
        area,
    );
}

pub fn draw(f: &mut Frame, area: Rect, s: &AppState, t: &Theme) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    // `visible_tracks` is the single source of truth for which rows are on screen:
    // it picks the right list for the pane and applies the filter. Reading raw
    // state drew library songs under an open artist and scrolling stopped early.
    let rows: Vec<&ytm_core::Track> = s.visible_tracks();

    if rows.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(
                s.empty_message(),
                Style::default().fg(t.fg_dim),
            )),
            area,
        );
        return;
    }

    let h = area.height as usize;
    let (start, end) = visible_window(s.selected, s.scroll_offset, h, rows.len());
    let w = area.width as usize;

    // Columns in display cells, not bytes: mark + title + artist + duration.
    let text_w = w.saturating_sub(DURATION_WIDTH + MARK_WIDTH);
    let title_w = (text_w * TITLE_SHARE) / 10;
    let artist_w = text_w.saturating_sub(title_w);

    let items: Vec<ListItem> = rows[start..end]
        .iter()
        .enumerate()
        .map(|(i, track)| {
            let idx = start + i;
            let is_sel = idx == s.selected;
            let is_now = s
                .now_playing
                .as_ref()
                .is_some_and(|n| n.video_id == track.video_id);
            let marked = s.marked.contains(&track.video_id);

            let base = if is_now {
                Style::default().fg(t.accent).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(t.fg)
            };
            // Selection and marked rows receive a reversed background tint (spec §6, FR-FIX2).
            let style = if is_sel || marked {
                base.bg(t.bg_sel)
            } else {
                base
            };
            let dim = if is_sel || marked {
                style
            } else {
                Style::default().fg(t.fg_dim)
            };

            ListItem::new(Line::from(vec![
                Span::styled(
                    if marked { "\u{2022} " } else { "  " },
                    if is_sel || marked {
                        Style::default().fg(t.accent).bg(t.bg_sel)
                    } else {
                        Style::default().fg(t.accent)
                    },
                ),
                Span::styled(pad_to_width(&track.title, title_w), style),
                Span::styled(pad_to_width(&track.artist_display(), artist_w), dim),
                Span::styled(
                    format!(
                        "{:>width$}",
                        track.duration.to_string(),
                        width = DURATION_WIDTH
                    ),
                    dim,
                ),
            ]))
            .style(if is_sel || marked {
                Style::default().bg(t.bg_sel)
            } else {
                Style::default()
            })
        })
        .collect();

    f.render_widget(List::new(items), area);
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
    fn a_populated_track_pane_draws_a_column_header() {
        use crate::app::{AppState, Pane};
        use ytm_core::Track;
        let s = AppState {
            pane: Pane::Songs,
            tracks: vec![Track::stub("v1", "Roygbiv")],
            ..Default::default()
        };
        let text = buffer_text(&s);
        assert!(text.contains("Title"), "header labels the title column");
        assert!(text.contains("Artist"), "header labels the artist column");
        assert!(text.contains("Time"), "header labels the duration column");
        // The row still renders below the header.
        assert!(text.contains("Roygbiv"));
    }

    #[test]
    fn an_empty_track_pane_has_no_column_header() {
        // A header over "No liked songs yet" would label an empty grid.
        use crate::app::{AppState, Pane};
        let s = AppState {
            pane: Pane::Songs,
            ..Default::default()
        };
        assert!(!buffer_text(&s).contains("Title"));
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
            .draw(|f| draw(f, f.area(), &state, &theme))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let rendered: String = buffer.content().iter().map(|c| c.symbol()).collect();
        assert!(rendered.contains('•'));
        let cell = buffer.cell((0, 0)).unwrap();
        assert_eq!(cell.bg, theme.bg_sel);
    }
}
