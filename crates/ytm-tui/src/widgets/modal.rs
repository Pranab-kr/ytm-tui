//! Confirm and prompt boxes. A modal owns the keyboard while it is open.
//!
//! Both footers name the keys to press. A modal the user cannot see their way
//! out of is worse than no modal, so `[esc] cancel` is always on screen.

use crate::{
    app::{AppState, Modal},
    theme::Theme,
    util::text::{tail_to_width, truncate_to_width},
    widgets::toast::centered_rect,
};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

/// Block cursor at the end of the editable line, matching the search field.
const CURSOR: &str = "\u{258F}";

/// Box size as a share of the frame (spec §6).
const WIDTH_PCT: u16 = 50;
const HEIGHT_PCT: u16 = 20;

pub fn draw(f: &mut Frame, area: Rect, s: &AppState, t: &Theme) {
    let Some(modal) = &s.modal else { return };
    if area.width == 0 || area.height == 0 {
        return;
    }

    if matches!(modal, Modal::Help | Modal::Login { .. }) {
        return;
    }

    let rect = match modal {
        Modal::PickPlaylist { choices, .. } => {
            // Keep PickPlaylist compact on wide/large displays:
            // - width capped at 54 columns (comfortable for playlist names without sprawling across screen)
            // - height adapts to choices count (between 8 and 15 rows) so there are no empty blank rows
            let w = (area.width * 45 / 100).clamp(42, 54).min(area.width);
            let content_rows = (choices.len() as u16 + 4).clamp(8, 15);
            let h = content_rows.min(area.height);
            let x = area.x + area.width.saturating_sub(w) / 2;
            let y = area.y + area.height.saturating_sub(h) / 2;
            Rect::new(x, y, w, h)
        }
        _ => centered_rect(WIDTH_PCT, HEIGHT_PCT, area),
    };

    if rect.width == 0 || rect.height == 0 {
        return;
    }

    let (title, lines) = match modal {
        Modal::Confirm { text, .. } => {
            let inner_w = rect.width.saturating_sub(2) as usize;
            let inner_h = rect.height.saturating_sub(2) as usize;
            let mut lines = Vec::new();
            lines.push(Line::from(Span::styled(
                truncate_to_width(text, inner_w),
                Style::default().fg(t.fg_bright),
            )));
            while lines.len() + 1 < inner_h {
                lines.push(Line::from(""));
            }
            lines.push(Line::from(Span::styled(
                truncate_to_width("[y] yes   [n] no   [esc] cancel", inner_w),
                Style::default().fg(t.fg_dim),
            )));
            (" Confirm ".to_string(), lines)
        }
        Modal::Prompt { title, value, .. } => {
            let inner_w = rect.width.saturating_sub(2) as usize;
            let inner_h = rect.height.saturating_sub(2) as usize;
            let mut lines = Vec::new();
            lines.push(Line::from(Span::styled(
                truncate_to_width(title, inner_w),
                Style::default().fg(t.fg_bright),
            )));
            let val_with_cursor = format!("{value}{CURSOR}");
            lines.push(Line::from(Span::styled(
                tail_to_width(&val_with_cursor, inner_w),
                Style::default().fg(t.fg),
            )));
            while lines.len() + 1 < inner_h {
                lines.push(Line::from(""));
            }
            lines.push(Line::from(Span::styled(
                truncate_to_width("[enter] save   [esc] cancel", inner_w),
                Style::default().fg(t.fg_dim),
            )));
            (" Edit ".to_string(), lines)
        }
        Modal::PickPlaylist {
            targets,
            choices,
            selected,
        } => {
            let inner_w = rect.width.saturating_sub(2) as usize;
            let inner_h = rect.height.saturating_sub(2) as usize;
            let selected_idx = (*selected).min(choices.len().saturating_sub(1));
            let title = if choices.len() > 1 {
                format!(" Add to playlist ({}/{}) ", selected_idx + 1, choices.len())
            } else {
                " Add to playlist ".to_string()
            };

            let mut lines = Vec::new();
            let header = format!("Add {} track(s) to which playlist?", targets.len());
            lines.push(Line::from(Span::styled(
                truncate_to_width(&header, inner_w),
                Style::default().fg(t.fg_bright),
            )));

            let available_rows = inner_h.saturating_sub(2);
            let start = if choices.len() <= available_rows || selected_idx < available_rows {
                0
            } else {
                (selected_idx + 1 - available_rows)
                    .min(choices.len().saturating_sub(available_rows))
            };
            let end = (start + available_rows).min(choices.len());

            for (idx, (_, ptitle)) in choices.iter().enumerate().take(end).skip(start) {
                let is_sel = idx == selected_idx;
                let marker = if is_sel { "> " } else { "  " };
                let text = tail_to_width(&format!("{marker}{ptitle}"), inner_w);
                let style = if is_sel {
                    Style::default()
                        .fg(t.fg_bright)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(t.fg)
                };
                lines.push(Line::from(Span::styled(text, style)));
            }

            while lines.len() + 1 < inner_h {
                lines.push(Line::from(""));
            }
            lines.push(Line::from(Span::styled(
                truncate_to_width("[enter] add   [esc] cancel", inner_w),
                Style::default().fg(t.fg_dim),
            )));

            (title, lines)
        }
        Modal::Help | Modal::Login { .. } => return,
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            title,
            Style::default()
                .fg(t.fg_bright)
                .add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(t.fg_dim));
    let inner = block.inner(rect);

    f.render_widget(Clear, rect);
    f.render_widget(block, rect);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    f.render_widget(Paragraph::new(lines), inner);
}

#[cfg(test)]
mod tests {
    use crate::{
        app::{AppState, ConfirmAction, Modal, PromptAction},
        event::{AppEvent, InputAction},
        keymap::KeyMap,
        theme::Theme,
    };
    use ytm_core::PlaylistId;

    fn text_of(s: &AppState) -> String {
        use ratatui::{Terminal, backend::TestBackend};
        let mut t = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let theme = Theme::default();
        t.draw(|f| {
            crate::render::render(
                f,
                s,
                &theme,
                &KeyMap::default(),
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

    fn confirming() -> AppState {
        AppState {
            modal: Some(Modal::Confirm {
                text: "Delete \"Focus\"?".into(),
                action: ConfirmAction::DeletePlaylist(PlaylistId::from("p1")),
            }),
            ..Default::default()
        }
    }

    fn prompting(value: &str) -> AppState {
        AppState {
            modal: Some(Modal::Prompt {
                title: "New playlist name".into(),
                value: value.to_owned(),
                action: PromptAction::CreatePlaylist,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn confirm_modal_shows_the_question_and_both_choices() {
        let text = text_of(&confirming());
        assert!(text.contains("Delete"));
        assert!(
            text.contains("y") && text.contains("n"),
            "must show the keys to press"
        );
    }

    #[test]
    fn prompt_modal_shows_the_title_and_current_value() {
        let text = text_of(&prompting("Chill"));
        assert!(text.contains("New playlist name"));
        assert!(text.contains("Chill"));
    }

    #[test]
    fn the_prompt_shows_a_cursor_so_it_looks_typeable() {
        // The user's report was two bugs: letters did not arrive, and the box
        // gave no sign it was a text field at all.
        assert!(
            text_of(&prompting("")).contains('\u{258f}'),
            "an empty prompt must still show where typing goes"
        );
        assert!(text_of(&prompting("Chill")).contains('\u{258f}'));
    }

    #[test]
    fn a_confirm_box_has_no_cursor() {
        // It is not a text field; a cursor there would invite typing.
        assert!(!text_of(&confirming()).contains('\u{258f}'));
    }

    #[test]
    fn typing_in_a_prompt_appends_to_the_value() {
        let mut s = prompting("");
        s.apply(AppEvent::Input(InputAction::Char('a')));
        s.apply(AppEvent::Input(InputAction::Char('b')));
        match &s.modal {
            Some(Modal::Prompt { value, .. }) => assert_eq!(value, "ab"),
            other => panic!("expected a prompt, got {other:?}"),
        }
    }

    #[test]
    fn backspace_in_a_prompt_deletes_the_last_character() {
        let mut s = prompting("abc");
        s.apply(AppEvent::Input(InputAction::Backspace));
        match &s.modal {
            Some(Modal::Prompt { value, .. }) => assert_eq!(value, "ab"),
            other => panic!("expected a prompt, got {other:?}"),
        }
    }

    #[test]
    fn backspace_on_an_empty_prompt_does_not_panic() {
        let mut s = prompting("");
        s.apply(AppEvent::Input(InputAction::Backspace));
        assert!(s.modal.is_some());
    }

    #[test]
    fn escape_closes_any_modal_without_acting() {
        let mut s = confirming();
        s.apply(AppEvent::Input(InputAction::Cancel));
        assert!(s.modal.is_none());
        assert!(s.pending.is_empty(), "cancelling must not start a mutation");
    }

    #[test]
    fn a_multibyte_character_deletes_cleanly() {
        // Byte-slicing here would panic; pop() is char-aware.
        let mut s = prompting("日本");
        s.apply(AppEvent::Input(InputAction::Backspace));
        match &s.modal {
            Some(Modal::Prompt { value, .. }) => assert_eq!(value, "日"),
            other => panic!("expected a prompt, got {other:?}"),
        }
    }

    #[test]
    fn a_letter_bound_to_a_command_is_text_inside_a_prompt() {
        // `q` must not quit while the user is naming a playlist.
        let mut s = prompting("");
        for c in "quiet".chars() {
            s.apply(AppEvent::Input(InputAction::Char(c)));
        }
        assert!(!s.should_quit, "typing must not trigger commands");
        match &s.modal {
            Some(Modal::Prompt { value, .. }) => assert_eq!(value, "quiet"),
            other => panic!("expected a prompt, got {other:?}"),
        }
    }

    #[test]
    fn typing_never_reaches_a_confirm_box() {
        // A confirm has no text field; stray letters must not be swallowed
        // silently into one, nor act on the list behind it.
        let mut s = confirming();
        s.apply(AppEvent::Input(InputAction::Char('x')));
        match &s.modal {
            Some(Modal::Confirm { text, .. }) => assert!(text.contains("Delete")),
            other => panic!("expected a confirm, got {other:?}"),
        }
    }

    #[test]
    fn the_prompt_shows_a_long_value_without_overflowing_the_box() {
        // 80x24 = 1920 cells; layout math that overflows would panic or clip.
        let s = prompting(&"x".repeat(300));
        assert_eq!(text_of(&s).chars().count(), 1920);
    }

    #[test]
    fn a_modal_survives_a_terminal_too_small_to_hold_it() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut t = Terminal::new(TestBackend::new(6, 3)).unwrap();
        let theme = Theme::default();
        t.draw(|f| {
            crate::render::render(
                f,
                &confirming(),
                &theme,
                &KeyMap::default(),
                &mut crate::widgets::art::ArtCache::disabled(),
            )
        })
        .unwrap();
        t.draw(|f| {
            crate::render::render(
                f,
                &prompting("abc"),
                &theme,
                &KeyMap::default(),
                &mut crate::widgets::art::ArtCache::disabled(),
            )
        })
        .unwrap();
    }

    #[test]
    fn nothing_is_drawn_when_no_modal_is_open() {
        let s = AppState {
            pane: crate::app::Pane::Songs,
            tracks: vec![ytm_core::Track::stub("v1", "Roygbiv")],
            ..Default::default()
        };
        let text = text_of(&s);
        assert!(text.contains("Roygbiv"));
        assert!(!text.contains("[esc]"));
    }

    fn picking_playlist(selected: usize) -> AppState {
        let choices = (0..15)
            .map(|i| (PlaylistId::from(format!("p{i}")), format!("Playlist {i}")))
            .collect();
        AppState {
            modal: Some(Modal::PickPlaylist {
                targets: vec![ytm_core::VideoId::from("v1")],
                choices,
                selected,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn pick_playlist_renders_selected_item_at_start_and_end() {
        let text_start = text_of(&picking_playlist(0));
        assert!(
            text_start.contains("> Playlist 0"),
            "first item selected must be visible"
        );

        let text_end = text_of(&picking_playlist(14));
        assert!(
            text_end.contains("> Playlist 14"),
            "last item selected must be visible when scrolled"
        );
    }

    #[test]
    fn pick_playlist_modal_is_compact_on_wide_screens() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut t = Terminal::new(TestBackend::new(200, 50)).unwrap();
        let theme = Theme::default();
        let s = picking_playlist(0);
        t.draw(|f| {
            crate::render::render(
                f,
                &s,
                &theme,
                &KeyMap::default(),
                &mut crate::widgets::art::ArtCache::disabled(),
            )
        })
        .unwrap();

        let buf = t.backend().buffer().clone();
        // Modal is centered and at most 54 columns wide.
        // On a 200 column screen, modal x range is ~73..127.
        // Columns 0..70 should be outside the modal (not contain modal box characters)
        let left_cell = &buf[(20, 25)];
        assert_ne!(
            left_cell.symbol(),
            "│",
            "modal should not extend to column 20 on wide screens"
        );
    }
}
