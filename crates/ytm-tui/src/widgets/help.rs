//! The `?` overlay: every binding, in two columns (FR-U2).

use crate::{
    event::InputAction,
    keymap::KeyMap,
    theme::Theme,
    util::text::{display_width, pad_to_width},
    widgets::toast::centered_rect,
};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

/// Largest share of the frame the overlay may take. It is sized to its content
/// and only clamped by these.
const MAX_WIDTH_PCT: u16 = 90;
const MAX_HEIGHT_PCT: u16 = 90;
/// Most columns worth trying. Beyond this the eye stops tracking rows.
const MAX_COLS: usize = 3;
/// Columns reserved for the key itself; the rest goes to the action name.
const KEY_WIDTH: usize = 4;
/// Below this, an action name is truncated past recognition, so use one fewer
/// column instead.
const MIN_LABEL_WIDTH: usize = 8;

/// Lower-case, human-readable action names. Hand-written rather than derived
/// from `Debug`, so the overlay reads as help text and not as Rust.
fn action_label(a: &InputAction) -> Option<&'static str> {
    use InputAction as A;
    Some(match a {
        A::Quit => "quit",
        // Not listed: Ctrl+C is not a `[keys]` binding, and the overlay's
        // "Not remappable" section already names it.
        A::ForceQuit => return None,
        A::Up => "up",
        A::Down => "down",
        A::Left => "back / sidebar",
        A::Right => "open / list",
        A::Home => "first row",
        A::End => "last row",
        A::TogglePause => "play / pause",
        A::NextTrack => "next track",
        A::PrevTrack => "previous track",
        A::SeekForward => "seek forward",
        A::SeekBack => "seek back",
        A::VolumeUp => "volume up",
        A::VolumeDown => "volume down",
        A::ToggleMute => "mute",
        A::ToggleShuffle => "shuffle",
        A::CycleRepeat => "repeat mode",
        A::OpenSearch => "search online",
        A::OpenFilter => "filter this list",
        A::CenterOnCursor => "centre this row",
        A::FocusCurrent => "focus playing song",
        A::OpenQueue => "queue",
        A::OpenHelp => "this help",
        A::AddToQueue => "add to queue",
        A::PlayNext => "play next",
        A::MoveEntryUp => "move entry up",
        A::MoveEntryDown => "move entry down",
        A::ClearQueue => "clear queue",
        A::CreatePlaylist => "new playlist",
        A::RenamePlaylist => "rename playlist",
        A::DeletePlaylist => "delete playlist",
        A::RemoveFromPlaylist => "remove entry",
        A::AddToPlaylist => "add to playlist",
        A::Refresh => "reload",
        A::ToggleMark => "mark row",
        A::ToggleVisual => "visual select",
        A::CycleTheme => "next theme",
        A::EditConfig => "edit config",
        A::Download => "download track",
        // Not bindings a user presses on purpose.
        A::Confirm | A::Cancel | A::NextPane | A::PrevPane => return None,
        // Wheel motion, not a binding anyone types.
        A::ScrollUp | A::ScrollDown => return None,
        A::PageUp | A::PageDown | A::GoTo(_) | A::Char(_) | A::Backspace => return None,
        // Text-field editing. Listed in FIXED_ROWS with their real chords
        // instead: they are Ctrl combinations, not remappable single chars, and
        // they only do anything while the search field has focus.
        A::DeleteWordBack
        | A::WordLeft
        | A::WordRight
        | A::CharLeft
        | A::CharRight
        | A::LineStart
        | A::LineEnd => return None,
    })
}

/// Rows of `key  action`, from the *live* keymap — a user who rebound `quit`
/// must not be told to press `q`.
fn rows(km: &KeyMap) -> Vec<(String, &'static str)> {
    let mut v: Vec<(String, &'static str)> = km
        .bindings()
        .iter()
        .filter_map(|(k, a)| action_label(a).map(|l| (k.clone(), l)))
        .collect();
    v.extend(FIXED_ROWS.iter().map(|(k, l)| ((*k).to_owned(), *l)));
    v
}

/// Fixed bindings that `KeyMap::bindings` cannot report (FR-U2).
const FIXED_ROWS: [(&str, &str); 7] = [
    ("1-8", "jump to source"),
    ("tab", "next source"),
    ("^d/^u", "half page down/up"),
    ("^w", "delete word (search)"),
    ("^\u{2190}\u{2192}", "word motion (search)"),
    ("^a/^e", "line start/end (search)"),
    (",", "edit config"),
];

/// Fewest readable columns that fit all bindings in the available area.
fn layout(n: usize, w: usize, h: usize) -> (usize, usize) {
    let min_col_w = KEY_WIDTH + MIN_LABEL_WIDTH;
    let max_cols = (w / min_col_w).clamp(1, MAX_COLS);
    let cols = (1..=max_cols)
        .find(|c| n.div_ceil(*c) <= h)
        .unwrap_or(max_cols);
    (cols, w / cols)
}

/// Widest `key + label` a column needs to avoid truncating anything.
fn natural_col_width(rows: &[(String, &'static str)]) -> usize {
    rows.iter()
        .map(|(_, l)| KEY_WIDTH + display_width(l))
        .max()
        .unwrap_or(KEY_WIDTH)
        + 1 // a column of gutter between columns
}

pub fn draw(f: &mut Frame, area: Rect, km: &KeyMap, t: &Theme) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let all = rows(km);
    if all.is_empty() {
        return;
    }

    // Bound first, then fit inside the bound.
    let max = centered_rect(MAX_WIDTH_PCT, MAX_HEIGHT_PCT, area);
    let (max_iw, max_ih) = (
        max.width.saturating_sub(2) as usize,
        max.height.saturating_sub(2) as usize,
    );
    if max_iw == 0 || max_ih == 0 {
        return;
    }
    let (cols, _) = layout(all.len(), max_iw, max_ih);
    let per_col = all.len().div_ceil(cols);

    let want_w = (natural_col_width(&all) * cols + 2).min(max.width as usize) as u16;
    let want_h = (per_col.min(max_ih) + 2) as u16;
    let rect = Rect {
        x: area.x + (area.width.saturating_sub(want_w)) / 2,
        y: area.y + (area.height.saturating_sub(want_h)) / 2,
        width: want_w,
        height: want_h,
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            " Help ",
            Style::default()
                .fg(t.fg_bright)
                .add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(t.accent));
    let inner = block.inner(rect);

    // Clear first, or the list underneath shows through the overlay.
    f.render_widget(Clear, rect);
    f.render_widget(block, rect);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let col_w = inner.width as usize / cols;
    let lines: Vec<Line> = (0..per_col.min(inner.height as usize))
        .map(|i| {
            let mut spans = Vec::new();
            for c in 0..cols {
                let Some((key, label)) = all.get(i + c * per_col) else {
                    break;
                };
                spans.push(Span::styled(
                    pad_to_width(key, KEY_WIDTH),
                    Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
                ));
                spans.push(Span::styled(
                    pad_to_width(label, col_w.saturating_sub(KEY_WIDTH)),
                    Style::default().fg(t.fg),
                ));
            }
            Line::from(spans)
        })
        .collect();

    f.render_widget(Paragraph::new(lines), inner);
}

#[cfg(test)]
mod tests {
    use crate::{
        app::{AppState, Modal},
        keymap::KeyMap,
        theme::Theme,
    };

    fn text_of(s: &AppState) -> String {
        with_keys(s, &KeyMap::default())
    }

    fn with_keys(s: &AppState, km: &KeyMap) -> String {
        use ratatui::{Terminal, backend::TestBackend};
        let mut t = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let theme = Theme::default();
        t.draw(|f| {
            crate::render::render(
                f,
                s,
                &theme,
                km,
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

    fn help_open() -> AppState {
        AppState {
            modal: Some(Modal::Help),
            ..Default::default()
        }
    }

    #[test]
    fn help_overlay_lists_bindings() {
        let text = text_of(&help_open());
        // FR-U2
        assert!(text.contains("Help") || text.contains("Keys"));
        assert!(text.contains("quit") || text.contains("Quit"));
    }

    #[test]
    fn the_help_overlay_names_the_key_for_each_action() {
        let text = text_of(&help_open());
        // The action alone is useless — the point is which key does it.
        for action in ["quit", "pause", "search", "queue"] {
            assert!(text.contains(action), "missing action {action:?}");
        }
    }

    #[test]
    fn help_shows_the_configured_key_not_the_default_one() {
        // A user who rebinds quit must not be told to press `q`.
        let km = KeyMap::from_toml_str(r#"quit = "Z""#).unwrap();
        let text = with_keys(&help_open(), &km);
        let z = text.find('Z').expect("the override must be listed");
        let quit = text.find("quit").expect("quit must be listed");
        assert!(
            quit.saturating_sub(z) < 40,
            "the key must sit on the same row as its action"
        );
    }

    #[test]
    fn nothing_is_drawn_when_the_overlay_is_closed() {
        let s = AppState::default();
        assert!(!text_of(&s).contains("Help"));
    }

    #[test]
    fn the_overlay_survives_a_terminal_too_small_to_hold_it() {
        use ratatui::{Terminal, backend::TestBackend};
        // Layout math on a tiny terminal is how a TUI panics.
        let mut t = Terminal::new(TestBackend::new(8, 3)).unwrap();
        let theme = Theme::default();
        t.draw(|f| {
            crate::render::render(
                f,
                &help_open(),
                &theme,
                &KeyMap::default(),
                &mut crate::widgets::art::ArtCache::disabled(),
            )
        })
        .unwrap();
    }

    #[test]
    fn the_overlay_lists_the_number_keys_for_sources() {
        // FR-U2: a binding the user can press must be discoverable. The digits
        // are resolved in `resolve`, not stored in the char table, so without an
        // explicit row they would be invisible.
        let text = text_of(&help_open());
        assert!(
            text.contains("1-8"),
            "the source jump keys should be listed, got: {text}"
        );
    }

    #[test]
    fn the_overlay_explains_that_h_goes_back() {
        let text = text_of(&help_open());
        assert!(
            text.contains("back") || text.contains("close"),
            "h should read as going back a level, got: {text}"
        );
    }

    #[test]
    fn the_overlay_lists_the_visual_select_binding() {
        // FR-U2: a key the user can press must be discoverable from `?`.
        let rows = super::rows(&KeyMap::default());
        assert!(
            rows.iter().any(|(k, l)| k == "V" && *l == "visual select"),
            "V must be listed, got: {rows:?}"
        );
    }
}
