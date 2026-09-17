//! Playlist, album, and artist rows. Same shape as `tracklist`: zero-area
//! guard, empty-state message, `visible_window` scrolling, column widths in
//! display cells, reversed background for selection.

use crate::{
    app::AppState, theme::Theme, util::text::pad_to_width, widgets::tracklist::visible_window,
};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, Paragraph},
};

/// Right-aligned `1234 tracks`.
const COUNT_WIDTH: usize = 12;
/// Album year column, e.g. `2002`.
const YEAR_WIDTH: usize = 6;
/// Shown on playlists YouTube will not let us edit (FR-C).
const READONLY: &str = "read-only";
/// Right-hand tag naming what a home row is: `track`, `playlist`, `album`, `artist`.
const KIND_WIDTH: usize = 10;

/// Scaffolding every list shares: zero-area guard, empty state, scroll window,
/// and the selection style. `row` builds the spans for one index, receiving the
/// normal and dimmed styles already resolved for that row's selection state.
fn draw_list<F>(f: &mut Frame, area: Rect, s: &AppState, t: &Theme, len: usize, mut row: F)
where
    F: FnMut(usize, Style, Style) -> Vec<Span<'static>>,
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

    let (start, end) = visible_window(s.selected, s.scroll_offset, area.height as usize, len);
    let items: Vec<ListItem> = (start..end)
        .map(|idx| {
            let is_sel = idx == s.selected;
            let base = Style::default().fg(t.fg);
            // Selection is a reversed background, not a '>' marker (spec §6).
            let style = if is_sel { base.bg(t.bg_sel) } else { base };
            let dim = if is_sel {
                style
            } else {
                Style::default().fg(t.fg_dim)
            };
            ListItem::new(Line::from(row(idx, style, dim))).style(if is_sel {
                Style::default().bg(t.bg_sel)
            } else {
                Style::default()
            })
        })
        .collect();

    f.render_widget(List::new(items), area);
}

/// The home feed (FR-B6): shelf headings interleaved with their cards. Headings
/// scroll rather than stick, since a carousel's title only means something next to
/// its own cards. Each card carries a kind tag — one shelf mixes all four.
pub fn draw_home(f: &mut Frame, area: Rect, s: &AppState, t: &Theme) {
    use crate::app::HomeRow;
    use ytm_core::HomeTarget;

    if area.width == 0 || area.height == 0 {
        return;
    }
    if s.home_rows.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(
                s.empty_message(),
                Style::default().fg(t.fg_dim),
            )),
            area,
        );
        return;
    }

    let w = area.width as usize;
    let kind_w = KIND_WIDTH;
    let text_w = w.saturating_sub(kind_w);
    let title_w = (text_w * 55) / 100;
    let sub_w = text_w.saturating_sub(title_w);
    const GUTTER_WIDTH: usize = 2;
    let item_title_w = title_w.saturating_sub(GUTTER_WIDTH);

    let (start, end) = visible_window(
        s.selected,
        s.scroll_offset,
        area.height as usize,
        s.home_rows.len(),
    );

    let items: Vec<ListItem> = (start..end)
        .map(|idx| match &s.home_rows[idx] {
            HomeRow::Heading(title) => ListItem::new(Line::from(vec![Span::styled(
                pad_to_width(title, w),
                Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
            )])),
            HomeRow::Item(i) => {
                let is_sel = idx == s.selected;
                let marked = match &i.target {
                    HomeTarget::Track(v) => s.marked.contains(v),
                    _ => false,
                };
                let highlighted = is_sel || marked;
                let base = Style::default().fg(t.fg);
                let style = if highlighted { base.bg(t.bg_sel) } else { base };
                let dim = if highlighted {
                    style
                } else {
                    Style::default().fg(t.fg_dim)
                };

                let gutter = if marked { "\u{2022} " } else { "  " };
                let gutter_style = if highlighted {
                    Style::default().fg(t.accent).bg(t.bg_sel)
                } else {
                    Style::default().fg(t.accent)
                };

                ListItem::new(Line::from(vec![
                    Span::styled(gutter, gutter_style),
                    Span::styled(pad_to_width(&i.title, item_title_w), style),
                    Span::styled(pad_to_width(&i.subtitle, sub_w), dim),
                    Span::styled(format!("{:>kind_w$}", i.kind_label()), dim),
                ]))
                .style(if highlighted {
                    Style::default().bg(t.bg_sel)
                } else {
                    Style::default()
                })
            }
        })
        .collect();

    f.render_widget(List::new(items), area);
}

pub fn draw_playlists(f: &mut Frame, area: Rect, s: &AppState, t: &Theme) {
    let w = area.width as usize;
    // A playlist row has one text column, so the title takes everything the
    // count and marker do not. (The plan said 60%, which would leave ~40% of
    // every row blank and truncate long titles for no gain.)
    let ro_w = READONLY.len() + 1;
    let title_w = w.saturating_sub(COUNT_WIDTH + ro_w);

    // The filtered view, not `s.playlists`: the reducer counts and indexes this
    // list, so drawing the raw one makes `/` appear to do nothing and puts the
    // cursor on a different row than Enter acts on.
    let rows = s.visible_playlists();
    draw_list(f, area, s, t, rows.len(), |idx, style, dim| {
        let p = &rows[idx];
        let count = match p.track_count {
            Some(n) => format!("{n} tracks"),
            None => "\u{2014}".to_owned(),
        };
        vec![
            Span::styled(pad_to_width(&p.title, title_w), style),
            Span::styled(format!("{count:>COUNT_WIDTH$}"), dim),
            Span::styled(
                if p.is_system {
                    format!(" {READONLY}")
                } else {
                    " ".repeat(ro_w)
                },
                Style::default().fg(t.fg_dim).add_modifier(Modifier::ITALIC),
            ),
        ]
    });
}

pub fn draw_albums(f: &mut Frame, area: Rect, s: &AppState, t: &Theme) {
    let w = area.width as usize;
    let text_w = w.saturating_sub(YEAR_WIDTH);
    let title_w = text_w / 2;
    let artist_w = text_w.saturating_sub(title_w);

    let rows = s.visible_albums();
    draw_list(f, area, s, t, rows.len(), |idx, style, dim| {
        let a = &rows[idx];
        let artists = if a.artists.is_empty() {
            "Unknown artist".to_owned()
        } else {
            a.artists.join(", ")
        };
        vec![
            Span::styled(pad_to_width(&a.title, title_w), style),
            Span::styled(pad_to_width(&artists, artist_w), dim),
            Span::styled(
                format!(
                    "{:>YEAR_WIDTH$}",
                    a.year.clone().unwrap_or_else(|| "\u{2014}".to_owned())
                ),
                dim,
            ),
        ]
    });
}

pub fn draw_artists(f: &mut Frame, area: Rect, s: &AppState, t: &Theme) {
    let w = area.width as usize;
    let name_w = (w * 6) / 10;
    let subs_w = w.saturating_sub(name_w);

    let rows = s.visible_artists();
    draw_list(f, area, s, t, rows.len(), |idx, style, dim| {
        let a = &rows[idx];
        vec![
            Span::styled(pad_to_width(&a.name, name_w), style),
            Span::styled(
                pad_to_width(a.subscribers.as_deref().unwrap_or(""), subs_w),
                dim,
            ),
        ]
    });
}

#[cfg(test)]
mod tests {
    use crate::{
        app::{AppState, Pane},
        theme::Theme,
    };
    use ratatui::{Terminal, backend::TestBackend};
    use ytm_core::{Album, AlbumId, Artist, ArtistId, Playlist};

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
        assert!(text.contains("42"), "track count missing");
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
        assert!(text_of(&s).contains("Mystery"));
    }

    #[test]
    fn albums_show_title_and_artist() {
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
    }

    #[test]
    fn artists_show_names() {
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
        assert!(text_of(&s).contains("Aphex Twin"));
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
            .draw(|f| super::draw_home(f, f.area(), &s, &theme))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let rendered: String = buffer.content().iter().map(|c| c.symbol()).collect();
        assert!(rendered.contains('•'), "marked home track needs a bullet");
        // Row 1 is Song 1 (marked). Cell (0, 1) should have bg_sel.
        let cell = buffer.cell((0, 1)).unwrap();
        assert_eq!(
            cell.bg, theme.bg_sel,
            "marked home track needs background highlight"
        );
    }
}
