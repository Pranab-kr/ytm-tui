//! The top-level frame layout: sidebar | main, with the now-playing bar pinned
//! to the bottom. Every widget guards on a zero-sized `Rect` — layout math on a
//! tiny terminal is how a TUI panics and loses the user's session.

use crate::{
    app::{AppState, Modal, Pane},
    keymap::KeyMap,
    theme::Theme,
    util::text::{display_width, truncate_to_width},
    widgets::{art, help, modal, nowplaying, playlists, queue, search, sidebar, toast, tracklist},
};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

/// Spec §6: sidebar ~22 columns. The bar's own height comes from the widget that
/// draws it, so adding a row there cannot leave the layout reserving too few.
pub const SIDEBAR_WIDTH: u16 = 22;
const NOWPLAYING_HEIGHT: u16 = nowplaying::HEIGHT;
/// Columns the art panel takes when it appears.
const ART_WIDTH: u16 = 24;
/// Blank columns between the track list and the art panel. Without it the
/// artwork's left edge sits flush against the duration column and the two read
/// as one smeared block.
const ART_GAP: u16 = 2;
/// The main pane keeps at least this much, or the art panel does not appear.
/// A shredded track list is worse than absent art (FR-U5).
const MAIN_MIN_WIDTH: u16 = 48;

/// What sits under a mouse click. Hit-testing and `list_rows_for` derive from
/// the same layout constants `render` does, or they drift and a click selects
/// the wrong row; the loop sets the row count, since `render` cannot write back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickTarget {
    /// The nth sidebar source.
    Source(usize),
    /// The nth row of the list, counting from the top of the visible window.
    Row(usize),
    /// The progress row of the now-playing bar, with the column clicked. Only
    /// `nowplaying` knows where the bar starts, so the caller turns the column
    /// into a position — duplicating that here seeks to the wrong second.
    Progress(u16),
    /// Chrome: the heading, the rest of the now-playing bar, the rule.
    Nothing,
}

/// Resolve a click at `(col, row)` in a terminal of size `area`.
pub fn click_target(
    area: Rect,
    col: u16,
    row: u16,
    search_row: bool,
    filter_row: bool,
    header_row: bool,
) -> ClickTarget {
    let (body, np) = body_and_nowplaying(area);
    // The progress row is the one clickable part of the now-playing bar. The
    // margin, the rule, and the title row stay inert, so a click on the title
    // cannot move playback.
    if row >= body.height {
        return if row == np.y + nowplaying::PROGRESS_ROW && col >= np.x && col < np.x + np.width {
            ClickTarget::Progress(col - np.x)
        } else {
            ClickTarget::Nothing
        };
    }
    if col < SIDEBAR_WIDTH {
        // The sidebar draws grouped rows with blank spacers between them, so a
        // screen row maps to a source through `source_at_row` rather than
        // directly — a click on a spacer selects nothing.
        return match sidebar::source_at_row(row as usize) {
            Some(i) => ClickTarget::Source(i),
            None => ClickTarget::Nothing,
        };
    }
    // The rule column between sidebar and list.
    if col == SIDEBAR_WIDTH {
        return ClickTarget::Nothing;
    }
    let top = list_top_for(search_row, filter_row, header_row);
    match row.checked_sub(top) {
        Some(offset) => ClickTarget::Row(offset as usize),
        // The heading row, and any query/filter/column-header rows above the list.
        None => ClickTarget::Nothing,
    }
}

/// Screen row the list's first entry is drawn on; the click handler subtracts it
/// to get a row index. Takes the flags, not the pane: Search always grows a query
/// row and Artists does while `S` is open, and pane-derived math already drifted.
pub fn list_top_for(search_row: bool, filter_row: bool, header_row: bool) -> u16 {
    // Row 0 is the pane heading; a query row takes the next; a visible filter row
    // one more; the column-header row one more still, directly above the list.
    // Miss any of these and a click lands on a different row than the pointer.
    1 + u16::from(search_row) + u16::from(filter_row) + u16::from(header_row)
}

pub fn list_rows_for(area: Rect, search_row: bool, filter_row: bool, header_row: bool) -> usize {
    // Now-playing bar, then the pane heading inside the main area.
    let body = area.height.saturating_sub(NOWPLAYING_HEIGHT);
    body.saturating_sub(1)
        .saturating_sub(u16::from(search_row))
        .saturating_sub(u16::from(filter_row))
        .saturating_sub(u16::from(header_row)) as usize
}

/// The body area and the now-playing bar below it. Shared with the click handler:
/// hit-testing the progress bar must use the same split `render` draws from, or a
/// click seeks to a position other than the one under the pointer.
pub fn body_and_nowplaying(area: Rect) -> (Rect, Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(NOWPLAYING_HEIGHT)])
        .split(area);
    (rows[0], rows[1])
}

/// Split the main area into list and art panel, or leave it whole. Pure math, so
/// the rule is testable with no terminal or image protocol: the panel appears only
/// when art is displayable, something is playing, and the list stays readable.
pub fn split_for_art(area: Rect, art_enabled: bool, has_art: bool) -> (Rect, Option<Rect>) {
    if !art_enabled || !has_art || area.width < MAIN_MIN_WIDTH + ART_WIDTH + ART_GAP {
        return (area, None);
    }
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(ART_GAP),
            Constraint::Length(ART_WIDTH),
        ])
        .split(area);
    // cols[1] is the gap: deliberately left unpainted.
    (cols[0], Some(cols[2]))
}

pub fn render(f: &mut Frame, s: &AppState, t: &Theme, km: &KeyMap, art: &mut art::ArtCache) {
    let area = f.area();
    if area.width == 0 || area.height == 0 {
        return;
    }

    let (body, np) = body_and_nowplaying(area);
    let rows = [body, np];

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(SIDEBAR_WIDTH),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(rows[0]);

    // Art is keyed by the playing track's thumbnail, and only drawn once the
    // bytes have arrived — an in-flight URL leaves the layout unsplit rather
    // than reserving a gap that may never fill.
    let art_url = s.art_url();
    let has_art = art_url.as_deref().is_some_and(|u| art.get(u).is_some());
    let (main_area, art_area) = split_for_art(cols[2], art.is_enabled(), has_art);

    sidebar::draw(f, cols[0], s, t);
    draw_rule(f, cols[1], t);
    draw_main(f, main_area, s, t);
    if let Some(a) = art_area {
        art::draw(f, a, art_url.as_deref(), art);
    }
    nowplaying::draw(f, rows[1], s, t);

    // Overlays go last, over everything they describe.
    // The login modal belongs to the auth pane (Task 33).
    if let Some(Modal::Help) = &s.modal {
        help::draw(f, area, km, t);
    } else {
        modal::draw(f, area, s, t);
    }
    toast::draw(f, area, s, t);
}

/// A single dim vertical rule between sidebar and main. No heavy boxes.
fn draw_rule(f: &mut Frame, area: Rect, t: &Theme) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let lines: Vec<Line> = (0..area.height)
        .map(|_| Line::from(Span::styled("│", Style::default().fg(t.fg_dim))))
        .collect();
    f.render_widget(Paragraph::new(lines), area);
}

/// The main pane: a heading row, then the list for whichever pane is active. The
/// match is exhaustive on `Pane` rather than ending in a `_` arm, so adding a pane
/// later fails to compile instead of silently rendering nothing.
fn draw_main(f: &mut Frame, area: Rect, s: &AppState, t: &Theme) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let w = area.width as usize;

    // A filter row above the list whenever one is being typed or is narrowing
    // rows. It must be visible: without it the user types and sees only rows
    // vanishing, with nothing to say what the filter holds or how to leave it.
    let show_filter = s.filter_row_visible();
    // The query row is drawn here rather than inside the Search pane, because
    // Artists borrows the same field for `S`. Owning it in one place is what
    // keeps the layout, the click math, and `list_top_for` in agreement.
    let show_search = s.search_row_visible();
    // A dim column header sits directly above the list on the track-shaped
    // panes. It is the last row before the list, so its constraint follows the
    // query and filter rows and `list_top_for` counts it the same way.
    let show_header = s.column_header_visible();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(u16::from(show_search)),
            Constraint::Length(u16::from(show_filter)),
            Constraint::Length(u16::from(show_header)),
            Constraint::Min(0),
        ])
        .split(area);
    let (search_row, filter_row, header_row, list_row) = (rows[1], rows[2], rows[3], rows[4]);

    let (heading, badge) = heading_parts(s, w);
    let mut spans = vec![Span::styled(
        heading,
        Style::default()
            .fg(t.fg_bright)
            .add_modifier(Modifier::BOLD),
    )];
    if let Some(b) = badge {
        spans.push(Span::styled(
            b,
            Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), rows[0]);
    // Top-right of the heading row: tied to the pane whose data is loading.
    toast::draw_spinner(f, rows[0], s, t);

    if show_search {
        search::draw_input(f, search_row, s, t);
    }
    if show_filter {
        search::draw_filter(f, filter_row, s, t);
    }
    if show_header {
        tracklist::draw_column_header(f, header_row, t);
    }

    match s.pane {
        Pane::Home => playlists::draw_home(f, list_row, s, t),
        // An open playlist shows its tracks; the list of playlists otherwise.
        Pane::Playlists if s.open_playlist.is_some() => tracklist::draw(f, list_row, s, t),
        Pane::Playlists => playlists::draw_playlists(f, list_row, s, t),
        Pane::Songs => tracklist::draw(f, list_row, s, t),
        Pane::Queue => queue::draw(f, list_row, s, t),
        Pane::Search => draw_search(f, list_row, s, t),
        // An open album shows its songs, like an open playlist does.
        Pane::Albums if s.open_album.is_some() => tracklist::draw(f, list_row, s, t),
        Pane::Albums => playlists::draw_albums(f, list_row, s, t),
        // An open artist shows their tracks, like an open playlist does.
        Pane::Artists if s.open_artist.is_some() => tracklist::draw(f, list_row, s, t),
        Pane::Artists => playlists::draw_artists(f, list_row, s, t),
        Pane::Downloads => tracklist::draw(f, list_row, s, t),
    }
}

/// The results below the query row, which `draw_main` has already drawn.
fn draw_search(f: &mut Frame, area: Rect, s: &AppState, t: &Theme) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    // "Searched and found nothing" must not look like "hasn't searched yet",
    // which is what `tracklist`'s generic empty state would say.
    if s.search_results.is_empty() && !s.search_query.trim().is_empty() {
        search::draw_no_matches(f, area, t);
    } else {
        tracklist::draw(f, area, s, t);
    }
}

/// The heading row: the pane name, plus a visual-mode badge when one fits. Visual
/// mode is otherwise invisible — its marks look exactly like the ones `v` makes.
/// Two pieces so the badge styles apart; dropped, not truncated, when narrow.
fn heading_parts(s: &AppState, w: usize) -> (String, Option<String>) {
    let title = pane_title(s);
    if !s.in_visual_mode() {
        return (truncate_to_width(&title, w), None);
    }
    let badge = format!("  VISUAL {}", s.marked.len());
    if display_width(&title) + display_width(&badge) <= w {
        (title, Some(badge))
    } else {
        (truncate_to_width(&title, w), None)
    }
}

fn pane_title(s: &AppState) -> String {
    match s.pane {
        Pane::Playlists => match &s.open_playlist {
            Some(id) => s
                .playlists
                .iter()
                .find(|p| &p.id == id)
                .map(|p| p.title.clone())
                .unwrap_or_else(|| "Playlist".to_owned()),
            None => "Playlists".to_owned(),
        },
        Pane::Home => "Home".to_owned(),
        // Renamed at the owner's request: the pane is the liked/saved songs, and
        // "Songs" read as if it were every song.
        Pane::Songs => "Fav".to_owned(),
        // An open album is headed by its title, like an open playlist.
        Pane::Albums => match &s.open_album {
            Some((_, name)) => name.clone(),
            None => "Albums".to_owned(),
        },
        // An open artist is headed by their name, like an open playlist.
        Pane::Artists => match &s.open_artist {
            Some((_, name)) => name.clone(),
            None => "Artists".to_owned(),
        },
        Pane::Search => "Search".to_owned(),
        Pane::Queue => "Queue".to_owned(),
        Pane::Downloads => "Downloads".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::art::ArtCache;
    use ratatui::{Terminal, backend::TestBackend};
    use ytm_core::Track;

    fn frame_text(s: &AppState, art: &mut ArtCache, w: u16, h: u16) -> String {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        let theme = Theme::default();
        t.draw(|f| render(f, s, &theme, &KeyMap::default(), art))
            .unwrap();
        t.backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    fn playing() -> AppState {
        AppState {
            pane: Pane::Songs,
            tracks: vec![Track::stub("v1", "Roygbiv")],
            now_playing: Some(Track {
                thumbnail_url: Some("https://example.com/a.jpg".into()),
                ..Track::stub("v1", "Roygbiv")
            }),
            ..Default::default()
        }
    }

    #[test]
    fn without_image_support_the_main_pane_keeps_its_full_width() {
        // FR-U5: no art must mean no reserved gap, not an empty panel.
        let s = playing();
        let text = frame_text(&s, &mut ArtCache::disabled(), 80, 24);
        assert!(
            text.contains("Roygbiv"),
            "the track list still renders, got: {text}"
        );
    }

    #[test]
    fn art_layout_reserves_a_panel_only_when_art_is_enabled_and_wide_enough() {
        // The split is pure math, so it is testable without a real terminal.
        // 100 columns leaves the list well above MAIN_MIN_WIDTH after the panel.
        let full = Rect::new(0, 0, 100, 20);
        let (main, art) = split_for_art(full, false, true);
        assert_eq!(main, full, "disabled art must not shrink the main pane");
        assert!(art.is_none());

        let (main, art) = split_for_art(full, true, true);
        assert!(main.width < full.width, "enabled art takes columns");
        assert!(art.is_some());
    }

    #[test]
    fn a_narrow_terminal_gets_no_art_panel_however_capable_it_is() {
        // Splitting a 40-column pane would leave the track list unreadable,
        // and unreadable text is worse than absent art.
        let narrow = Rect::new(0, 0, 40, 20);
        let (main, art) = split_for_art(narrow, true, true);
        assert_eq!(main, narrow);
        assert!(art.is_none());
    }

    #[test]
    fn art_is_skipped_when_nothing_is_playing() {
        let wide = Rect::new(0, 0, 100, 30);
        let (main, art) = split_for_art(wide, true, false);
        assert_eq!(main, wide, "no track means no panel");
        assert!(art.is_none());
    }

    #[test]
    fn an_eight_by_four_terminal_still_does_not_panic_with_art_enabled() {
        let s = playing();
        let _ = frame_text(&s, &mut ArtCache::disabled(), 8, 4);
    }

    #[test]
    fn the_art_panel_is_separated_from_the_list_by_a_gap() {
        // Without it the artwork's left edge sits flush against the duration
        // column, and the two read as one smeared block.
        let full = Rect::new(0, 0, 100, 20);
        let (main, art) = split_for_art(full, true, true);
        let art = art.expect("a 100-column frame has room for art");
        assert!(
            art.x > main.x + main.width,
            "art at x={} must start past the list's right edge at {}",
            art.x,
            main.x + main.width
        );
        assert_eq!(
            art.x - (main.x + main.width),
            ART_GAP,
            "the gap should be exactly ART_GAP columns"
        );
    }

    #[test]
    fn the_gap_is_counted_when_deciding_whether_art_fits() {
        // A frame with room for the panel but not the gap must get no panel,
        // rather than a panel that steals a column from the list.
        let exact = Rect::new(0, 0, MAIN_MIN_WIDTH + ART_WIDTH, 20);
        let (main, art) = split_for_art(exact, true, true);
        assert_eq!(main, exact, "one column short of the gap means no art");
        assert!(art.is_none());

        let enough = Rect::new(0, 0, MAIN_MIN_WIDTH + ART_WIDTH + ART_GAP, 20);
        assert!(split_for_art(enough, true, true).1.is_some());
    }

    #[test]
    fn visual_mode_says_so_in_the_heading_with_a_count() {
        // The mode is otherwise invisible: marks look identical to hand-made
        // ones, so nothing on screen would say arrow keys are now extending a
        // range.
        let mut s = AppState {
            pane: Pane::Songs,
            focus: crate::app::Focus::Main,
            tracks: (0..4).map(|i| Track::stub(&format!("v{i}"), "T")).collect(),
            ..Default::default()
        };
        s.apply(crate::event::AppEvent::Input(
            crate::event::InputAction::ToggleVisual,
        ));
        s.apply(crate::event::AppEvent::Input(
            crate::event::InputAction::Down,
        ));
        let text = frame_text(&s, &mut ArtCache::disabled(), 80, 24);
        assert!(
            text.contains("VISUAL"),
            "the mode must be named, got: {text}"
        );
        assert!(
            text.contains('2'),
            "the selected count must show, got: {text}"
        );
    }

    #[test]
    fn the_artists_pane_draws_a_query_row_while_its_search_is_open() {
        // The bug the owner hit: `S` here focused the search field but only the
        // Search pane ever drew one, so the field was invisible. Every later key
        // went into it — `a`, `v`, digits, Tab — and the pane looked frozen.
        let mut s = AppState {
            pane: Pane::Artists,
            focus: crate::app::Focus::Main,
            ..Default::default()
        };
        s.apply(crate::event::AppEvent::Input(
            crate::event::InputAction::OpenSearch,
        ));
        let text = frame_text(&s, &mut ArtCache::disabled(), 80, 24);
        assert!(
            text.contains("Search:"),
            "the artist query field must be on screen, got: {text}"
        );
    }

    #[test]
    fn the_artists_pane_has_no_query_row_before_its_search_opens() {
        // Guards the test above: drawing the row unconditionally would satisfy it
        // while stealing a row from the artist list.
        let s = AppState {
            pane: Pane::Artists,
            focus: crate::app::Focus::Main,
            ..Default::default()
        };
        let text = frame_text(&s, &mut ArtCache::disabled(), 80, 24);
        assert!(!text.contains("Search:"));
    }

    #[test]
    fn the_heading_is_clean_outside_visual_mode() {
        // Without this the indicator could be painted unconditionally and the
        // test above would still pass.
        let s = AppState {
            pane: Pane::Songs,
            tracks: vec![Track::stub("v1", "Roygbiv")],
            ..Default::default()
        };
        let text = frame_text(&s, &mut ArtCache::disabled(), 80, 24);
        assert!(!text.contains("VISUAL"));
    }

    #[test]
    fn a_narrow_frame_drops_the_indicator_rather_than_the_pane_name() {
        // Truncation must not leave the user looking at a heading that says
        // only "VIS".
        let mut s = AppState {
            pane: Pane::Songs,
            focus: crate::app::Focus::Main,
            tracks: vec![Track::stub("v1", "Roygbiv")],
            ..Default::default()
        };
        s.apply(crate::event::AppEvent::Input(
            crate::event::InputAction::ToggleVisual,
        ));
        let text = frame_text(&s, &mut ArtCache::disabled(), 30, 10);
        assert!(text.contains("Fav"), "the pane name survives, got: {text}");
    }

    #[test]
    fn the_viewport_row_count_excludes_the_chrome() {
        // Paging and `zz` are computed from this. If it counted the now-playing
        // bar or the heading, a half-page jump would overshoot the screen.
        let area = Rect::new(0, 0, 80, 24);
        // 24 - 4 (now playing: margin, rule, title, progress) - 1 (heading) = 19
        assert_eq!(list_rows_for(area, false, false, false), 19);
        // The search pane also spends a row on the query line.
        assert_eq!(list_rows_for(area, true, false, false), 18);
        // A column header costs one more row of list.
        assert_eq!(list_rows_for(area, false, false, true), 18);
    }

    #[test]
    fn a_tiny_terminal_reports_no_rows_rather_than_underflowing() {
        // These are u16 subtractions; without saturation a short terminal would
        // wrap to 65535 and every page key would jump to the end of the list.
        let area = Rect::new(0, 0, 80, 2);
        assert_eq!(list_rows_for(area, false, false, false), 0);
        assert_eq!(
            list_rows_for(Rect::new(0, 0, 80, 0), false, false, false),
            0
        );
    }

    #[test]
    fn the_list_starts_below_the_heading() {
        // A click handler that assumed row 0 would select one row too high in
        // every pane, and two too high in Search.
        assert_eq!(list_top_for(false, false, false), 1);
        assert_eq!(list_top_for(true, false, false), 2);
        // A column header pushes the first list row down one more.
        assert_eq!(list_top_for(false, false, true), 2);
        assert_eq!(list_top_for(true, true, true), 4);
    }

    #[test]
    fn a_click_in_the_sidebar_names_its_source() {
        let area = Rect::new(0, 0, 80, 24);
        // Row 0 is Home. Rows below run through the grouped layout, so row 4 is
        // Albums (source 3), not source 4 — the spacer after Home shifts them.
        assert_eq!(
            click_target(area, 3, 0, false, false, false),
            ClickTarget::Source(0)
        );
        assert_eq!(
            click_target(area, 3, 4, false, false, false),
            ClickTarget::Source(3)
        );
    }

    #[test]
    fn a_click_on_a_sidebar_spacer_selects_nothing() {
        // The blank row between groups belongs to no source; a click there must
        // not fall through to the pane one row down.
        let area = Rect::new(0, 0, 80, 24);
        assert_eq!(
            click_target(area, 3, 1, false, false, false),
            ClickTarget::Nothing
        );
    }

    #[test]
    fn a_click_in_the_list_names_a_row_offset_not_a_screen_row() {
        // Row 0 of the main area is the heading, so the first list row is screen
        // row 1. Off by one here selects the wrong track on every click.
        let area = Rect::new(0, 0, 80, 24);
        assert_eq!(
            click_target(area, 40, 1, false, false, false),
            ClickTarget::Row(0)
        );
        assert_eq!(
            click_target(area, 40, 5, false, false, false),
            ClickTarget::Row(4)
        );
        // Search spends another row on the query line.
        assert_eq!(
            click_target(area, 40, 2, true, false, false),
            ClickTarget::Row(0)
        );
        // A column header pushes the first list row down one more: with search
        // and header both present the first row is at screen row 3.
        assert_eq!(
            click_target(area, 40, 3, true, false, true),
            ClickTarget::Row(0)
        );
    }

    #[test]
    fn clicks_on_chrome_select_nothing() {
        let area = Rect::new(0, 0, 80, 24);
        // The pane heading.
        assert_eq!(
            click_target(area, 40, 0, false, false, false),
            ClickTarget::Nothing
        );
        // The rule between sidebar and list.
        assert_eq!(
            click_target(area, SIDEBAR_WIDTH, 3, false, false, false),
            ClickTarget::Nothing
        );
        // The now-playing bar, and anything past the frame.
        assert_eq!(
            click_target(area, 40, 21, false, false, false),
            ClickTarget::Nothing
        );
        assert_eq!(
            click_target(area, 40, 200, false, false, false),
            ClickTarget::Nothing
        );
    }
}
