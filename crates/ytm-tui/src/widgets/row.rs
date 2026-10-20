//! The shared two-line list row: a cover gutter, a title line, and a detail
//! line. Every pane that lists songs, playlists, albums or artists draws through
//! here so the columns cannot drift between panes.
//!
//! ```text
//! [cover]  Title                                      duration
//!          Artist • Album
//! ```
//!
//! The cover is four terminal columns wide and two lines tall, with one blank
//! column between it and the text. A dim `♪` stays in that gutter until the
//! image cache has bytes (and forever when the terminal cannot show images), so
//! a missing cover never shifts the text.

use crate::{
    app::AppState,
    theme::Theme,
    util::text::{display_width, pad_to_width, truncate_to_width},
    widgets::{art, tracklist::visible_window},
};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, Paragraph},
};

/// Lines one item consumes. Two-line rows are what make the click and paging
/// math item-based: a heading and a song cost the same number of lines.
pub const HEIGHT: u16 = 2;
/// Columns reserved for the cover.
pub const ART_WIDTH: u16 = 4;
/// Blank columns between the cover and the text.
pub const ART_GAP: u16 = 1;
/// Columns reserved for the mark or play glyph on the title line.
pub const MARK_WIDTH: usize = 2;
/// Right-aligned detail column on the second line (duration, count, year…).
pub const RIGHT_WIDTH: usize = 10;
/// Largest cover requested from the CDN. Four columns is under 96px on any
/// sane font, so a bigger image would only waste memory.
pub const ROW_ART_PX: u32 = 96;

/// Column where the text starts: cover plus the gap.
const TEXT_X: usize = ART_WIDTH as usize + ART_GAP as usize;
/// Column where the title and subtitle start: text plus the mark gutter.
const CONTENT_X: usize = TEXT_X + MARK_WIDTH;

/// One rendered row. Owned strings keep the caller free to build from borrowed
/// tracks; a screenful is roughly twenty rows, so the allocation is cheap.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Row {
    pub title: String,
    pub subtitle: String,
    pub right: String,
    /// Raw thumbnail URL. [`thumbnail_key`] sizes it for the cache; the field
    /// deliberately stays raw so fetch and draw both go through one helper.
    pub thumbnail: Option<String>,
    /// Mark or play glyph, at most [`MARK_WIDTH`] columns.
    pub marker: String,
    /// Now-playing rows get the accent colour and bold text.
    pub accent: bool,
    /// Draw the selection background over both lines.
    pub marked: bool,
    /// A section heading: title on line one, blank line two, no cover gutter.
    pub heading: bool,
}

/// The cache key for a row cover. Fetch and draw must both call this, or a URL
/// fetched at 120px is looked up at 96px and never appears.
pub fn thumbnail_key(url: &str) -> String {
    ytm_core::mapping::thumbnail_at_size(url, ROW_ART_PX)
}

/// Right-align by display width. `format!("{:>width$}")` pads by bytes, which
/// miscounts any non-ASCII detail (an artist count with a narrow space, say).
fn right_aligned(s: &str, width: usize) -> String {
    let text = truncate_to_width(s, width);
    let pad = width.saturating_sub(display_width(&text));
    format!("{}{}", " ".repeat(pad), text)
}

/// Draw a list of two-line rows plus their covers. `row_at` is called once per
/// visible row and may return a heading, which occupies the same two lines.
pub fn draw_list<F>(
    f: &mut Frame,
    area: Rect,
    s: &AppState,
    t: &Theme,
    art: &mut art::ArtCache,
    len: usize,
    mut row_at: F,
) where
    F: FnMut(usize) -> Row,
{
    if area.width == 0 || area.height == 0 {
        return;
    }
    if len == 0 {
        f.render_widget(
            Paragraph::new(Span::styled(
                s.empty_message(),
                Style::default().fg(t.fg_dim),
            )),
            area,
        );
        return;
    }

    let viewport = area.height as usize / HEIGHT as usize;
    if viewport == 0 {
        return;
    }
    let (start, end) = visible_window(s.selected, s.scroll_offset, viewport, len);
    if start >= end {
        return;
    }

    let rows: Vec<Row> = (start..end).map(&mut row_at).collect();
    let width = area.width as usize;
    let title_w = width.saturating_sub(CONTENT_X);
    let right_w = RIGHT_WIDTH.min(title_w);
    let subtitle_w = title_w.saturating_sub(right_w);
    // The placeholder is always one cell followed by spaces: the cover overlay
    // replaces those cells once bytes arrive, and nothing moves if they never do.
    let gutter = format!("\u{266a}{}", " ".repeat(TEXT_X - 1));
    let blank_gutter = " ".repeat(TEXT_X);
    let mark_blank = " ".repeat(MARK_WIDTH);

    let items: Vec<ListItem> = rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let is_sel = start + i == s.selected;
            let highlighted = is_sel || row.marked;
            let item_style = if highlighted {
                Style::default().bg(t.bg_sel)
            } else {
                Style::default()
            };

            if row.heading {
                // A heading is a label at the pane's left edge, but it still
                // consumes two lines so navigation and click math stay uniform.
                let style = Style::default().fg(t.accent).add_modifier(Modifier::BOLD);
                let style = if highlighted {
                    style.bg(t.bg_sel)
                } else {
                    style
                };
                let blank = if highlighted {
                    Style::default().bg(t.bg_sel)
                } else {
                    Style::default()
                };
                return ListItem::new(vec![
                    Line::from(Span::styled(pad_to_width(&row.title, width), style)),
                    Line::from(Span::styled(" ".repeat(width), blank)),
                ])
                .style(item_style);
            }

            let base = if row.accent {
                Style::default().fg(t.accent).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(t.fg)
            };
            let title_style = if highlighted { base.bg(t.bg_sel) } else { base };
            let dim = if highlighted {
                Style::default().fg(t.fg_dim).bg(t.bg_sel)
            } else {
                Style::default().fg(t.fg_dim)
            };
            let marker_style = if highlighted {
                Style::default().fg(t.accent).bg(t.bg_sel)
            } else {
                Style::default().fg(t.accent)
            };

            let line1 = Line::from(vec![
                Span::styled(gutter.clone(), dim),
                Span::styled(pad_to_width(&row.marker, MARK_WIDTH), marker_style),
                Span::styled(pad_to_width(&row.title, title_w), title_style),
            ]);
            let line2 = Line::from(vec![
                Span::styled(blank_gutter.clone(), dim),
                Span::styled(mark_blank.clone(), dim),
                Span::styled(pad_to_width(&row.subtitle, subtitle_w), dim),
                Span::styled(right_aligned(&row.right, right_w), dim),
            ]);
            ListItem::new(vec![line1, line2]).style(item_style)
        })
        .collect();

    f.render_widget(List::new(items), area);

    // Covers go over the placeholder text, never before it: a half-drawn image
    // in a terminal that clears cells differently would otherwise leave holes.
    let bottom = area.y + area.height;
    for (i, row) in rows.iter().enumerate() {
        if row.heading {
            continue;
        }
        let Some(url) = row.thumbnail.as_deref().filter(|u| !u.trim().is_empty()) else {
            continue;
        };
        let y = area.y + (i as u16) * HEIGHT;
        if y >= bottom {
            continue;
        }
        let height = HEIGHT.min(bottom - y);
        let width = ART_WIDTH.min(area.width);
        if width == 0 || height == 0 {
            continue;
        }
        let key = thumbnail_key(url);
        art::draw_row(f, Rect::new(area.x, y, width, height), Some(&key), art);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{app::Pane, theme::Theme, widgets::art::ArtCache};
    use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

    fn text_rows(buf: &Buffer) -> Vec<String> {
        let w = usize::from(buf.area.width);
        buf.content()
            .chunks(w)
            .map(|row| row.iter().map(|c| c.symbol()).collect())
            .collect()
    }

    fn render_rows(
        width: u16,
        height: u16,
        s: &AppState,
        art: &mut ArtCache,
        len: usize,
        mut row_at: impl FnMut(usize) -> Row,
    ) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let theme = Theme::default();
        terminal
            .draw(|f| {
                let area = f.area();
                draw_list(f, area, s, &theme, art, len, &mut row_at);
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn song(title: &str) -> Row {
        Row {
            title: title.to_owned(),
            subtitle: "Boards of Canada • Geogaddi".to_owned(),
            right: "2:29".to_owned(),
            ..Row::default()
        }
    }

    #[test]
    fn a_row_is_two_lines_with_a_placeholder_cover_gutter() {
        let s = AppState {
            selected: 0,
            ..Default::default()
        };
        let mut art = ArtCache::disabled();
        let buf = render_rows(60, 2, &s, &mut art, 1, |_| song("Roygbiv"));
        let rows = text_rows(&buf);

        assert_eq!(
            rows[0].chars().take(7).collect::<String>(),
            "♪      ",
            "the cover gutter is four columns plus a gap, then the mark gutter"
        );
        assert!(
            rows[0]
                .chars()
                .skip(7)
                .collect::<String>()
                .starts_with("Roygbiv"),
            "title starts after the cover and mark gutters, got {:?}",
            &rows[0][..]
        );
        assert!(
            rows[1]
                .chars()
                .skip(7)
                .collect::<String>()
                .starts_with("Boards of Canada"),
            "subtitle must line up with the title, got {:?}",
            &rows[1][..]
        );
        assert!(
            rows[1].ends_with("2:29"),
            "duration lives on the second line, right-aligned"
        );
    }

    #[test]
    fn the_mark_gutter_sits_between_the_cover_and_the_title() {
        let s = AppState::default();
        let mut art = ArtCache::disabled();
        let buf = render_rows(40, 2, &s, &mut art, 1, |_| Row {
            marker: "\u{2022} ".to_owned(),
            ..song("Marked")
        });
        let first = &text_rows(&buf)[0];
        assert_eq!(
            first.chars().skip(5).take(2).collect::<String>(),
            "\u{2022} ",
            "the mark takes the two columns after the cover gutter"
        );
        assert!(
            first
                .chars()
                .skip(7)
                .collect::<String>()
                .starts_with("Marked")
        );
    }

    #[test]
    fn the_right_detail_is_aligned_in_the_last_ten_columns() {
        let s = AppState::default();
        let mut art = ArtCache::disabled();
        let buf = render_rows(40, 2, &s, &mut art, 1, |_| Row {
            title: "Song".to_owned(),
            subtitle: "Artist".to_owned(),
            right: "1:02:03".to_owned(),
            ..Row::default()
        });
        let second = &text_rows(&buf)[1];
        assert_eq!(second.chars().count(), 40);
        assert_eq!(
            second.chars().skip(30).collect::<String>(),
            "   1:02:03",
            "the detail is right-aligned in RIGHT_WIDTH columns"
        );
    }

    #[test]
    fn unicode_titles_truncate_by_display_width_without_shifting_columns() {
        let s = AppState::default();
        let mut art = ArtCache::disabled();
        let buf = render_rows(20, 2, &s, &mut art, 1, |_| Row {
            title: "日本語のとても長いタイトル".to_owned(),
            subtitle: "Artist".to_owned(),
            right: "1:23".to_owned(),
            ..Row::default()
        });

        assert_eq!(buf.cell((7, 0)).unwrap().symbol(), "日");
        assert_eq!(
            buf.cell((19, 0)).unwrap().symbol(),
            "…",
            "a 13-column title budget leaves room for the ellipsis"
        );
        assert_eq!(
            buf.cell((7, 1)).unwrap().symbol(),
            "A",
            "the subtitle must still start at column 7"
        );
        let second: String = (0..20)
            .map(|x| buf.cell((x, 1)).unwrap().symbol())
            .collect();
        assert!(second.ends_with("1:23"));
    }

    #[test]
    fn a_heading_uses_the_panes_left_edge_and_leaves_the_second_line_blank() {
        let s = AppState::default();
        let mut art = ArtCache::disabled();
        let buf = render_rows(30, 4, &s, &mut art, 2, |idx| {
            if idx == 0 {
                Row {
                    title: "Quick picks".to_owned(),
                    heading: true,
                    ..Row::default()
                }
            } else {
                song("Song 1")
            }
        });
        let rows = text_rows(&buf);
        assert!(
            rows[0].starts_with("Quick picks"),
            "headings start at column zero, got {:?}",
            rows[0]
        );
        assert!(
            rows[1].trim().is_empty(),
            "the heading's second line is blank, got {:?}",
            rows[1]
        );
        assert_eq!(
            buf.cell((0, 2)).unwrap().symbol(),
            "♪",
            "the item below the heading gets the cover gutter"
        );
    }

    #[test]
    fn marks_alone_cover_both_lines() {
        use crate::theme::Theme;
        // selected is outside this one-item list, so only `marked` highlights.
        let s = AppState {
            selected: 1,
            ..Default::default()
        };
        let mut art = ArtCache::disabled();
        let buf = render_rows(40, 2, &s, &mut art, 1, |_| Row {
            marked: true,
            marker: "\u{2022} ".to_owned(),
            ..song("Marked")
        });
        let theme = Theme::default();
        assert_eq!(buf.cell((0, 0)).unwrap().bg, theme.bg_sel);
        assert_eq!(buf.cell((0, 1)).unwrap().bg, theme.bg_sel);
        assert_eq!(buf.cell((39, 1)).unwrap().bg, theme.bg_sel);
    }

    #[test]
    fn selection_alone_covers_both_lines() {
        use crate::theme::Theme;
        let s = AppState {
            selected: 0,
            ..Default::default()
        };
        let mut art = ArtCache::disabled();
        let buf = render_rows(40, 2, &s, &mut art, 1, |_| song("Selected"));
        let theme = Theme::default();
        assert_eq!(buf.cell((0, 0)).unwrap().bg, theme.bg_sel);
        assert_eq!(buf.cell((0, 1)).unwrap().bg, theme.bg_sel);
    }

    #[test]
    fn the_window_counts_two_line_items_not_screen_lines() {
        let s = AppState {
            selected: 4,
            ..Default::default()
        };
        let mut art = ArtCache::disabled();
        let buf = render_rows(30, 6, &s, &mut art, 10, |idx| song(&format!("Track {idx}")));
        let text = text_rows(&buf).concat();
        assert!(text.contains("Track 2"), "first visible row");
        assert!(text.contains("Track 3"));
        assert!(text.contains("Track 4"), "selection must stay visible");
        assert!(!text.contains("Track 1"), "one row above the window");
        assert!(!text.contains("Track 5"), "one row below the window");
    }

    #[test]
    fn an_empty_list_shows_the_pane_specific_message() {
        let s = AppState {
            pane: Pane::Songs,
            ..Default::default()
        };
        let mut art = ArtCache::disabled();
        let buf = render_rows(40, 4, &s, &mut art, 0, |_| Row::default());
        assert!(text_rows(&buf).concat().contains("No liked songs"));
    }

    #[test]
    fn a_zero_sized_area_is_a_no_op() {
        let s = AppState::default();
        let theme = Theme::default();
        let mut art = ArtCache::disabled();
        let mut terminal = Terminal::new(TestBackend::new(4, 4)).unwrap();
        terminal
            .draw(|f| {
                draw_list(f, Rect::new(0, 0, 0, 0), &s, &theme, &mut art, 5, |_| {
                    song("Song")
                })
            })
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.trim().is_empty());
    }

    #[test]
    fn thumbnail_keys_ask_the_cdn_for_the_row_size() {
        assert_eq!(
            thumbnail_key("https://example.com/a.jpg=w120-h120-l90-rj"),
            "https://example.com/a.jpg=w96-h96-l90-rj"
        );
        assert_eq!(
            thumbnail_key("https://example.com/plain.jpg"),
            "https://example.com/plain.jpg",
            "an unsized URL is left alone"
        );
    }

    #[test]
    fn a_cached_cover_is_drawn_over_the_placeholder() {
        let url = "https://example.com/a.jpg=w120-h120-l90-rj";
        let key = thumbnail_key(url);
        let mut art = ArtCache::for_test();
        // A solid colour encodes to spaces; variation guarantees painted cells.
        let mut img = image::RgbImage::new(120, 120);
        for (x, y, pixel) in img.enumerate_pixels_mut() {
            *pixel = image::Rgb([x as u8, y as u8, (x + y) as u8]);
        }
        art.insert_row(&key, image::DynamicImage::ImageRgb8(img));

        let s = AppState::default();
        let buf = render_rows(40, 2, &s, &mut art, 1, |_| Row {
            thumbnail: Some(url.to_owned()),
            ..song("With cover")
        });
        let painted = (0..4).any(|x| {
            (0..2).any(|y| {
                let symbol = buf.cell((x, y)).unwrap().symbol();
                symbol != " " && symbol != "♪"
            })
        });
        assert!(
            painted,
            "the cached cover must replace the placeholder in the gutter"
        );
    }

    #[test]
    fn a_scrolled_window_draws_the_cover_of_the_track_it_shows() {
        // The cover loop indexes the visible slice, not the whole list. If it
        // used absolute indices after scrolling, the top row would wear another
        // track's cover or none at all.
        let url = "https://example.com/t2.jpg";
        let mut art = ArtCache::for_test();
        let mut img = image::RgbImage::new(96, 96);
        for (x, y, pixel) in img.enumerate_pixels_mut() {
            *pixel = image::Rgb([x as u8, y as u8, (x + y) as u8]);
        }
        art.insert_row(&thumbnail_key(url), image::DynamicImage::ImageRgb8(img));

        let s = AppState {
            selected: 2,
            ..Default::default()
        };
        let buf = render_rows(40, 2, &s, &mut art, 3, |idx| Row {
            title: format!("Track {idx}"),
            thumbnail: (idx == 2).then(|| url.to_owned()),
            ..Row::default()
        });

        assert!(
            text_rows(&buf)[0].contains("Track 2"),
            "the window must show the selected track"
        );
        let painted = (0..4).any(|x| {
            (0..2).any(|y| {
                let symbol = buf.cell((x, y)).unwrap().symbol();
                symbol != " " && symbol != "♪"
            })
        });
        assert!(
            painted,
            "the scrolled-to track's cover must be painted at the top of the window"
        );
    }
}
