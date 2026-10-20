//! Key -> InputAction resolution. Focus-sensitive: while typing, letters are
//! letters, not commands.

use crate::{app::Focus, event::InputAction};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::HashMap;

pub struct KeyMap {
    /// Char bindings active in navigation focus.
    chars: HashMap<char, InputAction>,
}

/// Multi-key chord state, kept outside the configured keymap.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Pending {
    #[default]
    None,
    /// `z` seen; the next key completes or abandons the chord.
    Z,
}

impl Default for KeyMap {
    /// `ui.vim_keys` defaults to true.
    fn default() -> Self {
        Self::new(true)
    }
}

impl KeyMap {
    /// The built-in bindings. `vim_keys = false` drops `h`/`j`/`k`/`l`, leaving
    /// the arrow keys to navigate; `g`/`G` stay Home/End, and `J`/`K` stay
    /// MoveEntryDown/Up since those are not navigation.
    pub fn new(vim_keys: bool) -> Self {
        let mut chars = HashMap::new();
        for (c, a) in [
            ('j', InputAction::Down),
            ('k', InputAction::Up),
            ('h', InputAction::Left),
            ('l', InputAction::Right),
            ('g', InputAction::Home),
            ('G', InputAction::End),
            ('q', InputAction::Quit),
            ('?', InputAction::OpenHelp),
            // Owner request: `/` filters the list in place (like less/vim), and
            // `S` opens the server-side search pane.
            ('/', InputAction::OpenFilter),
            ('S', InputAction::OpenSearch),
            ('u', InputAction::OpenQueue),
            (' ', InputAction::TogglePause),
            ('n', InputAction::NextTrack),
            ('p', InputAction::PrevTrack),
            ('f', InputAction::SeekForward),
            ('b', InputAction::SeekBack),
            ('+', InputAction::VolumeUp),
            ('-', InputAction::VolumeDown),
            ('m', InputAction::ToggleMute),
            ('s', InputAction::ToggleShuffle),
            ('r', InputAction::CycleRepeat),
            ('c', InputAction::FocusCurrent),
            ('a', InputAction::AddToQueue),
            ('A', InputAction::AddToPlaylist),
            ('N', InputAction::CreatePlaylist),
            ('R', InputAction::RenamePlaylist),
            ('D', InputAction::DeletePlaylist),
            ('x', InputAction::RemoveFromPlaylist),
            ('v', InputAction::ToggleMark),
            ('V', InputAction::ToggleVisual),
            ('t', InputAction::CycleTheme),
            (',', InputAction::EditConfig),
            ('e', InputAction::PlayNext),
            ('J', InputAction::MoveEntryDown),
            ('K', InputAction::MoveEntryUp),
            ('C', InputAction::ClearQueue),
            ('L', InputAction::Refresh),
            ('d', InputAction::Download),
        ] {
            chars.insert(c, a);
        }
        if !vim_keys {
            for c in ['h', 'j', 'k', 'l'] {
                chars.remove(&c);
            }
        }
        Self { chars }
    }
}

impl KeyMap {
    /// Resolve a chord; text fields treat `z` as text rather than a prefix.
    pub fn resolve_chord(
        &self,
        key: KeyEvent,
        focus: Focus,
        pending: Pending,
    ) -> (Option<InputAction>, Pending) {
        let typing = matches!(focus, Focus::SearchInput | Focus::FilterInput);
        if !typing && !key.modifiers.contains(KeyModifiers::CONTROL) {
            match (pending, key.code) {
                // `zz` centres. A second `z` is the only completion; anything
                // else abandons the prefix and is handled normally, so a
                // mistyped `z` costs nothing.
                (Pending::Z, KeyCode::Char('z')) => {
                    return (Some(InputAction::CenterOnCursor), Pending::None);
                }
                (Pending::None, KeyCode::Char('z')) => return (None, Pending::Z),
                (Pending::Z, _) => return (self.resolve(key, focus), Pending::None),
                _ => {}
            }
        }
        (self.resolve(key, focus), Pending::None)
    }

    pub fn resolve(&self, key: KeyEvent, focus: Focus) -> Option<InputAction> {
        // Ctrl-C escapes everything, including a text field.
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            // Ctrl+C first: it is the escape hatch and must not be shadowed by
            // any of the editing chords below.
            if key.code == KeyCode::Char('c') {
                return Some(InputAction::ForceQuit);
            }
            // The readline chords only mean anything where there is text and a
            // caret. Outside a field they stay unbound rather than being given
            // some second meaning in a list.
            if focus == Focus::SearchInput || focus == Focus::FilterInput {
                return match key.code {
                    KeyCode::Char('w') => Some(InputAction::DeleteWordBack),
                    KeyCode::Char('a') => Some(InputAction::LineStart),
                    KeyCode::Char('e') => Some(InputAction::LineEnd),
                    KeyCode::Left => Some(InputAction::WordLeft),
                    KeyCode::Right => Some(InputAction::WordRight),
                    // Paging the results while the query has focus is still useful.
                    KeyCode::Char('d') => Some(InputAction::PageDown),
                    KeyCode::Char('u') => Some(InputAction::PageUp),
                    _ => None,
                };
            }
            return match key.code {
                KeyCode::Char('d') => Some(InputAction::PageDown),
                KeyCode::Char('u') => Some(InputAction::PageUp),
                _ => None,
            };
        }

        // A filter field is a text field: the same editing keys, but Esc and
        // Enter mean "stop editing", not "clear".
        if focus == Focus::FilterInput {
            return match key.code {
                KeyCode::Char(c) => Some(InputAction::Char(c)),
                KeyCode::Backspace => Some(InputAction::Backspace),
                KeyCode::Esc => Some(InputAction::Cancel),
                KeyCode::Enter => Some(InputAction::Confirm),
                KeyCode::Down => Some(InputAction::Down),
                KeyCode::Up => Some(InputAction::Up),
                _ => None,
            };
        }

        if focus == Focus::SearchInput {
            return match key.code {
                KeyCode::Char(c) => Some(InputAction::Char(c)),
                KeyCode::Backspace => Some(InputAction::Backspace),
                KeyCode::Esc => Some(InputAction::Cancel),
                KeyCode::Enter => Some(InputAction::Confirm),
                // Down/Up move through the results below; Left/Right move the
                // caret, which is what they mean in every other text field.
                KeyCode::Down => Some(InputAction::Down),
                KeyCode::Up => Some(InputAction::Up),
                KeyCode::Left => Some(InputAction::CharLeft),
                KeyCode::Right => Some(InputAction::CharRight),
                KeyCode::Home => Some(InputAction::LineStart),
                KeyCode::End => Some(InputAction::LineEnd),
                _ => None,
            };
        }

        match key.code {
            // Source digits are fixed; 9 and 0 remain unbound.
            KeyCode::Char(c @ '1'..='8') => Some(InputAction::GoTo(c as u8 - b'0')),
            KeyCode::Char(c) => self.chars.get(&c).cloned(),
            KeyCode::Down => Some(InputAction::Down),
            KeyCode::Up => Some(InputAction::Up),
            KeyCode::Left => Some(InputAction::Left),
            KeyCode::Right => Some(InputAction::Right),
            KeyCode::Home => Some(InputAction::Home),
            KeyCode::End => Some(InputAction::End),
            KeyCode::PageDown => Some(InputAction::PageDown),
            KeyCode::PageUp => Some(InputAction::PageUp),
            KeyCode::Enter => Some(InputAction::Confirm),
            KeyCode::Esc => Some(InputAction::Cancel),
            KeyCode::Tab => Some(InputAction::NextPane),
            KeyCode::BackTab => Some(InputAction::PrevPane),
            _ => None,
        }
    }

    /// For the help overlay, sorted for stable display.
    pub fn bindings(&self) -> Vec<(String, InputAction)> {
        let mut v: Vec<_> = self
            .chars
            .iter()
            .map(|(c, a)| (c.to_string(), a.clone()))
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    /// Override defaults from `[keys]` in config. Key names are snake_case
    /// action names; values are single characters.
    pub fn from_toml_str(s: &str) -> Result<Self, toml::de::Error> {
        Self::from_toml_str_with(s, true)
    }

    /// `[keys]` applied on top of the built-in map. Overrides run *after* the
    /// vim-key removal, so `down = "j"` with `vim_keys = false` still binds `j` —
    /// the explicit instruction is the more specific one.
    pub fn from_toml_str_with(s: &str, vim_keys: bool) -> Result<Self, toml::de::Error> {
        let table: HashMap<String, String> = toml::from_str(s)?;
        let mut m = Self::new(vim_keys);
        for (action_name, ch) in table {
            let Some(c) = ch.chars().next() else { continue };
            if let Some(action) = action_from_name(&action_name) {
                m.chars.retain(|_, a| *a != action); // a binding is exclusive
                m.chars.insert(c, action);
            }
        }
        Ok(m)
    }
}

impl KeyMap {
    /// Every action name `[keys]` accepts. Used to check the shipped example
    /// config against the real table — a typo there is silently ignored, so the
    /// user rebinds a key and nothing happens.
    pub fn action_names() -> &'static [&'static str] {
        ACTION_NAMES
    }
}

/// Kept beside `action_from_name`; the test below pins the two together.
const ACTION_NAMES: &[&str] = &[
    "down",
    "up",
    "left",
    "right",
    "home",
    "end",
    "quit",
    "toggle_pause",
    "next_track",
    "prev_track",
    "toggle_shuffle",
    "cycle_repeat",
    "open_search",
    "open_queue",
    "open_help",
    "move_entry_up",
    "move_entry_down",
    "clear_queue",
    "toggle_mark",
    "toggle_visual",
    "cycle_theme",
    "edit_config",
    "add_to_queue",
    "play_next",
    "add_to_playlist",
    "remove_from_playlist",
    "create_playlist",
    "rename_playlist",
    "delete_playlist",
    "seek_forward",
    "seek_back",
    "volume_up",
    "volume_down",
    "toggle_mute",
    "refresh",
    "open_filter",
    "center_on_cursor",
    "focus_current",
    "page_down",
    "page_up",
    "download",
];

fn action_from_name(n: &str) -> Option<InputAction> {
    Some(match n {
        "down" => InputAction::Down,
        "up" => InputAction::Up,
        "left" => InputAction::Left,
        "right" => InputAction::Right,
        "quit" => InputAction::Quit,
        "toggle_pause" => InputAction::TogglePause,
        "next_track" => InputAction::NextTrack,
        "prev_track" => InputAction::PrevTrack,
        "toggle_shuffle" => InputAction::ToggleShuffle,
        "cycle_repeat" => InputAction::CycleRepeat,
        "open_search" => InputAction::OpenSearch,
        "open_queue" => InputAction::OpenQueue,
        "open_help" => InputAction::OpenHelp,
        "move_entry_up" => InputAction::MoveEntryUp,
        "move_entry_down" => InputAction::MoveEntryDown,
        "clear_queue" => InputAction::ClearQueue,
        "toggle_mark" => InputAction::ToggleMark,
        "toggle_visual" => InputAction::ToggleVisual,
        "cycle_theme" => InputAction::CycleTheme,
        "edit_config" => InputAction::EditConfig,
        "add_to_queue" => InputAction::AddToQueue,
        "play_next" => InputAction::PlayNext,
        "add_to_playlist" => InputAction::AddToPlaylist,
        "remove_from_playlist" => InputAction::RemoveFromPlaylist,
        "create_playlist" => InputAction::CreatePlaylist,
        "rename_playlist" => InputAction::RenamePlaylist,
        "delete_playlist" => InputAction::DeletePlaylist,
        "seek_forward" => InputAction::SeekForward,
        "seek_back" => InputAction::SeekBack,
        "volume_up" => InputAction::VolumeUp,
        "volume_down" => InputAction::VolumeDown,
        "toggle_mute" => InputAction::ToggleMute,
        "refresh" => InputAction::Refresh,
        "open_filter" => InputAction::OpenFilter,
        "center_on_cursor" => InputAction::CenterOnCursor,
        "focus_current" => InputAction::FocusCurrent,
        "page_down" => InputAction::PageDown,
        "page_up" => InputAction::PageUp,
        "home" => InputAction::Home,
        "end" => InputAction::End,
        "download" => InputAction::Download,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Focus;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn vim_keys_off_unbinds_hjkl_but_leaves_the_arrows() {
        let m = KeyMap::new(false);
        for ch in ['h', 'j', 'k', 'l'] {
            assert_eq!(
                m.resolve(key(ch), Focus::Main),
                None,
                "{ch} must be unbound when ui.vim_keys = false"
            );
        }
        assert_eq!(
            m.resolve(
                KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
                Focus::Main
            ),
            Some(InputAction::Down),
            "the arrow keys are how you navigate without vim keys"
        );
        assert_eq!(
            m.resolve(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), Focus::Main),
            Some(InputAction::Up)
        );
    }

    #[test]
    fn vim_keys_off_leaves_g_and_the_other_letters_alone() {
        // Owner decision: h/j/k/l only. g/G stay Home/End.
        let m = KeyMap::new(false);
        assert_eq!(m.resolve(key('g'), Focus::Main), Some(InputAction::Home));
        assert_eq!(m.resolve(key('G'), Focus::Main), Some(InputAction::End));
        // And nothing else was collateral damage.
        assert_eq!(m.resolve(key('q'), Focus::Main), Some(InputAction::Quit));
        assert_eq!(
            m.resolve(key('s'), Focus::Main),
            Some(InputAction::ToggleShuffle)
        );
        // J/K are MoveEntryDown/Up, not navigation — they must survive.
        assert_eq!(
            m.resolve(key('J'), Focus::Main),
            Some(InputAction::MoveEntryDown)
        );
        assert_eq!(
            m.resolve(key('K'), Focus::Main),
            Some(InputAction::MoveEntryUp)
        );
    }

    #[test]
    fn default_still_means_vim_keys_on() {
        let m = KeyMap::default();
        assert_eq!(
            m.resolve(key('j'), Focus::Main),
            Some(InputAction::Down),
            "vim_keys defaults to true; default() must not change meaning"
        );
    }

    #[test]
    fn an_explicit_rebind_wins_over_vim_keys_off() {
        // The user turned vim keys off and then deliberately bound j. The
        // explicit binding is the more specific instruction.
        let m = KeyMap::from_toml_str_with(r#"down = "j""#, false).unwrap();
        assert_eq!(m.resolve(key('j'), Focus::Main), Some(InputAction::Down));
        assert_eq!(
            m.resolve(key('k'), Focus::Main),
            None,
            "k was not rebound, so it stays unbound"
        );
    }

    #[test]
    fn vim_navigation_keys_map_to_movement() {
        let m = KeyMap::default();
        assert_eq!(m.resolve(key('j'), Focus::Main), Some(InputAction::Down));
        assert_eq!(m.resolve(key('k'), Focus::Main), Some(InputAction::Up));
        assert_eq!(m.resolve(key('h'), Focus::Main), Some(InputAction::Left));
        assert_eq!(m.resolve(key('l'), Focus::Main), Some(InputAction::Right));
    }

    #[test]
    fn arrow_keys_work_alongside_vim_keys() {
        let m = KeyMap::default();
        let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(m.resolve(down, Focus::Main), Some(InputAction::Down));
    }

    #[test]
    fn transport_keys_are_bound() {
        let m = KeyMap::default();
        assert_eq!(
            m.resolve(key(' '), Focus::Main),
            Some(InputAction::TogglePause)
        );
        assert_eq!(
            m.resolve(key('n'), Focus::Main),
            Some(InputAction::NextTrack)
        );
        assert_eq!(
            m.resolve(key('p'), Focus::Main),
            Some(InputAction::PrevTrack)
        );
        assert_eq!(
            m.resolve(key('s'), Focus::Main),
            Some(InputAction::ToggleShuffle)
        );
        assert_eq!(
            m.resolve(key('r'), Focus::Main),
            Some(InputAction::CycleRepeat)
        );
    }

    #[test]
    fn c_focuses_the_current_song() {
        let m = KeyMap::default();
        assert_eq!(
            m.resolve(key('c'), Focus::Main),
            Some(InputAction::FocusCurrent)
        );
    }

    #[test]
    fn typing_in_the_search_field_produces_characters_not_commands() {
        // Critical: 'j' while typing must insert a letter, not scroll the list.
        let m = KeyMap::default();
        assert_eq!(
            m.resolve(key('j'), Focus::SearchInput),
            Some(InputAction::Char('j'))
        );
        assert_eq!(
            m.resolve(key(' '), Focus::SearchInput),
            Some(InputAction::Char(' '))
        );
    }

    #[test]
    fn every_letter_is_text_while_a_prompt_is_open() {
        // Resolved against the focus a prompt reports, so a name containing
        // command letters types normally.
        let m = KeyMap::default();
        for c in "quit and next".chars() {
            assert_eq!(
                m.resolve(key(c), Focus::SearchInput),
                Some(InputAction::Char(c)),
                "{c:?} must be text"
            );
        }
    }

    #[test]
    fn escape_cancels_from_the_search_field() {
        let m = KeyMap::default();
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(
            m.resolve(esc, Focus::SearchInput),
            Some(InputAction::Cancel)
        );
    }

    #[test]
    fn ctrl_c_always_quits_even_while_typing() {
        // ForceQuit, not Quit: behaviour.confirm_on_quit must not be able to
        // shadow the escape hatch.
        let m = KeyMap::default();
        let c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(
            m.resolve(c, Focus::SearchInput),
            Some(InputAction::ForceQuit)
        );
        assert_eq!(m.resolve(c, Focus::Main), Some(InputAction::ForceQuit));
    }

    #[test]
    fn the_queue_editing_keys_are_reachable() {
        // The dispatch tests pass actions in directly, so without this nothing
        // proves a user can actually produce them.
        let m = KeyMap::default();
        assert_eq!(
            m.resolve(key('J'), Focus::Main),
            Some(InputAction::MoveEntryDown)
        );
        assert_eq!(
            m.resolve(key('K'), Focus::Main),
            Some(InputAction::MoveEntryUp)
        );
        assert_eq!(
            m.resolve(key('C'), Focus::Main),
            Some(InputAction::ClearQueue)
        );
        // Lowercase must stay navigation, or reordering would hijack j/k.
        assert_eq!(m.resolve(key('j'), Focus::Main), Some(InputAction::Down));
        assert_eq!(m.resolve(key('k'), Focus::Main), Some(InputAction::Up));
    }

    #[test]
    fn unbound_keys_resolve_to_nothing() {
        let m = KeyMap::default();
        assert_eq!(m.resolve(key('Z'), Focus::Main), None);
    }

    #[test]
    fn a_user_override_replaces_the_default_binding() {
        let m = KeyMap::from_toml_str(r#"down = "e""#).unwrap();
        assert_eq!(m.resolve(key('e'), Focus::Main), Some(InputAction::Down));
    }

    #[test]
    fn bindings_list_is_non_empty_for_the_help_overlay() {
        // FR-U2: '?' must show something real.
        assert!(!KeyMap::default().bindings().is_empty());
    }

    #[test]
    fn number_keys_jump_straight_to_a_source() {
        let m = KeyMap::default();
        assert_eq!(m.resolve(key('1'), Focus::Main), Some(InputAction::GoTo(1)));
        assert_eq!(m.resolve(key('8'), Focus::Main), Some(InputAction::GoTo(8)));
    }

    #[test]
    fn digits_outside_the_source_range_are_not_bound() {
        // 9 and 0 name no source; binding them would swallow the key.
        let m = KeyMap::default();
        assert_eq!(m.resolve(key('9'), Focus::Main), None);
        assert_eq!(m.resolve(key('0'), Focus::Main), None);
    }

    #[test]
    fn a_digit_typed_into_the_search_field_is_a_character_not_a_jump() {
        // Searching for "90's" must be possible.
        let m = KeyMap::default();
        assert_eq!(
            m.resolve(key('9'), Focus::SearchInput),
            Some(InputAction::Char('9'))
        );
        assert_eq!(
            m.resolve(key('1'), Focus::SearchInput),
            Some(InputAction::Char('1'))
        );
    }

    #[test]
    fn shift_v_starts_a_visual_range_and_lowercase_v_still_marks_one_row() {
        // The pair must stay distinct: if `V` resolved to ToggleMark the range
        // key would silently be the old single-row mark.
        let m = KeyMap::default();
        assert_eq!(
            m.resolve(key('V'), Focus::Main),
            Some(InputAction::ToggleVisual)
        );
        assert_eq!(
            m.resolve(key('v'), Focus::Main),
            Some(InputAction::ToggleMark)
        );
    }

    #[test]
    fn a_capital_v_typed_into_a_prompt_is_a_letter() {
        // A playlist named "Vinyl" must be typeable.
        let m = KeyMap::default();
        assert_eq!(
            m.resolve(key('V'), Focus::SearchInput),
            Some(InputAction::Char('V'))
        );
    }

    #[test]
    fn the_visual_binding_can_be_remapped_from_config() {
        let m = KeyMap::from_toml_str(r#"toggle_visual = "z""#).unwrap();
        assert_eq!(
            m.resolve(key('z'), Focus::Main),
            Some(InputAction::ToggleVisual)
        );
        assert_eq!(
            m.resolve(key('V'), Focus::Main),
            None,
            "a remap is exclusive, so the default must be gone"
        );
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn ctrl_code(k: KeyCode) -> KeyEvent {
        KeyEvent::new(k, KeyModifiers::CONTROL)
    }

    #[test]
    fn ctrl_w_in_the_search_field_deletes_a_word() {
        // Through `resolve`, not straight into the reducer: the reducer handling
        // an action proves nothing if no key press can produce it.
        let m = KeyMap::default();
        assert_eq!(
            m.resolve(ctrl('w'), Focus::SearchInput),
            Some(InputAction::DeleteWordBack)
        );
    }

    #[test]
    fn ctrl_arrows_in_the_search_field_move_by_word() {
        let m = KeyMap::default();
        assert_eq!(
            m.resolve(ctrl_code(KeyCode::Left), Focus::SearchInput),
            Some(InputAction::WordLeft)
        );
        assert_eq!(
            m.resolve(ctrl_code(KeyCode::Right), Focus::SearchInput),
            Some(InputAction::WordRight)
        );
    }

    #[test]
    fn ctrl_a_and_ctrl_e_jump_to_the_ends_of_the_search_line() {
        let m = KeyMap::default();
        assert_eq!(
            m.resolve(ctrl('a'), Focus::SearchInput),
            Some(InputAction::LineStart)
        );
        assert_eq!(
            m.resolve(ctrl('e'), Focus::SearchInput),
            Some(InputAction::LineEnd)
        );
    }

    #[test]
    fn plain_arrows_in_the_search_field_move_the_caret() {
        // Left/Right were unhandled in the field, so the per-character motion
        // the reducer implements was unreachable too.
        let m = KeyMap::default();
        assert_eq!(
            m.resolve(
                KeyEvent::new(KeyCode::Left, KeyModifiers::NONE),
                Focus::SearchInput
            ),
            Some(InputAction::CharLeft)
        );
        assert_eq!(
            m.resolve(
                KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
                Focus::SearchInput
            ),
            Some(InputAction::CharRight)
        );
    }

    #[test]
    fn ctrl_c_still_quits_from_the_search_field() {
        // The chords above must not shadow the escape hatch — and neither may
        // behaviour.confirm_on_quit, which is why this is ForceQuit.
        let m = KeyMap::default();
        assert_eq!(
            m.resolve(ctrl('c'), Focus::SearchInput),
            Some(InputAction::ForceQuit)
        );
        assert_eq!(
            m.resolve(ctrl('c'), Focus::Main),
            Some(InputAction::ForceQuit)
        );
    }

    #[test]
    fn ctrl_d_and_ctrl_u_still_page_in_a_list() {
        let m = KeyMap::default();
        assert_eq!(
            m.resolve(ctrl('d'), Focus::Main),
            Some(InputAction::PageDown)
        );
        assert_eq!(m.resolve(ctrl('u'), Focus::Main), Some(InputAction::PageUp));
    }

    #[test]
    fn the_editing_chords_do_nothing_outside_a_text_field() {
        // In a list, Ctrl+W must not silently mean something else.
        let m = KeyMap::default();
        assert_eq!(m.resolve(ctrl('w'), Focus::Main), None);
        assert_eq!(m.resolve(ctrl('e'), Focus::Main), None);
        assert_eq!(m.resolve(ctrl_code(KeyCode::Left), Focus::Main), None);
    }

    #[test]
    fn the_theme_and_config_keys_are_reachable_from_a_key_press() {
        // Both are handled in the event loop, which no unit test enters, so
        // without this nothing proves a user can produce them.
        let m = KeyMap::default();
        assert_eq!(
            m.resolve(key('t'), Focus::Main),
            Some(InputAction::CycleTheme)
        );
        assert_eq!(
            m.resolve(key(','), Focus::Main),
            Some(InputAction::EditConfig)
        );
    }

    #[test]
    fn t_and_comma_are_text_while_typing_a_name() {
        // A playlist called "night, take 2" must be typeable.
        let m = KeyMap::default();
        assert_eq!(
            m.resolve(key('t'), Focus::SearchInput),
            Some(InputAction::Char('t'))
        );
        assert_eq!(
            m.resolve(key(','), Focus::SearchInput),
            Some(InputAction::Char(','))
        );
    }

    #[test]
    fn every_action_name_the_config_accepts_maps_to_a_real_action() {
        // ACTION_NAMES is hand-maintained beside action_from_name; a name in one
        // and not the other is a binding the user can write that does nothing.
        for n in KeyMap::action_names() {
            assert!(
                action_from_name(n).is_some(),
                "{n:?} is offered to users but maps to no action"
            );
        }
    }

    #[test]
    fn d_key_resolves_to_download_action() {
        let km = KeyMap::default();
        assert_eq!(
            km.resolve(key('d'), Focus::Main),
            Some(InputAction::Download)
        );
    }
}
