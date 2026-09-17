//! The source list down the left edge. Labels are asserted on by tests and by
//! the user's muscle memory — do not rename them casually.

use crate::{
    app::{AppState, Focus, Pane},
    theme::Theme,
    util::text::pad_to_width,
};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

/// Display order of the sources, top to bottom.
pub const SOURCES: [(Pane, &str); 8] = [
    (Pane::Home, "Home"),
    (Pane::Playlists, "Playlists"),
    // "Fav" rather than "Songs": the pane is the liked/saved songs, and the old
    // label read as if it listed everything.
    (Pane::Songs, "Fav"),
    (Pane::Albums, "Albums"),
    (Pane::Artists, "Artists"),
    (Pane::Search, "Search"),
    (Pane::Queue, "Queue"),
    (Pane::Downloads, "Downloads"),
];

/// One line of the sidebar as it is drawn: a source, or a blank row separating
/// one group from the next. The renderer and the click handler both walk this, so
/// a click on a spacer selects nothing rather than the pane one row down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarRow {
    /// The nth entry of `SOURCES`.
    Source(usize),
    /// A blank grouping row.
    Spacer,
}

/// The sidebar top to bottom: Home, the library (Playlists/Fav/Albums/Artists),
/// then Search and Queue, each group set apart by a blank row. `Source` indices
/// are into `SOURCES`/`PANE_ORDER`, and the number shown is the jump key.
pub const LAYOUT: [SidebarRow; 10] = [
    SidebarRow::Source(0), // Home
    SidebarRow::Spacer,
    SidebarRow::Source(1), // Playlists
    SidebarRow::Source(2), // Fav
    SidebarRow::Source(3), // Albums
    SidebarRow::Source(4), // Artists
    SidebarRow::Spacer,
    SidebarRow::Source(5), // Search
    SidebarRow::Source(6), // Queue
    SidebarRow::Source(7), // Downloads
];

/// The source index drawn at screen `row`, or `None` for a spacer or a row past
/// the end of the layout. Shared with the click handler so the two cannot
/// disagree about which row is which source.
pub fn source_at_row(row: usize) -> Option<usize> {
    match LAYOUT.get(row)? {
        SidebarRow::Source(i) => Some(*i),
        SidebarRow::Spacer => None,
    }
}

/// Columns the marker and the number take before the label: `▎` (or a space),
/// one digit, and a trailing space.
const PREFIX_WIDTH: usize = 3;

/// The label colour for one source row. A guest cannot enter the account panes,
/// so they read as dimmed rather than selectable; everything else keeps the
/// existing active/inactive contrast.
pub fn source_label_color(
    pane: Pane,
    active: bool,
    s: &AppState,
    t: &Theme,
) -> ratatui::style::Color {
    if s.guest && pane.requires_auth() {
        t.fg_dim
    } else if active {
        t.fg_bright
    } else {
        t.fg
    }
}

pub fn draw(f: &mut Frame, area: Rect, s: &AppState, t: &Theme) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let w = area.width as usize;
    let label_w = w.saturating_sub(PREFIX_WIDTH);
    let lines: Vec<Line> = LAYOUT
        .iter()
        .take(area.height as usize)
        .map(|slot| match slot {
            SidebarRow::Spacer => Line::from(""),
            SidebarRow::Source(i) => {
                let (pane, label) = SOURCES[*i];
                let selected = *i == s.sidebar_selected;
                let active = pane == s.pane;
                // The cursor and the active pane can land on the same row, so each
                // needs its own signal: the reversed bg says "the cursor is here",
                // the bold accent bar says "this is the pane you're in".
                let on_cursor = selected && s.focus == Focus::Sidebar;
                let bg = |st: Style| if on_cursor { st.bg(t.bg_sel) } else { st };

                let mut label_style = Style::default().fg(source_label_color(pane, active, s, t));
                if active {
                    label_style = label_style.add_modifier(Modifier::BOLD);
                }
                // The number is the key that jumps here (`1`-`7`); dim so it
                // reads as a hint beside the label, not as part of it.
                let num_style = Style::default().fg(t.fg_dim);
                let marker_style = Style::default().fg(if active { t.accent } else { t.fg_dim });
                // U+258E LEFT ONE QUARTER BLOCK: a thin accent rule, not a '>'
                // marker (spec §6 keeps selection to the reversed background).
                let marker = if active { "\u{258E}" } else { " " };

                Line::from(vec![
                    Span::styled(marker, bg(marker_style)),
                    Span::styled(format!("{} ", i + 1), bg(num_style)),
                    Span::styled(pad_to_width(label, label_w), bg(label_style)),
                ])
            }
        })
        .collect();

    f.render_widget(Paragraph::new(lines), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guest_account_source_style_is_dim() {
        let theme = Theme::default();
        let state = AppState {
            guest: true,
            ..Default::default()
        };
        assert_eq!(
            source_label_color(Pane::Playlists, false, &state, &theme),
            theme.fg_dim,
            "an account pane a guest cannot enter reads as dimmed"
        );
        assert_eq!(
            source_label_color(Pane::Search, false, &state, &theme),
            theme.fg,
            "Search stays available to a guest"
        );
    }

    #[test]
    fn signed_in_sources_keep_their_normal_contrast() {
        let theme = Theme::default();
        let state = AppState::default();
        assert_eq!(
            source_label_color(Pane::Playlists, true, &state, &theme),
            theme.fg_bright
        );
        assert_eq!(
            source_label_color(Pane::Playlists, false, &state, &theme),
            theme.fg
        );
    }

    #[test]
    fn the_number_keys_match_the_order_the_sidebar_renders() {
        // `goto_source` indexes PANE_ORDER; this widget renders SOURCES. If the
        // two ever disagree, pressing 3 highlights one row and opens another.
        use crate::app::PANE_ORDER;
        let rendered: Vec<_> = SOURCES.iter().map(|(p, _)| *p).collect();
        assert_eq!(
            rendered,
            PANE_ORDER.to_vec(),
            "sidebar order and PANE_ORDER must stay identical"
        );
    }

    #[test]
    fn the_layout_covers_every_source_exactly_once() {
        // The LAYOUT indices are hand-written, so a source added to SOURCES
        // without a row here would silently vanish from the sidebar, and a
        // duplicated index would draw one twice and shift every click below it.
        let mut seen = vec![0usize; SOURCES.len()];
        for slot in LAYOUT {
            if let SidebarRow::Source(i) = slot {
                assert!(i < SOURCES.len(), "layout points at a missing source {i}");
                seen[i] += 1;
            }
        }
        assert!(
            seen.iter().all(|&c| c == 1),
            "every source must appear exactly once, got {seen:?}"
        );
    }

    #[test]
    fn a_spacer_row_belongs_to_no_source() {
        // The click handler relies on this: a click on the blank gap must select
        // nothing, not the pane that follows it.
        assert_eq!(source_at_row(0), Some(0), "row 0 is Home");
        assert_eq!(source_at_row(1), None, "row 1 is the spacer after Home");
        assert_eq!(source_at_row(2), Some(1), "row 2 is Playlists");
        assert_eq!(source_at_row(99), None, "past the end is nothing");
    }
}
